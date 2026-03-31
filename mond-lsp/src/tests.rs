use tower_lsp::lsp_types::{CompletionItemKind, Position, Range, Url};

use crate::{
    project::Project,
    state::{DocumentState, ServerState},
};

use super::*;
use std::{
    fs,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

fn unique_temp_root() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    std::env::temp_dir().join(format!("mond-lsp-test-{}-{nanos}", std::process::id()))
}

fn write_project_file(root: &Path, relative: &str, source: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, source).expect("write project file");
}

fn test_project(
    external_modules: BTreeMap<String, ModuleSource>,
    src_modules: BTreeMap<String, ModuleSource>,
    test_modules: BTreeMap<String, ModuleSource>,
    package_name: Option<&str>,
) -> Project {
    Project {
        root: None,
        external_modules: Arc::new(external_modules.clone()),
        src_modules: Arc::new(src_modules.clone()),
        test_modules: Arc::new(test_modules),
        analysis: Arc::new(
            build_project_analysis(&external_modules, &src_modules, package_name)
                .expect("project analysis"),
        ),
    }
}

#[test]
fn position_offset_roundtrip_handles_ascii() {
    let src = "(let main {} (io/debug 1))\n";
    let pos = Position::new(0, 14);
    let offset = position_to_offset(src, pos).unwrap();
    assert_eq!(offset_to_position(src, offset), pos);
}

#[test]
fn hover_target_finds_imported_function_reference() {
    let src = "(use std/testing [assert_eq])\n(let main {} (assert_eq 1 1))";
    let offset = src.rfind("assert_eq").unwrap();
    let target = find_hover_target(Path::new("src/main.mond"), src, offset);
    match target {
        Some(HoverTarget::Unqualified(name)) => assert_eq!(name, "assert_eq"),
        other => panic!("unexpected target: {other:?}"),
    }
}

#[test]
fn hover_target_finds_top_level_function_reference_inside_call() {
    let src = "(let add_one {x} (+ x 1))\n(let main {} (add_one 2))";
    let offset = src.rfind("add_one").unwrap();
    let target = find_hover_target(Path::new("src/main.mond"), src, offset);
    match target {
        Some(HoverTarget::Unqualified(name)) => assert_eq!(name, "add_one"),
        other => panic!("unexpected target: {other:?}"),
    }
}

#[test]
fn hover_target_ignores_local_bindings() {
    let src = "(let main {} (let [assert_eq 1] assert_eq))";
    let offset = src.rfind("assert_eq").unwrap();
    assert!(find_hover_target(Path::new("src/main.mond"), src, offset).is_none());
}

#[test]
fn hover_target_finds_record_update_field_accessor_reference() {
    let src = "(let expire_cookie {attributes} (with attributes :max_age (Some 0)))";
    let offset = src.find("max_age").unwrap() + 2;
    let target = find_hover_target(Path::new("src/main.mond"), src, offset);
    match target {
        Some(HoverTarget::Unqualified(name)) => assert_eq!(name, ":max_age"),
        other => panic!("unexpected target: {other:?}"),
    }
}

#[test]
fn full_document_range_covers_entire_source() {
    let src = "(let add {a b} (+ a b))\n";
    let range = full_document_range(src);
    assert_eq!(range.start, Position::new(0, 0));
    assert_eq!(range.end, Position::new(1, 0));
}

#[test]
fn best_expr_type_prefers_smallest_matching_span() {
    let expr_types = vec![(0..10, "Int".to_string()), (4..7, "String".to_string())];
    assert_eq!(
        best_expr_type_at_offset(&expr_types, 5),
        Some("String".to_string())
    );
}

#[test]
fn find_top_level_definition_range_points_at_function_name() {
    let src = "(let add_one {x} (+ x 1))\n";
    let range =
        find_top_level_definition_range(Path::new("src/main.mond"), src, "add_one").unwrap();
    assert_eq!(
        range,
        Some(Range::new(Position::new(0, 5), Position::new(0, 12)))
    );
}

#[test]
fn symbol_at_resolves_top_level_definition_site() {
    let src = "(let add_one {x} (+ x 1))\n(let main {} (add_one 2))";
    let imports = mondc::ResolvedImports {
        imports: HashMap::new(),
        import_origins: HashMap::new(),
        imported_schemes: HashMap::new(),
        imported_type_decls: Vec::new(),
        imported_extern_types: Vec::new(),
        imported_field_indices: HashMap::new(),
        imported_private_records: HashMap::new(),
        module_aliases: HashMap::new(),
    };
    let offset = src.find("add_one").unwrap();
    let symbol = symbol_at(Path::new("src/main.mond"), src, "main", &imports, offset)
        .unwrap()
        .unwrap();
    assert_eq!(
        symbol,
        Symbol {
            module: "main".to_string(),
            function: "add_one".to_string(),
        }
    );
}

#[test]
fn symbol_at_resolves_import_list_entries() {
    let src = "(use std/testing [assert_eq])\n(let main {} (assert_eq 1 1))";
    let mut import_origins = HashMap::new();
    import_origins.insert("assert_eq".to_string(), "testing".to_string());
    let imports = mondc::ResolvedImports {
        imports: HashMap::new(),
        import_origins,
        imported_schemes: HashMap::new(),
        imported_type_decls: Vec::new(),
        imported_extern_types: Vec::new(),
        imported_field_indices: HashMap::new(),
        imported_private_records: HashMap::new(),
        module_aliases: HashMap::new(),
    };
    let offset = src.find("assert_eq").unwrap();
    let symbol = symbol_at(Path::new("src/main.mond"), src, "main", &imports, offset)
        .unwrap()
        .unwrap();
    assert_eq!(
        symbol,
        Symbol {
            module: "testing".to_string(),
            function: "assert_eq".to_string(),
        }
    );
}

#[test]
fn collect_symbol_occurrences_includes_imports_defs_and_refs() {
    let src = "(use util [map])\n(let map {x} x)\n(let main {} (util/map (map 1)))";
    let mut import_origins = HashMap::new();
    import_origins.insert("map".to_string(), "util".to_string());
    let imports = mondc::ResolvedImports {
        imports: HashMap::new(),
        import_origins,
        imported_schemes: HashMap::new(),
        imported_type_decls: Vec::new(),
        imported_extern_types: Vec::new(),
        imported_field_indices: HashMap::new(),
        imported_private_records: HashMap::new(),
        module_aliases: HashMap::new(),
    };
    let occurrences =
        collect_symbol_occurrences(Path::new("src/main.mond"), src, "main", &imports).unwrap();

    let main_map = occurrences
        .iter()
        .filter(|occ| occ.symbol.module == "main" && occ.symbol.function == "map")
        .count();
    let util_map = occurrences
        .iter()
        .filter(|occ| occ.symbol.module == "util" && occ.symbol.function == "map")
        .count();

    assert_eq!(main_map, 2);
    assert_eq!(util_map, 2);
}

#[test]
fn completion_context_detects_qualified_prefix() {
    let src = "(io/pri)";
    let offset = src.find("pri").unwrap() + 3;
    match completion_context(src, offset) {
        Some(CompletionContext::Qualified { module, prefix }) => {
            assert_eq!(module, "io");
            assert_eq!(prefix, "pri");
        }
        other => panic!("unexpected completion context: {other:?}"),
    }
}

#[test]
fn completion_context_detects_use_import_path_prefix() {
    let src = "(use std/pr)";
    let offset = src.find("pr").unwrap() + 2;
    match completion_context(src, offset) {
        Some(CompletionContext::ImportPath { root, prefix }) => {
            assert_eq!(root, "std");
            assert_eq!(prefix, "pr");
        }
        other => panic!("unexpected completion context: {other:?}"),
    }
}

#[test]
fn completion_context_detects_use_import_list_prefix() {
    let src = "(use std/process [sp)";
    let offset = src.find("sp").unwrap() + 2;
    match completion_context(src, offset) {
        Some(CompletionContext::UseImportList { module, prefix }) => {
            assert_eq!(module, "process");
            assert_eq!(prefix, "sp");
        }
        other => panic!("unexpected completion context: {other:?}"),
    }
}

#[test]
fn completion_context_detects_unqualified_prefix() {
    let src = "(prin)";
    let offset = src.find("prin").unwrap() + 4;
    match completion_context(src, offset) {
        Some(CompletionContext::Unqualified { prefix }) => assert_eq!(prefix, "prin"),
        other => panic!("unexpected completion context: {other:?}"),
    }
}

#[test]
fn completion_context_detects_record_constructor_field_prefix() {
    let src = "(Point :x)";
    let offset = src.find(":x").unwrap() + 2;
    match completion_context(src, offset) {
        Some(CompletionContext::RecordField {
            record_name,
            prefix,
        }) => {
            assert_eq!(record_name.as_deref(), Some("Point"));
            assert_eq!(prefix, "x");
        }
        other => panic!("unexpected completion context: {other:?}"),
    }
}

#[test]
fn completion_context_detects_record_update_field_prefix() {
    let src = "(with point :x)";
    let offset = src.find(":x").unwrap() + 2;
    match completion_context(src, offset) {
        Some(CompletionContext::RecordField {
            record_name,
            prefix,
        }) => {
            assert_eq!(record_name, None);
            assert_eq!(prefix, "x");
        }
        other => panic!("unexpected completion context: {other:?}"),
    }
}

#[test]
fn local_names_at_offset_includes_let_match_and_lambda_bindings() {
    let src = "(let main {arg}\n\
                     (let [local 1]\n\
                       (match local\n\
                         value ~> (f {inner} -> (+ arg (+ local (+ value inner)))))))";
    let offset = src.find("inner").unwrap() + 2;
    let names = local_names_at_offset(Path::new("src/main.mond"), src, offset).unwrap();
    assert!(names.contains(&"arg".to_string()));
    assert!(names.contains(&"local".to_string()));
    assert!(names.contains(&"value".to_string()));
    assert!(names.contains(&"inner".to_string()));
}

#[test]
fn completion_items_filter_by_prefix() {
    let items = completion_items_from_names(
        vec![
            "println".to_string(),
            "print".to_string(),
            "debug".to_string(),
        ],
        "pri",
        CompletionItemKind::FUNCTION,
    );
    let labels: Vec<_> = items.into_iter().map(|item| item.label).collect();
    assert_eq!(labels, vec!["print".to_string(), "println".to_string()]);
}

#[test]
fn completion_item_can_describe_modules() {
    let item = completion_item(
        "io/".to_string(),
        CompletionItemKind::MODULE,
        Some("module".to_string()),
        None,
    );
    assert_eq!(item.label, "io/");
    assert_eq!(item.kind, Some(CompletionItemKind::MODULE));
    assert_eq!(item.detail.as_deref(), Some("module"));
}

#[test]
fn unqualified_completion_includes_local_functions_when_typecheck_fails() {
    let src_modules = BTreeMap::from([(
        "main".to_string(),
        ModuleSource {
            name: "main".to_string(),
            path: PathBuf::from("src/main.mond"),
            source: "(let update_x {point new_x} point)\n\
                         (let main {} (up 1))"
                .to_string(),
        },
    )]);
    let project = test_project(BTreeMap::new(), src_modules.clone(), BTreeMap::new(), None);

    let doc = project.src_modules.get("main").expect("main module");
    let analysis = project.analyze_document(doc).expect("document analysis");
    assert!(analysis.bindings.is_empty(), "expected typecheck failure");

    let offset = doc.source.find("up").expect("up call") + 2;
    let items = project
        .unqualified_completion_items(doc, &analysis, offset, "up")
        .expect("completions");
    assert!(
        items.iter().any(|item| item.label == "update_x"),
        "expected completion labels to include update_x, got: {:?}",
        items.into_iter().map(|item| item.label).collect::<Vec<_>>()
    );
}

#[test]
fn unqualified_completion_includes_local_type_names() {
    let src_modules = BTreeMap::from([(
        "main".to_string(),
        ModuleSource {
            name: "main".to_string(),
            path: PathBuf::from("src/main.mond"),
            source: "(type Point [(:x ~ Int) (:y ~ Int)])\n\
                         (let main {} Po)"
                .to_string(),
        },
    )]);
    let project = test_project(BTreeMap::new(), src_modules.clone(), BTreeMap::new(), None);

    let doc = project.src_modules.get("main").expect("main module");
    let analysis = project.analyze_document(doc).expect("document analysis");
    let offset = doc.source.rfind("Po").expect("Po prefix") + 2;
    let items = project
        .unqualified_completion_items(doc, &analysis, offset, "Po")
        .expect("completions");
    let point = items
        .iter()
        .find(|item| item.label == "Point")
        .expect("Point completion");
    assert_eq!(point.kind, Some(CompletionItemKind::STRUCT));
}

#[test]
fn unqualified_completion_uses_curried_arrow_type_formatting() {
    let src_modules = BTreeMap::from([(
        "main".to_string(),
        ModuleSource {
            name: "main".to_string(),
            path: PathBuf::from("src/main.mond"),
            source: "(type ContinuePayload [(:selector ~ Int)])\n\
                         (type Initialised [(:selector ~ Int)])\n\
                         (let read_selector {x} (:selector x))\n\
                         (let main {} (read_selector (ContinuePayload :selector 1)))"
                .to_string(),
        },
    )]);
    let project = test_project(BTreeMap::new(), src_modules.clone(), BTreeMap::new(), None);

    let doc = project.src_modules.get("main").expect("main module");
    let analysis = project.analyze_document(doc).expect("document analysis");
    let offset = doc.source.len();
    let items = project
        .unqualified_completion_items(doc, &analysis, offset, "read")
        .expect("completions");
    let item = items
        .iter()
        .find(|item| item.label == "read_selector")
        .expect("read_selector completion");
    let detail = item.detail.as_deref().unwrap_or_default();
    assert!(
        detail.contains("->") && !detail.contains("=>"),
        "expected curried arrow formatting in completion detail, got {detail}"
    );
}

#[test]
fn record_field_completion_suggests_fields_for_constructor() {
    let src_modules = BTreeMap::from([(
        "main".to_string(),
        ModuleSource {
            name: "main".to_string(),
            path: PathBuf::from("src/main.mond"),
            source: "(type Point [(:x ~ Int) (:y ~ Int)])\n\
                         (let main {} (Point :))"
                .to_string(),
        },
    )]);
    let project = test_project(BTreeMap::new(), src_modules.clone(), BTreeMap::new(), None);

    let doc = project.src_modules.get("main").expect("main module");
    let analysis = project.analyze_document(doc).expect("document analysis");
    let offset = doc.source.rfind(':').expect("field start") + 1;
    let ctx = completion_context(&doc.source, offset).expect("completion context");
    let CompletionContext::RecordField {
        record_name,
        prefix,
    } = ctx
    else {
        panic!("expected record field context");
    };
    let items =
        project.record_field_completion_items(doc, &analysis, record_name.as_deref(), &prefix);
    let labels: Vec<String> = items.into_iter().map(|item| item.label).collect();
    assert_eq!(labels, vec!["x".to_string(), "y".to_string()]);
}

#[test]
fn record_field_completion_suggests_fields_for_with_update() {
    let src_modules = BTreeMap::from([(
        "main".to_string(),
        ModuleSource {
            name: "main".to_string(),
            path: PathBuf::from("src/main.mond"),
            source: "(type Point [(:x ~ Int) (:y ~ Int)])\n\
                         (let p (Point :x 1 :y 2))\n\
                         (let main {} (with p :))"
                .to_string(),
        },
    )]);
    let project = test_project(BTreeMap::new(), src_modules.clone(), BTreeMap::new(), None);

    let doc = project.src_modules.get("main").expect("main module");
    let analysis = project.analyze_document(doc).expect("document analysis");
    let offset = doc.source.rfind(':').expect("field start") + 1;
    let ctx = completion_context(&doc.source, offset).expect("completion context");
    let CompletionContext::RecordField {
        record_name,
        prefix,
    } = ctx
    else {
        panic!("expected record field context");
    };
    assert_eq!(record_name, None);
    let items =
        project.record_field_completion_items(doc, &analysis, record_name.as_deref(), &prefix);
    let labels: Vec<String> = items.into_iter().map(|item| item.label).collect();
    assert_eq!(labels, vec!["x".to_string(), "y".to_string()]);
}

#[test]
fn import_path_completion_items_include_std_submodules() {
    let std_modules = BTreeMap::from([
        (
            "std".to_string(),
            ModuleSource {
                name: "std".to_string(),
                path: PathBuf::from("target/deps/std/src/lib.mond"),
                source: "(pub let hello {} 1)".to_string(),
            },
        ),
        (
            "process".to_string(),
            ModuleSource {
                name: "process".to_string(),
                path: PathBuf::from("target/deps/std/src/process.mond"),
                source: "(pub let exit {} 1)".to_string(),
            },
        ),
    ]);
    let project = test_project(std_modules.clone(), BTreeMap::new(), BTreeMap::new(), None);

    let labels = project
        .import_path_completion_items("std", "pr")
        .into_iter()
        .map(|item| item.label)
        .collect::<Vec<_>>();

    assert_eq!(labels, vec!["process".to_string()]);
}

#[test]
fn use_import_list_completion_items_include_std_process_exports() {
    let std_modules = BTreeMap::from([
        (
            "std".to_string(),
            ModuleSource {
                name: "std".to_string(),
                path: PathBuf::from("target/deps/std/src/lib.mond"),
                source: "(pub let hello {} 1)".to_string(),
            },
        ),
        (
            "process".to_string(),
            ModuleSource {
                name: "process".to_string(),
                path: PathBuf::from("target/deps/std/src/process.mond"),
                source: "(pub let spawn {task} task)\n\
                             (let hidden {} 1)\n\
                             (pub let sleep {ms} ms)\n\
                             (pub type ExitReason [Normal])\n\
                             (pub extern type ['p] Selector)"
                    .to_string(),
            },
        ),
    ]);
    let project = test_project(std_modules.clone(), BTreeMap::new(), BTreeMap::new(), None);

    let labels = project
        .use_import_list_completion_items("process", "s")
        .into_iter()
        .map(|item| item.label)
        .collect::<Vec<_>>();

    assert_eq!(labels, vec!["sleep".to_string(), "spawn".to_string()]);
}

#[test]
fn use_import_list_completion_items_include_extern_types() {
    let std_modules = BTreeMap::from([
        (
            "std".to_string(),
            ModuleSource {
                name: "std".to_string(),
                path: PathBuf::from("target/deps/std/src/lib.mond"),
                source: "(pub let hello {} 1)".to_string(),
            },
        ),
        (
            "process".to_string(),
            ModuleSource {
                name: "process".to_string(),
                path: PathBuf::from("target/deps/std/src/process.mond"),
                source: "(pub let spawn {task} task)\n\
                             (pub type ExitReason [Normal])\n\
                             (pub extern type ['p] Selector)"
                    .to_string(),
            },
        ),
    ]);
    let project = test_project(std_modules.clone(), BTreeMap::new(), BTreeMap::new(), None);

    let labels = project
        .use_import_list_completion_items("process", "Sel")
        .into_iter()
        .map(|item| item.label)
        .collect::<Vec<_>>();

    assert_eq!(labels, vec!["Selector".to_string()]);
}

#[test]
fn top_level_symbols_collect_functions_and_types() {
    let src = "(type Option [None])\n\
                   (extern let debug ~ (Unit -> String) io/debug)\n\
                   (let main {} (debug))";
    let symbols = top_level_symbols(Path::new("src/main.mond"), src).unwrap();
    let names: Vec<_> = symbols.into_iter().map(|symbol| symbol.name).collect();
    assert_eq!(
        names,
        vec![
            "Option".to_string(),
            "debug".to_string(),
            "main".to_string()
        ]
    );
}

#[test]
fn top_level_symbols_attach_doc_comments() {
    let src = ";;; adds one\n;;; to its input\n(let add_one {x} (+ x 1))\n";
    let symbols = top_level_symbols(Path::new("src/main.mond"), src).unwrap();
    assert_eq!(
        symbols[0].documentation.as_deref(),
        Some("adds one\nto its input")
    );
}

#[test]
fn plain_comments_do_not_attach_as_docs() {
    let src = ";;; docs\n;; note\n(let add_one {x} (+ x 1))\n";
    let symbols = top_level_symbols(Path::new("src/main.mond"), src).unwrap();
    assert_eq!(symbols[0].documentation, None);
}

#[test]
fn signature_target_finds_unqualified_call_argument_index() {
    let src = "(let add {a b} (+ a b))\n(let main {} (add 1 2))";
    let imports = mondc::ResolvedImports {
        imports: HashMap::new(),
        import_origins: HashMap::new(),
        imported_schemes: HashMap::new(),
        imported_type_decls: Vec::new(),
        imported_extern_types: Vec::new(),
        imported_field_indices: HashMap::new(),
        imported_private_records: HashMap::new(),
        module_aliases: HashMap::new(),
    };
    let offset = src.rfind('2').unwrap();
    let target = signature_target_at(Path::new("src/main.mond"), src, "main", &imports, offset)
        .unwrap()
        .unwrap();
    assert_eq!(target.symbol.module, "main");
    assert_eq!(target.symbol.function, "add");
    assert_eq!(target.arg_index, 1);
}

#[test]
fn signature_target_finds_qualified_call_argument_index() {
    let src = "(use std/io)\n(let main {} (io/println \"hello\"))";
    let imports = mondc::ResolvedImports {
        imports: HashMap::new(),
        import_origins: HashMap::new(),
        imported_schemes: HashMap::new(),
        imported_type_decls: Vec::new(),
        imported_extern_types: Vec::new(),
        imported_field_indices: HashMap::new(),
        imported_private_records: HashMap::new(),
        module_aliases: HashMap::new(),
    };
    let offset = src.find("hello").unwrap();
    let target = signature_target_at(Path::new("src/main.mond"), src, "main", &imports, offset)
        .unwrap()
        .unwrap();
    assert_eq!(target.symbol.module, "io");
    assert_eq!(target.symbol.function, "println");
    assert_eq!(target.arg_index, 0);
}

#[test]
fn external_modules_include_submodules_without_root_reexports() {
    let external_modules = BTreeMap::from([
        (
            "std".to_string(),
            ModuleSource {
                name: "std".to_string(),
                path: PathBuf::from("std/lib.mond"),
                source: "(pub let hello {} 1)".to_string(),
            },
        ),
        (
            "io".to_string(),
            ModuleSource {
                name: "io".to_string(),
                path: PathBuf::from("std/io.mond"),
                source: "(pub let println {x} x)".to_string(),
            },
        ),
    ]);
    let external_mods = external_modules
        .iter()
        .map(|(module_name, module)| (module_name.clone(), module.source.clone()))
        .collect::<Vec<(String, String)>>();
    let external_mods =
        mondc::external_modules_from_sources(&external_mods).expect("external modules");
    assert!(external_mods.iter().any(|(name, _, _)| name == "std"));
    assert!(external_mods.iter().any(|(name, _, _)| name == "io"));
}

#[test]
fn resolve_imports_supports_std_submodules_without_root_reexports() {
    let external_modules = BTreeMap::from([
        (
            "std".to_string(),
            ModuleSource {
                name: "std".to_string(),
                path: PathBuf::from("std/lib.mond"),
                source: "(pub let hello {} 1)".to_string(),
            },
        ),
        (
            "io".to_string(),
            ModuleSource {
                name: "io".to_string(),
                path: PathBuf::from("std/io.mond"),
                source: "(pub let println {x} x)".to_string(),
            },
        ),
    ]);
    let analysis = build_project_analysis(&external_modules, &BTreeMap::new(), None)
        .expect("project analysis");
    let imports = mondc::resolve_imports_for_source(
        "(use std/io)\n(let main {} ())",
        &analysis.module_exports,
        &analysis,
    );
    assert!(analysis.module_exports.contains_key("io"));
    assert!(imports.module_aliases.contains_key("io"));
}

#[test]
fn package_name_aliases_lib_module_for_import_resolution() {
    let src_modules = BTreeMap::from([(
        "lib".to_string(),
        ModuleSource {
            name: "lib".to_string(),
            path: PathBuf::from("src/lib.mond"),
            source: "(pub let now {} 1)".to_string(),
        },
    )]);
    let analysis = build_project_analysis(&BTreeMap::new(), &src_modules, Some("time"))
        .expect("project analysis");
    let imports = mondc::resolve_imports_for_source(
        "(use time)\n(let main {} (time/now))",
        &analysis.module_exports,
        &analysis,
    );

    assert!(analysis.module_exports.contains_key("time"));
    assert_eq!(
        imports.module_aliases.get("time").map(String::as_str),
        Some("lib")
    );
}

#[test]
fn collect_external_modules_and_analysis_include_cached_dependencies() {
    let root = unique_temp_root();
    let dep_src = root.join("target").join("deps").join("time").join("src");
    fs::create_dir_all(&dep_src).expect("create dependency src");
    fs::write(dep_src.join("lib.mond"), "(pub let now {} 1)").expect("write lib");
    fs::write(dep_src.join("duration.mond"), "(pub let seconds {} 1)").expect("write submodule");

    let external_modules = collect_external_modules(Some(&root));
    assert!(external_modules.contains_key("time"));
    assert!(external_modules.contains_key("duration"));

    let analysis = build_project_analysis(&external_modules, &BTreeMap::new(), None)
        .expect("project analysis");
    assert!(analysis.module_exports.contains_key("time"));
    assert!(analysis.module_exports.contains_key("duration"));
    assert_eq!(
        analysis.module_aliases.get("time").map(String::as_str),
        Some("d_time_time")
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn local_symbol_at_resolves_let_binding_and_use() {
    let src = "(let main {}\n  (let [x 1]\n    (+ x x)))";
    let offset = src.rfind("x").unwrap();
    let symbol = local_symbol_at(Path::new("src/main.mond"), src, offset)
        .unwrap()
        .unwrap();
    let def_start = src.find("[x").unwrap() + 1;
    assert_eq!(symbol.name, "x");
    assert_eq!(symbol.def_range, def_start..def_start + 1);
}

#[test]
fn local_symbol_at_resolves_match_binding() {
    let src = "(let main {x}\n  (match x\n    value ~> (+ value 1)))";
    let offset = src.rfind("value").unwrap();
    let symbol = local_symbol_at(Path::new("src/main.mond"), src, offset)
        .unwrap()
        .unwrap();
    let def_start = src.find("value").unwrap();
    assert_eq!(symbol.name, "value");
    assert_eq!(symbol.def_range, def_start..def_start + "value".len());
}

#[test]
fn project_diagnostics_include_non_focused_module() {
    let src_modules = BTreeMap::from([
        (
            "main".to_string(),
            ModuleSource {
                name: "main".to_string(),
                path: PathBuf::from("src/main.mond"),
                source: "(use helper)\n(let main {} (helper/value))".to_string(),
            },
        ),
        (
            "helper".to_string(),
            ModuleSource {
                name: "helper".to_string(),
                path: PathBuf::from("src/helper.mond"),
                source: "(let broken {} unknown)".to_string(),
            },
        ),
    ]);
    let project = test_project(BTreeMap::new(), src_modules.clone(), BTreeMap::new(), None);

    let batches =
        project_diagnostic_batches(&project, project.src_modules.values().cloned().collect());

    let helper_diags = batches
        .iter()
        .find(|(module, _)| module.name == "helper")
        .map(|(_, diags)| diags)
        .expect("helper diagnostics");
    assert!(
        helper_diags
            .iter()
            .any(|diag| diag.message.contains("unbound variable `unknown`")),
        "expected helper diagnostics, got {helper_diags:?}"
    );
}

#[test]
fn project_load_reuses_cached_workspace_when_inputs_match() {
    let root = unique_temp_root();
    write_project_file(&root, "bahn.toml", "[package]\nname = \"demo\"\n");
    write_project_file(&root, "src/main.mond", "(let main {} 1)\n");

    let state = Arc::new(Mutex::new(ServerState::default()));
    let uri = Url::from_file_path(root.join("src/main.mond")).expect("main uri");

    let first = Project::load(Some(&root), &state, &uri).expect("first load");
    let second = Project::load(Some(&root), &state, &uri).expect("second load");

    assert!(Arc::ptr_eq(&first.analysis, &second.analysis));
    assert!(Arc::ptr_eq(&first.src_modules, &second.src_modules));
    assert_eq!(
        second
            .document_for_path(&root.join("src/main.mond"))
            .map(|doc| doc.source),
        Some("(let main {} 1)\n".to_string())
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn project_load_rebuilds_analysis_when_src_overlay_changes() {
    let root = unique_temp_root();
    write_project_file(&root, "bahn.toml", "[package]\nname = \"demo\"\n");
    write_project_file(&root, "src/main.mond", "(let main {} 1)\n");

    let state = Arc::new(Mutex::new(ServerState::default()));
    let main_path = root.join("src/main.mond");
    let uri = Url::from_file_path(&main_path).expect("main uri");

    let first = Project::load(Some(&root), &state, &uri).expect("first load");
    state.lock().unwrap().open_docs.insert(
        uri.clone(),
        DocumentState {
            version: 1,
            text: "(let main {} 2)\n".to_string(),
        },
    );
    let second = Project::load(Some(&root), &state, &uri).expect("second load");

    assert!(!Arc::ptr_eq(&first.analysis, &second.analysis));
    assert!(!Arc::ptr_eq(&first.src_modules, &second.src_modules));
    assert_eq!(
        second.document_for_path(&main_path).map(|doc| doc.source),
        Some("(let main {} 2)\n".to_string())
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn project_load_reuses_analysis_when_only_test_overlay_changes() {
    let root = unique_temp_root();
    write_project_file(&root, "bahn.toml", "[package]\nname = \"demo\"\n");
    write_project_file(&root, "src/main.mond", "(let main {} 1)\n");
    write_project_file(&root, "tests/main_test.mond", "(let smoke_test {} 1)\n");

    let state = Arc::new(Mutex::new(ServerState::default()));
    let test_path = root.join("tests/main_test.mond");
    let uri = Url::from_file_path(&test_path).expect("test uri");

    let first = Project::load(Some(&root), &state, &uri).expect("first load");
    state.lock().unwrap().open_docs.insert(
        uri.clone(),
        DocumentState {
            version: 1,
            text: "(let smoke_test {} 2)\n".to_string(),
        },
    );
    let second = Project::load(Some(&root), &state, &uri).expect("second load");

    assert!(Arc::ptr_eq(&first.analysis, &second.analysis));
    assert!(!Arc::ptr_eq(&first.test_modules, &second.test_modules));
    assert_eq!(
        second.document_for_path(&test_path).map(|doc| doc.source),
        Some("(let smoke_test {} 2)\n".to_string())
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn project_load_restores_disk_backed_source_after_overlay_close() {
    let root = unique_temp_root();
    write_project_file(&root, "bahn.toml", "[package]\nname = \"demo\"\n");
    write_project_file(&root, "src/main.mond", "(let main {} 1)\n");

    let state = Arc::new(Mutex::new(ServerState::default()));
    let main_path = root.join("src/main.mond");
    let uri = Url::from_file_path(&main_path).expect("main uri");

    state.lock().unwrap().open_docs.insert(
        uri.clone(),
        DocumentState {
            version: 1,
            text: "(let main {} 2)\n".to_string(),
        },
    );
    let overlay_project = Project::load(Some(&root), &state, &uri).expect("overlay load");
    state.lock().unwrap().open_docs.remove(&uri);
    let disk_project = Project::load(Some(&root), &state, &uri).expect("disk load");

    assert!(!Arc::ptr_eq(
        &overlay_project.analysis,
        &disk_project.analysis
    ));
    assert_eq!(
        disk_project
            .document_for_path(&main_path)
            .map(|doc| doc.source),
        Some("(let main {} 1)\n".to_string())
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn project_load_ignores_unrelated_open_documents_for_cache_reuse() {
    let root = unique_temp_root();
    write_project_file(&root, "bahn.toml", "[package]\nname = \"demo\"\n");
    write_project_file(&root, "src/main.mond", "(let main {} 1)\n");

    let state = Arc::new(Mutex::new(ServerState::default()));
    let uri = Url::from_file_path(root.join("src/main.mond")).expect("main uri");

    let first = Project::load(Some(&root), &state, &uri).expect("first load");
    let unrelated_uri =
        Url::from_file_path(root.join("scratch/notes.mond")).expect("unrelated uri");
    state.lock().unwrap().open_docs.insert(
        unrelated_uri,
        DocumentState {
            version: 1,
            text: "(let note {} 1)\n".to_string(),
        },
    );
    let second = Project::load(Some(&root), &state, &uri).expect("second load");

    assert!(Arc::ptr_eq(&first.analysis, &second.analysis));
    assert!(Arc::ptr_eq(&first.src_modules, &second.src_modules));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn collect_local_occurrences_respects_shadowing() {
    let src = "(let main {x}\n  (let [x 1]\n    (+ x ((f {x} -> x)))))";
    let occurrences = collect_local_occurrences(Path::new("src/main.mond"), src).unwrap();

    let outer_x = src.find("{x}").unwrap() + 1;
    let let_x = src.find("[x").unwrap() + 1;
    let lambda_x = src.rfind("{x}").unwrap() + 1;

    let outer_refs = occurrences
        .iter()
        .filter(|occ| occ.symbol.def_range == (outer_x..outer_x + 1))
        .count();
    let let_refs = occurrences
        .iter()
        .filter(|occ| occ.symbol.def_range == (let_x..let_x + 1))
        .count();
    let lambda_refs = occurrences
        .iter()
        .filter(|occ| occ.symbol.def_range == (lambda_x..lambda_x + 1))
        .count();

    assert_eq!(outer_refs, 1);
    assert_eq!(let_refs, 2);
    assert_eq!(lambda_refs, 2);
}
