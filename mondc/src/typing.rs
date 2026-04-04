use std::collections::{HashMap, HashSet};

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

fn local_type_name(name: &str) -> &str {
    name.rsplit_once('/')
        .map_or(name, |(_, local_name)| local_name)
}

fn collect_qualified_name_candidates(
    name: &str,
    qualified_candidates: &mut HashMap<String, HashSet<String>>,
) {
    if !is_qualified_type_name(name) {
        return;
    }
    let local_name = local_type_name(name).to_string();
    qualified_candidates
        .entry(local_name)
        .or_default()
        .insert(name.to_string());
}

fn collect_usage_qualified_candidates(
    usage: &ast::TypeUsage,
    qualified_candidates: &mut HashMap<String, HashSet<String>>,
) {
    match usage {
        ast::TypeUsage::Named(name, _) => {
            collect_qualified_name_candidates(name, qualified_candidates)
        }
        ast::TypeUsage::Generic(_, _) => {}
        ast::TypeUsage::App(head, args, _) => {
            collect_qualified_name_candidates(head, qualified_candidates);
            for arg in args {
                collect_usage_qualified_candidates(arg, qualified_candidates);
            }
        }
        ast::TypeUsage::Fun(arg, ret, _) => {
            collect_usage_qualified_candidates(arg, qualified_candidates);
            collect_usage_qualified_candidates(ret, qualified_candidates);
        }
    }
}

fn collect_decl_qualified_candidates(
    type_decl: &ast::TypeDecl,
    qualified_candidates: &mut HashMap<String, HashSet<String>>,
) {
    collect_qualified_name_candidates(type_decl_name(type_decl), qualified_candidates);
    match type_decl {
        ast::TypeDecl::Record { fields, .. } => {
            for (_, field_ty) in fields {
                collect_usage_qualified_candidates(field_ty, qualified_candidates);
            }
        }
        ast::TypeDecl::Variant { constructors, .. } => {
            for (_, payloads) in constructors {
                for payload_ty in payloads {
                    collect_usage_qualified_candidates(payload_ty, qualified_candidates);
                }
            }
        }
    }
}

fn collect_type_qualified_candidates(
    ty: &typecheck::Type,
    qualified_candidates: &mut HashMap<String, HashSet<String>>,
) {
    match ty {
        typecheck::Type::Var(_) => {}
        typecheck::Type::Fun(arg, ret) => {
            collect_type_qualified_candidates(arg, qualified_candidates);
            collect_type_qualified_candidates(ret, qualified_candidates);
        }
        typecheck::Type::Con(name, args) => {
            collect_qualified_name_candidates(name, qualified_candidates);
            for arg in args {
                collect_type_qualified_candidates(arg, qualified_candidates);
            }
        }
    }
}

fn collect_scheme_qualified_candidates(
    scheme: &typecheck::Scheme,
    qualified_candidates: &mut HashMap<String, HashSet<String>>,
) {
    collect_type_qualified_candidates(&scheme.ty, qualified_candidates);
    for predicate in &scheme.preds {
        match predicate {
            typecheck::Predicate::HasField {
                record_ty,
                field_ty,
                ..
            } => {
                collect_type_qualified_candidates(record_ty, qualified_candidates);
                collect_type_qualified_candidates(field_ty, qualified_candidates);
            }
        }
    }
}

fn collect_unqualified_type_names(ty: &typecheck::Type, out: &mut HashSet<String>) {
    match ty {
        typecheck::Type::Var(_) => {}
        typecheck::Type::Fun(arg, ret) => {
            collect_unqualified_type_names(arg, out);
            collect_unqualified_type_names(ret, out);
        }
        typecheck::Type::Con(name, args) => {
            if !is_qualified_type_name(name) {
                out.insert(name.clone());
            }
            for arg in args {
                collect_unqualified_type_names(arg, out);
            }
        }
    }
}

fn collect_scheme_unqualified_names(scheme: &typecheck::Scheme, out: &mut HashSet<String>) {
    collect_unqualified_type_names(&scheme.ty, out);
    for predicate in &scheme.preds {
        match predicate {
            typecheck::Predicate::HasField {
                record_ty,
                field_ty,
                ..
            } => {
                collect_unqualified_type_names(record_ty, out);
                collect_unqualified_type_names(field_ty, out);
            }
        }
    }
}

pub(crate) fn build_qualified_type_aliases(
    imported_type_decls: &[ast::TypeDecl],
    imported_extern_types: &[ast::ExternTypeInfo],
    imported_schemes: &typecheck::TypeEnv,
) -> HashMap<String, String> {
    let mut qualified_candidates: HashMap<String, HashSet<String>> = HashMap::new();
    let mut unqualified_in_scope = HashSet::new();

    for type_decl in imported_type_decls {
        let name = type_decl_name(type_decl);
        if is_qualified_type_name(name) {
            collect_qualified_name_candidates(name, &mut qualified_candidates);
        } else {
            unqualified_in_scope.insert(name.to_string());
        }
        collect_decl_qualified_candidates(type_decl, &mut qualified_candidates);
    }
    for extern_type in imported_extern_types {
        if is_qualified_type_name(&extern_type.name) {
            collect_qualified_name_candidates(&extern_type.name, &mut qualified_candidates);
        } else {
            unqualified_in_scope.insert(extern_type.name.clone());
        }
    }
    for scheme in imported_schemes.values() {
        collect_scheme_qualified_candidates(scheme, &mut qualified_candidates);
        collect_scheme_unqualified_names(scheme, &mut unqualified_in_scope);
    }

    let mut aliases = HashMap::new();
    for unqualified_name in unqualified_in_scope {
        let Some(candidates) = qualified_candidates.get(&unqualified_name) else {
            continue;
        };
        if candidates.len() == 1
            && let Some(canonical_name) = candidates.iter().next()
        {
            aliases.insert(unqualified_name, canonical_name.clone());
        }
    }
    aliases
}

fn canonicalize_type_decl_name(
    type_decl: &ast::TypeDecl,
    aliases: &HashMap<String, String>,
) -> ast::TypeDecl {
    let canonical_name = aliases
        .get(type_decl_name(type_decl))
        .cloned()
        .unwrap_or_else(|| type_decl_name(type_decl).to_string());
    match type_decl {
        ast::TypeDecl::Record {
            is_pub,
            params,
            fields,
            span,
            ..
        } => ast::TypeDecl::Record {
            is_pub: *is_pub,
            name: canonical_name,
            params: params.clone(),
            fields: fields.clone(),
            span: span.clone(),
        },
        ast::TypeDecl::Variant {
            is_pub,
            params,
            constructors,
            span,
            ..
        } => ast::TypeDecl::Variant {
            is_pub: *is_pub,
            name: canonical_name,
            params: params.clone(),
            constructors: constructors.clone(),
            span: span.clone(),
        },
    }
}

pub(crate) fn prepare_typechecker(
    imported_type_decls: &[ast::TypeDecl],
    imported_extern_types: &[ast::ExternTypeInfo],
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
    let mut imported_type_decls_canonical = Vec::new();
    let mut seen_canonical_type_names = HashSet::new();
    for type_decl in imported_type_decls {
        let canonical_decl = canonicalize_type_decl_name(type_decl, &qualified_type_aliases);
        let canonical_name = type_decl_name(&canonical_decl).to_string();
        if seen_canonical_type_names.insert(canonical_name) {
            imported_type_decls_canonical.push(canonical_decl);
        }
    }

    let mut checker = typecheck::TypeChecker::new();
    checker.seed_qualified_type_aliases(qualified_type_aliases.clone());
    checker.seed_imported_type_info(&imported_type_decls_canonical);
    checker.seed_private_record_origins(imported_private_records.clone());

    let mut env = typecheck::primitive_env();
    for type_decl in &imported_type_decls_unqualified {
        env.extend(typecheck::constructor_schemes_with_aliases(
            type_decl,
            &qualified_type_aliases,
        ));
    }
    for type_decl in &imported_type_decls_canonical {
        let schemes =
            typecheck::constructor_schemes_with_aliases(type_decl, &qualified_type_aliases);
        for (name, scheme) in schemes {
            if !name.starts_with(':') {
                continue;
            }
            if name[1..].contains(':') {
                env.insert(name, scheme);
            } else {
                env.entry(name).or_insert(scheme);
            }
        }
    }
    env.extend(typecheck::normalize_env_type_aliases(
        imported_schemes,
        &qualified_type_aliases,
    ));

    (checker, env)
}
