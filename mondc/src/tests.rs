use std::collections::HashMap;

use crate::{
    compile_with_imports, compile_with_imports_in_session, compile_with_imports_report,
    exported_type_decls, infer_module_exports, infer_module_expr_types, lower, pub_reexports,
    session, typecheck, warnings,
};

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
        "main.mond",
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
        "main.mond",
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
        "main.mond",
        HashMap::new(),
        &module_exports,
        HashMap::new(),
        &[],
        &HashMap::new(),
    );
    assert!(result.is_none());
}

#[test]
fn duplicate_top_level_function_defs_error() {
    let src = "(let hello {} 1)\n(let hello {} 2)";
    let result = compile_with_imports(
        "main",
        src,
        "main.mond",
        HashMap::new(),
        &HashMap::new(),
        HashMap::new(),
        &[],
        &HashMap::new(),
    );
    assert!(result.is_none());
}

#[test]
fn top_level_function_conflicts_with_unqualified_import() {
    let mut module_exports = HashMap::new();
    module_exports.insert("greetings".to_string(), vec!["hello".to_string()]);

    let src = "(use greetings [hello])\n(let hello {} 1)";
    let result = compile_with_imports(
        "main",
        src,
        "main.mond",
        HashMap::new(),
        &module_exports,
        HashMap::new(),
        &[],
        &HashMap::new(),
    );
    assert!(result.is_none());
}

#[test]
fn top_level_function_does_not_conflict_with_qualified_only_import() {
    let mut module_exports = HashMap::new();
    module_exports.insert("greetings".to_string(), vec!["hello".to_string()]);

    let src = "(use greetings)\n(let hello {} 1)";
    let result = compile_with_imports(
        "main",
        src,
        "main.mond",
        HashMap::new(),
        &module_exports,
        HashMap::new(),
        &[],
        &HashMap::new(),
    );
    assert!(result.is_some());
}

#[test]
fn duplicate_module_use_without_unqualified_imports_is_allowed() {
    let mut module_exports = HashMap::new();
    module_exports.insert("io".to_string(), vec!["println".to_string()]);

    let src = "(use std/io)\n(use std/io)\n(let main {} (io/println \"Hey!\"))";
    let result = compile_with_imports(
        "main",
        src,
        "main.mond",
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
        "main.mond",
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
        "main.mond",
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
    let src = include_str!("../../mond-std/src/result.mond");
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
    let result_src = include_str!("../../mond-std/src/result.mond");
    let io_src = include_str!("../../mond-std/src/io.mond");

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
        "main.mond",
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
    let result_src = include_str!("../../mond-std/src/result.mond");
    let io_src = include_str!("../../mond-std/src/io.mond");
    let testing_src = include_str!("../../mond-std/src/testing.mond");

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
        "tests/string_test.mond",
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
            .any(|msg| msg.contains("`bind` expects `Unit -> Result")),
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
        "main.mond",
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
        "main.mond",
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
    let file_id = lowerer.add_file("scan.mond".into(), src.into());
    let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .expect("parse");
    let decls = lowerer.lower_file(file_id, &sexprs);

    let mut unused: Vec<String> = warnings::unused_function_spans(&decls)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    unused.sort();
    assert_eq!(unused, vec!["dead".to_string()]);
}

#[test]
fn unused_local_analysis_marks_only_unused_locals() {
    let src = "(let main {} (let [used 1 dead 2] used))";
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(src).lex();
    let file_id = lowerer.add_file("scan.mond".into(), src.into());
    let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .expect("parse");
    let decls = lowerer.lower_file(file_id, &sexprs);

    let mut unused: Vec<String> = warnings::unused_local_spans(&decls)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    unused.sort();
    assert_eq!(unused, vec!["dead".to_string()]);
}

#[test]
fn unused_local_analysis_ignores_underscore_and_shadowed_outer_usage() {
    let src = "(let main {} (let [_ 1 x 2] (let [x 3] x)))";
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(src).lex();
    let file_id = lowerer.add_file("scan.mond".into(), src.into());
    let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .expect("parse");
    let decls = lowerer.lower_file(file_id, &sexprs);

    let unused: Vec<String> = warnings::unused_local_spans(&decls)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(unused, vec!["x".to_string()]);
}

#[test]
fn compile_emits_unused_local_binding_warning() {
    let src = "(let main {} (let [x 1 y 2] x))";
    let report = compile_with_imports_report(
        "main",
        src,
        "main.mond",
        HashMap::new(),
        &HashMap::new(),
        HashMap::new(),
        &[],
        &HashMap::new(),
    );

    let messages: Vec<String> = report.diagnostics.into_iter().map(|d| d.message).collect();
    assert!(
        messages
            .iter()
            .any(|m| m.contains("unused local binding `y`")),
        "missing unused local warning: {messages:?}"
    );
}

#[test]
fn unqualified_import_warnings_skip_qualified_only_use() {
    let src = "(use std/io)\n(let main {} (io/println \"hello\"))";
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(src).lex();
    let file_id = lowerer.add_file("scan.mond".into(), src.into());
    let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .expect("parse");
    let decls = lowerer.lower_file(file_id, &sexprs);

    let mut module_exports = HashMap::new();
    module_exports.insert("io".to_string(), vec!["println".to_string()]);
    let warnings =
        warnings::unused_unqualified_import_diagnostics(&decls, file_id, &module_exports, &[]);
    assert!(warnings.is_empty());
}

#[test]
fn qualified_only_import_warning_flags_unused_module_import() {
    let src = "(use std/io)\n(let main {} ())";
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(src).lex();
    let file_id = lowerer.add_file("scan.mond".into(), src.into());
    let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .expect("parse");
    let decls = lowerer.lower_file(file_id, &sexprs);

    let mut module_exports = HashMap::new();
    module_exports.insert("io".to_string(), vec!["println".to_string()]);
    let warnings =
        warnings::unused_unqualified_import_diagnostics(&decls, file_id, &module_exports, &[]);
    assert_eq!(warnings.len(), 1);
    assert!(
        warnings[0].message.contains("unused import `io`"),
        "unexpected warning: {:?}",
        warnings[0].message
    );
}

#[test]
fn unqualified_import_warnings_flag_unused_specific_and_wildcard() {
    let src =
        "(use std/io)\n(use std/result [Result bind])\n(use std/option [*])\n(let main {} ())";
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(src).lex();
    let file_id = lowerer.add_file("scan.mond".into(), src.into());
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
    let warnings =
        warnings::unused_unqualified_import_diagnostics(&decls, file_id, &module_exports, &[]);
    assert_eq!(warnings.len(), 3);
    let messages: Vec<String> = warnings.into_iter().map(|d| d.message).collect();
    assert!(
        messages.iter().any(|m| m.contains("unused import `io`")),
        "missing qualified import warning: {messages:?}"
    );
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
    let src =
        "(use std/option [Option])\n(type Attributes ((:max_age ~ Option Int)))\n(let main {} ())";
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(src).lex();
    let file_id = lowerer.add_file("scan.mond".into(), src.into());
    let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .expect("parse");
    let decls = lowerer.lower_file(file_id, &sexprs);

    let mut module_exports = HashMap::new();
    module_exports.insert(
        "option".to_string(),
        vec!["Option".to_string(), "Some".to_string(), "None".to_string()],
    );
    let warnings =
        warnings::unused_unqualified_import_diagnostics(&decls, file_id, &module_exports, &[]);
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
    let file_id = lowerer.add_file("scan.mond".into(), src.into());
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
    let warnings = warnings::unused_unqualified_import_diagnostics(
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
    let file_id = lowerer.add_file("scan.mond".into(), src.into());
    let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .expect("parse");
    let decls = lowerer.lower_file(file_id, &sexprs);

    let mut module_exports = HashMap::new();
    module_exports.insert("unknown".to_string(), vec!["DecodeError".to_string()]);
    let imported_type_decls =
        exported_type_decls("(pub type DecodeError ((:expected ~ String) (:found ~ String)))");
    let warnings = warnings::unused_unqualified_import_diagnostics(
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
    let file_id = lowerer.add_file("scan.mond".into(), src.into());
    let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .expect("parse");
    let decls = lowerer.lower_file(file_id, &sexprs);

    let mut unused: Vec<String> = warnings::unused_type_spans(&decls)
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
    let file_id = lowerer.add_file("scan.mond".into(), src.into());
    let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .expect("parse");
    let decls = lowerer.lower_file(file_id, &sexprs);

    assert!(
        warnings::unused_type_spans(&decls).is_empty(),
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
