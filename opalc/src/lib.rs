use std::collections::HashMap;

use codespan_reporting::term::{
    self,
    termcolor::{ColorChoice, StandardStream},
};

pub mod ast;
pub mod codegen;
pub mod ir;
pub mod lexer;
pub mod lower;
pub mod sexpr;
pub mod typecheck;

/// Compile without any imports (single-file or when imports are already resolved).
pub fn compile(module_name: &str, source: &str) -> Option<String> {
    compile_with_imports(
        module_name,
        source,
        HashMap::new(),
        &HashMap::new(),
        HashMap::new(),
        &[],
        &HashMap::new(),
    )
}

/// Compile with import resolution.
/// - `imports`: unqualified name → module (from `use` declarations)
/// - `module_exports`: module name → exported function names (for validating qualified calls)
/// - `imported_type_decls`: pub type declarations from imported modules (brings constructors into scope)
/// - `imported_schemes`: real type schemes from imported modules (keyed by "fn" or "module/fn")
pub fn compile_with_imports(
    module_name: &str,
    source: &str,
    imports: HashMap<String, String>,
    module_exports: &HashMap<String, Vec<String>>,
    module_aliases: HashMap<String, String>,
    imported_type_decls: &[ast::TypeDecl],
    imported_schemes: &typecheck::TypeEnv,
) -> Option<String> {
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(source).lex();
    let writer = StandardStream::stderr(ColorChoice::Always);
    let config = codespan_reporting::term::Config::default();

    let file_name = format!("{module_name}.opal");
    let file_id = lowerer.add_file(file_name, source.to_string());

    let sexprs = match crate::sexpr::SExprParser::new(tokens, file_id).parse() {
        Ok(res) => res,
        Err(diag) => {
            term::emit_to_write_style(&mut writer.lock(), &config, &lowerer.files, &diag).unwrap();
            return None;
        }
    };

    let decls = lowerer.lower_file(file_id, &sexprs);

    for diag in &lowerer.diagnostics {
        term::emit_to_write_style(&mut writer.lock(), &config, &lowerer.files, diag).unwrap();
    }
    if !lowerer.diagnostics.is_empty() {
        return None;
    }

    // Validate `use` declarations — emit a proper diagnostic for unknown modules
    let mut use_errors = false;
    for decl in &decls {
        if let ast::Declaration::Use {
            path: (_, mod_name),
            span,
            ..
        } = decl
            && !module_exports.contains_key(mod_name.as_str())
        {
            let diag = codespan_reporting::diagnostic::Diagnostic::error()
                .with_message(format!("unknown module `{mod_name}`"))
                .with_labels(vec![
                    codespan_reporting::diagnostic::Label::primary(file_id, span.clone())
                        .with_message(format!("`{mod_name}` is not a module in this project")),
                ]);
            term::emit_to_write_style(&mut writer.lock(), &config, &lowerer.files, &diag).unwrap();
            use_errors = true;
        }
    }
    if use_errors {
        return None;
    }

    let mut checker = typecheck::TypeChecker::new();
    let mut env = typecheck::primitive_env();

    // Seed env with constructors and field accessors from imported type declarations
    for type_decl in imported_type_decls {
        env.extend(typecheck::constructor_schemes(type_decl));
    }

    // Seed env with real schemes from imported modules where available,
    // falling back to ∀a. a for names with no known type.
    env.extend(imported_schemes.clone());

    // Collect the names we still need to seed (not covered by imported_schemes).
    let import_names: Vec<String> = imports.keys().cloned().collect();
    let used_modules: std::collections::HashSet<&str> = decls
        .iter()
        .filter_map(|d| {
            if let ast::Declaration::Use { path: (_, m), .. } = d {
                Some(m.as_str())
            } else {
                None
            }
        })
        .collect();
    let qualified_names: Vec<String> = module_exports
        .iter()
        .filter(|(m, _)| !used_modules.contains(m.as_str()))
        .flat_map(|(m, fns)| fns.iter().map(move |f| format!("{m}/{f}")))
        .collect();
    let unresolved: Vec<String> = import_names
        .iter()
        .chain(qualified_names.iter())
        .filter(|n| !env.contains_key(*n))
        .cloned()
        .collect();
    env.extend(typecheck::import_env(&unresolved));

    if let Err(err) = checker.check_program(&mut env, &decls, file_id) {
        let diagnostics = err.0.to_diagnostics(file_id, err.1.span());
        for diag in diagnostics {
            term::emit_to_write_style(&mut writer.lock(), &config, &lowerer.files, &diag).unwrap();
        }
        return None;
    }

    // Build codegen metadata from imported type declarations
    let mut imported_constructors: HashMap<String, usize> = HashMap::new();
    let mut imported_field_indices: HashMap<String, usize> = HashMap::new();
    for type_decl in imported_type_decls {
        match type_decl {
            ast::TypeDecl::Variant { constructors, .. } => {
                for (ctor_name, payload) in constructors {
                    imported_constructors
                        .insert(ctor_name.clone(), if payload.is_some() { 1 } else { 0 });
                }
            }
            ast::TypeDecl::Record { fields, .. } => {
                for (i, (field_name, _)) in fields.iter().enumerate() {
                    imported_field_indices.insert(field_name.clone(), i + 2);
                }
            }
        }
    }

    let module = codegen::lower_module(
        module_name,
        &decls,
        imports,
        module_aliases,
        imported_constructors,
        imported_field_indices,
    );
    Some(codegen::emit_module(&module))
}

/// Extract the names of `pub` top-level functions declared in a source file.
/// Only pub functions are importable by other modules.
pub fn exported_names(source: &str) -> Vec<String> {
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(source).lex();
    let file_id = lowerer.add_file("scan.opal".into(), source.into());
    let Ok(sexprs) = crate::sexpr::SExprParser::new(tokens, file_id).parse() else {
        return vec![];
    };
    let decls = lowerer.lower_file(file_id, &sexprs);
    decls
        .into_iter()
        .filter_map(|d| match d {
            ast::Declaration::Expression(ast::Expr::LetFunc {
                name, is_pub: true, ..
            }) => Some(name),
            ast::Declaration::ExternLet {
                name, is_pub: true, ..
            } => Some(name),
            _ => None,
        })
        .collect()
}

/// Extract the modules that a lib.opal publicly re-exports via `(pub use X)`.
/// Used to gate `(use std/io)` — io must be pub-used by lib.opal.
pub fn pub_reexports(source: &str) -> Vec<String> {
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(source).lex();
    let file_id = lowerer.add_file("scan.opal".into(), source.into());
    let Ok(sexprs) = crate::sexpr::SExprParser::new(tokens, file_id).parse() else {
        return vec![];
    };
    let decls = lowerer.lower_file(file_id, &sexprs);
    decls
        .into_iter()
        .filter_map(|d| {
            if let ast::Declaration::Use {
                is_pub: true,
                path: (_, module),
                ..
            } = d
            {
                Some(module)
            } else {
                None
            }
        })
        .collect()
}

/// Extract the `use` declarations from a source file.
/// Returns `(namespace, module)` pairs — local modules have an empty namespace.
pub fn used_modules(source: &str) -> Vec<(String, String)> {
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(source).lex();
    let file_id = lowerer.add_file("scan.opal".into(), source.into());
    let Ok(sexprs) = crate::sexpr::SExprParser::new(tokens, file_id).parse() else {
        return vec![];
    };
    let decls = lowerer.lower_file(file_id, &sexprs);
    decls
        .into_iter()
        .filter_map(|d| {
            if let ast::Declaration::Use { path, .. } = d {
                Some(path)
            } else {
                None
            }
        })
        .collect()
}

/// Extract `pub` type declarations from a source file.
/// Used to bring constructors and field accessors into scope when the module is imported.
pub fn exported_type_decls(source: &str) -> Vec<ast::TypeDecl> {
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(source).lex();
    let file_id = lowerer.add_file("scan.opal".into(), source.into());
    let Ok(sexprs) = crate::sexpr::SExprParser::new(tokens, file_id).parse() else {
        return vec![];
    };
    let decls = lowerer.lower_file(file_id, &sexprs);
    decls
        .into_iter()
        .filter_map(|d| match d {
            ast::Declaration::Type(type_decl) => {
                let is_pub = match &type_decl {
                    ast::TypeDecl::Record { is_pub, .. } => *is_pub,
                    ast::TypeDecl::Variant { is_pub, .. } => *is_pub,
                };
                if is_pub { Some(type_decl) } else { None }
            }
            _ => None,
        })
        .collect()
}

/// Type-check a module and return the inferred schemes for its pub-exported functions.
/// Keys are plain function names ("get", "put", ...) — the caller prefixes with the
/// module name when building the imported_schemes map for dependent modules.
///
/// Returns an empty map if the module fails to parse or type-check.
pub fn infer_module_exports(
    module_name: &str,
    source: &str,
    imports: HashMap<String, String>,
    module_exports: &HashMap<String, Vec<String>>,
    imported_type_decls: &[ast::TypeDecl],
    imported_schemes: &typecheck::TypeEnv,
) -> typecheck::TypeEnv {
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(source).lex();
    let file_id = lowerer.add_file(format!("{module_name}.opal"), source.to_string());

    let sexprs = match crate::sexpr::SExprParser::new(tokens, file_id).parse() {
        Ok(s) => s,
        Err(_) => return HashMap::new(),
    };
    let decls = lowerer.lower_file(file_id, &sexprs);
    if !lowerer.diagnostics.is_empty() {
        return HashMap::new();
    }

    let mut checker = typecheck::TypeChecker::new();
    let mut env = typecheck::primitive_env();

    for type_decl in imported_type_decls {
        env.extend(typecheck::constructor_schemes(type_decl));
    }
    env.extend(imported_schemes.clone());

    let import_names: Vec<String> = imports.keys().cloned().collect();
    let used_modules: std::collections::HashSet<&str> = decls
        .iter()
        .filter_map(|d| {
            if let ast::Declaration::Use { path: (_, m), .. } = d {
                Some(m.as_str())
            } else {
                None
            }
        })
        .collect();
    let qualified_names: Vec<String> = module_exports
        .iter()
        .filter(|(m, _)| !used_modules.contains(m.as_str()))
        .flat_map(|(m, fns)| fns.iter().map(move |f| format!("{m}/{f}")))
        .collect();
    let unresolved: Vec<String> = import_names
        .iter()
        .chain(qualified_names.iter())
        .filter(|n| !env.contains_key(*n))
        .cloned()
        .collect();
    env.extend(typecheck::import_env(&unresolved));

    // Also seed type def spans from imported type decls (for better errors — optional)
    for type_decl in imported_type_decls {
        env.extend(typecheck::constructor_schemes(type_decl));
    }

    if checker.check_program(&mut env, &decls, file_id).is_err() {
        return HashMap::new();
    }

    // Collect pub function names from this module
    let pub_names: std::collections::HashSet<&str> = decls
        .iter()
        .filter_map(|d| match d {
            ast::Declaration::Expression(ast::Expr::LetFunc {
                name, is_pub: true, ..
            }) => Some(name.as_str()),
            ast::Declaration::ExternLet {
                name, is_pub: true, ..
            } => Some(name.as_str()),
            _ => None,
        })
        .collect();

    env.into_iter()
        .filter(|(k, _)| pub_names.contains(k.as_str()))
        .collect()
}

pub fn dummy_compile(source: &str) {
    compile("test", source);
}
