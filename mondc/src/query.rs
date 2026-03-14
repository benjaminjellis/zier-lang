use std::collections::HashMap;

use crate::{ast, lower, resolve, session, sexpr, typecheck};

fn parse_decls(source_path: &str, source: &str) -> Option<Vec<ast::Declaration>> {
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(source).lex();
    let file_id = lowerer.add_file(source_path.to_string(), source.to_string());
    let Ok(sexprs) = sexpr::SExprParser::new(tokens, file_id).parse() else {
        return None;
    };
    Some(lowerer.lower_file(file_id, &sexprs))
}

fn type_decl_name(type_decl: &ast::TypeDecl) -> &str {
    match type_decl {
        ast::TypeDecl::Record { name, .. } => name,
        ast::TypeDecl::Variant { name, .. } => name,
    }
}

fn is_qualified_type_name(name: &str) -> bool {
    name.contains('/')
}

fn build_qualified_type_aliases(
    imported_type_decls: &[ast::TypeDecl],
    imported_extern_types: &[String],
) -> HashMap<String, String> {
    let mut aliases = HashMap::new();
    for type_decl in imported_type_decls {
        let name = type_decl_name(type_decl);
        if let Some((_, local_name)) = name.split_once('/') {
            aliases.insert(name.to_string(), local_name.to_string());
        }
    }
    for name in imported_extern_types {
        if let Some((_, local_name)) = name.split_once('/') {
            aliases.insert(name.clone(), local_name.to_string());
        }
    }
    aliases
}

pub fn exported_names(source: &str) -> Vec<String> {
    parse_decls("scan.mond", source)
        .unwrap_or_default()
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

pub fn has_nullary_main(source: &str) -> bool {
    parse_decls("scan.mond", source)
        .unwrap_or_default()
        .into_iter()
        .any(|d| {
            matches!(
                d,
                ast::Declaration::Expression(ast::Expr::LetFunc { name, args, .. })
                if name == "main" && args.is_empty()
            )
        })
}

pub fn pub_reexports(source: &str) -> Vec<String> {
    parse_decls("scan.mond", source)
        .unwrap_or_default()
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

pub fn used_modules(source: &str) -> Vec<(String, String, ast::UnqualifiedImports)> {
    parse_decls("scan.mond", source)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|d| {
            if let ast::Declaration::Use {
                path, unqualified, ..
            } = d
            {
                Some((path.0, path.1, unqualified))
            } else {
                None
            }
        })
        .collect()
}

pub fn exported_type_decls(source: &str) -> Vec<ast::TypeDecl> {
    parse_decls("scan.mond", source)
        .unwrap_or_default()
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

pub fn exported_extern_types(source: &str) -> Vec<String> {
    parse_decls("scan.mond", source)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|d| match d {
            ast::Declaration::ExternType {
                is_pub: true, name, ..
            } => Some(name),
            _ => None,
        })
        .collect()
}

pub fn infer_module_bindings(
    module_name: &str,
    source: &str,
    imports: HashMap<String, String>,
    module_exports: &HashMap<String, Vec<String>>,
    imported_type_decls: &[ast::TypeDecl],
    imported_extern_types: &[String],
    imported_schemes: &typecheck::TypeEnv,
) -> typecheck::TypeEnv {
    let mut sess = session::CompilerSession::new(session::SessionOptions {
        emit_diagnostics: false,
        emit_warnings: false,
    });
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(source).lex();
    let file_id = lowerer.add_file(format!("{module_name}.mond"), source.to_string());

    let sexprs = match sexpr::SExprParser::new(tokens, file_id).parse() {
        Ok(s) => s,
        Err(_) => return HashMap::new(),
    };
    let decls = lowerer.lower_file(file_id, &sexprs);
    if !lowerer.diagnostics.is_empty() {
        return HashMap::new();
    }

    let qualified_type_aliases =
        build_qualified_type_aliases(imported_type_decls, imported_extern_types);
    let imported_type_decls_unqualified: Vec<ast::TypeDecl> = imported_type_decls
        .iter()
        .filter(|type_decl| !is_qualified_type_name(type_decl_name(type_decl)))
        .cloned()
        .collect();

    let mut checker = typecheck::TypeChecker::new();
    checker.seed_qualified_type_aliases(qualified_type_aliases.clone());
    checker.seed_imported_type_info(&imported_type_decls_unqualified);
    let mut env = typecheck::primitive_env();

    for type_decl in &imported_type_decls_unqualified {
        env.extend(typecheck::constructor_schemes_with_aliases(
            type_decl,
            &qualified_type_aliases,
        ));
    }
    env.extend(imported_schemes.clone());

    let unresolved = resolve::unresolved_env_names(
        &decls,
        imports.keys().cloned(),
        &env,
        sess.symbol_table(module_exports),
    );
    env.extend(typecheck::import_env(&unresolved));

    for type_decl in &imported_type_decls_unqualified {
        env.extend(typecheck::constructor_schemes_with_aliases(
            type_decl,
            &qualified_type_aliases,
        ));
    }

    if checker.check_program(&mut env, &decls, file_id).is_err() {
        return HashMap::new();
    }

    let binding_names: std::collections::HashSet<&str> = decls
        .iter()
        .filter_map(|d| match d {
            ast::Declaration::Expression(ast::Expr::LetFunc { name, .. }) => Some(name.as_str()),
            ast::Declaration::ExternLet { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();

    env.into_iter()
        .filter(|(k, _)| binding_names.contains(k.as_str()))
        .collect()
}

pub fn infer_module_expr_types(
    module_name: &str,
    source: &str,
    imports: HashMap<String, String>,
    module_exports: &HashMap<String, Vec<String>>,
    imported_type_decls: &[ast::TypeDecl],
    imported_extern_types: &[String],
    imported_schemes: &typecheck::TypeEnv,
) -> Vec<(std::ops::Range<usize>, String)> {
    let mut sess = session::CompilerSession::new(session::SessionOptions {
        emit_diagnostics: false,
        emit_warnings: false,
    });
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(source).lex();
    let file_id = lowerer.add_file(format!("{module_name}.mond"), source.to_string());

    let sexprs = match sexpr::SExprParser::new(tokens, file_id).parse() {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let decls = lowerer.lower_file(file_id, &sexprs);
    if !lowerer.diagnostics.is_empty() {
        return Vec::new();
    }

    let qualified_type_aliases =
        build_qualified_type_aliases(imported_type_decls, imported_extern_types);
    let imported_type_decls_unqualified: Vec<ast::TypeDecl> = imported_type_decls
        .iter()
        .filter(|type_decl| !is_qualified_type_name(type_decl_name(type_decl)))
        .cloned()
        .collect();

    let mut checker = typecheck::TypeChecker::new();
    checker.seed_qualified_type_aliases(qualified_type_aliases.clone());
    checker.seed_imported_type_info(&imported_type_decls_unqualified);
    let mut env = typecheck::primitive_env();
    for type_decl in &imported_type_decls_unqualified {
        env.extend(typecheck::constructor_schemes_with_aliases(
            type_decl,
            &qualified_type_aliases,
        ));
    }
    env.extend(imported_schemes.clone());

    let unresolved = resolve::unresolved_env_names(
        &decls,
        imports.keys().cloned(),
        &env,
        sess.symbol_table(module_exports),
    );
    env.extend(typecheck::import_env(&unresolved));

    if checker.check_program(&mut env, &decls, file_id).is_err() {
        return Vec::new();
    }

    checker
        .inferred_expr_types()
        .iter()
        .map(|(span, ty)| (span.clone(), typecheck::type_display(ty)))
        .collect()
}

pub fn infer_module_exports(
    module_name: &str,
    source: &str,
    imports: HashMap<String, String>,
    module_exports: &HashMap<String, Vec<String>>,
    imported_type_decls: &[ast::TypeDecl],
    imported_extern_types: &[String],
    imported_schemes: &typecheck::TypeEnv,
) -> typecheck::TypeEnv {
    let all_bindings = infer_module_bindings(
        module_name,
        source,
        imports,
        module_exports,
        imported_type_decls,
        imported_extern_types,
        imported_schemes,
    );

    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(source).lex();
    let file_id = lowerer.add_file(format!("{module_name}.mond"), source.to_string());
    let Ok(sexprs) = sexpr::SExprParser::new(tokens, file_id).parse() else {
        return HashMap::new();
    };
    let decls = lowerer.lower_file(file_id, &sexprs);
    if !lowerer.diagnostics.is_empty() {
        return HashMap::new();
    }

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

    all_bindings
        .into_iter()
        .filter(|(k, _)| pub_names.contains(k.as_str()))
        .collect()
}

pub fn test_declarations(source: &str) -> Vec<(String, String)> {
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(source).lex();
    let file_id = lowerer.add_file("tests/scan.mond".into(), source.into());
    let Ok(sexprs) = sexpr::SExprParser::new(tokens, file_id).parse() else {
        return vec![];
    };
    let decls = lowerer.lower_file(file_id, &sexprs);
    let mut test_idx = 0;
    decls
        .into_iter()
        .filter_map(|d| {
            if let ast::Declaration::Test { name, .. } = d {
                let fn_name = format!("mond_test_{test_idx}");
                test_idx += 1;
                Some((name, fn_name))
            } else {
                None
            }
        })
        .collect()
}
