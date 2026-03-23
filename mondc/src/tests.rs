use std::collections::HashMap;

use crate::{
    compile_with_imports, compile_with_imports_in_session, compile_with_imports_report,
    exported_type_decls, infer_module_exports, infer_module_expr_types, lower, pub_reexports,
    session, typecheck, warnings,
};

const RESULT_STD_SRC: &str = r#"
(pub type ['a 'e] Result [(Ok ~ 'a) (Error ~ 'e)])
(pub let bind {m func} (match m (Ok x) ~> (func x) (Error e) ~> (Error e)))
"#;

const IO_STD_SRC: &str = r#"
(pub extern let println ~ (String -> Unit) io/format)
(pub extern let debug ~ ('a -> Unit) io/format)
"#;

const TESTING_STD_SRC: &str = r#"
(use result [Result])
(pub let assert {cond} (if cond (Ok ()) (Error "assertion failed")))
(pub extern let assert_eq ~ ('a -> 'a -> (Result Unit String)) mond_testing_helpers/assert_eq)
(pub extern let assert_ne ~ ('a -> 'a -> (Result Unit String)) mond_testing_helpers/assert_ne)
"#;

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
        &[],
        &HashMap::new(),
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
        &[],
        &HashMap::new(),
        &HashMap::new(),
    );
    assert!(with_use_result.is_some());
}

#[test]
fn error_identifier_is_allowed_in_bindings() {
    let src = "(let main {error} (match error _ ~> error))";
    let result = compile_with_imports(
        "main",
        src,
        "main.mond",
        HashMap::new(),
        &HashMap::new(),
        HashMap::new(),
        &[],
        &[],
        &HashMap::new(),
        &HashMap::new(),
    );
    assert!(
        result.is_some(),
        "`error` should lex and lower as a normal identifier"
    );
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
        &[],
        &HashMap::new(),
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
        &[],
        &HashMap::new(),
        &HashMap::new(),
    );
    assert!(result.is_none());
}

#[test]
fn top_level_forward_reference_compiles() {
    let src = "(let main {} (helper 10))\n(let helper {x} (+ x 1))";
    let result = compile_with_imports(
        "main",
        src,
        "main.mond",
        HashMap::new(),
        &HashMap::new(),
        HashMap::new(),
        &[],
        &[],
        &HashMap::new(),
        &HashMap::new(),
    );
    assert!(result.is_some(), "forward reference should type-check");
}

#[test]
fn top_level_mutual_recursion_compiles() {
    let src = r#"
        (let even {n} (if (= n 0) True (odd (- n 1))))
        (let odd  {n} (if (= n 0) False (even (- n 1))))
        (let main {} (even 4))
    "#;
    let result = compile_with_imports(
        "main",
        src,
        "main.mond",
        HashMap::new(),
        &HashMap::new(),
        HashMap::new(),
        &[],
        &[],
        &HashMap::new(),
        &HashMap::new(),
    );
    assert!(result.is_some(), "mutual recursion should type-check");
}

#[test]
fn duplicate_record_fields_error() {
    let src = "(type LotsOfFields [(:record ~ String) (:record ~ String)])";
    let result = compile_with_imports(
        "main",
        src,
        "main.mond",
        HashMap::new(),
        &HashMap::new(),
        HashMap::new(),
        &[],
        &[],
        &HashMap::new(),
        &HashMap::new(),
    );
    assert!(result.is_none());
}

#[test]
fn record_constructor_type_mismatch_points_to_field_value() {
    let src = r#"
        (type Builder [(:initialised ~ Int)])
        (let new {}
          (Builder :initialised "oops"))
    "#;
    let report = compile_with_imports_report(
        "main",
        src,
        "main.mond",
        HashMap::new(),
        &HashMap::new(),
        HashMap::new(),
        &[],
        &[],
        &HashMap::new(),
        &HashMap::new(),
    );
    assert!(report.has_errors(), "expected type error");
    let mismatch = report
        .diagnostics
        .iter()
        .find(|d| d.message.contains("type mismatch"))
        .expect("missing type mismatch diagnostic");
    let primary = mismatch
        .labels
        .iter()
        .find(|label| {
            matches!(
                label.style,
                codespan_reporting::diagnostic::LabelStyle::Primary
            )
        })
        .expect("missing primary label");
    assert_eq!(&src[primary.range.clone()], "\"oops\"");
}

#[test]
fn duplicate_variant_constructors_error() {
    let src = "(type LotsOVariants [One One Two (Three ~ Int)])";
    let result = compile_with_imports(
        "main",
        src,
        "main.mond",
        HashMap::new(),
        &HashMap::new(),
        HashMap::new(),
        &[],
        &[],
        &HashMap::new(),
        &HashMap::new(),
    );
    assert!(result.is_none());
}

#[test]
fn duplicate_variant_constructors_across_types_error() {
    let src = r#"
        (type DiffVariant [One Five (Six ~ String)])
        (type LotsOVariants [One Two (Three ~ Int) Four Five (Six ~ String)])
    "#;
    let result = compile_with_imports(
        "main",
        src,
        "main.mond",
        HashMap::new(),
        &HashMap::new(),
        HashMap::new(),
        &[],
        &[],
        &HashMap::new(),
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
        &[],
        &HashMap::new(),
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
        &[],
        &HashMap::new(),
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
        &[],
        &HashMap::new(),
        &HashMap::new(),
    );
    assert!(result.is_some());
}

#[test]
fn qualified_only_use_does_not_import_variant_constructors_unqualified() {
    let result_src =
        "(pub type ['a 'e] Result [(Ok ~ 'a) (Error ~ 'e)])\n(pub let bind {m f} (f m))";
    let std_mods = vec![(
        "result".to_string(),
        "mond_result".to_string(),
        result_src.to_string(),
    )];
    let analysis = crate::build_project_analysis(&std_mods, &[]).expect("analysis");
    let src = "(use result)\n(let main {} (Ok 1))";
    let resolved = crate::resolve_imports_for_source(src, &analysis.module_exports, &analysis);
    let report = compile_with_imports_report(
        "main",
        src,
        "main.mond",
        resolved.imports,
        &analysis.module_exports,
        resolved.module_aliases,
        &resolved.imported_type_decls,
        &resolved.imported_extern_types,
        &resolved.imported_field_indices,
        &resolved.imported_schemes,
    );
    assert!(
        report.has_errors(),
        "qualified-only use should not import constructors"
    );
    let messages: Vec<String> = report
        .diagnostics
        .iter()
        .map(|d| d.message.clone())
        .collect();
    assert!(
        messages.iter().any(|m| m.contains("unbound variable `Ok`")),
        "expected Ok to be unbound, diagnostics: {messages:?}"
    );
}

#[test]
fn importing_type_name_brings_variant_constructors_into_scope() {
    let result_src =
        "(pub type ['a 'e] Result [(Ok ~ 'a) (Error ~ 'e)])\n(pub let bind {m f} (f m))";
    let std_mods = vec![(
        "result".to_string(),
        "mond_result".to_string(),
        result_src.to_string(),
    )];
    let analysis = crate::build_project_analysis(&std_mods, &[]).expect("analysis");
    let src = "(use result [Result])\n(let main {} (Ok 1))";
    let resolved = crate::resolve_imports_for_source(src, &analysis.module_exports, &analysis);
    let report = compile_with_imports_report(
        "main",
        src,
        "main.mond",
        resolved.imports,
        &analysis.module_exports,
        resolved.module_aliases,
        &resolved.imported_type_decls,
        &resolved.imported_extern_types,
        &resolved.imported_field_indices,
        &resolved.imported_schemes,
    );
    assert!(
        !report.has_errors(),
        "type import should make constructors available: {:?}",
        report
            .diagnostics
            .iter()
            .map(|d| d.message.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn qualified_only_use_keeps_record_field_accessors_available() {
    let map_src = "(pub type ['a 'b] TakeResult [(:value ~ 'a) (:rest ~ 'b)])\n(pub let take {} (TakeResult :value \"mond\" :rest \"std\"))";
    let std_mods = vec![(
        "map".to_string(),
        "mond_map".to_string(),
        map_src.to_string(),
    )];
    let analysis = crate::build_project_analysis(&std_mods, &[]).expect("analysis");
    let src = "(use map)\n(let main {} (:value (map/take)))";
    let resolved = crate::resolve_imports_for_source(src, &analysis.module_exports, &analysis);
    let report = compile_with_imports_report(
        "main",
        src,
        "main.mond",
        resolved.imports,
        &analysis.module_exports,
        resolved.module_aliases,
        &resolved.imported_type_decls,
        &resolved.imported_extern_types,
        &resolved.imported_field_indices,
        &resolved.imported_schemes,
    );
    assert!(
        !report.has_errors(),
        "record field access should work without unqualified type import: {:?}",
        report
            .diagnostics
            .iter()
            .map(|d| d.message.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn field_access_uses_the_record_type_not_just_field_name() {
    let src = "(type ['s] ContinuePayload [(:state ~ 's)])\n\
               (type ['s] Initialised [(:state ~ 's)])\n\
               (let continue_payload_state {continue}\n\
                 (:state continue))\n\
               (let main {}\n\
                 (continue_payload_state (ContinuePayload :state 1)))";
    let report = compile_with_imports_report(
        "main",
        src,
        "main.mond",
        HashMap::new(),
        &HashMap::new(),
        HashMap::new(),
        &[],
        &[],
        &HashMap::new(),
        &HashMap::new(),
    );
    assert!(
        !report.has_errors(),
        "expected call-site type to disambiguate field access: {:?}",
        report
            .diagnostics
            .iter()
            .map(|d| d.message.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn field_access_codegen_uses_record_qualified_index_when_labels_overlap() {
    let src = "(type ContinuePayload [(:id ~ Int) (:state ~ Int)])\n\
               (type Initialised [(:state ~ Int)])\n\
               (let main {} (:state (ContinuePayload :id 0 :state 1)))";
    let output = compile_with_imports(
        "main",
        src,
        "main.mond",
        HashMap::new(),
        &HashMap::new(),
        HashMap::new(),
        &[],
        &[],
        &HashMap::new(),
        &HashMap::new(),
    )
    .expect("compile");
    assert!(
        output.contains("erlang:element(3"),
        "expected ContinuePayload.:state to use tuple index 3, got:\n{output}"
    );
}

#[test]
fn record_update_codegen_uses_record_qualified_index_when_labels_overlap() {
    let src = "(type ContinuePayload [(:id ~ Int) (:state ~ Int)])\n\
               (type Initialised [(:state ~ Int)])\n\
               (let main {} (with (ContinuePayload :id 0 :state 1) :state 2))";
    let output = compile_with_imports(
        "main",
        src,
        "main.mond",
        HashMap::new(),
        &HashMap::new(),
        HashMap::new(),
        &[],
        &[],
        &HashMap::new(),
        &HashMap::new(),
    )
    .expect("compile");
    assert!(
        output.contains("erlang:setelement(3"),
        "expected ContinuePayload.:state update to use tuple index 3, got:\n{output}"
    );
}

#[test]
fn nested_constructor_pattern_keeps_payload_bindings() {
    let src = "(type ContinuePayload [(:state ~ Int) (:selector ~ Int)])\n\
               (type Next [(Continue ~ ContinuePayload) Stop])\n\
               (let unwrap {value}\n\
                 (match value\n\
                   (Continue (ContinuePayload current_state _)) ~> current_state\n\
                   Stop ~> 0))";
    let output = compile_with_imports(
        "main",
        src,
        "main.mond",
        HashMap::new(),
        &HashMap::new(),
        HashMap::new(),
        &[],
        &[],
        &HashMap::new(),
        &HashMap::new(),
    )
    .expect("compile");
    assert!(
        output.contains("{continue, {continuepayload"),
        "expected nested constructor payload tuple pattern, got:\n{output}"
    );
}

#[test]
fn qualified_type_reference_in_type_declaration_is_supported() {
    let process_src = "(pub extern type ['m] Name)";
    let std_mods = vec![(
        "process".to_string(),
        "mond_process".to_string(),
        process_src.to_string(),
    )];
    let analysis = crate::build_project_analysis(&std_mods, &[]).expect("analysis");
    let src = "(use process)\n(type ['m] Actor [(:name ~ (process/Name 'm))])\n(let main {} ())";
    let resolved = crate::resolve_imports_for_source(src, &analysis.module_exports, &analysis);
    let report = compile_with_imports_report(
        "main",
        src,
        "main.mond",
        resolved.imports,
        &analysis.module_exports,
        resolved.module_aliases,
        &resolved.imported_type_decls,
        &resolved.imported_extern_types,
        &resolved.imported_field_indices,
        &resolved.imported_schemes,
    );
    assert!(
        !report.has_errors(),
        "expected qualified type reference to resolve: {:?}",
        report
            .diagnostics
            .iter()
            .map(|d| d.message.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn qualified_only_use_still_requires_qualified_type_spelling() {
    let process_src = "(pub extern type ['m] Name)";
    let std_mods = vec![(
        "process".to_string(),
        "mond_process".to_string(),
        process_src.to_string(),
    )];
    let analysis = crate::build_project_analysis(&std_mods, &[]).expect("analysis");
    let src = "(use process)\n(type ['m] Actor [(:name ~ (Name 'm))])\n(let main {} ())";
    let resolved = crate::resolve_imports_for_source(src, &analysis.module_exports, &analysis);
    let report = compile_with_imports_report(
        "main",
        src,
        "main.mond",
        resolved.imports,
        &analysis.module_exports,
        resolved.module_aliases,
        &resolved.imported_type_decls,
        &resolved.imported_extern_types,
        &resolved.imported_field_indices,
        &resolved.imported_schemes,
    );
    assert!(
        report.has_errors(),
        "expected unknown type without unqualified import"
    );
    let messages: Vec<String> = report
        .diagnostics
        .iter()
        .map(|d| d.message.clone())
        .collect();
    assert!(
        messages
            .iter()
            .any(|m| m.contains("unknown type") && m.contains("Name")),
        "expected unknown type Name diagnostic, got: {messages:?}"
    );
}

#[test]
fn extern_signature_accepts_qualified_type_reference() {
    let process_src = "(pub extern type ['m] Name)";
    let std_mods = vec![(
        "process".to_string(),
        "mond_process".to_string(),
        process_src.to_string(),
    )];
    let analysis = crate::build_project_analysis(&std_mods, &[]).expect("analysis");
    let src = "(use process)\n(pub extern let set_name ~ ((process/Name 'm) -> Unit) process/set_name)\n(let main {} ())";
    let resolved = crate::resolve_imports_for_source(src, &analysis.module_exports, &analysis);
    let report = compile_with_imports_report(
        "main",
        src,
        "main.mond",
        resolved.imports,
        &analysis.module_exports,
        resolved.module_aliases,
        &resolved.imported_type_decls,
        &resolved.imported_extern_types,
        &resolved.imported_field_indices,
        &resolved.imported_schemes,
    );
    assert!(
        !report.has_errors(),
        "unexpected diagnostics for qualified extern signature: {:?}",
        report
            .diagnostics
            .iter()
            .map(|d| d.message.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn extern_signature_reports_unknown_type_without_type_import() {
    let src = "(pub extern let nth ~ (Int -> (List 'a) -> Option 'a) mond_list_helpers/nth)\n(let main {} ())";
    let report = compile_with_imports_report(
        "main",
        src,
        "main.mond",
        HashMap::new(),
        &HashMap::new(),
        HashMap::new(),
        &[],
        &[],
        &HashMap::new(),
        &HashMap::new(),
    );
    assert!(report.has_errors());
    let messages: Vec<String> = report
        .diagnostics
        .iter()
        .map(|d| d.message.clone())
        .collect();
    assert!(
        messages
            .iter()
            .any(|m| m.contains("unknown type") && m.contains("Option")),
        "expected unknown extern type diagnostic, got: {messages:?}"
    );
}

#[test]
fn extern_signature_accepts_type_imported_unqualified() {
    let src = "(use option [Option])\n(pub extern let nth ~ (Int -> (List 'a) -> Option 'a) mond_list_helpers/nth)\n(let main {} ())";
    let mut module_exports = HashMap::new();
    module_exports.insert("option".to_string(), vec![]);
    let imported_type_decls = exported_type_decls("(pub type ['a] Option [(Some ~ 'a) None])");
    let report = compile_with_imports_report(
        "main",
        src,
        "main.mond",
        HashMap::new(),
        &module_exports,
        HashMap::new(),
        &imported_type_decls,
        &[],
        &HashMap::new(),
        &HashMap::new(),
    );
    assert!(
        !report.has_errors(),
        "unexpected diagnostics: {:?}",
        report
            .diagnostics
            .iter()
            .map(|d| d.message.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn type_declaration_accepts_imported_extern_type() {
    let unknown_src = "(pub extern type Unknown)";
    let std_mods = vec![(
        "unknown".to_string(),
        "mond_unknown".to_string(),
        unknown_src.to_string(),
    )];
    let analysis = crate::build_project_analysis(&std_mods, &[]).expect("analysis");
    let src = r#"
        (use unknown [Unknown])
        (pub extern type Pid)
        (pub type SubjectPayload [(:owner ~ Pid) (:tag ~ Unknown)])
    "#;
    let resolved = crate::resolve_imports_for_source(src, &analysis.module_exports, &analysis);
    let report = compile_with_imports_report(
        "process",
        src,
        "process.mond",
        resolved.imports,
        &analysis.module_exports,
        resolved.module_aliases,
        &resolved.imported_type_decls,
        &resolved.imported_extern_types,
        &resolved.imported_field_indices,
        &resolved.imported_schemes,
    );
    assert!(
        !report.has_errors(),
        "unexpected diagnostics: {:?}",
        report
            .diagnostics
            .iter()
            .map(|d| d.message.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn type_declaration_reports_unknown_type_without_import() {
    let src = "(pub type Next [Continue (Stop ~ ExitReason)])";
    let report = compile_with_imports_report(
        "main",
        src,
        "main.mond",
        HashMap::new(),
        &HashMap::new(),
        HashMap::new(),
        &[],
        &[],
        &HashMap::new(),
        &HashMap::new(),
    );
    assert!(report.has_errors());
    let messages: Vec<String> = report
        .diagnostics
        .iter()
        .map(|d| d.message.clone())
        .collect();
    assert!(
        messages
            .iter()
            .any(|m| m.contains("unknown type") && m.contains("ExitReason")),
        "expected unknown type declaration diagnostic, got: {messages:?}"
    );
}

#[test]
fn type_declaration_reports_missing_type_arguments() {
    let src = "(type ['s 'm] ContinuePayload [(:state ~ 's) (:select ~ 'm)])\n(pub type ['s 'm] Next [(Continue ~ ContinuePayload)])";
    let report = compile_with_imports_report(
        "main",
        src,
        "main.mond",
        HashMap::new(),
        &HashMap::new(),
        HashMap::new(),
        &[],
        &[],
        &HashMap::new(),
        &HashMap::new(),
    );
    assert!(report.has_errors());
    let messages: Vec<String> = report
        .diagnostics
        .iter()
        .map(|d| d.message.clone())
        .collect();
    assert!(
        messages.iter().any(|m| {
            m.contains("wrong number of type arguments for `ContinuePayload`")
                && m.contains("expected 2, found 0")
        }),
        "expected type argument arity diagnostic, got: {messages:?}"
    );
}

#[test]
fn type_declaration_accepts_nested_type_application() {
    let src = "(type ['a] Option [None (Some ~ 'a)])\n\
               (pub extern type ['p] Selector)\n\
               (type ['m] ContinuePayload [(:select ~ (Selector (Option 'm)))])\n\
               (let main {} ())";
    let report = compile_with_imports_report(
        "main",
        src,
        "main.mond",
        HashMap::new(),
        &HashMap::new(),
        HashMap::new(),
        &[],
        &[],
        &HashMap::new(),
        &HashMap::new(),
    );
    assert!(
        !report.has_errors(),
        "expected nested type application to compile, got diagnostics: {:?}",
        report
            .diagnostics
            .iter()
            .map(|d| d.message.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn nullary_extern_call_lowers_to_zero_arity() {
    let src = "(extern let now ~ (Unit -> Int) erlang/system_time)\n(let main {} (now))";
    let output = compile_with_imports(
        "main",
        src,
        "main.mond",
        HashMap::new(),
        &HashMap::new(),
        HashMap::new(),
        &[],
        &[],
        &HashMap::new(),
        &HashMap::new(),
    )
    .expect("compile");
    assert!(
        output.contains("now()"),
        "expected nullary extern call to lower to now()/0:\n{output}"
    );
    assert!(
        !output.contains("now(unit)"),
        "unexpected unit-arg call for nullary extern:\n{output}"
    );
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
        &[],
        &HashMap::new(),
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
        &[],
        &HashMap::new(),
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
    let src = RESULT_STD_SRC;
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
fn letq_reports_continuation_mismatch_without_bind_in_scope() {
    let result_src = RESULT_STD_SRC;
    let io_src = IO_STD_SRC;

    let mut module_exports = HashMap::new();
    module_exports.insert(
        "result".to_string(),
        vec!["Result".to_string(), "bind".to_string()],
    );
    module_exports.insert(
        "io".to_string(),
        vec!["println".to_string(), "debug".to_string()],
    );

    let io_schemes = infer_module_exports(
        "io",
        io_src,
        HashMap::new(),
        &module_exports,
        &[],
        &[],
        &HashMap::new(),
    );

    let mut imported_schemes = HashMap::new();
    imported_schemes.insert("debug".to_string(), io_schemes["debug"].clone());
    imported_schemes.insert("io/debug".to_string(), io_schemes["debug"].clone());

    let imported_type_decls = exported_type_decls(result_src);
    let mut imports = HashMap::new();
    imports.insert("Result".to_string(), "result".to_string());
    imports.insert("debug".to_string(), "io".to_string());

    let src = r#"
            (use result [Result])
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
        &[],
        &HashMap::new(),
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
            .any(|msg| msg.contains("`let?` body must return a `Result`")),
        "unexpected diagnostics: {rendered:?}"
    );
}

#[test]
fn test_declaration_with_letq_reports_continuation_mismatch_without_bind_in_scope() {
    let result_src = RESULT_STD_SRC;
    let io_src = IO_STD_SRC;
    let testing_src = TESTING_STD_SRC;

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
        &[],
        &HashMap::new(),
    );
    let io_schemes = infer_module_exports(
        "io",
        io_src,
        HashMap::new(),
        &module_exports,
        &[],
        &[],
        &HashMap::new(),
    );
    let testing_schemes = infer_module_exports(
        "testing",
        testing_src,
        HashMap::new(),
        &module_exports,
        &exported_type_decls(result_src),
        &[],
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
    imports.insert("Result".to_string(), "result".to_string());
    imports.insert("debug".to_string(), "io".to_string());
    imports.insert("assert_eq".to_string(), "testing".to_string());

    let mut imported_type_decls = exported_type_decls(result_src);
    imported_type_decls.extend(exported_type_decls(testing_src));

    let src = r#"
            (use result [Result])
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
        &[],
        &HashMap::new(),
        &imported_schemes,
    );

    assert!(report.has_errors(), "expected type error");
    assert!(
        report
            .diagnostics
            .iter()
            .flat_map(|d| d.notes.iter())
            .any(|note| note.contains("hint: return `(Ok value)` from the `let?` body")),
        "unexpected diagnostics: {:?}",
        report.diagnostics
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
        &[],
        &HashMap::new(),
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
        &[],
        &HashMap::new(),
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
fn unused_local_analysis_marks_unused_match_pattern_bindings() {
    let src = "(let main {x} (match x y ~> 1))";
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
    assert_eq!(unused, vec!["y".to_string()]);
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
        &[],
        &HashMap::new(),
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
fn compile_emits_unused_match_binding_warning() {
    let src = "(let main {x} (match x y ~> 1))";
    let report = compile_with_imports_report(
        "main",
        src,
        "main.mond",
        HashMap::new(),
        &HashMap::new(),
        HashMap::new(),
        &[],
        &[],
        &HashMap::new(),
        &HashMap::new(),
    );

    let messages: Vec<String> = report.diagnostics.into_iter().map(|d| d.message).collect();
    assert!(
        messages
            .iter()
            .any(|m| m.contains("unused local binding `y`")),
        "missing unused match binding warning: {messages:?}"
    );
}

#[test]
fn compile_emits_unused_type_parameter_warning() {
    let src = "(type ['s 'm] Box [(:value ~ Int)])\n(let main {} ())";
    let report = compile_with_imports_report(
        "main",
        src,
        "main.mond",
        HashMap::new(),
        &HashMap::new(),
        HashMap::new(),
        &[],
        &[],
        &HashMap::new(),
        &HashMap::new(),
    );

    let messages: Vec<String> = report.diagnostics.into_iter().map(|d| d.message).collect();
    assert!(
        messages
            .iter()
            .any(|m| m.contains("unused type parameter `'s` in type `Box`")),
        "missing unused type parameter warning for 's: {messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("unused type parameter `'m` in type `Box`")),
        "missing unused type parameter warning for 'm: {messages:?}"
    );
}

#[test]
fn redundant_match_analysis_flags_arm_after_catch_all() {
    let src = "(let main {x} (match x _ ~> 0 True ~> 1))";
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(src).lex();
    let file_id = lowerer.add_file("scan.mond".into(), src.into());
    let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .expect("parse");
    let decls = lowerer.lower_file(file_id, &sexprs);

    let warnings = warnings::redundant_match_diagnostics(&decls, file_id, &[]);
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].message, "unreachable match arm");
}

#[test]
fn redundant_match_analysis_flags_duplicate_or_alternative() {
    let src = "(let main {x} (match x True | True ~> 1 False ~> 0))";
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(src).lex();
    let file_id = lowerer.add_file("scan.mond".into(), src.into());
    let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .expect("parse");
    let decls = lowerer.lower_file(file_id, &sexprs);

    let warnings = warnings::redundant_match_diagnostics(&decls, file_id, &[]);
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].message, "redundant match alternative");
}

#[test]
fn redundant_match_analysis_does_not_flag_nested_constructor_as_full_coverage() {
    let src =
        "(let main {result} (match result (Ok (Some x)) ~> 1 (Ok None) ~> 2 (Error err) ~> 3))";
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(src).lex();
    let file_id = lowerer.add_file("scan.mond".into(), src.into());
    let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .expect("parse");
    let decls = lowerer.lower_file(file_id, &sexprs);

    let warnings = warnings::redundant_match_diagnostics(&decls, file_id, &[]);
    assert!(
        warnings.is_empty(),
        "unexpected redundancy warnings: {:?}",
        warnings
            .iter()
            .map(|d| d.message.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn redundant_match_analysis_flags_duplicate_constructor_with_wildcard_payload() {
    let src = "(let main {result} (match result (Ok x) ~> 1 (Ok y) ~> 2 (Error err) ~> 3))";
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(src).lex();
    let file_id = lowerer.add_file("scan.mond".into(), src.into());
    let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .expect("parse");
    let decls = lowerer.lower_file(file_id, &sexprs);

    let warnings = warnings::redundant_match_diagnostics(&decls, file_id, &[]);
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].message, "unreachable match arm");
}

#[test]
fn redundant_match_analysis_flags_constructor_after_family_coverage() {
    let src = "(type Light [Red Amber Green])\n(let main {light} (match light Red ~> 0 Amber ~> 1 Green ~> 2 Red ~> 3))";
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(src).lex();
    let file_id = lowerer.add_file("scan.mond".into(), src.into());
    let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .expect("parse");
    let decls = lowerer.lower_file(file_id, &sexprs);

    let warnings = warnings::redundant_match_diagnostics(&decls, file_id, &[]);
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].message, "unreachable match arm");
}

#[test]
fn compile_report_emits_redundant_match_warning() {
    let src = "(let main {x} (match x True ~> 1 False ~> 0 True ~> 2))";
    let report = compile_with_imports_report(
        "main",
        src,
        "main.mond",
        HashMap::new(),
        &HashMap::new(),
        HashMap::new(),
        &[],
        &[],
        &HashMap::new(),
        &HashMap::new(),
    );

    let messages: Vec<String> = report.diagnostics.into_iter().map(|d| d.message).collect();
    assert!(
        messages.iter().any(|m| m == "unreachable match arm"),
        "missing redundant match warning in compile report: {messages:?}"
    );
}

#[test]
fn redundant_match_analysis_does_not_flag_specific_list_prefix_arm() {
    let src = "(let split_unix {path} (match (string/split path \"/\") [\"\"] ~> [] [\"\" | rest] ~> (list/append [\"/\"] rest) rest ~> rest))";
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(src).lex();
    let file_id = lowerer.add_file("scan.mond".into(), src.into());
    let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .expect("parse");
    let decls = lowerer.lower_file(file_id, &sexprs);

    let warnings = warnings::redundant_match_diagnostics(&decls, file_id, &[]);
    assert!(
        warnings.is_empty(),
        "unexpected redundancy warnings: {:?}",
        warnings
            .iter()
            .map(|d| d.message.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn redundant_match_analysis_flags_arm_after_non_empty_list_catch_all() {
    let src = "(let main {xs} (match xs [h | t] ~> 1 [x | xs2] ~> 2 [] ~> 0))";
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(src).lex();
    let file_id = lowerer.add_file("scan.mond".into(), src.into());
    let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .expect("parse");
    let decls = lowerer.lower_file(file_id, &sexprs);

    let warnings = warnings::redundant_match_diagnostics(&decls, file_id, &[]);
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].message, "unreachable match arm");
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
fn qualified_only_import_used_in_type_decl_is_not_flagged_unused() {
    let src =
        "(use std/process)\n(type ['m] Actor [(:name ~ (process/Name 'm))])\n(let main {} ())";
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(src).lex();
    let file_id = lowerer.add_file("scan.mond".into(), src.into());
    let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .expect("parse");
    let decls = lowerer.lower_file(file_id, &sexprs);

    let mut module_exports = HashMap::new();
    module_exports.insert("process".to_string(), vec![]);
    let warnings =
        warnings::unused_unqualified_import_diagnostics(&decls, file_id, &module_exports, &[]);
    assert!(
        warnings.is_empty(),
        "unexpected warnings: {:?}",
        warnings
            .iter()
            .map(|d| d.message.clone())
            .collect::<Vec<_>>()
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
        "(use std/option [Option])\n(type Attributes [(:max_age ~ Option Int)])\n(let main {} ())";
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
        exported_type_decls("(pub type ['a 'e] Result [(Ok ~ 'a) (Error ~ 'e)])");
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
        exported_type_decls("(pub type DecodeError [(:expected ~ String) (:found ~ String)])");
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
    let src = "(type Attributes [(:max_age ~ Int)])\n(let main {} ())";
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
    let src = "(type Flag [On Off])\n(let main {} On)";
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
fn unused_type_param_analysis_marks_variant_params_not_referenced() {
    let src = "(type ['s 'm] Next [(Continue ~ ContinuePayload) (Stop ~ ExitReason)])";
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(src).lex();
    let file_id = lowerer.add_file("scan.mond".into(), src.into());
    let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .expect("parse");
    let decls = lowerer.lower_file(file_id, &sexprs);

    let mut unused: Vec<(String, String)> = warnings::unused_type_param_spans(&decls)
        .into_iter()
        .map(|(type_name, param, _)| (type_name, param))
        .collect();
    unused.sort();
    assert_eq!(
        unused,
        vec![
            ("Next".to_string(), "'m".to_string()),
            ("Next".to_string(), "'s".to_string())
        ]
    );
}

#[test]
fn unused_type_param_analysis_ignores_referenced_params() {
    let src = "(type ['s 'm] Next [(Continue ~ ContinuePayload 's 'm) (Stop ~ ExitReason)])";
    let mut lowerer = lower::Lowerer::new();
    let tokens = crate::lexer::Lexer::new(src).lex();
    let file_id = lowerer.add_file("scan.mond".into(), src.into());
    let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .expect("parse");
    let decls = lowerer.lower_file(file_id, &sexprs);

    assert!(
        warnings::unused_type_param_spans(&decls).is_empty(),
        "expected referenced type params to avoid warnings"
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
