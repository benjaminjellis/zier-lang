use std::collections::{HashMap, HashSet};

use crate::{ast, codegen, lower, resolve, session, sexpr, typecheck, warnings};

const PRIMITIVE_TYPE_NAMES: [&str; 6] = ["Int", "Float", "Bool", "String", "Unit", "List"];

fn collect_type_sig_names(sig: &ast::TypeSig, out: &mut HashSet<String>) {
    match sig {
        ast::TypeSig::Named(name) => {
            out.insert(name.clone());
        }
        ast::TypeSig::Generic(_) => {}
        ast::TypeSig::App(head, args) => {
            out.insert(head.clone());
            for arg in args {
                collect_type_sig_names(arg, out);
            }
        }
        ast::TypeSig::Fun(a, b) => {
            collect_type_sig_names(a, out);
            collect_type_sig_names(b, out);
        }
    }
}

fn collect_type_usage_names(usage: &ast::TypeUsage, out: &mut HashSet<String>) {
    match usage {
        ast::TypeUsage::Named(name, _) => {
            out.insert(name.clone());
        }
        ast::TypeUsage::Generic(_, _) => {}
        ast::TypeUsage::App(head, args, _) => {
            out.insert(head.clone());
            for arg in args {
                collect_type_usage_names(arg, out);
            }
        }
        ast::TypeUsage::Fun(arg, ret, _) => {
            collect_type_usage_names(arg, out);
            collect_type_usage_names(ret, out);
        }
    }
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

fn known_type_arities(
    decls: &[ast::Declaration],
    imported_type_decls: &[ast::TypeDecl],
) -> HashMap<String, usize> {
    let mut arities = HashMap::new();
    for name in PRIMITIVE_TYPE_NAMES {
        let arity = if name == "List" { 1 } else { 0 };
        arities.insert(name.to_string(), arity);
    }
    for type_decl in imported_type_decls {
        match type_decl {
            ast::TypeDecl::Record { name, params, .. } => {
                arities.insert(name.clone(), params.len());
            }
            ast::TypeDecl::Variant { name, params, .. } => {
                arities.insert(name.clone(), params.len());
            }
        }
    }
    for decl in decls {
        match decl {
            ast::Declaration::Type(ast::TypeDecl::Record { name, params, .. }) => {
                arities.insert(name.clone(), params.len());
            }
            ast::Declaration::Type(ast::TypeDecl::Variant { name, params, .. }) => {
                arities.insert(name.clone(), params.len());
            }
            ast::Declaration::ExternType { name, params, .. } => {
                arities.insert(name.clone(), params.len());
            }
            _ => {}
        }
    }
    arities
}

fn collect_type_usage_arity_errors(
    usage: &ast::TypeUsage,
    known_arities: &HashMap<String, usize>,
    generic_params: &HashSet<String>,
    out: &mut Vec<(String, usize, usize, std::ops::Range<usize>)>,
) {
    match usage {
        ast::TypeUsage::Generic(_, _) => {}
        ast::TypeUsage::Named(name, span) => {
            if generic_params.contains(name) {
                return;
            }
            if let Some(expected) = known_arities.get(name)
                && *expected > 0
            {
                out.push((name.clone(), *expected, 0, span.clone()));
            }
        }
        ast::TypeUsage::App(head, args, span) => {
            if !generic_params.contains(head)
                && let Some(expected) = known_arities.get(head)
                && *expected != args.len()
            {
                out.push((head.clone(), *expected, args.len(), span.clone()));
            }
            for arg in args {
                collect_type_usage_arity_errors(arg, known_arities, generic_params, out);
            }
        }
        ast::TypeUsage::Fun(arg, ret, _) => {
            collect_type_usage_arity_errors(arg, known_arities, generic_params, out);
            collect_type_usage_arity_errors(ret, known_arities, generic_params, out);
        }
    }
}

fn known_type_names(
    decls: &[ast::Declaration],
    imported_type_decls: &[ast::TypeDecl],
    imported_extern_types: &[String],
) -> HashSet<String> {
    let mut known: HashSet<String> = PRIMITIVE_TYPE_NAMES
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    for type_decl in imported_type_decls {
        match type_decl {
            ast::TypeDecl::Record { name, .. } => {
                known.insert(name.clone());
            }
            ast::TypeDecl::Variant { name, .. } => {
                known.insert(name.clone());
            }
        }
    }
    known.extend(imported_extern_types.iter().cloned());
    for decl in decls {
        match decl {
            ast::Declaration::Type(ast::TypeDecl::Record { name, .. }) => {
                known.insert(name.clone());
            }
            ast::Declaration::Type(ast::TypeDecl::Variant { name, .. }) => {
                known.insert(name.clone());
            }
            ast::Declaration::ExternType { name, .. } => {
                known.insert(name.clone());
            }
            _ => {}
        }
    }
    known
}

/// Compile without any imports (single-file or when imports are already resolved).
#[cfg(test)]
pub(crate) fn compile(module_name: &str, source: &str) -> Option<String> {
    compile_with_imports(
        module_name,
        source,
        &format!("{module_name}.mond"),
        HashMap::new(),
        &HashMap::new(),
        HashMap::new(),
        &[],
        &[],
        &HashMap::new(),
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
    imported_extern_types: &[String],
    imported_field_indices: &HashMap<String, usize>,
    imported_schemes: &typecheck::TypeEnv,
) -> session::CompileReport {
    let mut diagnostics = Vec::new();
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(source).lex();

    let file_id = lowerer.add_file(source_path.to_string(), source.to_string());

    let sexprs = match sexpr::SExprParser::new(tokens, file_id).parse() {
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

    let duplicate_top_level_values =
        warnings::duplicate_top_level_value_diagnostics(&decls, file_id, module_exports);
    for diag in &duplicate_top_level_values {
        diagnostics.push(diag.clone());
        sess.emit(&lowerer.files, diag);
    }
    if !duplicate_top_level_values.is_empty() {
        return session::CompileReport {
            output: None,
            files: lowerer.files,
            diagnostics,
        };
    }

    let duplicate_type_constructors =
        warnings::duplicate_type_constructor_diagnostics(&decls, file_id);
    for diag in &duplicate_type_constructors {
        diagnostics.push(diag.clone());
        sess.emit(&lowerer.files, diag);
    }
    if !duplicate_type_constructors.is_empty() {
        return session::CompileReport {
            output: None,
            files: lowerer.files,
            diagnostics,
        };
    }

    let known_types = known_type_names(&decls, imported_type_decls, imported_extern_types);
    let known_type_arities = known_type_arities(&decls, imported_type_decls);
    let mut type_decl_errors = false;
    for decl in &decls {
        let (name, params, usages, span) = match decl {
            ast::Declaration::Type(ast::TypeDecl::Record {
                name,
                params,
                fields,
                span,
                ..
            }) => (
                name.as_str(),
                params.as_slice(),
                fields.iter().map(|(_, usage)| usage).collect::<Vec<_>>(),
                span.clone(),
            ),
            ast::Declaration::Type(ast::TypeDecl::Variant {
                name,
                params,
                constructors,
                span,
                ..
            }) => (
                name.as_str(),
                params.as_slice(),
                constructors
                    .iter()
                    .filter_map(|(_, usage)| usage.as_ref())
                    .collect::<Vec<_>>(),
                span.clone(),
            ),
            _ => continue,
        };

        let generic_params = params.iter().cloned().collect::<HashSet<_>>();
        let mut referenced_types = HashSet::new();
        for usage in &usages {
            collect_type_usage_names(usage, &mut referenced_types);
        }
        let mut unknown: Vec<String> = referenced_types
            .into_iter()
            .filter(|type_name| {
                !known_types.contains(type_name) && !generic_params.contains(type_name)
            })
            .collect();
        unknown.sort();
        if unknown.is_empty() {
        } else {
            let plural = if unknown.len() == 1 { "" } else { "s" };
            let diag = codespan_reporting::diagnostic::Diagnostic::error()
                .with_message(format!(
                    "unknown type{plural} in type declaration `{name}`: {}",
                    unknown.join(", ")
                ))
                .with_labels(vec![
                    codespan_reporting::diagnostic::Label::primary(file_id, span.clone()).with_message(
                        "declare these types in this module or import them unqualified before using them here",
                    ),
                ]);
            diagnostics.push(diag.clone());
            sess.emit(&lowerer.files, &diag);
            type_decl_errors = true;
        }

        let mut arity_errors = Vec::new();
        for usage in &usages {
            collect_type_usage_arity_errors(
                usage,
                &known_type_arities,
                &generic_params,
                &mut arity_errors,
            );
        }
        arity_errors.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        arity_errors.dedup();
        for (type_name, expected, found, usage_span) in arity_errors {
            let diag = codespan_reporting::diagnostic::Diagnostic::error()
                .with_message(format!(
                    "wrong number of type arguments for `{type_name}` in type declaration `{name}`: expected {expected}, found {found}"
                ))
                .with_labels(vec![
                    codespan_reporting::diagnostic::Label::primary(file_id, usage_span)
                        .with_message("use the required number of type arguments here"),
                ]);
            diagnostics.push(diag.clone());
            sess.emit(&lowerer.files, &diag);
            type_decl_errors = true;
        }
    }
    if type_decl_errors {
        return session::CompileReport {
            output: None,
            files: lowerer.files,
            diagnostics,
        };
    }

    let mut extern_type_errors = false;
    for decl in &decls {
        let ast::Declaration::ExternLet { name, ty, span, .. } = decl else {
            continue;
        };
        let mut referenced_types = HashSet::new();
        collect_type_sig_names(ty, &mut referenced_types);
        let mut unknown: Vec<String> = referenced_types
            .into_iter()
            .filter(|type_name| !known_types.contains(type_name))
            .collect();
        unknown.sort();
        if unknown.is_empty() {
            continue;
        }

        let plural = if unknown.len() == 1 { "" } else { "s" };
        let diag = codespan_reporting::diagnostic::Diagnostic::error()
            .with_message(format!(
                "unknown type{plural} in extern signature for `{name}`: {}",
                unknown.join(", ")
            ))
            .with_labels(vec![
                codespan_reporting::diagnostic::Label::primary(file_id, span.clone()).with_message(
                    "import these types (for example: `(use option [Option])`) or declare them in this module",
                ),
            ]);
        diagnostics.push(diag.clone());
        sess.emit(&lowerer.files, &diag);
        extern_type_errors = true;
    }
    if extern_type_errors {
        return session::CompileReport {
            output: None,
            files: lowerer.files,
            diagnostics,
        };
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

    for (name, span) in warnings::unused_function_spans(&decls) {
        let diag = codespan_reporting::diagnostic::Diagnostic::warning()
            .with_message(format!("unused function `{name}`"))
            .with_labels(vec![
                codespan_reporting::diagnostic::Label::primary(file_id, span)
                    .with_message("this private function is never used"),
            ]);
        diagnostics.push(diag.clone());
        sess.emit(&lowerer.files, &diag);
    }
    for (name, span) in warnings::unused_type_spans(&decls) {
        let diag = codespan_reporting::diagnostic::Diagnostic::warning()
            .with_message(format!("unused type `{name}`"))
            .with_labels(vec![
                codespan_reporting::diagnostic::Label::primary(file_id, span)
                    .with_message("this private type is never referenced"),
            ]);
        diagnostics.push(diag.clone());
        sess.emit(&lowerer.files, &diag);
    }
    for (type_name, param, span) in warnings::unused_type_param_spans(&decls) {
        let diag = codespan_reporting::diagnostic::Diagnostic::warning()
            .with_message(format!(
                "unused type parameter `{param}` in type `{type_name}`"
            ))
            .with_labels(vec![
                codespan_reporting::diagnostic::Label::primary(file_id, span)
                    .with_message("this type parameter is never used in the type definition"),
            ]);
        diagnostics.push(diag.clone());
        sess.emit(&lowerer.files, &diag);
    }
    for (name, span) in warnings::unused_local_spans(&decls) {
        let diag = codespan_reporting::diagnostic::Diagnostic::warning()
            .with_message(format!("unused local binding `{name}`"))
            .with_labels(vec![
                codespan_reporting::diagnostic::Label::primary(file_id, span)
                    .with_message("this local binding is never used"),
            ]);
        diagnostics.push(diag.clone());
        sess.emit(&lowerer.files, &diag);
    }
    for diag in warnings::unused_unqualified_import_diagnostics(
        &decls,
        file_id,
        module_exports,
        imported_type_decls,
    ) {
        diagnostics.push(diag.clone());
        sess.emit(&lowerer.files, &diag);
    }
    for diag in warnings::redundant_match_diagnostics(&decls, file_id, imported_type_decls) {
        diagnostics.push(diag.clone());
        sess.emit(&lowerer.files, &diag);
    }

    let mut imported_constructors: HashMap<String, usize> = HashMap::new();
    let mut merged_imported_field_indices = imported_field_indices.clone();
    let mut imported_record_layouts: HashMap<String, Vec<String>> = HashMap::new();
    for type_decl in imported_type_decls {
        match type_decl {
            ast::TypeDecl::Variant { constructors, .. } => {
                for (ctor_name, payload) in constructors {
                    imported_constructors
                        .insert(ctor_name.clone(), if payload.is_some() { 1 } else { 0 });
                }
            }
            ast::TypeDecl::Record { name, fields, .. } => {
                imported_constructors.insert(name.clone(), fields.len());
                for (i, (field_name, _)) in fields.iter().enumerate() {
                    merged_imported_field_indices.insert(field_name.clone(), i + 2);
                }
                imported_record_layouts.insert(
                    name.clone(),
                    fields
                        .iter()
                        .map(|(field_name, _)| field_name.clone())
                        .collect(),
                );
            }
        }
    }

    let module = codegen::lower_module(
        module_name,
        &decls,
        imports,
        module_aliases,
        imported_constructors,
        merged_imported_field_indices,
        imported_record_layouts,
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
    imported_extern_types: &[String],
    imported_field_indices: &HashMap<String, usize>,
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
        imported_extern_types,
        imported_field_indices,
        imported_schemes,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn compile_with_imports(
    module_name: &str,
    source: &str,
    source_path: &str,
    imports: HashMap<String, String>,
    module_exports: &HashMap<String, Vec<String>>,
    module_aliases: HashMap<String, String>,
    imported_type_decls: &[ast::TypeDecl],
    imported_extern_types: &[String],
    imported_field_indices: &HashMap<String, usize>,
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
        imported_extern_types,
        imported_field_indices,
        imported_schemes,
    );
    session::emit_compile_report(&report, true);
    report.output
}
