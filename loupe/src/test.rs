use std::{collections::HashMap, path::Path, process::Command};

use eyre::Context;

use crate::{
    TARGET_DIR, TEST_BUILD_DIR,
    build::{ErlSources, generate_erl_sources},
    utils::find_opal_files,
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
        println!("no tests/ directory found");
        return Ok(());
    }

    let test_files = find_opal_files(&test_dir);
    if test_files.is_empty() {
        println!("no test files found in tests/");
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

        let exports = opalc::exported_names(&source);
        test_module_exports.insert(module_name.clone(), exports);
        test_module_sources.push((module_name, source));
    }

    // Combined export map: std + src + test modules
    let mut all_exports = module_exports.clone();
    let std_module_exports: HashMap<String, Vec<String>> = std_mods
        .iter()
        .map(|(u, _, src)| (u.clone(), opalc::exported_names(src)))
        .collect();
    for (k, v) in &std_module_exports {
        all_exports.insert(k.clone(), v.clone());
    }
    for (k, v) in &test_module_exports {
        all_exports.insert(k.clone(), v.clone());
    }

    // Compile each test file
    let mut had_error = false;
    // (module_name, Vec<(display_name, erlang_fn_name)>)
    let mut test_fns_by_module: Vec<(String, Vec<(String, String)>)> = Vec::new();

    for (module_name, source) in &test_module_sources {
        let mut imports: HashMap<String, String> = HashMap::new();
        let mut imported_schemes: opalc::typecheck::TypeEnv = HashMap::new();

        // Resolve imports: std modules + src modules
        for (_, mod_name, unqualified) in opalc::used_modules(source) {
            let erlang_name = std_mods
                .iter()
                .find(|(user, _, _)| user == &mod_name)
                .map(|(_, erl, _)| erl.clone())
                .unwrap_or_else(|| mod_name.clone());

            if let Some(exports) = all_exports.get(&mod_name) {
                for fn_name in exports {
                    if unqualified.includes(fn_name) {
                        imports.insert(fn_name.clone(), erlang_name.clone());
                    }
                }
            }

            if let Some(mod_schemes) = all_module_schemes.get(&mod_name) {
                for (fn_name, scheme) in mod_schemes {
                    if unqualified.includes(fn_name) {
                        imported_schemes.insert(fn_name.clone(), scheme.clone());
                    }
                    imported_schemes.insert(format!("{mod_name}/{fn_name}"), scheme.clone());
                }
            }
        }

        // Inject qualified-name schemes for all known modules
        for (user_name, schemes) in &all_module_schemes {
            for (fn_name, scheme) in schemes {
                let qualified = format!("{user_name}/{fn_name}");
                imported_schemes
                    .entry(qualified)
                    .or_insert_with(|| scheme.clone());
            }
        }

        let module_aliases: HashMap<String, String> = std_mods
            .iter()
            .map(|(user, erlang, _)| (user.clone(), erlang.clone()))
            .collect();

        // Collect type decls from referenced modules
        let mut referenced_modules: std::collections::HashSet<String> = opalc::used_modules(source)
            .into_iter()
            .map(|(_, mod_name, _)| mod_name)
            .collect();
        for tok in opalc::lexer::Lexer::new(source).lex() {
            if let opalc::lexer::TokenKind::QualifiedIdent((module, _)) = tok.kind {
                referenced_modules.insert(module);
            }
        }
        let imported_type_decls: Vec<opalc::ast::TypeDecl> = referenced_modules
            .iter()
            .flat_map(|mod_name| module_type_decls.get(mod_name).cloned().unwrap_or_default())
            .collect();

        match opalc::compile_with_imports(
            module_name,
            source,
            &format!("tests/{module_name}.opal"),
            imports,
            &all_exports,
            module_aliases,
            &imported_type_decls,
            &imported_schemes,
        ) {
            Some(erl_src) => {
                let erl_path = build_dir.join(format!("{module_name}.erl"));
                std::fs::write(&erl_path, erl_src).expect("could not write .erl");
                erl_paths.push(erl_path);
            }
            None => {
                had_error = true;
            }
        }

        // Discover test declarations from source
        let test_fns = opalc::test_declarations(source);
        if !test_fns.is_empty() {
            test_fns_by_module.push((module_name.clone(), test_fns));
        }
    }

    if had_error {
        std::process::exit(1);
    }

    // Compile std modules needed by test files (any std module referenced via use or qualified ident)
    let std_sub_names: std::collections::HashSet<&str> =
        std_mods.iter().map(|(u, _, _)| u.as_str()).collect();
    let mut needed_std: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (_, source) in &test_module_sources {
        for (_, mod_name, _) in opalc::used_modules(source) {
            if std_sub_names.contains(mod_name.as_str()) {
                needed_std.insert(mod_name);
            }
        }
        for tok in opalc::lexer::Lexer::new(source).lex() {
            if let opalc::lexer::TokenKind::QualifiedIdent((module, _)) = tok.kind
                && std_sub_names.contains(module.as_str())
            {
                needed_std.insert(module);
            }
        }
    }

    for (user_name, erlang_name, source) in &std_mods {
        if !needed_std.contains(user_name.as_str()) {
            continue;
        }
        let erl_path = build_dir.join(format!("{erlang_name}.erl"));
        if erl_path.exists() {
            continue; // already compiled by generate_erl_sources
        }

        let mut std_imports: HashMap<String, String> = HashMap::new();
        let mut std_imported_schemes: opalc::typecheck::TypeEnv = HashMap::new();

        for (_, mod_name, unqualified) in opalc::used_modules(source) {
            let erl_name = std_aliases
                .get(&mod_name)
                .cloned()
                .unwrap_or_else(|| mod_name.clone());
            if let Some(exports) = std_module_exports.get(&mod_name) {
                for fn_name in exports {
                    if unqualified.includes(fn_name) {
                        std_imports.insert(fn_name.clone(), erl_name.clone());
                    }
                }
            }
            if let Some(dep_schemes) = all_module_schemes.get(&mod_name) {
                for (fn_name, scheme) in dep_schemes {
                    if unqualified.includes(fn_name) {
                        std_imported_schemes.insert(fn_name.clone(), scheme.clone());
                    }
                    std_imported_schemes.insert(format!("{mod_name}/{fn_name}"), scheme.clone());
                }
            }
        }

        let std_imported_type_decls: Vec<opalc::ast::TypeDecl> = opalc::used_modules(source)
            .into_iter()
            .flat_map(|(_, mod_name, _)| {
                module_type_decls
                    .get(&mod_name)
                    .cloned()
                    .unwrap_or_default()
            })
            .collect();

        if let Some(erl_src) = opalc::compile_with_imports(
            erlang_name,
            source,
            &format!("{erlang_name}.opal"),
            std_imports,
            &std_module_exports,
            std_aliases.clone(),
            &std_imported_type_decls,
            &std_imported_schemes,
        ) {
            std::fs::write(&erl_path, erl_src).expect("could not write .erl");
            erl_paths.push(erl_path);
        }
    }

    // Copy any hand-written .erl std helpers needed by test files into the build dir
    let std_dir = crate::build::std_dir();
    for file in std_dir.files() {
        if file.path().extension().and_then(|e| e.to_str()) == Some("erl") {
            let file_name = file.path().file_name().unwrap();
            let dest = build_dir.join(file_name);
            if !dest.exists() {
                std::fs::write(&dest, file.contents()).expect("could not write std .erl file");
                erl_paths.push(dest);
            }
        }
    }

    let total: usize = test_fns_by_module.iter().map(|(_, fns)| fns.len()).sum();
    if total == 0 {
        println!("no test declarations found");
        return Ok(());
    }

    // Generate the test runner Erlang module
    let runner_erl = generate_runner(&test_fns_by_module);
    let runner_path = build_dir.join("opal_test_runner.erl");
    std::fs::write(&runner_path, &runner_erl).context("could not write test runner")?;
    erl_paths.push(runner_path);

    // Compile all .erl files
    let erlc = Command::new("erlc")
        .arg("-o")
        .arg(&build_dir)
        .args(&erl_paths)
        .output()
        .unwrap_or_else(|e| {
            eprintln!("error: could not run erlc: {e}");
            std::process::exit(1);
        });

    if !erlc.status.success() {
        eprintln!("erlc failed:");
        eprintln!("{}", String::from_utf8_lossy(&erlc.stderr));
        std::process::exit(1);
    }

    // Run the test runner
    println!("running {total} test{}", if total == 1 { "" } else { "s" });
    let status = Command::new("erl")
        .arg("-noshell")
        .arg("-pz")
        .arg(&build_dir)
        .arg("-eval")
        .arg("opal_test_runner:run().")
        .status()
        .unwrap_or_else(|e| {
            eprintln!("error: could not run erl: {e}");
            std::process::exit(1);
        });

    std::process::exit(status.code().unwrap_or(1));
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
        r#"-module(opal_test_runner).
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
