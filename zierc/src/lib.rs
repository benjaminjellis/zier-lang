use std::collections::{HashMap, HashSet, VecDeque};

pub mod ast;
pub mod codegen;
pub mod ir;
pub mod lexer;
pub mod lower;
pub mod resolve;
pub mod session;
pub mod sexpr;
pub mod typecheck;

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

fn duplicate_import_diagnostics(
    decls: &[ast::Declaration],
    file_id: usize,
    module_exports: &HashMap<String, Vec<String>>,
) -> Vec<codespan_reporting::diagnostic::Diagnostic<usize>> {
    use codespan_reporting::diagnostic::{Diagnostic, Label};

    let mut imported: HashMap<String, (String, std::ops::Range<usize>)> = HashMap::new();
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

        for name in imported_names_for_use_decl(mod_name, unqualified, module_exports) {
            if let Some((first_module, first_span)) = imported.get(&name) {
                diags.push(
                    Diagnostic::error()
                        .with_message(format!("duplicate import `{name}`"))
                        .with_labels(vec![
                            Label::primary(file_id, span.clone())
                                .with_message(format!("`{name}` also imported here")),
                            Label::secondary(file_id, first_span.clone())
                                .with_message(format!("first imported from `{first_module}`")),
                        ]),
                );
            } else {
                imported.insert(name, (mod_name.clone(), span.clone()));
            }
        }
    }

    diags
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

fn unused_function_spans(decls: &[ast::Declaration]) -> Vec<(String, std::ops::Range<usize>)> {
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

fn unused_type_spans(decls: &[ast::Declaration]) -> Vec<(String, std::ops::Range<usize>)> {
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

fn unused_unqualified_import_diagnostics(
    decls: &[ast::Declaration],
    file_id: usize,
    module_exports: &HashMap<String, Vec<String>>,
    imported_type_decls: &[ast::TypeDecl],
) -> Vec<codespan_reporting::diagnostic::Diagnostic<usize>> {
    use codespan_reporting::diagnostic::{Diagnostic, Label};

    let used = used_unqualified_names(decls);
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
            ast::UnqualifiedImports::None => {}
            ast::UnqualifiedImports::Specific(names) => {
                let unused: Vec<String> = names
                    .iter()
                    .filter(|name| {
                        if used.contains(name.as_str()) {
                            return false;
                        }
                        // Importing a variant type also imports its constructors.
                        // Count constructor usage (e.g. Ok/Error) as usage of the type import.
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

/// Compile without any imports (single-file or when imports are already resolved).
pub fn compile(module_name: &str, source: &str) -> Option<String> {
    compile_with_imports(
        module_name,
        source,
        &format!("{module_name}.zier"),
        HashMap::new(),
        &HashMap::new(),
        HashMap::new(),
        &[],
        &HashMap::new(),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn compile_with_imports_in_session(
    sess: &mut session::CompilerSession,
    module_name: &str,
    source: &str,
    source_path: &str,
    imports: HashMap<String, String>,
    module_exports: &HashMap<String, Vec<String>>,
    module_aliases: HashMap<String, String>,
    imported_type_decls: &[ast::TypeDecl],
    imported_schemes: &typecheck::TypeEnv,
) -> session::CompileReport {
    let mut diagnostics = Vec::new();
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(source).lex();

    let file_id = lowerer.add_file(source_path.to_string(), source.to_string());

    let sexprs = match crate::sexpr::SExprParser::new(tokens, file_id).parse() {
        Ok(res) => res,
        Err(diag) => {
            diagnostics.push(diag.clone());
            sess.emit(&lowerer.files, &diag);
            return session::CompileReport {
                output: None,
                files: lowerer.files,
                diagnostics,
            };
        }
    };

    let decls = lowerer.lower_file(file_id, &sexprs);

    for diag in &lowerer.diagnostics {
        diagnostics.push(diag.clone());
        sess.emit(&lowerer.files, diag);
    }
    if !lowerer.diagnostics.is_empty() {
        return session::CompileReport {
            output: None,
            files: lowerer.files,
            diagnostics,
        };
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
            diagnostics.push(diag.clone());
            sess.emit(&lowerer.files, &diag);
            use_errors = true;
        }
    }
    if use_errors {
        return session::CompileReport {
            output: None,
            files: lowerer.files,
            diagnostics,
        };
    }
    let duplicate_imports = duplicate_import_diagnostics(&decls, file_id, module_exports);
    for diag in &duplicate_imports {
        diagnostics.push(diag.clone());
        sess.emit(&lowerer.files, diag);
    }
    if !duplicate_imports.is_empty() {
        return session::CompileReport {
            output: None,
            files: lowerer.files,
            diagnostics,
        };
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
    let symbols = sess.symbol_table(module_exports);
    let unresolved = resolve::unresolved_env_names(&decls, imports.keys().cloned(), &env, symbols);
    env.extend(typecheck::import_env(&unresolved));

    if let Err(err) = checker.check_program(&mut env, &decls, file_id) {
        let type_diags = err.0.to_diagnostics(file_id, err.1.span());
        for diag in type_diags {
            diagnostics.push(diag.clone());
            sess.emit(&lowerer.files, &diag);
        }
        return session::CompileReport {
            output: None,
            files: lowerer.files,
            diagnostics,
        };
    }

    for (name, span) in unused_function_spans(&decls) {
        let diag = codespan_reporting::diagnostic::Diagnostic::warning()
            .with_message(format!("unused function `{name}`"))
            .with_labels(vec![
                codespan_reporting::diagnostic::Label::primary(file_id, span)
                    .with_message("this private function is never used"),
            ]);
        diagnostics.push(diag.clone());
        sess.emit(&lowerer.files, &diag);
    }
    for (name, span) in unused_type_spans(&decls) {
        let diag = codespan_reporting::diagnostic::Diagnostic::warning()
            .with_message(format!("unused type `{name}`"))
            .with_labels(vec![
                codespan_reporting::diagnostic::Label::primary(file_id, span)
                    .with_message("this private type is never referenced"),
            ]);
        diagnostics.push(diag.clone());
        sess.emit(&lowerer.files, &diag);
    }
    for diag in
        unused_unqualified_import_diagnostics(&decls, file_id, module_exports, imported_type_decls)
    {
        diagnostics.push(diag.clone());
        sess.emit(&lowerer.files, &diag);
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
    session::CompileReport {
        output: Some(codegen::emit_module(&module)),
        files: lowerer.files,
        diagnostics,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn compile_with_imports_report(
    module_name: &str,
    source: &str,
    source_path: &str,
    imports: HashMap<String, String>,
    module_exports: &HashMap<String, Vec<String>>,
    module_aliases: HashMap<String, String>,
    imported_type_decls: &[ast::TypeDecl],
    imported_schemes: &typecheck::TypeEnv,
) -> session::CompileReport {
    let mut sess = session::CompilerSession::default();
    compile_with_imports_in_session(
        &mut sess,
        module_name,
        source,
        source_path,
        imports,
        module_exports,
        module_aliases,
        imported_type_decls,
        imported_schemes,
    )
}

/// Compile with import resolution.
/// - `imports`: unqualified name → module (from `use` declarations)
/// - `module_exports`: module name → exported function names (for validating qualified calls)
/// - `imported_type_decls`: pub type declarations from imported modules (brings constructors into scope)
/// - `imported_schemes`: real type schemes from imported modules (keyed by "fn" or "module/fn")
#[allow(clippy::too_many_arguments)]
pub fn compile_with_imports(
    module_name: &str,
    source: &str,
    source_path: &str,
    imports: HashMap<String, String>,
    module_exports: &HashMap<String, Vec<String>>,
    module_aliases: HashMap<String, String>,
    imported_type_decls: &[ast::TypeDecl],
    imported_schemes: &typecheck::TypeEnv,
) -> Option<String> {
    let report = compile_with_imports_report(
        module_name,
        source,
        source_path,
        imports,
        module_exports,
        module_aliases,
        imported_type_decls,
        imported_schemes,
    );
    session::emit_compile_report(&report, true);
    report.output
}

/// Extract the names of `pub` top-level functions declared in a source file.
/// Only pub functions are importable by other modules.
pub fn exported_names(source: &str) -> Vec<String> {
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(source).lex();
    let file_id = lowerer.add_file("scan.zier".into(), source.into());
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

/// Returns true when source defines a top-level nullary entrypoint:
/// `(let main {} ...)`.
pub fn has_nullary_main(source: &str) -> bool {
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(source).lex();
    let file_id = lowerer.add_file("scan.zier".into(), source.into());
    let Ok(sexprs) = crate::sexpr::SExprParser::new(tokens, file_id).parse() else {
        return false;
    };
    let decls = lowerer.lower_file(file_id, &sexprs);
    decls.into_iter().any(|d| {
        matches!(
            d,
            ast::Declaration::Expression(ast::Expr::LetFunc { name, args, .. })
            if name == "main" && args.is_empty()
        )
    })
}

/// Extract the modules that a lib.zier publicly re-exports via `(pub use X)`.
/// Used to gate `(use std/io)` — io must be pub-used by lib.zier.
pub fn pub_reexports(source: &str) -> Vec<String> {
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(source).lex();
    let file_id = lowerer.add_file("scan.zier".into(), source.into());
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
/// Returns `(namespace, module, unqualified)` triples — local modules have an empty namespace.
pub fn used_modules(source: &str) -> Vec<(String, String, ast::UnqualifiedImports)> {
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(source).lex();
    let file_id = lowerer.add_file("scan.zier".into(), source.into());
    let Ok(sexprs) = crate::sexpr::SExprParser::new(tokens, file_id).parse() else {
        return vec![];
    };
    let decls = lowerer.lower_file(file_id, &sexprs);
    decls
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

/// Extract `pub` type declarations from a source file.
/// Used to bring constructors and field accessors into scope when the module is imported.
pub fn exported_type_decls(source: &str) -> Vec<ast::TypeDecl> {
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(source).lex();
    let file_id = lowerer.add_file("scan.zier".into(), source.into());
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

/// Type-check a module and return the inferred schemes for its top-level functions.
/// Keys are plain function names ("get", "put", ...).
///
/// Returns an empty map if the module fails to parse or type-check.
pub fn infer_module_bindings(
    module_name: &str,
    source: &str,
    imports: HashMap<String, String>,
    module_exports: &HashMap<String, Vec<String>>,
    imported_type_decls: &[ast::TypeDecl],
    imported_schemes: &typecheck::TypeEnv,
) -> typecheck::TypeEnv {
    let mut sess = session::CompilerSession::new(session::SessionOptions {
        emit_diagnostics: false,
        emit_warnings: false,
    });
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(source).lex();
    let file_id = lowerer.add_file(format!("{module_name}.zier"), source.to_string());

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

    let unresolved = resolve::unresolved_env_names(
        &decls,
        imports.keys().cloned(),
        &env,
        sess.symbol_table(module_exports),
    );
    env.extend(typecheck::import_env(&unresolved));

    // Also seed type def spans from imported type decls (for better errors — optional)
    for type_decl in imported_type_decls {
        env.extend(typecheck::constructor_schemes(type_decl));
    }

    if checker.check_program(&mut env, &decls, file_id).is_err() {
        return HashMap::new();
    }

    // Collect top-level functions from this module.
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

/// Type-check a module and return best-effort inferred expression types keyed by source span.
///
/// This is intended for editor tooling such as hover. Types are recorded after local
/// inference steps and may be less precise than the final principal type for some
/// outer-constrained expressions, but they are accurate for most variables and subexpressions.
pub fn infer_module_expr_types(
    module_name: &str,
    source: &str,
    imports: HashMap<String, String>,
    module_exports: &HashMap<String, Vec<String>>,
    imported_type_decls: &[ast::TypeDecl],
    imported_schemes: &typecheck::TypeEnv,
) -> Vec<(std::ops::Range<usize>, String)> {
    let mut sess = session::CompilerSession::new(session::SessionOptions {
        emit_diagnostics: false,
        emit_warnings: false,
    });
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(source).lex();
    let file_id = lowerer.add_file(format!("{module_name}.zier"), source.to_string());

    let sexprs = match crate::sexpr::SExprParser::new(tokens, file_id).parse() {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let decls = lowerer.lower_file(file_id, &sexprs);
    if !lowerer.diagnostics.is_empty() {
        return Vec::new();
    }

    let mut checker = typecheck::TypeChecker::new();
    let mut env = typecheck::primitive_env();
    for type_decl in imported_type_decls {
        env.extend(typecheck::constructor_schemes(type_decl));
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
    let all_bindings = infer_module_bindings(
        module_name,
        source,
        imports,
        module_exports,
        imported_type_decls,
        imported_schemes,
    );

    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(source).lex();
    let file_id = lowerer.add_file(format!("{module_name}.zier"), source.to_string());
    let Ok(sexprs) = crate::sexpr::SExprParser::new(tokens, file_id).parse() else {
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

/// Extract test declarations from a test source file.
/// Returns `(display_name, erlang_fn_name)` pairs in declaration order.
/// The erlang_fn_name matches what codegen emits: `zier_test_0`, `zier_test_1`, ...
pub fn test_declarations(source: &str) -> Vec<(String, String)> {
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(source).lex();
    // Use a tests/ path so the lowerer accepts `test` declarations
    let file_id = lowerer.add_file("tests/scan.zier".into(), source.into());
    let Ok(sexprs) = crate::sexpr::SExprParser::new(tokens, file_id).parse() else {
        return vec![];
    };
    let decls = lowerer.lower_file(file_id, &sexprs);
    let mut test_idx = 0;
    decls
        .into_iter()
        .filter_map(|d| {
            if let ast::Declaration::Test { name, .. } = d {
                let fn_name = format!("zier_test_{test_idx}");
                test_idx += 1;
                Some((name, fn_name))
            } else {
                None
            }
        })
        .collect()
}

pub fn dummy_compile(source: &str) {
    compile("test", source);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualified_std_call_requires_use() {
        let mut module_exports = HashMap::new();
        module_exports.insert(
            "io".to_string(),
            vec!["println".to_string(), "debug".to_string()],
        );

        let without_use = "(let main {} (io/println \"hello\"))";
        let without_use_result = compile_with_imports(
            "main",
            without_use,
            "main.zier",
            HashMap::new(),
            &module_exports,
            HashMap::new(),
            &[],
            &HashMap::new(),
        );
        assert!(without_use_result.is_none());

        let with_use = "(use std/io)\n(let main {} (io/println \"hello\"))";
        let with_use_result = compile_with_imports(
            "main",
            with_use,
            "main.zier",
            HashMap::new(),
            &module_exports,
            HashMap::new(),
            &[],
            &HashMap::new(),
        );
        assert!(with_use_result.is_some());
    }

    #[test]
    fn duplicate_unqualified_imports_error() {
        let mut module_exports = HashMap::new();
        module_exports.insert("a".to_string(), vec!["map".to_string()]);
        module_exports.insert("b".to_string(), vec!["map".to_string()]);

        let src = "(use a [map])\n(use b [map])\n(let main {} map)";
        let result = compile_with_imports(
            "main",
            src,
            "main.zier",
            HashMap::new(),
            &module_exports,
            HashMap::new(),
            &[],
            &HashMap::new(),
        );
        assert!(result.is_none());
    }

    #[test]
    fn duplicate_module_use_without_unqualified_imports_is_allowed() {
        let mut module_exports = HashMap::new();
        module_exports.insert("io".to_string(), vec!["println".to_string()]);

        let src = "(use std/io)\n(use std/io)\n(let main {} (io/println \"Hey!\"))";
        let result = compile_with_imports(
            "main",
            src,
            "main.zier",
            HashMap::new(),
            &module_exports,
            HashMap::new(),
            &[],
            &HashMap::new(),
        );
        assert!(result.is_some());
    }

    #[test]
    fn wildcard_import_enables_unqualified_call() {
        let mut module_exports = HashMap::new();
        module_exports.insert("math".to_string(), vec!["inc".to_string()]);
        let mut imports = HashMap::new();
        imports.insert("inc".to_string(), "math".to_string());

        let src = "(use math [*])\n(let main {} (inc 1))";
        let result = compile_with_imports(
            "main",
            src,
            "main.zier",
            imports,
            &module_exports,
            HashMap::new(),
            &[],
            &HashMap::new(),
        );
        assert!(result.is_some());
    }

    #[test]
    fn local_shadowing_beats_unqualified_import() {
        let mut module_exports = HashMap::new();
        module_exports.insert("m".to_string(), vec!["x".to_string()]);
        let mut imports = HashMap::new();
        imports.insert("x".to_string(), "m".to_string());

        let src = "(use m [x])\n(let main {x} x)";
        let result = compile_with_imports(
            "main",
            src,
            "main.zier",
            imports,
            &module_exports,
            HashMap::new(),
            &[],
            &HashMap::new(),
        );
        assert!(result.is_some());
    }

    #[test]
    fn pub_reexports_only_include_pub_use_decls() {
        let src = "(pub use std/io)\n(use std/result)\n(pub use math)";
        let reexports = pub_reexports(src);
        assert_eq!(reexports, vec!["io".to_string(), "math".to_string()]);
    }

    #[test]
    fn infer_module_exports_preserves_result_bind_error_type() {
        let src = include_str!("../../zier-std/src/result.zier");
        let mut module_exports = HashMap::new();
        module_exports.insert(
            "result".to_string(),
            vec!["Result".to_string(), "bind".to_string()],
        );

        let schemes = infer_module_exports(
            "result",
            src,
            HashMap::new(),
            &module_exports,
            &[],
            &HashMap::new(),
        );

        let bind = schemes.get("bind").expect("missing bind export");
        assert_eq!(
            bind.vars.len(),
            3,
            "bind should quantify success, continuation result, and error"
        );
        let bind_var_set: std::collections::HashSet<u64> = bind.vars.iter().copied().collect();
        assert_eq!(
            bind_var_set.len(),
            3,
            "bind quantified vars should be distinct"
        );
        match bind.ty.as_ref() {
            typecheck::Type::Fun(m, rest) => {
                match m.as_ref() {
                    typecheck::Type::Con(name, args) => {
                        assert_eq!(name, "Result");
                        assert_eq!(args.len(), 2);
                        assert_ne!(args[0], args[1], "success and error vars collapsed");
                    }
                    other => panic!("expected Result argument, got {other:?}"),
                }
                match rest.as_ref() {
                    typecheck::Type::Fun(func, ret) => {
                        match func.as_ref() {
                            typecheck::Type::Fun(arg, func_ret) => {
                                assert_eq!(
                                    arg,
                                    &match m.as_ref() {
                                        typecheck::Type::Con(_, args) => args[0].clone(),
                                        _ => unreachable!(),
                                    }
                                );
                                match func_ret.as_ref() {
                                    typecheck::Type::Con(name, args) => {
                                        assert_eq!(name, "Result");
                                        assert_eq!(args.len(), 2);
                                    }
                                    other => panic!("expected Result return, got {other:?}"),
                                }
                            }
                            other => panic!("expected function continuation, got {other:?}"),
                        }
                        match ret.as_ref() {
                            typecheck::Type::Con(name, args) => {
                                assert_eq!(name, "Result");
                                assert_eq!(args.len(), 2);
                            }
                            other => panic!("expected Result return, got {other:?}"),
                        }
                    }
                    other => panic!("expected second function arg, got {other:?}"),
                }
            }
            other => panic!("expected function type, got {other:?}"),
        }
    }

    #[test]
    fn imported_result_bind_reports_continuation_mismatch() {
        let result_src = include_str!("../../zier-std/src/result.zier");
        let io_src = include_str!("../../zier-std/src/io.zier");

        let mut module_exports = HashMap::new();
        module_exports.insert(
            "result".to_string(),
            vec!["Result".to_string(), "bind".to_string()],
        );
        module_exports.insert(
            "io".to_string(),
            vec!["println".to_string(), "debug".to_string()],
        );

        let result_schemes = infer_module_exports(
            "result",
            result_src,
            HashMap::new(),
            &module_exports,
            &[],
            &HashMap::new(),
        );
        let io_schemes = infer_module_exports(
            "io",
            io_src,
            HashMap::new(),
            &module_exports,
            &[],
            &HashMap::new(),
        );

        let mut imported_schemes = HashMap::new();
        imported_schemes.insert("bind".to_string(), result_schemes["bind"].clone());
        imported_schemes.insert("result/bind".to_string(), result_schemes["bind"].clone());
        imported_schemes.insert("debug".to_string(), io_schemes["debug"].clone());
        imported_schemes.insert("io/debug".to_string(), io_schemes["debug"].clone());

        let imported_type_decls = exported_type_decls(result_src);
        let mut imports = HashMap::new();
        imports.insert("bind".to_string(), "result".to_string());
        imports.insert("debug".to_string(), "io".to_string());

        let src = r#"
            (use result [Result bind])
            (use io [debug])
            (let ok {} (Ok ()))
            (let main {}
              (let? [val (ok)]
                (debug val)))
        "#;

        let report = compile_with_imports_report(
            "main",
            src,
            "main.zier",
            imports,
            &module_exports,
            HashMap::new(),
            &imported_type_decls,
            &imported_schemes,
        );

        assert!(report.has_errors(), "expected type error");
        let rendered: Vec<String> = report
            .diagnostics
            .iter()
            .map(|d| d.message.clone())
            .collect();
        assert!(
            rendered
                .iter()
                .any(|msg| msg.contains("type mismatch: expected `Result")),
            "unexpected diagnostics: {rendered:?}"
        );
    }

    #[test]
    fn test_declaration_with_imported_bind_reports_continuation_mismatch() {
        let result_src = include_str!("../../zier-std/src/result.zier");
        let io_src = include_str!("../../zier-std/src/io.zier");
        let testing_src = include_str!("../../zier-std/src/testing.zier");

        let mut module_exports = HashMap::new();
        module_exports.insert(
            "result".to_string(),
            vec!["Result".to_string(), "bind".to_string()],
        );
        module_exports.insert(
            "io".to_string(),
            vec!["println".to_string(), "debug".to_string()],
        );
        module_exports.insert(
            "testing".to_string(),
            vec![
                "assert".to_string(),
                "assert_eq".to_string(),
                "assert_ne".to_string(),
            ],
        );

        let result_schemes = infer_module_exports(
            "result",
            result_src,
            HashMap::new(),
            &module_exports,
            &[],
            &HashMap::new(),
        );
        let io_schemes = infer_module_exports(
            "io",
            io_src,
            HashMap::new(),
            &module_exports,
            &[],
            &HashMap::new(),
        );
        let testing_schemes = infer_module_exports(
            "testing",
            testing_src,
            HashMap::new(),
            &module_exports,
            &exported_type_decls(result_src),
            &result_schemes,
        );

        let mut imported_schemes = HashMap::new();
        for (name, scheme) in &result_schemes {
            imported_schemes.insert(name.clone(), scheme.clone());
            imported_schemes.insert(format!("result/{name}"), scheme.clone());
        }
        for (name, scheme) in &io_schemes {
            imported_schemes.insert(name.clone(), scheme.clone());
            imported_schemes.insert(format!("io/{name}"), scheme.clone());
        }
        for (name, scheme) in &testing_schemes {
            imported_schemes.insert(name.clone(), scheme.clone());
            imported_schemes.insert(format!("testing/{name}"), scheme.clone());
        }

        let mut imports = HashMap::new();
        imports.insert("bind".to_string(), "result".to_string());
        imports.insert("debug".to_string(), "io".to_string());
        imports.insert("assert_eq".to_string(), "testing".to_string());

        let mut imported_type_decls = exported_type_decls(result_src);
        imported_type_decls.extend(exported_type_decls(testing_src));

        let src = r#"
            (use result [bind])
            (use io)
            (use testing [assert_eq])
            (test "x"
              (let? [val (assert_eq 1 1)]
                (io/debug val)))
        "#;

        let report = compile_with_imports_report(
            "string_test",
            src,
            "tests/string_test.zier",
            imports,
            &module_exports,
            HashMap::new(),
            &imported_type_decls,
            &imported_schemes,
        );

        assert!(report.has_errors(), "expected type error");
        let labels: Vec<String> = report
            .diagnostics
            .iter()
            .flat_map(|d| d.labels.iter().map(|l| l.message.clone()))
            .collect();
        assert!(
            labels
                .iter()
                .any(|msg| msg.contains("`bind` expects `(Unit -> Result")),
            "unexpected labels: {labels:?}"
        );
    }

    #[test]
    fn session_can_suppress_warning_emission() {
        let mut sess = session::CompilerSession::new(session::SessionOptions {
            emit_diagnostics: true,
            emit_warnings: false,
        });
        let src = "(let main {} 0)\n(let dead {} 1)";
        let result = compile_with_imports_in_session(
            &mut sess,
            "main",
            src,
            "main.zier",
            HashMap::new(),
            &HashMap::new(),
            HashMap::new(),
            &[],
            &HashMap::new(),
        );
        assert!(result.output.is_some());
        assert_eq!(sess.emitted_warnings, 0);
    }

    #[test]
    fn session_still_emits_errors_when_warnings_disabled() {
        let mut sess = session::CompilerSession::new(session::SessionOptions {
            emit_diagnostics: true,
            emit_warnings: false,
        });
        let src = "(let main {} unknown)";
        let result = compile_with_imports_in_session(
            &mut sess,
            "main",
            src,
            "main.zier",
            HashMap::new(),
            &HashMap::new(),
            HashMap::new(),
            &[],
            &HashMap::new(),
        );
        assert!(result.output.is_none());
        assert!(sess.emitted_errors > 0);
    }

    #[test]
    fn unused_function_analysis_marks_private_unreachable_only() {
        let src = "(let main {} (live))\n(let live {} (helper))\n(let helper {} 1)\n(let dead {} 42)\n(pub let api {} 0)";
        let mut lowerer = lower::Lowerer::new();
        let tokens = crate::lexer::Lexer::new(src).lex();
        let file_id = lowerer.add_file("scan.zier".into(), src.into());
        let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
            .parse()
            .expect("parse");
        let decls = lowerer.lower_file(file_id, &sexprs);

        let mut unused: Vec<String> = unused_function_spans(&decls)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        unused.sort();
        assert_eq!(unused, vec!["dead".to_string()]);
    }

    #[test]
    fn unqualified_import_warnings_skip_qualified_only_use() {
        let src = "(use std/io)\n(let main {} ())";
        let mut lowerer = lower::Lowerer::new();
        let tokens = crate::lexer::Lexer::new(src).lex();
        let file_id = lowerer.add_file("scan.zier".into(), src.into());
        let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
            .parse()
            .expect("parse");
        let decls = lowerer.lower_file(file_id, &sexprs);

        let mut module_exports = HashMap::new();
        module_exports.insert("io".to_string(), vec!["println".to_string()]);
        let warnings = unused_unqualified_import_diagnostics(&decls, file_id, &module_exports, &[]);
        assert!(warnings.is_empty());
    }

    #[test]
    fn unqualified_import_warnings_flag_unused_specific_and_wildcard() {
        let src =
            "(use std/io)\n(use std/result [Result bind])\n(use std/option [*])\n(let main {} ())";
        let mut lowerer = lower::Lowerer::new();
        let tokens = crate::lexer::Lexer::new(src).lex();
        let file_id = lowerer.add_file("scan.zier".into(), src.into());
        let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
            .parse()
            .expect("parse");
        let decls = lowerer.lower_file(file_id, &sexprs);

        let mut module_exports = HashMap::new();
        module_exports.insert("io".to_string(), vec!["println".to_string()]);
        module_exports.insert(
            "result".to_string(),
            vec!["Result".to_string(), "bind".to_string()],
        );
        module_exports.insert(
            "option".to_string(),
            vec!["Some".to_string(), "None".to_string()],
        );
        let warnings = unused_unqualified_import_diagnostics(&decls, file_id, &module_exports, &[]);
        assert_eq!(warnings.len(), 2);
        let messages: Vec<String> = warnings.into_iter().map(|d| d.message).collect();
        assert!(
            messages
                .iter()
                .any(|m| m.contains("unused unqualified imports from `result`")),
            "missing specific import warning: {messages:?}"
        );
        assert!(
            messages
                .iter()
                .any(|m| m.contains("unused wildcard import from `option`")),
            "missing wildcard import warning: {messages:?}"
        );
    }

    #[test]
    fn unqualified_import_warnings_count_type_decl_usage() {
        let src = "(use std/option [Option])\n(type Attributes ((:max_age ~ Option Int)))\n(let main {} ())";
        let mut lowerer = lower::Lowerer::new();
        let tokens = crate::lexer::Lexer::new(src).lex();
        let file_id = lowerer.add_file("scan.zier".into(), src.into());
        let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
            .parse()
            .expect("parse");
        let decls = lowerer.lower_file(file_id, &sexprs);

        let mut module_exports = HashMap::new();
        module_exports.insert(
            "option".to_string(),
            vec!["Option".to_string(), "Some".to_string(), "None".to_string()],
        );
        let warnings = unused_unqualified_import_diagnostics(&decls, file_id, &module_exports, &[]);
        assert!(
            warnings.is_empty(),
            "expected no unused import warnings, got: {:?}",
            warnings
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn unqualified_import_warnings_count_variant_constructor_usage_for_type_import() {
        let src = "(use std/result [Result])\n(let main {} (Ok 1))";
        let mut lowerer = lower::Lowerer::new();
        let tokens = crate::lexer::Lexer::new(src).lex();
        let file_id = lowerer.add_file("scan.zier".into(), src.into());
        let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
            .parse()
            .expect("parse");
        let decls = lowerer.lower_file(file_id, &sexprs);

        let mut module_exports = HashMap::new();
        module_exports.insert(
            "result".to_string(),
            vec!["Result".to_string(), "bind".to_string()],
        );
        let imported_type_decls =
            exported_type_decls("(pub type ['a 'e] Result ((Ok ~ 'a) (Error ~ 'e)))");
        let warnings = unused_unqualified_import_diagnostics(
            &decls,
            file_id,
            &module_exports,
            &imported_type_decls,
        );
        assert!(
            warnings.is_empty(),
            "expected no unused import warning for type import used via constructors, got: {:?}",
            warnings
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn unqualified_import_warnings_count_record_constructor_usage_for_type_import() {
        let src = "(use std/unknown [DecodeError])\n(let main {} (DecodeError :expected \"Int\" :found \"String\"))";
        let mut lowerer = lower::Lowerer::new();
        let tokens = crate::lexer::Lexer::new(src).lex();
        let file_id = lowerer.add_file("scan.zier".into(), src.into());
        let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
            .parse()
            .expect("parse");
        let decls = lowerer.lower_file(file_id, &sexprs);

        let mut module_exports = HashMap::new();
        module_exports.insert("unknown".to_string(), vec!["DecodeError".to_string()]);
        let imported_type_decls =
            exported_type_decls("(pub type DecodeError ((:expected ~ String) (:found ~ String)))");
        let warnings = unused_unqualified_import_diagnostics(
            &decls,
            file_id,
            &module_exports,
            &imported_type_decls,
        );
        assert!(
            warnings.is_empty(),
            "expected no unused import warning for record type import used via construction, got: {:?}",
            warnings
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn unused_type_analysis_marks_private_unreferenced_type() {
        let src = "(type Attributes ((:max_age ~ Int)))\n(let main {} ())";
        let mut lowerer = lower::Lowerer::new();
        let tokens = crate::lexer::Lexer::new(src).lex();
        let file_id = lowerer.add_file("scan.zier".into(), src.into());
        let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
            .parse()
            .expect("parse");
        let decls = lowerer.lower_file(file_id, &sexprs);

        let mut unused: Vec<String> = unused_type_spans(&decls)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        unused.sort();
        assert_eq!(unused, vec!["Attributes".to_string()]);
    }

    #[test]
    fn unused_type_analysis_counts_variant_constructor_usage() {
        let src = "(type Flag (On Off))\n(let main {} On)";
        let mut lowerer = lower::Lowerer::new();
        let tokens = crate::lexer::Lexer::new(src).lex();
        let file_id = lowerer.add_file("scan.zier".into(), src.into());
        let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
            .parse()
            .expect("parse");
        let decls = lowerer.lower_file(file_id, &sexprs);

        assert!(
            unused_type_spans(&decls).is_empty(),
            "expected type to be considered used via constructor"
        );
    }

    #[test]
    fn infer_module_expr_types_include_function_arg_and_match_binding_spans() {
        let src = "(let inspect {input}\n\
                     (match 1\n\
                       value ~> (+ value input)))";
        let expr_types = infer_module_expr_types(
            "main",
            src,
            HashMap::new(),
            &HashMap::new(),
            &[],
            &HashMap::new(),
        );

        let find_type = |needle: &str, nth: usize| -> Option<String> {
            src.match_indices(needle)
                .nth(nth)
                .and_then(|(start, needle)| {
                    expr_types
                        .iter()
                        .filter(|(span, _)| span.start <= start && start + needle.len() <= span.end)
                        .min_by_key(|(span, _)| span.end.saturating_sub(span.start))
                        .map(|(_, ty)| ty.clone())
                })
        };

        assert_eq!(find_type("input", 0).as_deref(), Some("Int"));
        assert_eq!(find_type("value", 0).as_deref(), Some("Int"));
        assert_eq!(find_type("input", 1).as_deref(), Some("Int"));
    }
}
