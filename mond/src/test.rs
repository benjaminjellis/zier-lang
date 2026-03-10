use std::{collections::HashMap, path::Path, process::Command};

use eyre::Context;

use crate::{
    TARGET_DIR, TEST_BUILD_DIR,
    build::{ErlSources, generate_erl_sources},
    ui,
    utils::find_mond_files,
};

const TEST_DIR: &str = "tests";

pub(crate) fn test(project_dir: &Path) -> eyre::Result<()> {
    let build_dir = project_dir.join(TARGET_DIR).join(TEST_BUILD_DIR);
    std::fs::create_dir_all(&build_dir).context("could not create build dir")?;

    // Compile src/ modules and get compilation state
    let ErlSources {
        mut erl_paths,
        module_exports,
        module_type_decls,
        all_module_schemes,
        std_mods,
        std_aliases,
        ..
    } = generate_erl_sources(project_dir, &build_dir)?;

    let test_dir = project_dir.join(TEST_DIR);
    if !test_dir.exists() {
        ui::warn("no tests/ directory found");
        return Ok(());
    }

    let test_files = find_mond_files(&test_dir);
    if test_files.is_empty() {
        ui::warn("no test files found in tests/");
        return Ok(());
    }

    // Scan test files to collect their exports and sources
    let mut test_module_sources: Vec<(String, String)> = Vec::new();
    let mut test_module_exports: HashMap<String, Vec<String>> = HashMap::new();

    for test_path in &test_files {
        let module_name = test_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let source = std::fs::read_to_string(test_path)
            .with_context(|| format!("could not read {}", test_path.display()))?;

        let exports = mondc::exported_names(&source);
        test_module_exports.insert(module_name.clone(), exports);
        test_module_sources.push((module_name, source));
    }

    // Combined export map: std + src + test modules
    let mut all_exports = module_exports.clone();
    let std_module_exports: HashMap<String, Vec<String>> = std_mods
        .iter()
        .map(|(u, _, src)| (u.clone(), mondc::exported_names(src)))
        .collect();
    for (k, v) in &std_module_exports {
        all_exports.insert(k.clone(), v.clone());
    }
    for (k, v) in &test_module_exports {
        all_exports.insert(k.clone(), v.clone());
    }

    // Compile each test file
    let mut had_error = false;
    let project = mondc::ProjectAnalysis {
        module_exports: all_exports.clone(),
        module_type_decls: module_type_decls.clone(),
        all_module_schemes: all_module_schemes.clone(),
        std_aliases: std_aliases.clone(),
    };
    // (module_name, Vec<(display_name, erlang_fn_name)>)
    let mut test_fns_by_module: Vec<(String, Vec<(String, String)>)> = Vec::new();

    for (module_name, source) in &test_module_sources {
        let resolved = mondc::resolve_imports_for_source(source, &all_exports, &project);

        let report = mondc::compile_with_imports_report(
            module_name,
            source,
            &format!("tests/{module_name}.mond"),
            resolved.imports,
            &all_exports,
            resolved.module_aliases,
            &resolved.imported_type_decls,
            &resolved.imported_schemes,
        );
        mondc::session::emit_compile_report_with_color(
            &report,
            true,
            ui::diagnostic_color_choice(),
        );
        match report.output {
            Some(erl_src) if !report.has_errors() => {
                let erl_path = build_dir.join(format!("{module_name}.erl"));
                std::fs::write(&erl_path, erl_src)
                    .with_context(|| format!("could not write {}", erl_path.display()))?;
                erl_paths.push(erl_path);
            }
            _ => {
                had_error = true;
            }
        }

        // Discover test declarations from source
        let test_fns = mondc::test_declarations(source);
        if !test_fns.is_empty() {
            test_fns_by_module.push((module_name.clone(), test_fns));
        }
    }

    if had_error {
        return Err(eyre::eyre!(
            "test compilation failed; see diagnostics above"
        ));
    }

    // Compile std modules needed by test files (only those referenced via `use`)
    let mut needed_std: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (_, source) in &test_module_sources {
        for (_, mod_name, _) in mondc::used_modules(source) {
            if std_mods.iter().any(|(u, _, _)| u == &mod_name) {
                needed_std.insert(mod_name);
            }
        }
    }

    let std_analysis =
        mondc::build_project_analysis(&std_mods, &[]).map_err(|err| eyre::eyre!(err))?;
    for (user_name, erlang_name, source) in &std_mods {
        if !needed_std.contains(user_name.as_str()) {
            continue;
        }
        let erl_path = build_dir.join(format!("{erlang_name}.erl"));
        if erl_path.exists() {
            continue; // already compiled by generate_erl_sources
        }

        let resolved =
            mondc::resolve_imports_for_source(source, &std_module_exports, &std_analysis);

        let report = mondc::compile_with_imports_report(
            erlang_name,
            source,
            &format!("{erlang_name}.mond"),
            resolved.imports,
            &std_module_exports,
            std_analysis.std_aliases.clone(),
            &resolved.imported_type_decls,
            &resolved.imported_schemes,
        );
        mondc::session::emit_compile_report_with_color(
            &report,
            true,
            ui::diagnostic_color_choice(),
        );
        if report.has_errors() {
            had_error = true;
            continue;
        }
        if let Some(erl_src) = report.output {
            std::fs::write(&erl_path, erl_src)
                .with_context(|| format!("could not write {}", erl_path.display()))?;
            erl_paths.push(erl_path);
        } else {
            had_error = true;
        }
    }
    if had_error {
        return Err(eyre::eyre!(
            "test compilation failed; see diagnostics above"
        ));
    }

    // Copy any hand-written .erl std helpers needed by test files into the build dir
    let std_dir = crate::build::std_dir();
    for file in std_dir.files() {
        if file.path().extension().and_then(|e| e.to_str()) == Some("erl") {
            let file_name = file.path().file_name().unwrap();
            let dest = build_dir.join(file_name);
            if !dest.exists() {
                std::fs::write(&dest, file.contents())
                    .with_context(|| format!("could not write {}", dest.display()))?;
                erl_paths.push(dest);
            }
        }
    }

    let total: usize = test_fns_by_module.iter().map(|(_, fns)| fns.len()).sum();
    if total == 0 {
        ui::warn("no test declarations found");
        return Ok(());
    }

    // Generate the test runner Erlang module
    let runner_erl = generate_runner(&test_fns_by_module);
    let runner_path = build_dir.join("mond_test_runner.erl");
    std::fs::write(&runner_path, &runner_erl).context("could not write test runner")?;
    erl_paths.push(runner_path);

    // Compile all .erl files
    let erlc = Command::new("erlc")
        .arg("-o")
        .arg(&build_dir)
        .args(&erl_paths)
        .output()
        .context("could not run erlc")?;

    if !erlc.status.success() {
        return Err(eyre::eyre!(
            "erlc failed:\n{}",
            String::from_utf8_lossy(&erlc.stderr)
        ));
    }

    // Run the test runner
    ui::info(&format!(
        "running {total} test{}",
        if total == 1 { "" } else { "s" }
    ));
    let status = Command::new("erl")
        .arg("-noshell")
        .arg("-pz")
        .arg(&build_dir)
        .arg("-eval")
        .arg("mond_test_runner:run().")
        .status()
        .context("could not run erl")?;

    if !status.success() {
        return Err(eyre::eyre!(
            "tests failed with status {}",
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "terminated by signal".to_string())
        ));
    }
    Ok(())
}

fn generate_runner(test_fns_by_module: &[(String, Vec<(String, String)>)]) -> String {
    let mut tests_list = String::new();
    let mut first = true;
    for (module, fns) in test_fns_by_module {
        for (display_name, erlang_fn) in fns {
            if !first {
                tests_list.push_str(",\n        ");
            }
            tests_list.push_str(&format!("{{\"{display_name}\", {module}, {erlang_fn}}}"));
            first = false;
        }
    }

    format!(
        r#"-module(mond_test_runner).
-export([run/0]).

run() ->
    Tests = [
        {tests_list}
    ],
    Results = lists:map(fun({{Name, Mod, Fun}}) ->
        try Mod:Fun(unit) of
            {{ok, _}} ->
                io:format("  ~s ... ok~n", [Name]),
                ok;
            {{error, Msg}} ->
                io:format("  ~s ... FAILED~n    ~s~n", [Name, Msg]),
                failed
        catch
            Class:Reason:Stack ->
                io:format("  ~s ... CRASHED~n    ~p:~p~n    ~p~n", [Name, Class, Reason, Stack]),
                failed
        end
    end, Tests),
    Passed = length(lists:filter(fun(R) -> R =:= ok end, Results)),
    Failed = length(lists:filter(fun(R) -> R =:= failed end, Results)),
    io:format("~ntest result: ~s. ~p passed; ~p failed~n",
              [case Failed of 0 -> "ok"; _ -> "FAILED" end, Passed, Failed]),
    case Failed of
        0 -> erlang:halt(0);
        _ -> erlang:halt(1)
    end.
"#
    )
}
