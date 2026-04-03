use std::collections::{HashMap, HashSet};

use crate::{ast, hir, lower, resolve, session, sexpr, typecheck, typing};

fn parse_decls(source_path: &str, source: &str) -> Option<Vec<ast::Declaration>> {
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(source).lex();
    let file_id = lowerer.add_file(source_path.to_string(), source.to_string());
    let Ok(sexprs) = sexpr::SExprParser::new(tokens, file_id).parse() else {
        return None;
    };
    let mut decls = lowerer.lower_file(file_id, &sexprs);
    ast::normalize_variant_payload_type_applications(&mut decls, &[], &[]);
    Some(decls)
}

struct TypeInferenceResult {
    decls: Vec<ast::Declaration>,
    env: typecheck::TypeEnv,
    checker: typecheck::TypeChecker,
}

fn infer_module_types(
    module_name: &str,
    source: &str,
    imports: HashMap<String, String>,
    module_exports: &HashMap<String, Vec<String>>,
    imported_type_decls: &[ast::TypeDecl],
    imported_extern_types: &[ast::ExternTypeInfo],
    imported_schemes: &typecheck::TypeEnv,
) -> Option<TypeInferenceResult> {
    let mut sess = session::CompilerSession::new(session::SessionOptions {
        emit_diagnostics: false,
        emit_warnings: false,
    });
    let mut lowered = hir::lower_source_to_hir(&format!("{module_name}.mond"), source);
    if !lowered.diagnostics.is_empty() {
        return None;
    }
    ast::normalize_variant_payload_type_applications(
        &mut lowered.decls,
        imported_type_decls,
        imported_extern_types,
    );

    let imported_private_records: HashMap<String, Vec<String>> = HashMap::new();
    let (mut checker, mut env) = typing::prepare_typechecker(
        imported_type_decls,
        imported_extern_types,
        &imported_private_records,
        imported_schemes,
    );

    let unresolved = resolve::unresolved_env_names(
        &lowered.decls,
        imports.keys().cloned(),
        &env,
        sess.symbol_table(module_exports),
    );
    env.extend(typecheck::import_env(&unresolved));

    if checker
        .check_program(&mut env, &lowered.decls, lowered.file_id)
        .is_err()
    {
        return None;
    }

    Some(TypeInferenceResult {
        decls: lowered.decls,
        env,
        checker,
    })
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

pub fn all_type_decls(source: &str) -> Vec<ast::TypeDecl> {
    parse_decls("scan.mond", source)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|d| match d {
            ast::Declaration::Type(type_decl) => Some(type_decl),
            _ => None,
        })
        .collect()
}

pub fn private_record_type_names(source: &str) -> Vec<String> {
    parse_decls("scan.mond", source)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|d| match d {
            ast::Declaration::Type(ast::TypeDecl::Record {
                is_pub: false,
                name,
                ..
            }) => Some(name),
            _ => None,
        })
        .collect()
}

pub fn exported_extern_types(source: &str) -> Vec<ast::ExternTypeInfo> {
    parse_decls("scan.mond", source)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|d| match d {
            ast::Declaration::ExternType {
                is_pub: true,
                name,
                params,
                ..
            } => Some(ast::ExternTypeInfo {
                name,
                arity: params.len(),
            }),
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
    imported_extern_types: &[ast::ExternTypeInfo],
    imported_schemes: &typecheck::TypeEnv,
) -> typecheck::TypeEnv {
    let Some(inferred) = infer_module_types(
        module_name,
        source,
        imports,
        module_exports,
        imported_type_decls,
        imported_extern_types,
        imported_schemes,
    ) else {
        return HashMap::new();
    };

    let binding_names: HashSet<&str> = inferred
        .decls
        .iter()
        .filter_map(|d| match d {
            ast::Declaration::Expression(ast::Expr::LetFunc { name, .. }) => Some(name.as_str()),
            ast::Declaration::ExternLet { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();

    inferred
        .env
        .into_iter()
        .filter(|(k, _)| binding_names.contains(k.as_str()))
        .collect()
}

pub fn infer_module_expr_types(
    module_name: &str,
    source: &str,
    imports: HashMap<String, String>,
    module_exports: &HashMap<String, Vec<String>>,
    imported_type_decls: &[ast::TypeDecl],
    imported_extern_types: &[ast::ExternTypeInfo],
    imported_schemes: &typecheck::TypeEnv,
) -> Vec<(std::ops::Range<usize>, String)> {
    let Some(inferred) = infer_module_types(
        module_name,
        source,
        imports,
        module_exports,
        imported_type_decls,
        imported_extern_types,
        imported_schemes,
    ) else {
        return Vec::new();
    };

    inferred
        .checker
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
    imported_extern_types: &[ast::ExternTypeInfo],
    imported_schemes: &typecheck::TypeEnv,
) -> typecheck::TypeEnv {
    let Some(inferred) = infer_module_types(
        module_name,
        source,
        imports,
        module_exports,
        imported_type_decls,
        imported_extern_types,
        imported_schemes,
    ) else {
        return HashMap::new();
    };

    let pub_names: HashSet<&str> = inferred
        .decls
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

    inferred
        .env
        .into_iter()
        .filter(|(k, _)| pub_names.contains(k.as_str()))
        .collect()
}

pub fn test_declarations(source: &str) -> Vec<(String, String)> {
    let mut test_idx = 0;
    parse_decls("tests/scan.mond", source)
        .unwrap_or_default()
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
