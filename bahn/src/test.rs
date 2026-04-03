use std::{collections::HashMap, path::Path, process::Command, sync::Arc};

use eyre::Context;

use crate::{
    TARGET_DIR, TEST_BUILD_DIR, TEST_DIR,
    build::{
        ErlSources, dependency_external_modules, dependency_source_label,
        ensure_dependency_module_closure_complete, generate_erl_sources_with_roots,
        reachable_dependency_modules,
    },
    compile_flow, manifest, ui,
    utils::find_mond_files,
};

const ERL_SOURCE_SUBDIR: &str = "erl";
const ERL_BEAM_SUBDIR: &str = "ebin";

fn sanitize_erlang_component(name: &str) -> String {
    let mut sanitized = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            sanitized.push(ch.to_ascii_lowercase());
        } else {
            sanitized.push('_');
        }
    }
    if sanitized.is_empty() || sanitized.starts_with(|c: char| c.is_ascii_digit()) {
        sanitized.insert(0, '_');
    }
    sanitized
}

fn prepare_test_build_dir(build_dir: &Path) -> eyre::Result<()> {
    if build_dir.exists() {
        std::fs::remove_dir_all(build_dir).context("could not clean test build dir")?;
    }
    std::fs::create_dir_all(build_dir).context("could not create build dir")?;
    Ok(())
}

pub(crate) async fn test(project_dir: &Path) -> eyre::Result<()> {
    let build_dir = project_dir.join(TARGET_DIR).join(TEST_BUILD_DIR);
    prepare_test_build_dir(&build_dir)?;
    let erl_dir = build_dir.join(ERL_SOURCE_SUBDIR);
    let ebin_dir = build_dir.join(ERL_BEAM_SUBDIR);
    std::fs::create_dir_all(&erl_dir).context("could not create test erl dir")?;
    std::fs::create_dir_all(&ebin_dir).context("could not create test ebin dir")?;

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

    let src_dir = project_dir.join(crate::SOURCE_DIR);
    let src_module_names: std::collections::HashSet<String> = find_mond_files(&src_dir)
        .into_iter()
        .filter_map(|path| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_string)
        })
        .collect();

    // Scan test files to collect their exports and sources
    let mut test_module_sources: Vec<(String, String)> = Vec::new();
    let mut test_module_exports: HashMap<String, Vec<String>> = HashMap::new();
    let mut seen_test_modules: HashMap<String, std::path::PathBuf> = HashMap::new();
    let mut extra_src_roots: std::collections::HashSet<String> = std::collections::HashSet::new();

    for test_path in &test_files {
        let module_name = test_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        if let Some(first_path) = seen_test_modules.insert(module_name.clone(), test_path.clone()) {
            return Err(eyre::eyre!(
                "test module name collision: `{module_name}` appears in both {} and {}",
                first_path.display(),
                test_path.display()
            ));
        }

        let source = std::fs::read_to_string(test_path)
            .with_context(|| format!("could not read {}", test_path.display()))?;

        let exports = mondc::exported_names(&source);
        test_module_exports.insert(module_name.clone(), exports);
        for module_name in mondc::referenced_modules(&source) {
            if src_module_names.contains(&module_name) {
                extra_src_roots.insert(module_name);
            }
        }
        test_module_sources.push((module_name, source));
    }

    let manifest = manifest::read_manifest(project_dir.into())?;

    // Compile src/ modules and get compilation state.
    let ErlSources {
        mut erl_paths,
        manifest,
        module_exports,
        module_type_decls,
        module_extern_types,
        all_module_schemes,
        dependency_mods,
        module_aliases,
        ..
    } = generate_erl_sources_with_roots(
        manifest,
        project_dir,
        &erl_dir,
        &extra_src_roots.into_iter().collect::<Vec<_>>(),
    )
    .await?;

    // Combined export map: dependencies + src + test modules
    let mut all_exports = module_exports.clone();
    let dependency_module_exports = compile_flow::dependency_module_exports(&dependency_mods);
    for (k, v) in &dependency_module_exports {
        all_exports.entry(k.clone()).or_insert_with(|| v.clone());
    }
    for (k, v) in &test_module_exports {
        all_exports.insert(k.clone(), v.clone());
    }

    // Compile each test file
    let project = Arc::new(mondc::ProjectAnalysis {
        module_exports: all_exports.clone(),
        module_type_decls: module_type_decls.clone(),
        module_all_type_decls: module_type_decls.clone(),
        module_private_record_types: HashMap::new(),
        module_extern_types: module_extern_types.clone(),
        all_module_schemes: all_module_schemes.clone(),
        module_aliases: module_aliases.clone(),
    });
    // (module_name, Vec<(display_name, erlang_fn_name)>)
    let mut test_fns_by_module: Vec<(String, Vec<(String, String)>)> = Vec::new();
    let test_compile_units: Vec<compile_flow::CompileUnit<'_>> = test_module_sources
        .iter()
        .map(|(module_name, source)| compile_flow::CompileUnit {
            output_module_name: module_name.as_str(),
            source,
            source_label: format!("tests/{module_name}.mond"),
        })
        .collect();
    let (test_outputs, test_had_error) = compile_flow::compile_units(
        &test_compile_units,
        Arc::clone(&project),
        true,
        mondc::CompileTarget::Dev,
    )
    .await;
    for output in test_outputs {
        let Some(erl_source) = output.erl_source() else {
            continue;
        };
        let erl_path = erl_dir.join(format!("{}.erl", output.output_module_name));
        if erl_path.exists() {
            return Err(eyre::eyre!(
                "Erlang module name collision: tests/{}.mond would overwrite {}",
                output.output_module_name,
                erl_path.display()
            ));
        }
        erl_paths.push(compile_flow::write_erl_output(
            &erl_dir,
            &output.output_module_name,
            erl_source,
        )?);
    }

    for (module_name, source) in &test_module_sources {
        let test_fns = mondc::test_declarations(source);
        if !test_fns.is_empty() {
            test_fns_by_module.push((module_name.clone(), test_fns));
        }
    }

    if test_had_error {
        return Err(eyre::eyre!(
            "test compilation failed; see diagnostics above"
        ));
    }

    // Compile dependency modules needed by test files (only those referenced via `use`)
    let mut needed_dependencies: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for (_, source) in &test_module_sources {
        for (_, mod_name, _) in mondc::used_modules(source) {
            if dependency_mods
                .iter()
                .any(|module| module.module_name == mod_name)
                && !src_module_names.contains(&mod_name)
            {
                needed_dependencies.insert(mod_name);
            }
        }
    }
    let selected_test_dependency_mods =
        reachable_dependency_modules(&dependency_mods, &needed_dependencies)?;
    let known_dependency_names: std::collections::HashSet<String> = dependency_mods
        .iter()
        .map(|module| module.module_name.clone())
        .collect();
    ensure_dependency_module_closure_complete(
        &selected_test_dependency_mods,
        &known_dependency_names,
        &src_module_names,
    )?;

    let dependency_external_mods = dependency_external_modules(&dependency_mods);
    let dependency_analysis = Arc::new(
        mondc::build_project_analysis(&dependency_external_mods, &[])
            .map_err(|err| eyre::eyre!(err))?,
    );
    let dependency_compile_units: Vec<compile_flow::CompileUnit<'_>> =
        selected_test_dependency_mods
            .iter()
            .map(|module| compile_flow::CompileUnit {
                output_module_name: module.erlang_name.as_str(),
                source: &module.source,
                source_label: dependency_source_label(module),
            })
            .collect();
    let (dependency_outputs, dependency_had_error) = compile_flow::compile_units(
        &dependency_compile_units,
        Arc::clone(&dependency_analysis),
        true,
        mondc::CompileTarget::Dev,
    )
    .await;
    for (module, output) in selected_test_dependency_mods
        .iter()
        .zip(dependency_outputs.into_iter())
    {
        debug_assert_eq!(output.output_module_name, module.erlang_name);
        let Some(erl_source) = output.erl_source() else {
            continue;
        };
        let erl_path = erl_dir.join(format!("{}.erl", output.output_module_name));
        if erl_path.exists() {
            if erl_paths.iter().any(|p| p == &erl_path) {
                continue; // already compiled by generate_erl_sources
            }
            return Err(eyre::eyre!(
                "Erlang module name collision: dependency module `{}` would overwrite {}",
                module.module_name,
                erl_path.display()
            ));
        }
        erl_paths.push(compile_flow::write_erl_output(
            &erl_dir,
            &output.output_module_name,
            erl_source,
        )?);
    }
    if dependency_had_error {
        return Err(eyre::eyre!(
            "test compilation failed; see diagnostics above"
        ));
    }

    let total: usize = test_fns_by_module.iter().map(|(_, fns)| fns.len()).sum();
    if total == 0 {
        ui::warn("no test declarations found");
        return Ok(());
    }

    // Generate the test runner Erlang module
    let runner_module = format!(
        "i_{}_test_runner",
        sanitize_erlang_component(&manifest.package.name)
    );
    let runner_erl = generate_runner(&runner_module, &test_fns_by_module);
    let runner_path = erl_dir.join(format!("{runner_module}.erl"));
    if runner_path.exists() {
        return Err(eyre::eyre!(
            "Erlang module name collision: generated test runner would overwrite {}",
            runner_path.display()
        ));
    }
    std::fs::write(&runner_path, &runner_erl).context("could not write test runner")?;
    erl_paths.push(runner_path);

    crate::utils::verify_erlc_installed()?;

    // Compile all .erl files
    let erlc = {
        let ebin_dir = ebin_dir.clone();
        let erl_paths = erl_paths.clone();
        tokio::task::spawn_blocking(move || {
            Command::new("erlc")
                .arg("-o")
                .arg(&ebin_dir)
                .args(&erl_paths)
                .output()
                .context("could not run erlc")
        })
        .await
        .map_err(|err| eyre::eyre!("failed to join erlc task: {err}"))??
    };

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
    let status = {
        let ebin_dir = ebin_dir.clone();
        tokio::task::spawn_blocking(move || {
            Command::new("erl")
                .arg("-noshell")
                .arg("-pz")
                .arg(&ebin_dir)
                .arg("-eval")
                .arg(format!("{runner_module}:run()."))
                .status()
                .context("could not run erl")
        })
        .await
        .map_err(|err| eyre::eyre!("failed to join erl task: {err}"))??
    };

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

fn generate_runner(
    runner_module: &str,
    test_fns_by_module: &[(String, Vec<(String, String)>)],
) -> String {
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
        r#"-module({runner_module}).
-export([run/0]).

run() ->
    Green = "\e[32m",
    Red = "\e[31m",
    Reset = "\e[0m",
    Tests = [
        {tests_list}
    ],
    % Keep deterministic execution order to avoid flakiness from shared state.
    Results = lists:map(fun({{Name, Mod, Fun}}) ->
        try Mod:Fun(unit) of
            {{ok, _}} ->
                io:format("  ~s ... ~sok~s~n", [Name, Green, Reset]),
                ok;
            {{error, Msg}} ->
                io:format("  ~s ... ~sFAILED~s~n    ~s~s~s~n", [Name, Red, Reset, Red, Msg, Reset]),
                failed
        catch
            Class:Reason:Stack ->
                io:format("  ~s ... ~sCRASHED~s~n    ~s~p:~p~s~n    ~s~p~s~n",
                          [Name, Red, Reset, Red, Class, Reason, Reset, Red, Stack, Reset]),
                failed
        end
    end, Tests),
    Passed = length(lists:filter(fun(R) -> R =:= ok end, Results)),
    Failed = length(lists:filter(fun(R) -> R =:= failed end, Results)),
    Summary = case Failed of 0 -> {{Green, "ok"}}; _ -> {{Red, "FAILED"}} end,
    {{SummaryColor, SummaryText}} = Summary,
    io:format("~ntest result: ~s~s~s. ~p passed; ~p failed~n",
              [SummaryColor, SummaryText, Reset, Passed, Failed]),
    case Failed of
        0 -> erlang:halt(0);
        _ -> erlang:halt(1)
    end.
"#
    )
}

#[cfg(test)]
mod tests {
    use super::prepare_test_build_dir;
    use std::{
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn unique_temp_root() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "mond-test-build-clean-{}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn prepare_test_build_dir_removes_stale_artifacts() {
        let root = unique_temp_root();
        let build_dir = root.join("target/tests");
        std::fs::create_dir_all(&build_dir).expect("create build dir");
        std::fs::write(build_dir.join("stale.erl"), "stale").expect("write stale artifact");

        prepare_test_build_dir(&build_dir).expect("prepare build dir");

        assert!(
            build_dir.exists(),
            "build dir should exist after preparing: {}",
            build_dir.display()
        );
        assert!(
            !build_dir.join("stale.erl").exists(),
            "stale artifacts should be removed"
        );

        std::fs::remove_dir_all(&root).expect("cleanup");
    }
}
