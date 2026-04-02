use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use crate::{BIN_ENTRY_POINT, LIB_ROOT, TARGET_DIR, manifest::BahnManifest};
use eyre::Context;
use semver::Version;
use walkdir::WalkDir;

use crate::{
    DEBUG_BUILD_DIR, ProjectType, SOURCE_DIR, compile_flow, deps, manifest, ui,
    utils::find_mond_files,
};

const ERL_SOURCE_SUBDIR: &str = "erl";
const ERL_BEAM_SUBDIR: &str = "ebin";

fn local_module_prefix(package_name: &str) -> String {
    let mut sanitized = String::with_capacity(package_name.len());
    for ch in package_name.chars() {
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

fn local_erlang_alias(package_name: &str, module_name: &str) -> String {
    format!("p_{}_{}", local_module_prefix(package_name), module_name)
}

fn apply_local_module_aliases(
    analysis: &mut mondc::ProjectAnalysis,
    module_sources: &[(String, String)],
    package_name: &str,
) {
    for (module_name, _) in module_sources {
        analysis.module_aliases.insert(
            module_name.clone(),
            local_erlang_alias(package_name, module_name),
        );
    }

    if package_name != "lib"
        && let Some(lib_alias) = analysis.module_aliases.get("lib").cloned()
    {
        analysis
            .module_aliases
            .insert(package_name.to_string(), lib_alias);
    }
}

fn register_erl_output_name(
    seen: &mut HashMap<String, String>,
    erl_module_name: &str,
    origin: String,
) -> eyre::Result<()> {
    if let Some(existing_origin) = seen.get(erl_module_name) {
        if existing_origin != &origin {
            return Err(eyre::eyre!(
                "Erlang module name collision: `{erl_module_name}` is produced by `{existing_origin}` and `{origin}`"
            ));
        }
        return Ok(());
    }
    seen.insert(erl_module_name.to_string(), origin);
    Ok(())
}

fn ensure_no_erl_output_collisions(
    ordered_module_sources: &[(String, String)],
    module_aliases: &HashMap<String, String>,
    dependency_mods: &[mondc::DependencyModuleSource],
    used_dependency_names: &HashSet<String>,
    dependency_helper_erls: &[deps::HelperErlFile],
    local_helper_erls: &[deps::HelperErlFile],
) -> eyre::Result<()> {
    let mut seen: HashMap<String, String> = HashMap::new();

    for (module_name, _) in ordered_module_sources {
        let erlang_name = module_aliases
            .get(module_name.as_str())
            .map(String::as_str)
            .unwrap_or(module_name);
        register_erl_output_name(&mut seen, erlang_name, format!("src/{module_name}.mond"))?;
    }

    for module in dependency_mods {
        if !used_dependency_names.contains(module.module_name.as_str()) {
            continue;
        }
        register_erl_output_name(
            &mut seen,
            &module.erlang_name,
            format!(
                "dependency module `deps/{}/{}`",
                module.package_name, module.source_relpath
            ),
        )?;
    }

    for helper in dependency_helper_erls {
        register_erl_output_name(
            &mut seen,
            &helper.module_name,
            format!("dependency helper `{}`", helper.file_name),
        )?;
    }

    for helper in local_helper_erls {
        register_erl_output_name(
            &mut seen,
            &helper.module_name,
            format!("local helper `{}`", helper.file_name),
        )?;
    }

    Ok(())
}

fn ensure_no_local_dependency_module_conflicts(
    module_sources: &[(String, String)],
    dependency_mods: &[mondc::DependencyModuleSource],
) -> eyre::Result<()> {
    let local_module_names: HashSet<String> = module_sources
        .iter()
        .map(|(module_name, _)| module_name.clone())
        .collect();
    let mut conflicts: Vec<String> = dependency_mods
        .iter()
        .map(|module| module.module_name.clone())
        .filter(|module_name| local_module_names.contains(module_name))
        .collect();
    conflicts.sort();
    conflicts.dedup();
    if conflicts.is_empty() {
        return Ok(());
    }
    Err(eyre::eyre!(
        "module name conflict between src/ and dependencies: {}. Rename the local module(s) or dependency module(s) to avoid ambiguous imports",
        conflicts.join(", ")
    ))
}

fn collect_local_helper_erls(src_dir: &Path) -> eyre::Result<Vec<deps::HelperErlFile>> {
    let mut helper_erls: Vec<deps::HelperErlFile> = Vec::new();
    for entry in WalkDir::new(src_dir)
        .into_iter()
        .filter_map(|entry| entry.ok())
    {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("erl") {
            continue;
        }
        let file_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| eyre::eyre!("invalid helper file name at {}", path.display()))?
            .to_string();
        let file_stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| eyre::eyre!("invalid helper module name at {}", path.display()))?
            .to_string();
        let contents =
            std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
        let module_name = deps::parse_erlang_module_name(&contents).unwrap_or(file_stem);
        helper_erls.push(deps::HelperErlFile {
            file_name,
            module_name,
            contents,
        });
    }
    helper_erls.sort_by(|a, b| a.file_name.cmp(&b.file_name));
    Ok(helper_erls)
}

pub(crate) struct ErlSources {
    pub erl_paths: Vec<PathBuf>,
    pub manifest: manifest::BahnManifest,
    pub project_type: ProjectType,
    // Compilation state exposed for `bahn test`
    pub module_exports: HashMap<String, Vec<String>>,
    pub module_type_decls: HashMap<String, Vec<mondc::ast::TypeDecl>>,
    pub module_extern_types: HashMap<String, Vec<String>>,
    pub all_module_schemes: HashMap<String, mondc::typecheck::TypeEnv>,
    pub dependency_mods: Vec<mondc::DependencyModuleSource>,
    pub module_aliases: HashMap<String, String>,
}

pub(crate) fn dependency_external_modules(
    dependency_mods: &[mondc::DependencyModuleSource],
) -> Vec<(String, String, String)> {
    dependency_mods
        .iter()
        .map(mondc::DependencyModuleSource::as_external_module)
        .collect()
}

pub(crate) fn dependency_source_label(module: &mondc::DependencyModuleSource) -> String {
    format!("deps/{}/{}", module.package_name, module.source_relpath)
}

fn entry_module_names(project_type: &ProjectType) -> Vec<String> {
    match project_type {
        ProjectType::Bin => vec!["main".to_string()],
        ProjectType::Lib => vec!["lib".to_string()],
    }
}

fn reachable_src_modules(
    project_type: &ProjectType,
    module_sources: &[(String, String)],
    extra_roots: &[String],
) -> eyre::Result<Vec<(String, String)>> {
    if matches!(project_type, ProjectType::Lib) {
        return Ok(module_sources.to_vec());
    }
    let mut roots = entry_module_names(project_type);
    for root in extra_roots {
        if !roots.contains(root) {
            roots.push(root.clone());
        }
    }
    mondc::reachable_module_sources(module_sources, &roots).map_err(|err| eyre::eyre!(err))
}

pub(crate) fn reachable_dependency_modules(
    dependency_mods: &[mondc::DependencyModuleSource],
    roots: &HashSet<String>,
) -> eyre::Result<Vec<mondc::DependencyModuleSource>> {
    if roots.is_empty() {
        return Ok(Vec::new());
    }

    let dep_sources: Vec<(String, String)> = dependency_mods
        .iter()
        .map(|module| (module.module_name.clone(), module.source.clone()))
        .collect();
    let root_list: Vec<String> = roots.iter().cloned().collect();
    let reachable = mondc::reachable_module_sources(&dep_sources, &root_list)
        .map_err(|err| eyre::eyre!(err))?;
    let modules_by_name: HashMap<String, mondc::DependencyModuleSource> = dependency_mods
        .iter()
        .map(|module| (module.module_name.clone(), module.clone()))
        .collect();

    reachable
        .into_iter()
        .map(|(module_name, _)| {
            modules_by_name.get(&module_name).cloned().ok_or_else(|| {
                eyre::eyre!("internal error: missing dependency module `{module_name}`")
            })
        })
        .collect::<eyre::Result<Vec<_>>>()
}

/// Compile all Mond source files and write `.erl` output into `erl_dir`.
/// Returns the generated file paths, the project manifest, and detected project type.
pub(crate) async fn generate_erl_sources(
    manifest: BahnManifest,
    project_dir: &Path,
    erl_dir: &Path,
) -> eyre::Result<ErlSources> {
    generate_erl_sources_for_target(manifest, project_dir, erl_dir, mondc::CompileTarget::Dev).await
}

pub(crate) async fn generate_erl_sources_for_target(
    manifest: BahnManifest,
    project_dir: &Path,
    erl_dir: &Path,
    compile_target: mondc::CompileTarget,
) -> eyre::Result<ErlSources> {
    let mond_version = Version::parse(crate::VERSION).context("Failed to parse mond version no")?;
    if let Some(min_mond_version) = &manifest.package.min_mond_version
        && &mond_version < min_mond_version
    {
        return Err(eyre::eyre!(format!(
            "Cannot build current package. Package has a min mond version of {min_mond_version} but the current mond version is {mond_version}"
        )));
    }

    generate_erl_sources_with_roots_for_target(manifest, project_dir, erl_dir, &[], compile_target)
        .await
}

pub(crate) async fn generate_erl_sources_with_roots(
    manifest: BahnManifest,
    project_dir: &Path,
    erl_dir: &Path,
    extra_roots: &[String],
) -> eyre::Result<ErlSources> {
    generate_erl_sources_with_roots_for_target(
        manifest,
        project_dir,
        erl_dir,
        extra_roots,
        mondc::CompileTarget::Dev,
    )
    .await
}

pub(crate) async fn generate_erl_sources_with_roots_for_target(
    manifest: BahnManifest,
    project_dir: &Path,
    erl_dir: &Path,
    extra_roots: &[String],
    compile_target: mondc::CompileTarget,
) -> eyre::Result<ErlSources> {
    let loaded_dependencies = deps::load_dependencies(project_dir, &manifest)?;
    let dependency_mods = loaded_dependencies.modules.clone();

    let src_dir = project_dir.join(SOURCE_DIR);
    // Collect any local Erlang helper files.
    let local_helper_erls = collect_local_helper_erls(&src_dir)?;
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
    let mut module_extern_types: HashMap<String, Vec<String>> = HashMap::new();
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
        let extern_types = mondc::exported_extern_types(&source);
        module_exports.insert(module_name.clone(), exports);
        module_type_decls.insert(module_name.clone(), type_decls);
        module_extern_types.insert(module_name.clone(), extern_types);
        module_sources.push((module_name, source));
    }
    ensure_no_local_dependency_module_conflicts(&module_sources, &dependency_mods)?;
    let ordered_module_sources =
        mondc::ordered_module_sources(&module_sources).map_err(|err| eyre::eyre!(err))?;
    let dependency_external_mods = dependency_external_modules(&dependency_mods);

    // Phase 1b: seed module_exports with dependency modules provided in manifest deps.
    let mut analysis = mondc::build_project_analysis_with_modules_and_package(
        &dependency_external_mods,
        &module_sources,
        Some(&manifest.package.name),
    )
    .map_err(|err| eyre::eyre!(err))?;
    apply_local_module_aliases(&mut analysis, &module_sources, &manifest.package.name);
    let analysis = Arc::new(analysis);
    module_exports = analysis.module_exports.clone();
    module_type_decls = analysis.module_type_decls.clone();
    module_extern_types = analysis.module_extern_types.clone();
    let all_module_schemes = analysis.all_module_schemes.clone();
    let module_aliases = analysis.module_aliases.clone();
    let selected_module_sources =
        reachable_src_modules(&project_type, &ordered_module_sources, extra_roots)?;

    // Compile only dependency modules that are actually used.
    let local_module_names: HashSet<String> = selected_module_sources
        .iter()
        .map(|(module_name, _)| module_name.clone())
        .collect();
    let known_dependency_names: HashSet<String> = dependency_mods
        .iter()
        .map(|module| module.module_name.clone())
        .collect();
    let direct_dependency_roots: HashSet<String> = selected_module_sources
        .iter()
        .flat_map(|(_, src)| mondc::used_modules(src))
        .map(|(_, m, _)| m)
        .filter(|module_name| {
            known_dependency_names.contains(module_name)
                && !local_module_names.contains(module_name)
        })
        .collect();
    let selected_dependency_mods =
        reachable_dependency_modules(&dependency_mods, &direct_dependency_roots)?;
    let used_dependency_names: HashSet<String> = selected_dependency_mods
        .iter()
        .map(|module| module.module_name.clone())
        .collect();
    ensure_no_erl_output_collisions(
        &selected_module_sources,
        &module_aliases,
        &dependency_mods,
        &used_dependency_names,
        &loaded_dependencies.helper_erls,
        &local_helper_erls,
    )?;

    // Phase 2: compile each user file with its resolved import map
    let mut erl_paths: Vec<PathBuf> = Vec::new();
    let source_compile_units: Vec<compile_flow::CompileUnit<'_>> = selected_module_sources
        .iter()
        .map(|(module_name, source)| compile_flow::CompileUnit {
            output_module_name: module_aliases
                .get(module_name.as_str())
                .map(String::as_str)
                .unwrap_or(module_name.as_str()),
            source,
            source_label: format!("{module_name}.mond"),
        })
        .collect();
    let (source_outputs, source_had_error) = compile_flow::compile_units(
        &source_compile_units,
        Arc::clone(&analysis),
        true,
        compile_target,
    )
    .await;
    for output in source_outputs {
        if let Some(erl_source) = output.erl_source() {
            erl_paths.push(compile_flow::write_erl_output(
                erl_dir,
                &output.output_module_name,
                erl_source,
            )?);
        }
    }

    if source_had_error {
        return Err(eyre::eyre!("compilation failed; see diagnostics above"));
    }

    validate_bin_entrypoint(&project_type, &selected_module_sources)?;

    let dependency_analysis = Arc::new(
        mondc::build_project_analysis(&dependency_external_mods, &[])
            .map_err(|err| eyre::eyre!(err))?,
    );
    let dependency_compile_units: Vec<compile_flow::CompileUnit<'_>> = selected_dependency_mods
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
        compile_target,
    )
    .await;
    for output in dependency_outputs {
        if let Some(erl_source) = output.erl_source() {
            erl_paths.push(compile_flow::write_erl_output(
                erl_dir,
                &output.output_module_name,
                erl_source,
            )?);
        }
    }
    if dependency_had_error {
        return Err(eyre::eyre!("compilation failed; see diagnostics above"));
    }

    // Copy any hand-written Erlang helpers bundled with dependencies.
    for helper in &loaded_dependencies.helper_erls {
        let dest = erl_dir.join(&helper.file_name);
        std::fs::write(&dest, &helper.contents)
            .with_context(|| format!("could not write {}", dest.display()))?;
        erl_paths.push(dest);
    }

    // Copy any hand-written Erlang helpers in this project's src/ directory.
    for helper in &local_helper_erls {
        let dest = erl_dir.join(&helper.file_name);
        std::fs::write(&dest, &helper.contents)
            .with_context(|| format!("could not write {}", dest.display()))?;
        erl_paths.push(dest);
    }

    Ok(ErlSources {
        erl_paths,
        manifest,
        project_type,
        module_exports,
        module_type_decls,
        module_extern_types,
        all_module_schemes,
        dependency_mods,
        module_aliases,
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

pub(crate) async fn build(project_dir: &Path, run: bool) -> eyre::Result<()> {
    let build_dir = project_dir.join(TARGET_DIR).join(DEBUG_BUILD_DIR);
    let erl_dir = build_dir.join(ERL_SOURCE_SUBDIR);
    let ebin_dir = build_dir.join(ERL_BEAM_SUBDIR);
    std::fs::create_dir_all(&erl_dir)
        .context(format!("could not create {ERL_SOURCE_SUBDIR} dir"))?;
    std::fs::create_dir_all(&ebin_dir)
        .context(format!("could not create {ERL_BEAM_SUBDIR} dir"))?;
    std::fs::create_dir_all(&build_dir)
        .context(format!("could not create {DEBUG_BUILD_DIR} dir"))?;

    let manifest = manifest::read_manifest(project_dir.into())?;

    let ErlSources {
        erl_paths,
        manifest,
        project_type,
        module_aliases,
        ..
    } = generate_erl_sources(manifest, project_dir, &erl_dir).await?;

    if matches!(project_type, ProjectType::Lib) && run {
        return Err(eyre::eyre!("bahn cannot run a library project"));
    }

    crate::utils::verify_erlc_installed()?;

    // Run erlc on all .erl files at once
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
    if run {
        let main_module = module_aliases
            .get("main")
            .map(String::as_str)
            .unwrap_or("main");
        let status = {
            let ebin_dir = ebin_dir.clone();
            let main_module = main_module.to_string();
            tokio::task::spawn_blocking(move || {
                Command::new("erl")
                    .arg("-noinput")
                    .arg("-pa")
                    .arg(&ebin_dir)
                    .arg("-eval")
                    .arg(format!("{main_module}:main(unit), init:stop()."))
                    .status()
                    .context("could not run erl")
            })
            .await
            .map_err(|err| eyre::eyre!("failed to join erl task: {err}"))??
        };

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
    use std::{
        future::Future,
        path::{Path, PathBuf},
        process::Command,
        thread,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    fn unique_temp_root() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!("mond-build-test-{}-{nanos}", std::process::id()))
    }

    fn cleanup_temp_root(root: &Path) {
        for _ in 0..5 {
            match std::fs::remove_dir_all(root) {
                Ok(()) => return,
                Err(err) if err.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
                    thread::sleep(Duration::from_millis(25));
                }
                Err(err) => panic!("cleanup temp root: {err}"),
            }
        }
        std::fs::remove_dir_all(root).expect("cleanup temp root");
    }

    fn block_on<T>(future: impl Future<Output = T>) -> T {
        tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(future)
    }

    fn run_ok(cmd: &mut Command) {
        let output = cmd.output().expect("run command");
        assert!(
            output.status.success(),
            "command failed: {}\n{}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
    }

    fn create_std_dependency_repo(root: &Path) -> PathBuf {
        let repo_dir = root.join("std-src");
        let repo_src_dir = repo_dir.join("src");
        std::fs::create_dir_all(&repo_src_dir).expect("create std src");
        let mut manifest = crate::manifest::create_new_manifest("std".to_string());
        manifest.dependencies.clear();
        crate::manifest::write_manifest(&manifest, &repo_dir.join(crate::MANIFEST_NAME))
            .expect("write std manifest");
        std::fs::write(
            repo_src_dir.join("lib.mond"),
            ";;; std - test fixture root module\n",
        )
        .expect("write std lib");
        std::fs::write(
            repo_src_dir.join("result.mond"),
            r#"(pub type ['a 'e] Result
  [(Ok ~ 'a)
   (Error ~ 'e)])"#,
        )
        .expect("write std result");
        std::fs::write(
            repo_src_dir.join("unknown.mond"),
            r#"(pub extern type Unknown)"#,
        )
        .expect("write std unknown");

        run_ok(Command::new("git").arg("init").current_dir(&repo_dir));
        run_ok(
            Command::new("git")
                .args(["add", "."])
                .current_dir(&repo_dir),
        );
        run_ok(
            Command::new("git")
                .args([
                    "-c",
                    "user.email=test@example.com",
                    "-c",
                    "user.name=test",
                    "commit",
                    "-m",
                    "snapshot",
                ])
                .current_dir(&repo_dir),
        );

        repo_dir
    }

    fn repo_head_rev(repo_dir: &Path) -> String {
        let output = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo_dir)
            .output()
            .expect("git rev-parse");
        assert!(
            output.status.success(),
            "git rev-parse failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn compile_and_run_main(project_dir: &Path) -> String {
        let build_dir = project_dir.join("target").join("integration-run");
        let erl_dir = build_dir.join(ERL_SOURCE_SUBDIR);
        let ebin_dir = build_dir.join(ERL_BEAM_SUBDIR);
        std::fs::create_dir_all(&erl_dir).expect("create erl dir");
        std::fs::create_dir_all(&ebin_dir).expect("create ebin dir");

        let manifest = crate::manifest::read_manifest(project_dir.into()).expect("read manifest");
        let ErlSources {
            erl_paths,
            module_aliases,
            ..
        } = block_on(generate_erl_sources(manifest, project_dir, &erl_dir))
            .expect("generate erl sources");

        let erlc = Command::new("erlc")
            .arg("-o")
            .arg(&ebin_dir)
            .args(&erl_paths)
            .output()
            .expect("run erlc");
        assert!(
            erlc.status.success(),
            "erlc failed:\n{}",
            String::from_utf8_lossy(&erlc.stderr)
        );

        let main_module = module_aliases
            .get("main")
            .map(String::as_str)
            .unwrap_or("main");
        let erl = Command::new("erl")
            .arg("-noinput")
            .arg("-pa")
            .arg(&ebin_dir)
            .arg("-eval")
            .arg(format!("{main_module}:main(unit), init:stop()."))
            .output()
            .expect("run erl");
        assert!(
            erl.status.success(),
            "erl failed:\n{}{}",
            String::from_utf8_lossy(&erl.stdout),
            String::from_utf8_lossy(&erl.stderr)
        );

        String::from_utf8_lossy(&erl.stdout).to_string()
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
        let mut manifest = crate::manifest::create_new_manifest("app".to_string());
        manifest.dependencies.clear();
        crate::manifest::write_manifest(&manifest, &project_dir.join(crate::MANIFEST_NAME))
            .expect("write manifest");
        std::fs::write(
            project_dir.join("src").join(crate::BIN_ENTRY_POINT),
            r#"(use std/io)
(use std/list)

(let some_list {} [1 2 3])
(let main {} (debug (list/map fn {x} (+ x 1) (some_list))))
"#,
        )
        .expect("write main");

        let err = match block_on(generate_erl_sources(
            manifest,
            &project_dir,
            &project_dir.join("target/test-build"),
        )) {
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

        cleanup_temp_root(&root);
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
    fn external_modules_from_sources_discovers_files_without_lib_reexports() {
        let modules = vec![
            ("io".to_string(), "(let println {x} x)".to_string()),
            ("extra".to_string(), "(let helper {} 1)".to_string()),
            ("std".to_string(), "(let hello {} 1)".to_string()),
        ];

        let discovered = mondc::external_modules_from_sources(&modules).expect("external modules");
        let names: Vec<String> = discovered.into_iter().map(|(name, _, _)| name).collect();

        assert!(names.contains(&"io".to_string()));
        assert!(names.contains(&"extra".to_string()));
        assert!(names.contains(&"std".to_string()));
    }

    #[test]
    fn reachable_dependency_modules_include_transitive_dependencies() {
        let dependency_mods = vec![
            mondc::DependencyModuleSource {
                package_name: "std".to_string(),
                module_name: "list".to_string(),
                erlang_name: "mond_list".to_string(),
                source: "(pub let map {f xs} xs)".to_string(),
                source_relpath: "src/list.mond".to_string(),
            },
            mondc::DependencyModuleSource {
                package_name: "std".to_string(),
                module_name: "io".to_string(),
                erlang_name: "mond_io".to_string(),
                source: "(use list)\n(pub let println {x} (list/map x))".to_string(),
                source_relpath: "src/io.mond".to_string(),
            },
            mondc::DependencyModuleSource {
                package_name: "std".to_string(),
                module_name: "unused".to_string(),
                erlang_name: "mond_unused".to_string(),
                source: "(pub let noop {} ())".to_string(),
                source_relpath: "src/unused.mond".to_string(),
            },
        ];

        let selected =
            reachable_dependency_modules(&dependency_mods, &HashSet::from(["io".to_string()]))
                .expect("reachable dependency modules");
        let names: Vec<String> = selected
            .into_iter()
            .map(|module| module.module_name)
            .collect();

        assert_eq!(names, vec!["list", "io"]);
    }

    #[test]
    fn dependency_source_label_uses_dependency_relative_path() {
        let module = mondc::DependencyModuleSource {
            package_name: "http".to_string(),
            module_name: "request".to_string(),
            erlang_name: "d_http_request".to_string(),
            source: "(pub let get {} 1)".to_string(),
            source_relpath: "src/client/request.mond".to_string(),
        };

        assert_eq!(
            dependency_source_label(&module),
            "deps/http/src/client/request.mond"
        );
    }

    #[test]
    fn register_erl_output_name_rejects_collisions() {
        let mut seen = HashMap::new();
        register_erl_output_name(&mut seen, "mond_io", "src/mond_io.mond".to_string())
            .expect("first insert");
        let err =
            register_erl_output_name(&mut seen, "mond_io", "dependency module `io`".to_string())
                .expect_err("expected collision");
        assert!(err.to_string().contains("Erlang module name collision"));
    }

    #[test]
    fn ensure_no_erl_output_collisions_catches_helper_conflicts() {
        let user = vec![(
            "mond_unknown_helpers".to_string(),
            "(let main {} ())".to_string(),
        )];
        let used_dependency_names: HashSet<String> = HashSet::new();
        let helpers = vec![deps::HelperErlFile {
            file_name: "mond_unknown_helpers.erl".to_string(),
            module_name: "mond_unknown_helpers".to_string(),
            contents: vec![],
        }];
        let err = ensure_no_erl_output_collisions(
            &user,
            &HashMap::new(),
            &[],
            &used_dependency_names,
            &helpers,
            &[],
        )
        .expect_err("expected helper collision");
        assert!(err.to_string().contains("Erlang module name collision"));
    }

    #[test]
    fn ensure_no_local_dependency_module_conflicts_rejects_overlaps() {
        let local_modules = vec![
            ("io".to_string(), "(let io_local {} 1)".to_string()),
            ("main".to_string(), "(let main {} 1)".to_string()),
        ];
        let dependency_mods = vec![
            mondc::DependencyModuleSource {
                package_name: "std".to_string(),
                module_name: "io".to_string(),
                erlang_name: "d_std_io".to_string(),
                source: "(pub let println {x} x)".to_string(),
                source_relpath: "src/io.mond".to_string(),
            },
            mondc::DependencyModuleSource {
                package_name: "std".to_string(),
                module_name: "list".to_string(),
                erlang_name: "d_std_list".to_string(),
                source: "(pub let map {f xs} xs)".to_string(),
                source_relpath: "src/list.mond".to_string(),
            },
        ];
        let err = ensure_no_local_dependency_module_conflicts(&local_modules, &dependency_mods)
            .expect_err("expected conflict");
        assert!(
            err.to_string()
                .contains("module name conflict between src/ and dependencies: io"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn generate_erl_sources_copies_local_helper_erls() {
        let root = unique_temp_root();
        std::fs::create_dir_all(&root).expect("create temp root");

        let project_dir = root.join("app");
        std::fs::create_dir_all(project_dir.join("src")).expect("create src dir");
        let mut manifest = crate::manifest::create_new_manifest("app".to_string());
        manifest.dependencies.clear();
        crate::manifest::write_manifest(&manifest, &project_dir.join(crate::MANIFEST_NAME))
            .expect("write manifest");
        std::fs::write(
            project_dir.join("src").join(crate::LIB_ROOT),
            r#"(pub let hello {} 1)"#,
        )
        .expect("write lib");
        std::fs::write(
            project_dir.join("src").join("mond_local_helpers.erl"),
            "-module(mond_local_helpers).\n-export([hello/0]).\nhello() -> ok.\n",
        )
        .expect("write local helper");

        let out_dir = project_dir.join("target/test-build");
        std::fs::create_dir_all(&out_dir).expect("create output dir");
        let generated = block_on(generate_erl_sources(manifest, &project_dir, &out_dir))
            .expect("generate sources");
        let helper_path = out_dir.join("mond_local_helpers.erl");
        assert!(helper_path.exists(), "local helper should be copied");
        assert!(
            generated.erl_paths.iter().any(|p| p == &helper_path),
            "generated erl paths should include local helper"
        );

        cleanup_temp_root(&root);
    }

    #[test]
    fn generate_erl_sources_does_not_write_debug_registry() {
        let root = unique_temp_root();
        std::fs::create_dir_all(&root).expect("create temp root");

        let project_dir = root.join("app");
        std::fs::create_dir_all(project_dir.join("src")).expect("create src dir");
        let mut manifest = crate::manifest::create_new_manifest("app".to_string());
        manifest.dependencies.clear();
        crate::manifest::write_manifest(&manifest, &project_dir.join(crate::MANIFEST_NAME))
            .expect("write manifest");
        std::fs::write(
            project_dir.join("src").join(crate::LIB_ROOT),
            r#"(pub let root {} 1)"#,
        )
        .expect("write lib");
        std::fs::write(
            project_dir.join("src").join("io.mond"),
            r#"(pub let println {x} x)"#,
        )
        .expect("write io");

        let out_dir = project_dir.join("target/test-build");
        std::fs::create_dir_all(&out_dir).expect("create output dir");
        let generated = block_on(generate_erl_sources(manifest, &project_dir, &out_dir))
            .expect("generate sources");
        let registry_path = out_dir.join("mond_debug_registry.erl");

        assert!(
            !registry_path.exists(),
            "debug registry should not be written"
        );
        assert!(
            !generated.erl_paths.iter().any(|p| p == &registry_path),
            "generated erl paths should not include a registry module"
        );

        cleanup_temp_root(&root);
    }

    #[test]
    fn local_modules_use_package_prefixed_erlang_aliases() {
        let root = unique_temp_root();
        std::fs::create_dir_all(&root).expect("create temp root");

        let project_dir = root.join("app");
        std::fs::create_dir_all(project_dir.join("src")).expect("create src dir");
        let mut manifest = crate::manifest::create_new_manifest("app".to_string());
        manifest.dependencies.clear();
        crate::manifest::write_manifest(&manifest, &project_dir.join(crate::MANIFEST_NAME))
            .expect("write manifest");
        std::fs::write(
            project_dir.join("src").join(crate::LIB_ROOT),
            r#"(use io)
(use string)

(pub let main_value {} (string/concat (io/println 1)))"#,
        )
        .expect("write lib");
        std::fs::write(
            project_dir.join("src").join("io.mond"),
            r#"(pub let println {x} x)"#,
        )
        .expect("write io");
        std::fs::write(
            project_dir.join("src").join("string.mond"),
            r#"(pub let concat {x} x)"#,
        )
        .expect("write string");

        let out_dir = project_dir.join("target/test-build");
        std::fs::create_dir_all(&out_dir).expect("create output dir");
        let generated = block_on(generate_erl_sources(manifest, &project_dir, &out_dir))
            .expect("generate sources");
        assert!(
            generated.erl_paths.iter().any(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n == "p_app_io.erl")
            }),
            "io module should compile to p_app_io.erl"
        );
        assert!(
            generated.erl_paths.iter().any(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n == "p_app_string.erl")
            }),
            "string module should compile to p_app_string.erl"
        );
        assert_eq!(
            generated.module_aliases.get("io").map(String::as_str),
            Some("p_app_io")
        );
        assert_eq!(
            generated.module_aliases.get("string").map(String::as_str),
            Some("p_app_string")
        );
        let generated_files: Vec<String> = generated
            .erl_paths
            .iter()
            .filter_map(|path| path.file_name().and_then(|name| name.to_str()))
            .map(str::to_string)
            .collect();
        assert!(generated_files.contains(&"p_app_io.erl".to_string()));
        assert!(generated_files.contains(&"p_app_lib.erl".to_string()));
        assert!(generated_files.contains(&"p_app_string.erl".to_string()));

        cleanup_temp_root(&root);
    }

    #[test]
    fn generate_erl_sources_includes_selected_dependency_modules_without_registry() {
        let root = unique_temp_root();
        std::fs::create_dir_all(&root).expect("create temp root");

        let dep_repo = root.join("time-src");
        let dep_src_dir = dep_repo.join("src");
        std::fs::create_dir_all(&dep_src_dir).expect("create dependency src");
        let mut dep_manifest = crate::manifest::create_new_manifest("time".to_string());
        dep_manifest.dependencies.clear();
        crate::manifest::write_manifest(&dep_manifest, &dep_repo.join(crate::MANIFEST_NAME))
            .expect("write dependency manifest");
        std::fs::write(
            dep_src_dir.join("lib.mond"),
            r#"(use format)

(pub let now {} (format/iso 1))"#,
        )
        .expect("write dep lib");
        std::fs::write(dep_src_dir.join("format.mond"), r#"(pub let iso {x} x)"#)
            .expect("write dep format");
        run_ok(Command::new("git").arg("init").current_dir(&dep_repo));
        run_ok(
            Command::new("git")
                .args(["add", "."])
                .current_dir(&dep_repo),
        );
        run_ok(
            Command::new("git")
                .args([
                    "-c",
                    "user.email=test@example.com",
                    "-c",
                    "user.name=test",
                    "commit",
                    "-m",
                    "initial",
                ])
                .current_dir(&dep_repo),
        );
        run_ok(
            Command::new("git")
                .args(["tag", "0.0.1"])
                .current_dir(&dep_repo),
        );

        let project_dir = root.join("app");
        std::fs::create_dir_all(project_dir.join("src")).expect("create src dir");
        let mut manifest = crate::manifest::create_new_manifest("app".to_string());
        manifest.dependencies.clear();
        manifest.dependencies.insert(
            "time".to_string(),
            crate::manifest::DependencySpec {
                git: format!("file://{}", dep_repo.display()),
                reference: crate::manifest::GitReference::Tag("0.0.1".to_string()),
            },
        );
        crate::manifest::write_manifest(&manifest, &project_dir.join(crate::MANIFEST_NAME))
            .expect("write manifest");
        std::fs::write(
            project_dir.join("src").join(crate::LIB_ROOT),
            r#"(use time)

(pub let root {} (time/now))"#,
        )
        .expect("write lib");

        let out_dir = project_dir.join("target/test-build");
        std::fs::create_dir_all(&out_dir).expect("create output dir");
        let generated = block_on(generate_erl_sources(manifest, &project_dir, &out_dir))
            .expect("generate sources");
        let generated_files: Vec<String> = generated
            .erl_paths
            .iter()
            .filter_map(|path| path.file_name().and_then(|name| name.to_str()))
            .map(str::to_string)
            .collect();
        assert!(generated_files.contains(&"d_time_time.erl".to_string()));
        assert!(generated_files.contains(&"d_time_format.erl".to_string()));
        assert!(generated_files.contains(&"p_app_lib.erl".to_string()));
        assert!(
            !generated_files.contains(&"mond_debug_registry.erl".to_string()),
            "registry module should not be generated"
        );

        cleanup_temp_root(&root);
    }

    #[test]
    fn package_aliases_lib_module_as_package_name() {
        let root = unique_temp_root();
        std::fs::create_dir_all(&root).expect("create temp root");

        let project_dir = root.join("time");
        std::fs::create_dir_all(project_dir.join("src")).expect("create src dir");
        let mut manifest = crate::manifest::create_new_manifest("time".to_string());
        manifest.dependencies.clear();
        crate::manifest::write_manifest(&manifest, &project_dir.join(crate::MANIFEST_NAME))
            .expect("write manifest");
        std::fs::write(
            project_dir.join("src").join(crate::LIB_ROOT),
            r#"(pub let now {} 1)"#,
        )
        .expect("write lib");

        let out_dir = project_dir.join("target/test-build");
        std::fs::create_dir_all(&out_dir).expect("create output dir");
        let generated = block_on(generate_erl_sources(manifest, &project_dir, &out_dir))
            .expect("generate sources");
        assert!(generated.module_exports.contains_key("lib"));
        assert!(generated.module_exports.contains_key("time"));
        assert_eq!(
            generated.module_aliases.get("time").map(String::as_str),
            Some("p_time_lib"),
            "package alias should map to lib module"
        );
        assert!(
            generated
                .erl_paths
                .iter()
                .any(|path| path.file_name().and_then(|name| name.to_str())
                    == Some("p_time_lib.erl"))
        );

        cleanup_temp_root(&root);
    }

    #[test]
    fn build_and_run_debug_prints_mond_values() {
        let root = unique_temp_root();
        std::fs::create_dir_all(&root).expect("create temp root");

        let std_repo = create_std_dependency_repo(&root);
        let std_rev = repo_head_rev(&std_repo);

        let project_dir = root.join("app");
        std::fs::create_dir_all(project_dir.join("src")).expect("create src dir");
        let mut manifest = crate::manifest::create_new_manifest("app".to_string());
        manifest.dependencies.insert(
            "std".to_string(),
            crate::manifest::DependencySpec {
                git: format!("file://{}", std_repo.display()),
                reference: crate::manifest::GitReference::Rev(std_rev),
            },
        );
        crate::manifest::write_manifest(&manifest, &project_dir.join(crate::MANIFEST_NAME))
            .expect("write manifest");
        std::fs::write(
            project_dir.join("src").join("data.mond"),
            r#"(use std/result [Result])

(type Point [(:x ~ Int) (:y ~ Int)])
(type Payload [(:points ~ (List Point))])

(pub let ok_value {} (Ok 7))
(pub let point {} (Point :x 10 :y 12))
(pub let nested {}
  (Payload :points
    [(Point :x 1 :y 2)
     (Point :x 3 :y 4)]))"#,
        )
        .expect("write data module");
        std::fs::write(
            project_dir.join("src").join("main.mond"),
            r#"(use std/unknown [Unknown])
(use data)

(extern let foreign_list ~ (Unit -> Unknown) mond_io_fixture/foreign_list)
(extern let foreign_map ~ (Unit -> Unknown) mond_io_fixture/foreign_map)

(let main {}
  (debug "hello	world")
  (debug (foreign_list))
  (debug (data/ok_value))
  (debug (data/point))
  (debug (data/nested))
  (debug (foreign_map)))"#,
        )
        .expect("write main module");
        std::fs::write(
            project_dir.join("src").join("mond_io_fixture.erl"),
            "-module(mond_io_fixture).\n-export([foreign_list/0, foreign_map/0]).\nforeign_list() -> [1, [true, false], unit].\nforeign_map() -> #{answer => 42}.\n",
        )
        .expect("write fixture helper");

        let stdout = compile_and_run_main(&project_dir);
        assert_eq!(
            stdout,
            "\"hello\\tworld\"\n[1 [True False] ()]\n(Ok 7)\n(Point :x 10 :y 12)\n(Payload :points [(Point :x 1 :y 2) (Point :x 3 :y 4)])\n#{answer => 42}\n"
        );

        cleanup_temp_root(&root);
    }

    #[test]
    fn package_alias_errors_when_module_name_conflicts() {
        let root = unique_temp_root();
        std::fs::create_dir_all(&root).expect("create temp root");

        let project_dir = root.join("time");
        std::fs::create_dir_all(project_dir.join("src")).expect("create src dir");
        let mut manifest = crate::manifest::create_new_manifest("time".to_string());
        manifest.dependencies.clear();
        crate::manifest::write_manifest(&manifest, &project_dir.join(crate::MANIFEST_NAME))
            .expect("write manifest");
        std::fs::write(
            project_dir.join("src").join(crate::LIB_ROOT),
            r#"(pub let from_lib {} 1)"#,
        )
        .expect("write lib");
        std::fs::write(
            project_dir.join("src").join("time.mond"),
            r#"(pub let from_time {} 2)"#,
        )
        .expect("write time module");

        let out_dir = project_dir.join("target/test-build");
        std::fs::create_dir_all(&out_dir).expect("create output dir");
        let err = match block_on(generate_erl_sources(manifest, &project_dir, &out_dir)) {
            Ok(_) => panic!("expected alias collision"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("module name collision"),
            "unexpected error: {err}"
        );

        cleanup_temp_root(&root);
    }
}
