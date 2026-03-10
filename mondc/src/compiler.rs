use std::collections::HashMap;

use crate::{ast, codegen, lower, resolve, session, sexpr, typecheck, warnings};

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

    let mut checker = typecheck::TypeChecker::new();
    let mut env = typecheck::primitive_env();

    for type_decl in imported_type_decls {
        env.extend(typecheck::constructor_schemes(type_decl));
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
