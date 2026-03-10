use std::collections::{HashMap, HashSet, VecDeque};

use crate::ast;
use codespan_reporting::diagnostic::{Diagnostic, Label};

fn imported_names_for_use_decl(
    mod_name: &str,
    unqualified: &ast::UnqualifiedImports,
    module_exports: &HashMap<String, Vec<String>>,
) -> Vec<String> {
    match unqualified {
        ast::UnqualifiedImports::None => vec![],
        ast::UnqualifiedImports::Specific(names) => names.clone(),
        ast::UnqualifiedImports::Wildcard => {
            module_exports.get(mod_name).cloned().unwrap_or_default()
        }
    }
}

fn bind_pattern_names(pat: &ast::Pattern, out: &mut HashSet<String>) {
    match pat {
        ast::Pattern::Variable(name, _) => {
            out.insert(name.clone());
        }
        ast::Pattern::Constructor(_, args, _) => {
            for arg in args {
                bind_pattern_names(arg, out);
            }
        }
        ast::Pattern::Or(pats, _) => {
            for p in pats {
                bind_pattern_names(p, out);
            }
        }
        ast::Pattern::Cons(head, tail, _) => {
            bind_pattern_names(head, out);
            bind_pattern_names(tail, out);
        }
        ast::Pattern::Any(_) | ast::Pattern::Literal(_, _) | ast::Pattern::EmptyList(_) => {}
    }
}

fn collect_top_level_refs(
    expr: &ast::Expr,
    top_level: &HashSet<String>,
    locals: &HashSet<String>,
    out: &mut HashSet<String>,
) {
    use ast::Expr;
    match expr {
        Expr::Literal(_, _) => {}
        Expr::Variable(name, _) => {
            if top_level.contains(name.as_str()) && !locals.contains(name.as_str()) {
                out.insert(name.clone());
            }
        }
        Expr::List(items, _) => {
            for item in items {
                collect_top_level_refs(item, top_level, locals, out);
            }
        }
        Expr::LetFunc {
            name, args, value, ..
        } => {
            let mut inner = locals.clone();
            inner.insert(name.clone());
            inner.extend(args.iter().cloned());
            collect_top_level_refs(value, top_level, &inner, out);
        }
        Expr::LetLocal {
            name, value, body, ..
        } => {
            collect_top_level_refs(value, top_level, locals, out);
            let mut body_locals = locals.clone();
            body_locals.insert(name.clone());
            collect_top_level_refs(body, top_level, &body_locals, out);
        }
        Expr::If {
            cond, then, els, ..
        } => {
            collect_top_level_refs(cond, top_level, locals, out);
            collect_top_level_refs(then, top_level, locals, out);
            collect_top_level_refs(els, top_level, locals, out);
        }
        Expr::Call { func, args, .. } => {
            collect_top_level_refs(func, top_level, locals, out);
            for arg in args {
                collect_top_level_refs(arg, top_level, locals, out);
            }
        }
        Expr::Match { targets, arms, .. } => {
            for target in targets {
                collect_top_level_refs(target, top_level, locals, out);
            }
            for (patterns, body) in arms {
                let mut arm_locals = locals.clone();
                for pat in patterns {
                    bind_pattern_names(pat, &mut arm_locals);
                }
                collect_top_level_refs(body, top_level, &arm_locals, out);
            }
        }
        Expr::FieldAccess { record, .. } => {
            collect_top_level_refs(record, top_level, locals, out);
        }
        Expr::RecordConstruct { fields, .. } => {
            for (_, value) in fields {
                collect_top_level_refs(value, top_level, locals, out);
            }
        }
        Expr::Lambda { args, body, .. } => {
            let mut inner = locals.clone();
            inner.extend(args.iter().cloned());
            collect_top_level_refs(body, top_level, &inner, out);
        }
        Expr::QualifiedCall { args, .. } => {
            for arg in args {
                collect_top_level_refs(arg, top_level, locals, out);
            }
        }
    }
}

fn collect_unused_local_spans(
    expr: &ast::Expr,
    out: &mut Vec<(String, std::ops::Range<usize>)>,
) -> HashSet<String> {
    use ast::Expr;
    match expr {
        Expr::Literal(_, _) => HashSet::new(),
        Expr::Variable(name, _) => HashSet::from([name.clone()]),
        Expr::List(items, _) => {
            let mut free = HashSet::new();
            for item in items {
                free.extend(collect_unused_local_spans(item, out));
            }
            free
        }
        Expr::LetFunc {
            name, args, value, ..
        } => {
            let mut free = collect_unused_local_spans(value, out);
            free.remove(name);
            for arg in args {
                free.remove(arg);
            }
            free
        }
        Expr::LetLocal {
            name,
            name_span,
            value,
            body,
            ..
        } => {
            let mut free = collect_unused_local_spans(value, out);
            let mut body_free = collect_unused_local_spans(body, out);
            let is_used = body_free.remove(name);
            if name != "_" && !is_used {
                out.push((name.clone(), name_span.clone()));
            }
            free.extend(body_free);
            free
        }
        Expr::If {
            cond, then, els, ..
        } => {
            let mut free = collect_unused_local_spans(cond, out);
            free.extend(collect_unused_local_spans(then, out));
            free.extend(collect_unused_local_spans(els, out));
            free
        }
        Expr::Call { func, args, .. } => {
            let mut free = collect_unused_local_spans(func, out);
            for arg in args {
                free.extend(collect_unused_local_spans(arg, out));
            }
            free
        }
        Expr::Match { targets, arms, .. } => {
            let mut free = HashSet::new();
            for target in targets {
                free.extend(collect_unused_local_spans(target, out));
            }
            for (patterns, body) in arms {
                let mut body_free = collect_unused_local_spans(body, out);
                let mut bound = HashSet::new();
                for pat in patterns {
                    bind_pattern_names(pat, &mut bound);
                }
                for name in bound {
                    body_free.remove(&name);
                }
                free.extend(body_free);
            }
            free
        }
        Expr::FieldAccess { record, .. } => collect_unused_local_spans(record, out),
        Expr::RecordConstruct { fields, .. } => {
            let mut free = HashSet::new();
            for (_, value) in fields {
                free.extend(collect_unused_local_spans(value, out));
            }
            free
        }
        Expr::Lambda { args, body, .. } => {
            let mut free = collect_unused_local_spans(body, out);
            for arg in args {
                free.remove(arg);
            }
            free
        }
        Expr::QualifiedCall { args, .. } => {
            let mut free = HashSet::new();
            for arg in args {
                free.extend(collect_unused_local_spans(arg, out));
            }
            free
        }
    }
}

fn collect_unqualified_free_vars(
    expr: &ast::Expr,
    locals: &HashSet<String>,
    out: &mut HashSet<String>,
) {
    use ast::Expr;
    match expr {
        Expr::Literal(_, _) => {}
        Expr::Variable(name, _) => {
            if !locals.contains(name.as_str()) {
                out.insert(name.clone());
            }
        }
        Expr::List(items, _) => {
            for item in items {
                collect_unqualified_free_vars(item, locals, out);
            }
        }
        Expr::LetFunc {
            name, args, value, ..
        } => {
            let mut inner = locals.clone();
            inner.insert(name.clone());
            inner.extend(args.iter().cloned());
            collect_unqualified_free_vars(value, &inner, out);
        }
        Expr::LetLocal {
            name, value, body, ..
        } => {
            collect_unqualified_free_vars(value, locals, out);
            let mut inner = locals.clone();
            inner.insert(name.clone());
            collect_unqualified_free_vars(body, &inner, out);
        }
        Expr::If {
            cond, then, els, ..
        } => {
            collect_unqualified_free_vars(cond, locals, out);
            collect_unqualified_free_vars(then, locals, out);
            collect_unqualified_free_vars(els, locals, out);
        }
        Expr::Call { func, args, .. } => {
            collect_unqualified_free_vars(func, locals, out);
            for arg in args {
                collect_unqualified_free_vars(arg, locals, out);
            }
        }
        Expr::Match { targets, arms, .. } => {
            for target in targets {
                collect_unqualified_free_vars(target, locals, out);
            }
            for (patterns, body) in arms {
                for pat in patterns {
                    collect_pattern_constructor_names(pat, out);
                }
                let mut arm_locals = locals.clone();
                for pat in patterns {
                    bind_pattern_names(pat, &mut arm_locals);
                }
                collect_unqualified_free_vars(body, &arm_locals, out);
            }
        }
        Expr::FieldAccess { record, .. } => {
            collect_unqualified_free_vars(record, locals, out);
        }
        Expr::RecordConstruct { name, fields, .. } => {
            if !locals.contains(name.as_str()) {
                out.insert(name.clone());
            }
            for (_, value) in fields {
                collect_unqualified_free_vars(value, locals, out);
            }
        }
        Expr::Lambda { args, body, .. } => {
            let mut inner = locals.clone();
            inner.extend(args.iter().cloned());
            collect_unqualified_free_vars(body, &inner, out);
        }
        Expr::QualifiedCall { args, .. } => {
            for arg in args {
                collect_unqualified_free_vars(arg, locals, out);
            }
        }
    }
}

fn collect_qualified_module_refs(expr: &ast::Expr, out: &mut HashSet<String>) {
    use ast::Expr;
    match expr {
        Expr::Literal(_, _) | Expr::Variable(_, _) => {}
        Expr::List(items, _) => {
            for item in items {
                collect_qualified_module_refs(item, out);
            }
        }
        Expr::LetFunc { value, .. } => {
            collect_qualified_module_refs(value, out);
        }
        Expr::LetLocal { value, body, .. } => {
            collect_qualified_module_refs(value, out);
            collect_qualified_module_refs(body, out);
        }
        Expr::If {
            cond, then, els, ..
        } => {
            collect_qualified_module_refs(cond, out);
            collect_qualified_module_refs(then, out);
            collect_qualified_module_refs(els, out);
        }
        Expr::Call { func, args, .. } => {
            collect_qualified_module_refs(func, out);
            for arg in args {
                collect_qualified_module_refs(arg, out);
            }
        }
        Expr::Match { targets, arms, .. } => {
            for target in targets {
                collect_qualified_module_refs(target, out);
            }
            for (_, body) in arms {
                collect_qualified_module_refs(body, out);
            }
        }
        Expr::FieldAccess { record, .. } => {
            collect_qualified_module_refs(record, out);
        }
        Expr::RecordConstruct { fields, .. } => {
            for (_, value) in fields {
                collect_qualified_module_refs(value, out);
            }
        }
        Expr::Lambda { body, .. } => {
            collect_qualified_module_refs(body, out);
        }
        Expr::QualifiedCall { module, args, .. } => {
            out.insert(module.clone());
            for arg in args {
                collect_qualified_module_refs(arg, out);
            }
        }
    }
}

fn used_qualified_modules(decls: &[ast::Declaration]) -> HashSet<String> {
    let mut used = HashSet::new();
    for decl in decls {
        match decl {
            ast::Declaration::Expression(ast::Expr::LetFunc { value, .. }) => {
                collect_qualified_module_refs(value, &mut used);
            }
            ast::Declaration::Test { body, .. } => {
                collect_qualified_module_refs(body, &mut used);
            }
            _ => {}
        }
    }
    used
}

fn collect_type_usage_names(ty: &ast::TypeUsage, out: &mut HashSet<String>) {
    match ty {
        ast::TypeUsage::Named(name) => {
            out.insert(name.clone());
        }
        ast::TypeUsage::Generic(_) => {}
        ast::TypeUsage::App(head, args) => {
            out.insert(head.clone());
            for arg in args {
                collect_type_usage_names(arg, out);
            }
        }
    }
}

fn collect_decl_type_usage_names(decl: &ast::TypeDecl, out: &mut HashSet<String>) {
    match decl {
        ast::TypeDecl::Record { fields, .. } => {
            for (_, ty) in fields {
                collect_type_usage_names(ty, out);
            }
        }
        ast::TypeDecl::Variant { constructors, .. } => {
            for (_, payload) in constructors {
                if let Some(ty) = payload {
                    collect_type_usage_names(ty, out);
                }
            }
        }
    }
}

fn used_unqualified_names(decls: &[ast::Declaration]) -> HashSet<String> {
    let mut used = HashSet::new();
    let empty_locals = HashSet::new();
    for decl in decls {
        match decl {
            ast::Declaration::Expression(ast::Expr::LetFunc {
                name, args, value, ..
            }) => {
                let mut locals: HashSet<String> = args.iter().cloned().collect();
                locals.insert(name.clone());
                collect_unqualified_free_vars(value, &locals, &mut used);
            }
            ast::Declaration::Test { body, .. } => {
                collect_unqualified_free_vars(body, &empty_locals, &mut used);
            }
            ast::Declaration::Type(type_decl) => {
                collect_decl_type_usage_names(type_decl, &mut used);
            }
            _ => {}
        }
    }
    used
}

fn collect_pattern_constructor_names(pat: &ast::Pattern, out: &mut HashSet<String>) {
    match pat {
        ast::Pattern::Constructor(name, args, _) => {
            out.insert(name.clone());
            for arg in args {
                collect_pattern_constructor_names(arg, out);
            }
        }
        ast::Pattern::Or(pats, _) => {
            for p in pats {
                collect_pattern_constructor_names(p, out);
            }
        }
        ast::Pattern::Cons(head, tail, _) => {
            collect_pattern_constructor_names(head, out);
            collect_pattern_constructor_names(tail, out);
        }
        ast::Pattern::Any(_)
        | ast::Pattern::Variable(_, _)
        | ast::Pattern::Literal(_, _)
        | ast::Pattern::EmptyList(_) => {}
    }
}

fn collect_expr_type_decl_refs(
    expr: &ast::Expr,
    locals: &HashSet<String>,
    used_value_names: &mut HashSet<String>,
    used_record_type_names: &mut HashSet<String>,
) {
    use ast::Expr;
    match expr {
        Expr::Literal(_, _) => {}
        Expr::Variable(name, _) => {
            if !locals.contains(name.as_str()) {
                used_value_names.insert(name.clone());
            }
        }
        Expr::List(items, _) => {
            for item in items {
                collect_expr_type_decl_refs(item, locals, used_value_names, used_record_type_names);
            }
        }
        Expr::LetFunc {
            name, args, value, ..
        } => {
            let mut inner = locals.clone();
            inner.insert(name.clone());
            inner.extend(args.iter().cloned());
            collect_expr_type_decl_refs(value, &inner, used_value_names, used_record_type_names);
        }
        Expr::LetLocal {
            name, value, body, ..
        } => {
            collect_expr_type_decl_refs(value, locals, used_value_names, used_record_type_names);
            let mut inner = locals.clone();
            inner.insert(name.clone());
            collect_expr_type_decl_refs(body, &inner, used_value_names, used_record_type_names);
        }
        Expr::If {
            cond, then, els, ..
        } => {
            collect_expr_type_decl_refs(cond, locals, used_value_names, used_record_type_names);
            collect_expr_type_decl_refs(then, locals, used_value_names, used_record_type_names);
            collect_expr_type_decl_refs(els, locals, used_value_names, used_record_type_names);
        }
        Expr::Call { func, args, .. } => {
            collect_expr_type_decl_refs(func, locals, used_value_names, used_record_type_names);
            for arg in args {
                collect_expr_type_decl_refs(arg, locals, used_value_names, used_record_type_names);
            }
        }
        Expr::Match { targets, arms, .. } => {
            for target in targets {
                collect_expr_type_decl_refs(
                    target,
                    locals,
                    used_value_names,
                    used_record_type_names,
                );
            }
            for (patterns, body) in arms {
                for pat in patterns {
                    collect_pattern_constructor_names(pat, used_value_names);
                }
                let mut arm_locals = locals.clone();
                for pat in patterns {
                    bind_pattern_names(pat, &mut arm_locals);
                }
                collect_expr_type_decl_refs(
                    body,
                    &arm_locals,
                    used_value_names,
                    used_record_type_names,
                );
            }
        }
        Expr::FieldAccess { record, .. } => {
            collect_expr_type_decl_refs(record, locals, used_value_names, used_record_type_names);
        }
        Expr::RecordConstruct { name, fields, .. } => {
            used_record_type_names.insert(name.clone());
            for (_, value) in fields {
                collect_expr_type_decl_refs(
                    value,
                    locals,
                    used_value_names,
                    used_record_type_names,
                );
            }
        }
        Expr::Lambda { args, body, .. } => {
            let mut inner = locals.clone();
            inner.extend(args.iter().cloned());
            collect_expr_type_decl_refs(body, &inner, used_value_names, used_record_type_names);
        }
        Expr::QualifiedCall { args, .. } => {
            for arg in args {
                collect_expr_type_decl_refs(arg, locals, used_value_names, used_record_type_names);
            }
        }
    }
}

enum TopLevelValueOrigin {
    Function,
    Extern,
    Import { module: String },
}

impl TopLevelValueOrigin {
    fn first_message(&self) -> String {
        match self {
            TopLevelValueOrigin::Function => "first defined here".to_string(),
            TopLevelValueOrigin::Extern => "first declared here".to_string(),
            TopLevelValueOrigin::Import { module } => {
                format!("first imported from `{module}` here")
            }
        }
    }

    fn second_message(&self) -> String {
        match self {
            TopLevelValueOrigin::Function => "redefined here".to_string(),
            TopLevelValueOrigin::Extern => "redeclared here".to_string(),
            TopLevelValueOrigin::Import { module } => {
                format!("also imported from `{module}` here")
            }
        }
    }
}

pub(crate) fn duplicate_top_level_value_diagnostics(
    decls: &[ast::Declaration],
    file_id: usize,
    module_exports: &HashMap<String, Vec<String>>,
) -> Vec<Diagnostic<usize>> {
    let mut seen: HashMap<String, (TopLevelValueOrigin, std::ops::Range<usize>)> = HashMap::new();
    let mut diags = Vec::new();

    for decl in decls {
        match decl {
            ast::Declaration::Expression(ast::Expr::LetFunc {
                name, name_span, ..
            }) => {
                if let Some((first_origin, first_span)) = seen.get(name) {
                    diags.push(
                        Diagnostic::error()
                            .with_message(format!("duplicate top-level name `{name}`"))
                            .with_labels(vec![
                                Label::primary(file_id, name_span.clone())
                                    .with_message(format!("`{name}` redefined here")),
                                Label::secondary(file_id, first_span.clone())
                                    .with_message(first_origin.first_message()),
                            ]),
                    );
                } else {
                    seen.insert(
                        name.clone(),
                        (TopLevelValueOrigin::Function, name_span.clone()),
                    );
                }
            }
            ast::Declaration::ExternLet {
                name, name_span, ..
            } => {
                if let Some((first_origin, first_span)) = seen.get(name) {
                    diags.push(
                        Diagnostic::error()
                            .with_message(format!("duplicate top-level name `{name}`"))
                            .with_labels(vec![
                                Label::primary(file_id, name_span.clone())
                                    .with_message(format!("`{name}` redeclared here")),
                                Label::secondary(file_id, first_span.clone())
                                    .with_message(first_origin.first_message()),
                            ]),
                    );
                } else {
                    seen.insert(
                        name.clone(),
                        (TopLevelValueOrigin::Extern, name_span.clone()),
                    );
                }
            }
            ast::Declaration::Use {
                path: (_, mod_name),
                unqualified,
                span,
                ..
            } => {
                for name in imported_names_for_use_decl(mod_name, unqualified, module_exports) {
                    let origin = TopLevelValueOrigin::Import {
                        module: mod_name.clone(),
                    };
                    if let Some((first_origin, first_span)) = seen.get(&name) {
                        diags.push(
                            Diagnostic::error()
                                .with_message(format!("duplicate top-level name `{name}`"))
                                .with_labels(vec![
                                    Label::primary(file_id, span.clone())
                                        .with_message(origin.second_message()),
                                    Label::secondary(file_id, first_span.clone())
                                        .with_message(first_origin.first_message()),
                                ]),
                        );
                    } else {
                        seen.insert(name, (origin, span.clone()));
                    }
                }
            }
            _ => {}
        }
    }

    diags
}

pub(crate) fn unused_function_spans(
    decls: &[ast::Declaration],
) -> Vec<(String, std::ops::Range<usize>)> {
    let mut top_level: HashMap<String, (bool, Vec<String>, ast::Expr, std::ops::Range<usize>)> =
        HashMap::new();
    let mut test_roots: Vec<ast::Expr> = Vec::new();

    for decl in decls {
        match decl {
            ast::Declaration::Expression(ast::Expr::LetFunc {
                name,
                is_pub,
                args,
                value,
                span,
                ..
            }) => {
                top_level.insert(
                    name.clone(),
                    (*is_pub, args.clone(), value.as_ref().clone(), span.clone()),
                );
            }
            ast::Declaration::Test { body, .. } => {
                test_roots.push(body.as_ref().clone());
            }
            _ => {}
        }
    }

    let top_names: HashSet<String> = top_level.keys().cloned().collect();
    let mut refs: HashMap<String, HashSet<String>> = HashMap::new();
    for (name, (_, args, body, _)) in &top_level {
        let mut local_scope: HashSet<String> = args.iter().cloned().collect();
        local_scope.insert(name.clone());
        let mut used = HashSet::new();
        collect_top_level_refs(body, &top_names, &local_scope, &mut used);
        refs.insert(name.clone(), used);
    }

    let mut roots: Vec<String> = top_level
        .iter()
        .filter_map(|(name, (is_pub, _, _, _))| {
            if *is_pub || name == "main" {
                Some(name.clone())
            } else {
                None
            }
        })
        .collect();
    if !test_roots.is_empty() {
        let empty_scope = HashSet::new();
        let mut test_used = HashSet::new();
        for body in &test_roots {
            collect_top_level_refs(body, &top_names, &empty_scope, &mut test_used);
        }
        roots.extend(test_used);
    }

    let mut reachable = HashSet::new();
    let mut queue: VecDeque<String> = roots.into_iter().collect();
    while let Some(name) = queue.pop_front() {
        if !reachable.insert(name.clone()) {
            continue;
        }
        if let Some(neighbors) = refs.get(&name) {
            for n in neighbors {
                queue.push_back(n.clone());
            }
        }
    }

    top_level
        .into_iter()
        .filter_map(|(name, (is_pub, _, _, span))| {
            if !is_pub && !reachable.contains(&name) {
                Some((name, span))
            } else {
                None
            }
        })
        .collect()
}

pub(crate) fn unused_local_spans(
    decls: &[ast::Declaration],
) -> Vec<(String, std::ops::Range<usize>)> {
    let mut unused = Vec::new();
    for decl in decls {
        match decl {
            ast::Declaration::Expression(ast::Expr::LetFunc { value, .. }) => {
                collect_unused_local_spans(value, &mut unused);
            }
            ast::Declaration::Test { body, .. } => {
                collect_unused_local_spans(body, &mut unused);
            }
            _ => {}
        }
    }
    unused
}

pub(crate) fn unused_type_spans(
    decls: &[ast::Declaration],
) -> Vec<(String, std::ops::Range<usize>)> {
    let mut private_record_types: HashMap<String, std::ops::Range<usize>> = HashMap::new();
    let mut private_variant_types: HashMap<String, (Vec<String>, std::ops::Range<usize>)> =
        HashMap::new();
    for decl in decls {
        if let ast::Declaration::Type(type_decl) = decl {
            match type_decl {
                ast::TypeDecl::Record {
                    is_pub, name, span, ..
                } => {
                    if !is_pub {
                        private_record_types.insert(name.clone(), span.clone());
                    }
                }
                ast::TypeDecl::Variant {
                    is_pub,
                    name,
                    constructors,
                    span,
                    ..
                } => {
                    if !is_pub {
                        let constructor_names = constructors
                            .iter()
                            .map(|(constructor_name, _)| constructor_name.clone())
                            .collect();
                        private_variant_types
                            .insert(name.clone(), (constructor_names, span.clone()));
                    }
                }
            }
        }
    }

    let mut used_type_names = HashSet::new();
    let mut used_value_names = HashSet::new();
    let mut used_record_type_names = HashSet::new();
    let empty_locals = HashSet::new();
    for decl in decls {
        match decl {
            ast::Declaration::Type(type_decl) => {
                collect_decl_type_usage_names(type_decl, &mut used_type_names);
            }
            ast::Declaration::Expression(ast::Expr::LetFunc {
                name, args, value, ..
            }) => {
                let mut locals: HashSet<String> = args.iter().cloned().collect();
                locals.insert(name.clone());
                collect_expr_type_decl_refs(
                    value,
                    &locals,
                    &mut used_value_names,
                    &mut used_record_type_names,
                );
            }
            ast::Declaration::Test { body, .. } => {
                collect_expr_type_decl_refs(
                    body,
                    &empty_locals,
                    &mut used_value_names,
                    &mut used_record_type_names,
                );
            }
            _ => {}
        }
    }

    let mut unused = Vec::new();
    for (name, span) in private_record_types {
        if !used_type_names.contains(name.as_str())
            && !used_record_type_names.contains(name.as_str())
        {
            unused.push((name, span));
        }
    }
    for (name, (constructor_names, span)) in private_variant_types {
        let constructor_used = constructor_names
            .iter()
            .any(|constructor_name| used_value_names.contains(constructor_name.as_str()));
        if !used_type_names.contains(name.as_str()) && !constructor_used {
            unused.push((name, span));
        }
    }
    unused
}

pub(crate) fn unused_unqualified_import_diagnostics(
    decls: &[ast::Declaration],
    file_id: usize,
    module_exports: &HashMap<String, Vec<String>>,
    imported_type_decls: &[ast::TypeDecl],
) -> Vec<Diagnostic<usize>> {
    let used = used_unqualified_names(decls);
    let used_modules = used_qualified_modules(decls);
    let mut diags = Vec::new();

    for decl in decls {
        let ast::Declaration::Use {
            path: (_, mod_name),
            unqualified,
            span,
            ..
        } = decl
        else {
            continue;
        };

        match unqualified {
            ast::UnqualifiedImports::None => {
                if !used_modules.contains(mod_name.as_str()) {
                    diags.push(
                        Diagnostic::warning()
                            .with_message(format!("unused import `{mod_name}`"))
                            .with_labels(vec![
                                Label::primary(file_id, span.clone())
                                    .with_message("this module import is never used"),
                            ]),
                    );
                }
            }
            ast::UnqualifiedImports::Specific(names) => {
                let unused: Vec<String> = names
                    .iter()
                    .filter(|name| {
                        if used.contains(name.as_str()) {
                            return false;
                        }
                        let ctor_used = imported_type_decls.iter().any(|type_decl| {
                            let ast::TypeDecl::Variant {
                                name: type_name,
                                constructors,
                                ..
                            } = type_decl
                            else {
                                return false;
                            };
                            if type_name != *name {
                                return false;
                            }
                            constructors
                                .iter()
                                .any(|(ctor_name, _)| used.contains(ctor_name))
                        });
                        !ctor_used
                    })
                    .cloned()
                    .collect();
                if !unused.is_empty() {
                    diags.push(
                        Diagnostic::warning()
                            .with_message(format!(
                                "unused unqualified imports from `{mod_name}`: {}",
                                unused.join(", ")
                            ))
                            .with_labels(vec![
                                Label::primary(file_id, span.clone())
                                    .with_message("these imports are never used unqualified"),
                            ]),
                    );
                }
            }
            ast::UnqualifiedImports::Wildcard => {
                let exports = module_exports
                    .get(mod_name.as_str())
                    .cloned()
                    .unwrap_or_default();
                if !exports.is_empty() && !exports.iter().any(|name| used.contains(name.as_str())) {
                    diags.push(
                        Diagnostic::warning()
                            .with_message(format!("unused wildcard import from `{mod_name}`"))
                            .with_labels(vec![Label::primary(file_id, span.clone()).with_message(
                                "no unqualified names from this wildcard import are used",
                            )]),
                    );
                }
            }
        }
    }

    diags
}
