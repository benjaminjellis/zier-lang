use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use codespan_reporting::{diagnostic::Diagnostic, files::SimpleFiles};

use crate::{ast, codegen, pipeline::CompileTarget, resolve, session, typecheck, typing, warnings};

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

fn push_and_emit_diag(
    sess: &mut session::CompilerSession,
    files: &SimpleFiles<String, String>,
    diagnostics: &mut Vec<Diagnostic<usize>>,
    diag: Diagnostic<usize>,
) {
    sess.emit(files, &diag);
    diagnostics.push(diag);
}

fn push_and_emit_diags<I>(
    sess: &mut session::CompilerSession,
    files: &SimpleFiles<String, String>,
    diagnostics: &mut Vec<Diagnostic<usize>>,
    diags: I,
) -> usize
where
    I: IntoIterator<Item = Diagnostic<usize>>,
{
    let mut count = 0;
    for diag in diags {
        push_and_emit_diag(sess, files, diagnostics, diag);
        count += 1;
    }
    count
}

fn compile_error_report(
    files: SimpleFiles<String, String>,
    diagnostics: Vec<Diagnostic<usize>>,
) -> session::CompileReport {
    session::CompileReport {
        output: None,
        files,
        diagnostics,
    }
}

struct TypecheckStageInput<'a> {
    file_id: usize,
    decls: &'a [ast::Declaration],
    imports: &'a HashMap<String, String>,
    module_exports: &'a HashMap<String, Vec<String>>,
    imported_type_decls: &'a [ast::TypeDecl],
    imported_extern_types: &'a [String],
    imported_private_records: &'a HashMap<String, Vec<String>>,
    imported_schemes: &'a typecheck::TypeEnv,
}

struct ImportedTypeRuntimeInfo {
    imported_constructors: HashMap<String, usize>,
    imported_field_indices: HashMap<(String, String), usize>,
    imported_record_layouts: HashMap<String, Vec<String>>,
}

struct TypecheckStageOutput {
    inferred_expr_types: HashMap<(usize, usize), Arc<typecheck::Type>>,
    inferred_record_expr_types: HashMap<(usize, usize), String>,
}

pub struct CompileWithImportsInput<'a> {
    pub module_name: &'a str,
    pub source: &'a str,
    pub source_path: &'a str,
    pub imports: HashMap<String, String>,
    pub module_exports: &'a HashMap<String, Vec<String>>,
    pub module_aliases: HashMap<String, String>,
    pub imported_type_decls: &'a [ast::TypeDecl],
    pub debug_type_decls: &'a [ast::TypeDecl],
    pub imported_extern_types: &'a [String],
    pub imported_field_indices: &'a HashMap<(String, String), usize>,
    pub imported_private_records: &'a HashMap<String, Vec<String>>,
    pub imported_schemes: &'a typecheck::TypeEnv,
    pub compile_target: CompileTarget,
}

fn run_lower_stage(
    sess: &mut session::CompilerSession,
    source_path: &str,
    source: &str,
    diagnostics: &mut Vec<Diagnostic<usize>>,
) -> Result<crate::hir::HirModule, session::CompileReport> {
    let hir = crate::hir::lower_source_to_hir(source_path, source);
    if push_and_emit_diags(
        sess,
        &hir.files,
        diagnostics,
        hir.diagnostics.iter().cloned(),
    ) > 0
    {
        return Err(compile_error_report(hir.files, std::mem::take(diagnostics)));
    }
    Ok(hir)
}

fn validate_use_declarations(
    sess: &mut session::CompilerSession,
    files: &SimpleFiles<String, String>,
    diagnostics: &mut Vec<Diagnostic<usize>>,
    file_id: usize,
    decls: &[ast::Declaration],
    module_exports: &HashMap<String, Vec<String>>,
) -> bool {
    let mut use_errors = false;
    for decl in decls {
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
            push_and_emit_diag(sess, files, diagnostics, diag);
            use_errors = true;
        }
    }
    use_errors
}

fn validate_declaration_collisions(
    sess: &mut session::CompilerSession,
    files: &SimpleFiles<String, String>,
    diagnostics: &mut Vec<Diagnostic<usize>>,
    file_id: usize,
    decls: &[ast::Declaration],
    module_exports: &HashMap<String, Vec<String>>,
) -> bool {
    let duplicate_top_level_values =
        warnings::duplicate_top_level_value_diagnostics(decls, file_id, module_exports);
    let duplicate_type_constructors =
        warnings::duplicate_type_constructor_diagnostics(decls, file_id);

    let has_duplicate_values =
        push_and_emit_diags(sess, files, diagnostics, duplicate_top_level_values) > 0;
    let has_duplicate_type_constructors =
        push_and_emit_diags(sess, files, diagnostics, duplicate_type_constructors) > 0;

    has_duplicate_values || has_duplicate_type_constructors
}

fn validate_type_declarations(
    sess: &mut session::CompilerSession,
    files: &SimpleFiles<String, String>,
    diagnostics: &mut Vec<Diagnostic<usize>>,
    file_id: usize,
    decls: &[ast::Declaration],
    imported_type_decls: &[ast::TypeDecl],
    imported_extern_types: &[String],
) -> bool {
    let known_types = known_type_names(decls, imported_type_decls, imported_extern_types);
    let known_type_arities = known_type_arities(decls, imported_type_decls);
    let mut type_decl_errors = false;
    for decl in decls {
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
                    .flat_map(|(_, usages)| usages.iter())
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
        if !unknown.is_empty() {
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
            push_and_emit_diag(sess, files, diagnostics, diag);
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
            push_and_emit_diag(sess, files, diagnostics, diag);
            type_decl_errors = true;
        }
    }
    type_decl_errors
}

fn validate_extern_signatures(
    sess: &mut session::CompilerSession,
    files: &SimpleFiles<String, String>,
    diagnostics: &mut Vec<Diagnostic<usize>>,
    file_id: usize,
    decls: &[ast::Declaration],
    imported_type_decls: &[ast::TypeDecl],
    imported_extern_types: &[String],
) -> bool {
    let known_types = known_type_names(decls, imported_type_decls, imported_extern_types);
    let mut extern_type_errors = false;
    for decl in decls {
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
        push_and_emit_diag(sess, files, diagnostics, diag);
        extern_type_errors = true;
    }
    extern_type_errors
}

fn run_typecheck_stage(
    sess: &mut session::CompilerSession,
    files: &SimpleFiles<String, String>,
    diagnostics: &mut Vec<Diagnostic<usize>>,
    input: TypecheckStageInput<'_>,
) -> Option<TypecheckStageOutput> {
    let (mut checker, mut env) = typing::prepare_typechecker(
        input.imported_type_decls,
        input.imported_extern_types,
        input.imported_private_records,
        input.imported_schemes,
    );

    let symbols = sess.symbol_table(input.module_exports);
    let unresolved =
        resolve::unresolved_env_names(input.decls, input.imports.keys().cloned(), &env, symbols);
    env.extend(typecheck::import_env(&unresolved));

    if let Err(err) = checker.check_program(&mut env, input.decls, input.file_id) {
        let type_diags = err.0.to_diagnostics(input.file_id, err.1.span());
        push_and_emit_diags(sess, files, diagnostics, type_diags);
        return None;
    }

    Some(TypecheckStageOutput {
        inferred_expr_types: checker
            .inferred_expr_types()
            .iter()
            .map(|(span, ty)| ((span.start, span.end), ty.clone()))
            .collect(),
        inferred_record_expr_types: checker.inferred_record_expr_types(),
    })
}

fn emit_warning_stage(
    sess: &mut session::CompilerSession,
    files: &SimpleFiles<String, String>,
    diagnostics: &mut Vec<Diagnostic<usize>>,
    file_id: usize,
    decls: &[ast::Declaration],
    module_exports: &HashMap<String, Vec<String>>,
    imported_type_decls: &[ast::TypeDecl],
) {
    for (name, span) in warnings::unused_function_spans(decls) {
        let diag = codespan_reporting::diagnostic::Diagnostic::warning()
            .with_message(format!("unused function `{name}`"))
            .with_labels(vec![
                codespan_reporting::diagnostic::Label::primary(file_id, span)
                    .with_message("this private function is never used"),
            ]);
        push_and_emit_diag(sess, files, diagnostics, diag);
    }
    for (name, span) in warnings::unused_type_spans(decls) {
        let diag = codespan_reporting::diagnostic::Diagnostic::warning()
            .with_message(format!("unused type `{name}`"))
            .with_labels(vec![
                codespan_reporting::diagnostic::Label::primary(file_id, span)
                    .with_message("this private type is never referenced"),
            ]);
        push_and_emit_diag(sess, files, diagnostics, diag);
    }
    for (type_name, param, span) in warnings::unused_type_param_spans(decls) {
        let diag = codespan_reporting::diagnostic::Diagnostic::warning()
            .with_message(format!(
                "unused type parameter `{param}` in type `{type_name}`"
            ))
            .with_labels(vec![
                codespan_reporting::diagnostic::Label::primary(file_id, span)
                    .with_message("this type parameter is never used in the type definition"),
            ]);
        push_and_emit_diag(sess, files, diagnostics, diag);
    }
    for (name, span) in warnings::unused_local_spans(decls) {
        let diag = codespan_reporting::diagnostic::Diagnostic::warning()
            .with_message(format!("unused local binding `{name}`"))
            .with_labels(vec![
                codespan_reporting::diagnostic::Label::primary(file_id, span)
                    .with_message("this local binding is never used"),
            ]);
        push_and_emit_diag(sess, files, diagnostics, diag);
    }
    push_and_emit_diags(
        sess,
        files,
        diagnostics,
        warnings::unused_unqualified_import_diagnostics(
            decls,
            file_id,
            module_exports,
            imported_type_decls,
        ),
    );
    push_and_emit_diags(
        sess,
        files,
        diagnostics,
        warnings::redundant_match_diagnostics(decls, file_id, imported_type_decls),
    );
}

fn build_imported_type_runtime_info(
    imported_type_decls: &[ast::TypeDecl],
    imported_field_indices: &HashMap<(String, String), usize>,
) -> ImportedTypeRuntimeInfo {
    let mut imported_constructors: HashMap<String, usize> = HashMap::new();
    let mut merged_imported_field_indices = imported_field_indices.clone();
    let mut imported_record_layouts: HashMap<String, Vec<String>> = HashMap::new();

    for type_decl in imported_type_decls {
        match type_decl {
            ast::TypeDecl::Variant {
                name, constructors, ..
            } => {
                let qualified_module = name.split_once('/').map(|(module, _)| module.to_string());
                for (ctor_name, payloads) in constructors {
                    let arity = payloads.len();
                    imported_constructors.insert(ctor_name.clone(), arity);
                    if let Some(module) = &qualified_module {
                        imported_constructors.insert(format!("{module}/{ctor_name}"), arity);
                    }
                }
            }
            ast::TypeDecl::Record { name, fields, .. } => {
                imported_constructors.insert(name.clone(), fields.len());
                if let Some((module, record_name)) = name.split_once('/') {
                    imported_constructors.insert(format!("{module}/{record_name}"), fields.len());
                }
                for (i, (field_name, _)) in fields.iter().enumerate() {
                    merged_imported_field_indices.insert((name.clone(), field_name.clone()), i + 2);
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

    ImportedTypeRuntimeInfo {
        imported_constructors,
        imported_field_indices: merged_imported_field_indices,
        imported_record_layouts,
    }
}

/// Compile without any imports (single-file or when imports are already resolved).
#[cfg(test)]
pub(crate) fn compile(module_name: &str, source: &str) -> Option<String> {
    compile_with_imports(CompileWithImportsInput {
        module_name,
        source,
        source_path: &format!("{module_name}.mond"),
        imports: HashMap::new(),
        module_exports: &HashMap::new(),
        module_aliases: HashMap::new(),
        imported_type_decls: &[],
        debug_type_decls: &[],
        imported_extern_types: &[],
        imported_field_indices: &HashMap::new(),
        imported_private_records: &HashMap::new(),
        imported_schemes: &HashMap::new(),
        compile_target: CompileTarget::Dev,
    })
}

pub fn compile_with_imports_in_session(
    sess: &mut session::CompilerSession,
    input: CompileWithImportsInput<'_>,
) -> session::CompileReport {
    compile_with_imports_in_session_with_target_and_private_records(sess, input)
}

pub(crate) fn compile_with_imports_in_session_with_target_and_private_records(
    sess: &mut session::CompilerSession,
    input: CompileWithImportsInput<'_>,
) -> session::CompileReport {
    let CompileWithImportsInput {
        module_name,
        source,
        source_path,
        imports,
        module_exports,
        module_aliases,
        imported_type_decls,
        debug_type_decls,
        imported_extern_types,
        imported_field_indices,
        imported_private_records,
        imported_schemes,
        compile_target,
    } = input;
    let mut diagnostics = Vec::new();
    let hir = match run_lower_stage(sess, source_path, source, &mut diagnostics) {
        Ok(hir) => hir,
        Err(report) => return report,
    };
    let file_id = hir.file_id;
    let decls = hir.decls;
    let files = hir.files;

    if validate_use_declarations(
        sess,
        &files,
        &mut diagnostics,
        file_id,
        &decls,
        module_exports,
    ) {
        return compile_error_report(files, diagnostics);
    }

    if validate_declaration_collisions(
        sess,
        &files,
        &mut diagnostics,
        file_id,
        &decls,
        module_exports,
    ) {
        return compile_error_report(files, diagnostics);
    }

    if validate_type_declarations(
        sess,
        &files,
        &mut diagnostics,
        file_id,
        &decls,
        imported_type_decls,
        imported_extern_types,
    ) {
        return compile_error_report(files, diagnostics);
    }

    if validate_extern_signatures(
        sess,
        &files,
        &mut diagnostics,
        file_id,
        &decls,
        imported_type_decls,
        imported_extern_types,
    ) {
        return compile_error_report(files, diagnostics);
    }

    let typecheck_output = match run_typecheck_stage(
        sess,
        &files,
        &mut diagnostics,
        TypecheckStageInput {
            file_id,
            decls: &decls,
            imports: &imports,
            module_exports,
            imported_type_decls,
            imported_extern_types,
            imported_private_records,
            imported_schemes,
        },
    ) {
        Some(output) => output,
        None => return compile_error_report(files, diagnostics),
    };

    emit_warning_stage(
        sess,
        &files,
        &mut diagnostics,
        file_id,
        &decls,
        module_exports,
        imported_type_decls,
    );

    let imported_runtime =
        build_imported_type_runtime_info(imported_type_decls, imported_field_indices);

    let module = codegen::lower_module(
        module_name,
        &decls,
        codegen::LowerModuleInput {
            compile_target,
            imports,
            module_aliases,
            imported_type_decls: debug_type_decls.to_vec(),
            imported_constructors: imported_runtime.imported_constructors,
            imported_field_indices: imported_runtime.imported_field_indices,
            imported_record_layouts: imported_runtime.imported_record_layouts,
            inferred_expr_types: typecheck_output.inferred_expr_types,
            inferred_record_expr_types: typecheck_output.inferred_record_expr_types,
        },
    );
    session::CompileReport {
        output: Some(codegen::emit_module(&module)),
        files,
        diagnostics,
    }
}

pub fn compile_with_imports_in_session_with_private_records(
    sess: &mut session::CompilerSession,
    input: CompileWithImportsInput<'_>,
) -> session::CompileReport {
    compile_with_imports_in_session_with_target_and_private_records(sess, input)
}

pub fn compile_with_imports_report(input: CompileWithImportsInput<'_>) -> session::CompileReport {
    compile_with_imports_report_with_private_records(input)
}

pub fn compile_with_imports_report_with_private_records(
    input: CompileWithImportsInput<'_>,
) -> session::CompileReport {
    let mut sess = session::CompilerSession::default();
    compile_with_imports_in_session_with_private_records(&mut sess, input)
}

pub fn compile_with_imports(input: CompileWithImportsInput<'_>) -> Option<String> {
    let report = compile_with_imports_report(input);
    session::emit_compile_report(&report, true);
    report.output
}
