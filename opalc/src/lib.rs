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
    compile_with_imports(module_name, source, HashMap::new(), &HashMap::new())
}

/// Compile with import resolution.
/// - `imports`: unqualified name → module (from `use` declarations)
/// - `module_exports`: module name → exported function names (for validating qualified calls)
pub fn compile_with_imports(
    module_name: &str,
    source: &str,
    imports: HashMap<String, String>,
    module_exports: &HashMap<String, Vec<String>>,
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
        if let ast::Declaration::Use { path: (_, mod_name), span, .. } = decl {
            if !module_exports.contains_key(mod_name.as_str()) {
                let diag = codespan_reporting::diagnostic::Diagnostic::error()
                    .with_message(format!("unknown module `{mod_name}`"))
                    .with_labels(vec![codespan_reporting::diagnostic::Label::primary(
                        file_id,
                        span.clone(),
                    )
                    .with_message(format!("`{mod_name}` is not a module in this project"))]);
                term::emit_to_write_style(&mut writer.lock(), &config, &lowerer.files, &diag)
                    .unwrap();
                use_errors = true;
            }
        }
    }
    if use_errors {
        return None;
    }

    let mut checker = typecheck::TypeChecker::new();
    let mut env = typecheck::primitive_env();

    // Seed env with unqualified imported names (from `use`) as polymorphic
    let import_names: Vec<String> = imports.keys().cloned().collect();
    env.extend(typecheck::import_env(&import_names));

    // Seed env with "module/function" keys for all known remote functions,
    // so qualified calls like (math/double 10) can be validated
    let qualified_names: Vec<String> = module_exports
        .iter()
        .flat_map(|(m, fns)| fns.iter().map(move |f| format!("{m}/{f}")))
        .collect();
    env.extend(typecheck::import_env(&qualified_names));

    if let Err(err) = checker.check_program(&mut env, &decls, file_id) {
        let diagnostics = err.0.to_diagnostics(file_id, err.1.span());
        for diag in diagnostics {
            term::emit_to_write_style(&mut writer.lock(), &config, &lowerer.files, &diag).unwrap();
        }
        return None;
    }

    let module = codegen::lower_module(module_name, &decls, imports);
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
        .filter_map(|d| {
            if let ast::Declaration::Expression(ast::Expr::LetFunc { name, is_pub: true, .. }) = d {
                Some(name)
            } else {
                None
            }
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
            if let ast::Declaration::Use { is_pub: true, path: (_, module), .. } = d {
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

pub fn dummy_compile(source: &str) {
    compile("test", source);
}
