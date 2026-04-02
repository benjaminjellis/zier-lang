use std::collections::HashMap;

use crate::{ast, typecheck};

pub(crate) fn type_decl_name(type_decl: &ast::TypeDecl) -> &str {
    match type_decl {
        ast::TypeDecl::Record { name, .. } => name,
        ast::TypeDecl::Variant { name, .. } => name,
    }
}

pub(crate) fn is_qualified_type_name(name: &str) -> bool {
    name.contains('/')
}

fn add_qualified_type_alias(name: &str, aliases: &mut HashMap<String, String>) {
    if let Some((_, local_name)) = name.rsplit_once('/') {
        aliases
            .entry(name.to_string())
            .or_insert_with(|| local_name.to_string());
    }
}

fn collect_usage_aliases(usage: &ast::TypeUsage, aliases: &mut HashMap<String, String>) {
    match usage {
        ast::TypeUsage::Named(name, _) => add_qualified_type_alias(name, aliases),
        ast::TypeUsage::Generic(_, _) => {}
        ast::TypeUsage::App(head, args, _) => {
            add_qualified_type_alias(head, aliases);
            for arg in args {
                collect_usage_aliases(arg, aliases);
            }
        }
        ast::TypeUsage::Fun(arg, ret, _) => {
            collect_usage_aliases(arg, aliases);
            collect_usage_aliases(ret, aliases);
        }
    }
}

fn collect_decl_aliases(type_decl: &ast::TypeDecl, aliases: &mut HashMap<String, String>) {
    add_qualified_type_alias(type_decl_name(type_decl), aliases);
    match type_decl {
        ast::TypeDecl::Record { fields, .. } => {
            for (_, field_ty) in fields {
                collect_usage_aliases(field_ty, aliases);
            }
        }
        ast::TypeDecl::Variant { constructors, .. } => {
            for (_, payloads) in constructors {
                for payload_ty in payloads {
                    collect_usage_aliases(payload_ty, aliases);
                }
            }
        }
    }
}

fn collect_type_aliases(ty: &typecheck::Type, aliases: &mut HashMap<String, String>) {
    match ty {
        typecheck::Type::Var(_) => {}
        typecheck::Type::Fun(arg, ret) => {
            collect_type_aliases(arg, aliases);
            collect_type_aliases(ret, aliases);
        }
        typecheck::Type::Con(name, args) => {
            add_qualified_type_alias(name, aliases);
            for arg in args {
                collect_type_aliases(arg, aliases);
            }
        }
    }
}

fn collect_scheme_aliases(scheme: &typecheck::Scheme, aliases: &mut HashMap<String, String>) {
    collect_type_aliases(&scheme.ty, aliases);
    for predicate in &scheme.preds {
        match predicate {
            typecheck::Predicate::HasField {
                record_ty,
                field_ty,
                ..
            } => {
                collect_type_aliases(record_ty, aliases);
                collect_type_aliases(field_ty, aliases);
            }
        }
    }
}

pub(crate) fn build_qualified_type_aliases(
    imported_type_decls: &[ast::TypeDecl],
    imported_extern_types: &[String],
    imported_schemes: &typecheck::TypeEnv,
) -> HashMap<String, String> {
    let mut aliases = HashMap::new();
    for type_decl in imported_type_decls {
        collect_decl_aliases(type_decl, &mut aliases);
    }
    for name in imported_extern_types {
        add_qualified_type_alias(name, &mut aliases);
    }
    for scheme in imported_schemes.values() {
        collect_scheme_aliases(scheme, &mut aliases);
    }
    aliases
}

pub(crate) fn prepare_typechecker(
    imported_type_decls: &[ast::TypeDecl],
    imported_extern_types: &[String],
    imported_private_records: &HashMap<String, Vec<String>>,
    imported_schemes: &typecheck::TypeEnv,
) -> (typecheck::TypeChecker, typecheck::TypeEnv) {
    let qualified_type_aliases =
        build_qualified_type_aliases(imported_type_decls, imported_extern_types, imported_schemes);
    let imported_type_decls_unqualified: Vec<ast::TypeDecl> = imported_type_decls
        .iter()
        .filter(|type_decl| !is_qualified_type_name(type_decl_name(type_decl)))
        .cloned()
        .collect();

    let mut checker = typecheck::TypeChecker::new();
    checker.seed_qualified_type_aliases(qualified_type_aliases.clone());
    checker.seed_imported_type_info(&imported_type_decls_unqualified);
    checker.seed_private_record_origins(imported_private_records.clone());

    let mut env = typecheck::primitive_env();
    for type_decl in &imported_type_decls_unqualified {
        env.extend(typecheck::constructor_schemes_with_aliases(
            type_decl,
            &qualified_type_aliases,
        ));
    }
    env.extend(typecheck::normalize_env_type_aliases(
        imported_schemes,
        &qualified_type_aliases,
    ));

    (checker, env)
}
