use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{BIN_ENTRY_POINT, LIB_ROOT, TARGET_DIR, gitignore};
use eyre::Context;

use crate::{DEBUG_BUILD_DIR, ProjectType, SOURCE_DIR, manifest, ui, utils::find_mond_files};

// mond-std is embedded at compile time — std ships with mond,
use include_dir::{Dir, include_dir};
static STD_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../mond-std/src");

pub(crate) fn std_dir() -> &'static Dir<'static> {
    &STD_DIR
}

/// Return `(user_name, erlang_name, source)` for each std module:
///   - user_name:   the name users write in `(use std/io)` → "io"
///   - erlang_name: the compiled Erlang module name → "mond_io"
///     Prefixed with "mond_" to avoid shadowing Erlang/OTP built-in modules.
pub(crate) fn std_modules() -> Vec<(String, String, String)> {
    let mut std_sources: Vec<(String, String)> = STD_DIR
        .files()
        .filter_map(|file| {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("mond") {
                return None;
            }
            let module_name = path.file_stem()?.to_str()?;
            if module_name == "lib" {
                return None;
            }
            Some((module_name.to_string(), file.contents_utf8()?.to_string()))
        })
        .collect();

    if let Some(lib_src) = STD_DIR
        .get_file("lib.mond")
        .and_then(|file| file.contents_utf8())
    {
        std_sources.push(("std".to_string(), lib_src.to_string()));
    }

    mondc::std_modules_from_sources(&std_sources)
        .expect("embedded std modules should form a valid dependency graph")
}

pub(crate) struct ErlSources {
    pub erl_paths: Vec<PathBuf>,
    pub manifest: manifest::MondManifest,
    pub project_type: ProjectType,
    // Compilation state exposed for `mond test`
    pub module_exports: HashMap<String, Vec<String>>,
    pub module_type_decls: HashMap<String, Vec<mondc::ast::TypeDecl>>,
    pub all_module_schemes: HashMap<String, mondc::typecheck::TypeEnv>,
    pub std_mods: Vec<(String, String, String)>,
    pub std_aliases: HashMap<String, String>,
}

/// Compile all Mond source files and write `.erl` output into `erl_dir`.
/// Returns the generated file paths, the project manifest, and detected project type.
pub(crate) fn generate_erl_sources(project_dir: &Path, erl_dir: &Path) -> eyre::Result<ErlSources> {
    let manifest = manifest::read_manifest(project_dir.into())?;

    let src_dir = project_dir.join(SOURCE_DIR);
    let mond_files = find_mond_files(&src_dir);

    if mond_files.is_empty() {
        return Err(eyre::eyre!(
            "mond found no .mond files in {}",
            src_dir.display()
        ));
    }

    let project_type = verify_project_type(&mond_files)
        .ok_or_else(|| eyre::eyre!("mond failed to find one of {BIN_ENTRY_POINT} or {LIB_ROOT}"))?;

    // Phase 1: scan each module's source to collect its exported function names and type decls
    let mut module_exports: HashMap<String, Vec<String>> = HashMap::new();
    let mut module_type_decls: HashMap<String, Vec<mondc::ast::TypeDecl>> = HashMap::new();
    let mut module_sources: Vec<(String, String)> = Vec::new(); // (module_name, source)

    for mond_path in &mond_files {
        let module_name = mond_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let source = std::fs::read_to_string(mond_path)
            .with_context(|| format!("could not read {}", mond_path.display()))?;

        let exports = mondc::exported_names(&source);
        let type_decls = mondc::exported_type_decls(&source);
        module_exports.insert(module_name.clone(), exports);
        module_type_decls.insert(module_name.clone(), type_decls);
        module_sources.push((module_name, source));
    }
    let ordered_module_sources =
        mondc::ordered_module_sources(&module_sources).map_err(|err| eyre::eyre!(err))?;

    // Phase 1b: seed module_exports with embedded std modules so the compiler's
    // `use` validation and import building treats them identically to local modules.
    let std_mods = std_modules();
    let analysis = mondc::build_project_analysis(&std_mods, &module_sources)
        .map_err(|err| eyre::eyre!(err))?;
    module_exports = analysis.module_exports.clone();
    module_type_decls = analysis.module_type_decls.clone();
    let all_module_schemes = analysis.all_module_schemes.clone();
    let std_aliases = analysis.std_aliases.clone();

    // Phase 2: compile each user file with its resolved import map
    let mut erl_paths: Vec<PathBuf> = Vec::new();
    let mut had_error = false;
    for (module_name, source) in &ordered_module_sources {
        let resolved = mondc::resolve_imports_for_source(source, &module_exports, &analysis);

        let report = mondc::compile_with_imports_report(
            module_name,
            source,
            &format!("{module_name}.mond"),
            resolved.imports,
            &module_exports,
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
                let erl_path = erl_dir.join(format!("{module_name}.erl"));
                std::fs::write(&erl_path, erl_src)
                    .with_context(|| format!("could not write {}", erl_path.display()))?;
                erl_paths.push(erl_path);
            }
            _ => {
                had_error = true;
            }
        }
    }

    if had_error {
        return Err(eyre::eyre!("compilation failed; see diagnostics above"));
    }

    validate_bin_entrypoint(&project_type, &module_sources)?;

    // Compile only std modules that are actually used
    let used_std_names: std::collections::HashSet<String> = ordered_module_sources
        .iter()
        .flat_map(|(_, src)| mondc::used_modules(src))
        .map(|(_, m, _)| m)
        .collect();

    let std_analysis =
        mondc::build_project_analysis(&std_mods, &[]).map_err(|err| eyre::eyre!(err))?;
    let std_module_exports: HashMap<String, Vec<String>> = std_mods
        .iter()
        .map(|(user_name, _, _)| {
            (
                user_name.clone(),
                std_analysis
                    .module_exports
                    .get(user_name)
                    .cloned()
                    .unwrap_or_default(),
            )
        })
        .collect();

    for (user_name, erlang_name, source) in &std_mods {
        if !used_std_names.contains(user_name.as_str()) {
            continue;
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
        match report.output {
            Some(erl_src) if !report.has_errors() => {
                let erl_path = erl_dir.join(format!("{erlang_name}.erl"));
                std::fs::write(&erl_path, erl_src)
                    .with_context(|| format!("could not write {}", erl_path.display()))?;
                erl_paths.push(erl_path);
            }
            None => {
                had_error = true;
            }
            Some(_) => {
                had_error = true;
            }
        }
    }
    if had_error {
        return Err(eyre::eyre!("compilation failed; see diagnostics above"));
    }

    // Copy any hand-written .erl files from mond-std/src/ into the build dir.
    // These are embedded alongside the .mond sources via include_dir! and are
    // written verbatim — useful for helpers that are awkward to express in Mond
    // (e.g. functions that return Erlang atoms like `nomatch`).
    for file in STD_DIR.files() {
        if file.path().extension().and_then(|e| e.to_str()) == Some("erl") {
            let file_name = file.path().file_name().unwrap();
            let dest = erl_dir.join(file_name);
            std::fs::write(&dest, file.contents())
                .with_context(|| format!("could not write {}", dest.display()))?;
            erl_paths.push(dest);
        }
    }

    Ok(ErlSources {
        erl_paths,
        manifest,
        project_type,
        module_exports,
        module_type_decls,
        all_module_schemes,
        std_mods,
        std_aliases,
    })
}

fn validate_bin_entrypoint(
    project_type: &ProjectType,
    module_sources: &[(String, String)],
) -> eyre::Result<()> {
    if !matches!(project_type, ProjectType::Bin) {
        return Ok(());
    }
    let Some((_, main_source)) = module_sources
        .iter()
        .find(|(module_name, _)| module_name == "main")
    else {
        return Err(eyre::eyre!(
            "binary projects must include src/{BIN_ENTRY_POINT}"
        ));
    };
    if !mondc::has_nullary_main(main_source) {
        return Err(eyre::eyre!(
            "src/{BIN_ENTRY_POINT} must define a top-level nullary entrypoint: `(let main {{}} ...)`"
        ));
    }
    Ok(())
}

pub(crate) fn build(project_dir: &Path, run: bool) -> eyre::Result<()> {
    let build_dir = project_dir.join(TARGET_DIR).join(DEBUG_BUILD_DIR);
    std::fs::create_dir_all(&build_dir)
        .context(format!("could not create {DEBUG_BUILD_DIR} dir"))?;
    gitignore::write_gitignore(project_dir.into())?;

    let ErlSources {
        erl_paths,
        manifest,
        project_type,
        ..
    } = generate_erl_sources(project_dir, &build_dir)?;

    if matches!(project_type, ProjectType::Lib) && run {
        return Err(eyre::eyre!("mond cannot run a library project"));
    }

    // Run erlc on all .erl files at once
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
    if run {
        let status = Command::new("erl")
            .arg("-noinput")
            .arg("-pa")
            .arg(&build_dir)
            .arg("-eval")
            .arg("main:main(unit), init:stop().")
            .status()
            .context("could not run erl")?;

        if !status.success() {
            return Err(eyre::eyre!(
                "program exited with status {}",
                status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "terminated by signal".to_string())
            ));
        }
    } else {
        ui::success(&format!(
            "built {} ({} module(s))",
            manifest.package.name,
            erl_paths.len()
        ));
    }

    Ok(())
}

fn verify_project_type(source_files: &[PathBuf]) -> Option<ProjectType> {
    let has_root_bin = source_files.iter().any(|file| {
        file.file_name().and_then(|n| n.to_str()) == Some(BIN_ENTRY_POINT)
            && file
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .map(|n| n == SOURCE_DIR)
                .unwrap_or(false)
    });
    let has_root_lib = source_files.iter().any(|file| {
        file.file_name().and_then(|n| n.to_str()) == Some(LIB_ROOT)
            && file
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .map(|n| n == SOURCE_DIR)
                .unwrap_or(false)
    });

    if has_root_bin {
        Some(ProjectType::Bin)
    } else if has_root_lib {
        Some(ProjectType::Lib)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_root() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!("mond-build-test-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn validate_bin_entrypoint_accepts_nullary_main() {
        let modules = vec![("main".to_string(), "(let main {} 0)".to_string())];
        let result = validate_bin_entrypoint(&ProjectType::Bin, &modules);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_bin_entrypoint_rejects_missing_main_function() {
        let modules = vec![("main".to_string(), "(let helper {} 0)".to_string())];
        let result = validate_bin_entrypoint(&ProjectType::Bin, &modules);
        assert!(result.is_err());
    }

    #[test]
    fn validate_bin_entrypoint_does_not_mask_malformed_main_during_compilation() {
        let root = unique_temp_root();
        std::fs::create_dir_all(&root).expect("create temp root");

        let project_dir = root.join("app");
        std::fs::create_dir_all(project_dir.join("src")).expect("create src dir");
        let manifest = crate::manifest::create_new_manifest("app".to_string());
        crate::manifest::write_manifest(&manifest, &project_dir.join(crate::MANIFEST_NAME))
            .expect("write manifest");
        std::fs::write(
            project_dir.join("src").join(crate::BIN_ENTRY_POINT),
            r#"(use std/io)
(use std/list)

(let some_list {} [1 2 3])
(let main {} (io/debug (list/map fn {x} (+ x 1) (some_list))))
"#,
        )
        .expect("write main");

        let err = match generate_erl_sources(&project_dir, &project_dir.join("target/test-build")) {
            Ok(_) => panic!("malformed main should fail compilation first"),
            Err(err) => err,
        };
        let message = err.to_string();
        assert!(
            message.contains("compilation failed; see diagnostics above"),
            "unexpected error: {message}"
        );
        assert!(
            !message.contains("must define a top-level nullary entrypoint"),
            "entrypoint validation masked compile error: {message}"
        );

        std::fs::remove_dir_all(&root).expect("cleanup temp root");
    }

    #[test]
    fn validate_bin_entrypoint_ignored_for_lib_projects() {
        let modules = vec![("main".to_string(), "(let helper {} 0)".to_string())];
        let result = validate_bin_entrypoint(&ProjectType::Lib, &modules);
        assert!(result.is_ok());
    }

    #[test]
    fn verify_project_type_prefers_root_src_files() {
        let files = vec![
            PathBuf::from("/tmp/proj/src/lib.mond"),
            PathBuf::from("/tmp/proj/src/nested/main.mond"),
        ];
        let ty = verify_project_type(&files);
        assert!(matches!(ty, Some(ProjectType::Lib)));
    }

    #[test]
    fn ordered_user_modules_respects_dependencies() {
        let modules = vec![
            (
                "main".to_string(),
                "(use util)\n(let main {} (util_fn))".to_string(),
            ),
            ("util".to_string(), "(let util_fn {} 1)".to_string()),
            ("other".to_string(), "(let other {} 2)".to_string()),
        ];
        let ordered = mondc::ordered_module_sources(&modules).expect("topo order");
        let names: Vec<String> = ordered.into_iter().map(|(n, _)| n).collect();
        let pos_main = names
            .iter()
            .position(|n| n == "main")
            .expect("main present");
        let pos_util = names
            .iter()
            .position(|n| n == "util")
            .expect("util present");
        assert!(pos_util < pos_main, "dependency must come first: {names:?}");
    }

    #[test]
    fn ordered_user_modules_rejects_cycles() {
        let modules = vec![
            ("a".to_string(), "(use b)\n(let a {} 1)".to_string()),
            ("b".to_string(), "(use a)\n(let b {} 2)".to_string()),
        ];
        let err = mondc::ordered_module_sources(&modules).expect_err("expected cycle error");
        let msg = err.to_string();
        assert!(
            msg.contains("cyclic module dependency detected"),
            "unexpected error: {msg}"
        );
        assert!(msg.contains("a -> b -> a") || msg.contains("b -> a -> b"));
    }

    #[test]
    fn std_modules_from_sources_discovers_files_without_lib_reexports() {
        let modules = vec![
            ("io".to_string(), "(let println {x} x)".to_string()),
            ("extra".to_string(), "(let helper {} 1)".to_string()),
            ("std".to_string(), "(let hello {} 1)".to_string()),
        ];

        let discovered = mondc::std_modules_from_sources(&modules).expect("std modules");
        let names: Vec<String> = discovered.into_iter().map(|(name, _, _)| name).collect();

        assert!(names.contains(&"io".to_string()));
        assert!(names.contains(&"extra".to_string()));
        assert!(names.contains(&"std".to_string()));
    }
}
