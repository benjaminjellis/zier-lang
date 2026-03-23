use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    path::{Path, PathBuf},
    process::Command,
};

use eyre::Context;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

const LOCKFILE_FORMAT_VERSION: u32 = 2;

use crate::{
    LOCKFILE_NAME, SOURCE_DIR, TARGET_DIR, manifest,
    ui::{info, success},
};

#[derive(Clone, Debug)]
pub(crate) struct HelperErlFile {
    pub(crate) file_name: String,
    pub(crate) module_name: String,
    pub(crate) contents: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct LoadedDependencies {
    pub(crate) modules: Vec<(String, String, String)>,
    pub(crate) helper_erls: Vec<HelperErlFile>,
}

#[derive(Clone, Debug)]
struct ResolvedPackage {
    id: String,
    name: String,
    source: String,
    spec: manifest::DependencySpec,
    checkout_dir: PathBuf,
    rev: String,
    dependencies: Vec<String>,
}

#[derive(Clone, Debug)]
struct ResolvedRootDependency {
    alias: String,
    package: String,
    spec: manifest::DependencySpec,
}

#[derive(Clone, Debug)]
struct ResolvedGraph {
    roots: Vec<ResolvedRootDependency>,
    packages: BTreeMap<String, ResolvedPackage>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct MondLock {
    version: u32,
    root: Vec<LockedRootDependency>,
    package: Vec<LockedPackage>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LockedRootDependency {
    alias: String,
    package: String,
    git: String,
    #[serde(flatten)]
    reference: manifest::GitReference,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LockedPackage {
    id: String,
    name: String,
    source: String,
    git: String,
    #[serde(flatten)]
    reference: manifest::GitReference,
    resolved_rev: String,
    #[serde(default)]
    dependencies: Vec<String>,
}

pub(crate) fn parse_erlang_module_name(contents: &[u8]) -> Option<String> {
    let source = std::str::from_utf8(contents).ok()?;
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('%') {
            continue;
        }
        let Some(rest) = trimmed.strip_prefix("-module(") else {
            continue;
        };
        let end = rest.find(')')?;
        let module_name = rest[..end].trim().trim_matches('\'');
        if module_name.is_empty() {
            return None;
        }
        return Some(module_name.to_string());
    }
    None
}

fn validate_dependency_manifest_name(dep_name: &str, checkout_dir: &Path) -> eyre::Result<()> {
    let dep_manifest = manifest::read_manifest(checkout_dir.to_path_buf()).with_context(|| {
        format!(
            "could not read dependency manifest for `{dep_name}` at {}",
            checkout_dir.display()
        )
    })?;
    if dep_manifest.package.name != dep_name {
        return Err(eyre::eyre!(
            "dependency `{dep_name}` points to package `{}`; expected `{dep_name}`",
            dep_manifest.package.name
        ));
    }
    Ok(())
}

fn dependency_spec_summary(spec: &manifest::DependencySpec) -> String {
    match &spec.reference {
        manifest::GitReference::Tag(tag) => format!("{} @ tag `{tag}`", spec.git),
        manifest::GitReference::Branch(branch) => format!("{} @ branch `{branch}`", spec.git),
        manifest::GitReference::Rev(rev) => format!("{} @ rev `{rev}`", spec.git),
    }
}

fn read_lockfile(project_dir: &Path) -> eyre::Result<Option<MondLock>> {
    let lock_path = project_dir.join(LOCKFILE_NAME);
    if !lock_path.exists() {
        return Ok(None);
    }
    let lock_src = std::fs::read(&lock_path)
        .with_context(|| format!("failed to read {LOCKFILE_NAME} at {}", lock_path.display()))?;
    let lock: MondLock = toml::from_slice(&lock_src).map_err(|err| {
        eyre::eyre!(
            "failed to parse {LOCKFILE_NAME}: {err}\n{LOCKFILE_NAME} format changed; delete {LOCKFILE_NAME} or run `bahn deps --update` to regenerate it"
        )
    })?;
    if lock.version != LOCKFILE_FORMAT_VERSION {
        return Err(eyre::eyre!(
            "{LOCKFILE_NAME} uses unsupported version {} (expected {})",
            lock.version,
            LOCKFILE_FORMAT_VERSION
        ));
    }
    Ok(Some(lock))
}

fn dependency_source(spec: &manifest::DependencySpec) -> String {
    format!("git+{}", spec.git)
}

fn package_id(name: &str, source: &str, rev: &str) -> String {
    format!("{name}@{source}#{rev}")
}

fn lock_packages_by_id(lock: &MondLock) -> eyre::Result<BTreeMap<String, LockedPackage>> {
    let mut by_id: BTreeMap<String, LockedPackage> = BTreeMap::new();
    for package in &lock.package {
        if by_id.insert(package.id.clone(), package.clone()).is_some() {
            return Err(eyre::eyre!(
                "{LOCKFILE_NAME} contains duplicate package entries for id `{}`",
                package.id
            ));
        }
    }
    Ok(by_id)
}

fn lock_roots_by_alias(lock: &MondLock) -> eyre::Result<BTreeMap<String, LockedRootDependency>> {
    let mut by_alias: BTreeMap<String, LockedRootDependency> = BTreeMap::new();
    for root in &lock.root {
        if by_alias.insert(root.alias.clone(), root.clone()).is_some() {
            return Err(eyre::eyre!(
                "{LOCKFILE_NAME} contains duplicate root entries for alias `{}`",
                root.alias
            ));
        }
    }
    Ok(by_alias)
}

fn lock_matches_manifest(lock: &MondLock, manifest: &manifest::BahnManifest) -> eyre::Result<bool> {
    let mut root_from_manifest: Vec<String> = manifest.dependencies.keys().cloned().collect();
    root_from_manifest.sort();

    let root_from_lock = lock_roots_by_alias(lock)?;
    let mut root_aliases_from_lock: Vec<String> = root_from_lock.keys().cloned().collect();
    root_aliases_from_lock.sort();
    if root_from_manifest != root_aliases_from_lock {
        return Ok(false);
    }

    let lock_packages = lock_packages_by_id(lock)?;
    for alias in root_from_manifest {
        let root_dep = &root_from_lock[&alias];
        let manifest_spec = &manifest.dependencies[&alias];
        if root_dep.git != manifest_spec.git || root_dep.reference != manifest_spec.reference {
            return Ok(false);
        }
        if !lock_packages.contains_key(&root_dep.package) {
            return Err(eyre::eyre!(
                "{LOCKFILE_NAME} root alias `{alias}` points to missing package id `{}`",
                root_dep.package
            ));
        }
    }

    Ok(true)
}

fn write_lockfile(project_dir: &Path, resolved: &ResolvedGraph) -> eyre::Result<()> {
    let root: Vec<LockedRootDependency> = resolved
        .roots
        .iter()
        .map(|dep| LockedRootDependency {
            alias: dep.alias.clone(),
            package: dep.package.clone(),
            git: dep.spec.git.clone(),
            reference: dep.spec.reference.clone(),
        })
        .collect();

    let package: Vec<LockedPackage> = resolved
        .packages
        .values()
        .map(|dep| LockedPackage {
            id: dep.id.clone(),
            name: dep.name.clone(),
            source: dep.source.clone(),
            git: dep.spec.git.clone(),
            reference: dep.spec.reference.clone(),
            resolved_rev: dep.rev.clone(),
            dependencies: dep.dependencies.clone(),
        })
        .collect();

    let lock = MondLock {
        version: LOCKFILE_FORMAT_VERSION,
        root,
        package,
    };
    let lock_src =
        toml::to_string_pretty(&lock).context(format!("failed to serialize {LOCKFILE_NAME}"))?;
    std::fs::write(project_dir.join(LOCKFILE_NAME), lock_src)
        .context(format!("failed to write {LOCKFILE_NAME}"))?;
    Ok(())
}

fn current_dependency_rev(checkout_dir: &Path) -> eyre::Result<String> {
    run_git_output(
        Some(checkout_dir),
        &["rev-parse", "HEAD"],
        "failed to resolve dependency revision",
    )
}

fn resolve_dependencies(
    project_dir: &Path,
    manifest: &manifest::BahnManifest,
    refresh: bool,
) -> eyre::Result<ResolvedGraph> {
    if !refresh
        && let Some(lock) = read_lockfile(project_dir)?
        && lock_matches_manifest(&lock, manifest)?
    {
        return resolve_dependencies_from_lockfile(project_dir, &lock);
    }

    let resolved = resolve_dependencies_from_manifests(project_dir, manifest, refresh)?;
    write_lockfile(project_dir, &resolved)?;
    Ok(resolved)
}

fn resolve_dependencies_from_lockfile(
    project_dir: &Path,
    lock: &MondLock,
) -> eyre::Result<ResolvedGraph> {
    let lock_packages = lock_packages_by_id(lock)?;
    let lock_roots = lock_roots_by_alias(lock)?;

    for root in lock_roots.values() {
        if !lock_packages.contains_key(&root.package) {
            return Err(eyre::eyre!(
                "{LOCKFILE_NAME} root alias `{}` points to missing package id `{}`",
                root.alias,
                root.package
            ));
        }
    }
    for package in lock_packages.values() {
        for dep in &package.dependencies {
            if !lock_packages.contains_key(dep) {
                return Err(eyre::eyre!(
                    "{LOCKFILE_NAME} package id `{}` depends on missing package id `{dep}`",
                    package.id
                ));
            }
        }
    }

    let mut packages: BTreeMap<String, ResolvedPackage> = BTreeMap::new();
    for (id, package) in lock_packages {
        let checkout_spec = manifest::DependencySpec {
            git: package.git.clone(),
            reference: manifest::GitReference::Rev(package.resolved_rev.clone()),
        };
        let checkout_dir = checkout_dependency(project_dir, &package.name, &checkout_spec)?;
        let current_rev = current_dependency_rev(&checkout_dir)?;
        if current_rev != package.resolved_rev {
            return Err(eyre::eyre!(
                "failed to checkout locked dependency `{}` at rev `{}` (current: `{current_rev}`)",
                package.name,
                package.resolved_rev
            ));
        }
        packages.insert(
            id.clone(),
            ResolvedPackage {
                id,
                name: package.name,
                source: package.source,
                spec: manifest::DependencySpec {
                    git: package.git,
                    reference: package.reference,
                },
                checkout_dir,
                rev: current_rev,
                dependencies: package.dependencies,
            },
        );
    }

    let roots: Vec<ResolvedRootDependency> = lock_roots
        .into_values()
        .map(|root| ResolvedRootDependency {
            alias: root.alias,
            package: root.package,
            spec: manifest::DependencySpec {
                git: root.git,
                reference: root.reference,
            },
        })
        .collect();

    Ok(ResolvedGraph { roots, packages })
}

fn resolve_dependencies_from_manifests(
    project_dir: &Path,
    manifest: &manifest::BahnManifest,
    refresh: bool,
) -> eyre::Result<ResolvedGraph> {
    #[derive(Clone, Debug)]
    struct InterimResolvedPackage {
        name: String,
        spec: manifest::DependencySpec,
        checkout_dir: PathBuf,
        rev: String,
        dependency_names: Vec<String>,
    }

    let mut queue: VecDeque<(String, manifest::DependencySpec, String)> = VecDeque::new();
    let mut seen_specs: BTreeMap<String, (manifest::DependencySpec, String)> = BTreeMap::new();
    let mut interim: BTreeMap<String, InterimResolvedPackage> = BTreeMap::new();

    let mut root_dep_names: Vec<String> = manifest.dependencies.keys().cloned().collect();
    root_dep_names.sort();
    for dep_name in &root_dep_names {
        queue.push_back((
            dep_name.clone(),
            manifest.dependencies[dep_name].clone(),
            "root manifest".to_string(),
        ));
    }

    while let Some((dep_name, dep_spec, requested_by)) = queue.pop_front() {
        if let Some((existing_spec, existing_requested_by)) = seen_specs.get(&dep_name) {
            if existing_spec != &dep_spec {
                return Err(eyre::eyre!(
                    "dependency `{dep_name}` has conflicting requirements: {} requested `{}` while {} requested `{}`",
                    existing_requested_by,
                    dependency_spec_summary(existing_spec),
                    requested_by,
                    dependency_spec_summary(&dep_spec),
                ));
            }
            continue;
        }

        let checkout_dir =
            checkout_dependency_with_policy(project_dir, &dep_name, &dep_spec, refresh)?;
        let dep_manifest = manifest::read_manifest(checkout_dir.clone()).with_context(|| {
            format!(
                "could not read dependency manifest for `{dep_name}` at {}",
                checkout_dir.display()
            )
        })?;
        if dep_manifest.package.name != dep_name {
            return Err(eyre::eyre!(
                "dependency `{dep_name}` points to package `{}`; expected `{dep_name}`",
                dep_manifest.package.name
            ));
        }
        let rev = current_dependency_rev(&checkout_dir)?;

        seen_specs.insert(dep_name.clone(), (dep_spec.clone(), requested_by));
        let mut child_dep_names: Vec<String> = dep_manifest.dependencies.keys().cloned().collect();
        child_dep_names.sort();
        interim.insert(
            dep_name.clone(),
            InterimResolvedPackage {
                name: dep_name.clone(),
                spec: dep_spec,
                checkout_dir,
                rev,
                dependency_names: child_dep_names.clone(),
            },
        );

        for child_dep_name in child_dep_names {
            queue.push_back((
                child_dep_name.clone(),
                dep_manifest.dependencies[&child_dep_name].clone(),
                format!("dependency `{dep_name}`"),
            ));
        }
    }

    let mut package_id_by_name: BTreeMap<String, String> = BTreeMap::new();
    for (name, package) in &interim {
        let source = dependency_source(&package.spec);
        package_id_by_name.insert(name.clone(), package_id(name, &source, &package.rev));
    }

    let mut packages: BTreeMap<String, ResolvedPackage> = BTreeMap::new();
    for (name, package) in interim {
        let source = dependency_source(&package.spec);
        let id = package_id_by_name
            .get(&name)
            .cloned()
            .ok_or_else(|| eyre::eyre!("internal error: missing package id for `{name}`"))?;
        let mut dependency_ids: Vec<String> = Vec::new();
        for dep_name in &package.dependency_names {
            let dep_id = package_id_by_name.get(dep_name).ok_or_else(|| {
                eyre::eyre!(
                    "internal error: dependency `{dep_name}` referenced by `{name}` was not resolved"
                )
            })?;
            dependency_ids.push(dep_id.clone());
        }
        dependency_ids.sort();
        dependency_ids.dedup();
        packages.insert(
            id.clone(),
            ResolvedPackage {
                id,
                name: package.name,
                source,
                spec: package.spec,
                checkout_dir: package.checkout_dir,
                rev: package.rev,
                dependencies: dependency_ids,
            },
        );
    }

    let roots: Vec<ResolvedRootDependency> = root_dep_names
        .into_iter()
        .map(|alias| {
            let package = package_id_by_name.get(&alias).cloned().ok_or_else(|| {
                eyre::eyre!("internal error: root dependency `{alias}` was not resolved")
            })?;
            Ok(ResolvedRootDependency {
                alias: alias.clone(),
                package,
                spec: manifest.dependencies[&alias].clone(),
            })
        })
        .collect::<eyre::Result<Vec<_>>>()?;

    Ok(ResolvedGraph { roots, packages })
}

// TODO: we should be much smarter in when we actually reload deps / do git ops
pub(crate) fn load_dependencies(
    project_dir: &Path,
    manifest: &manifest::BahnManifest,
) -> eyre::Result<LoadedDependencies> {
    let mut loaded = LoadedDependencies::default();
    let mut module_owner: HashMap<String, String> = HashMap::new();
    let resolved = resolve_dependencies(project_dir, manifest, false)?;
    let mut package_ids_by_name: BTreeMap<String, String> = BTreeMap::new();
    for package in resolved.packages.values() {
        if let Some(existing_id) =
            package_ids_by_name.insert(package.name.clone(), package.id.clone())
            && existing_id != package.id
        {
            return Err(eyre::eyre!(
                "multiple package instances for `{}` are not supported yet: `{}` and `{}`",
                package.name,
                existing_id,
                package.id
            ));
        }
    }

    let mut packages: Vec<&ResolvedPackage> = resolved.packages.values().collect();
    packages.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
    for package in packages {
        let dep_name = &package.name;
        let checkout_dir = &package.checkout_dir;
        let dep_loaded = load_dependency_from_checkout(dep_name, checkout_dir)?;
        for (module_name, _, _) in &dep_loaded.modules {
            if let Some(existing_dep) = module_owner.insert(module_name.clone(), dep_name.clone())
                && existing_dep != *dep_name
            {
                return Err(eyre::eyre!(
                    "dependency module name collision: module `{module_name}` is provided by both `{existing_dep}` and `{dep_name}`"
                ));
            }
        }
        loaded.modules.extend(dep_loaded.modules);
        loaded.helper_erls.extend(dep_loaded.helper_erls);
    }

    Ok(loaded)
}

pub(crate) fn update_dependencies(project_dir: &Path) -> eyre::Result<Vec<String>> {
    let manifest = manifest::read_manifest(project_dir.to_path_buf())?;
    let resolved = resolve_dependencies(project_dir, &manifest, true)?;
    Ok(resolved
        .packages
        .values()
        .map(|package| package.name.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}

fn checkout_dependency(
    project_dir: &Path,
    dep_name: &str,
    dep_spec: &manifest::DependencySpec,
) -> eyre::Result<PathBuf> {
    checkout_dependency_with_policy(project_dir, dep_name, dep_spec, false)
}

fn checkout_dependency_with_policy(
    project_dir: &Path,
    dep_name: &str,
    dep_spec: &manifest::DependencySpec,
    refresh: bool,
) -> eyre::Result<PathBuf> {
    let deps_dir = project_dir.join(TARGET_DIR).join("deps");
    std::fs::create_dir_all(&deps_dir).with_context(|| {
        format!(
            "could not create dependency cache at {}",
            deps_dir.display()
        )
    })?;

    let checkout_dir = deps_dir.join(dep_name);
    let git_dir = checkout_dir.join(".git");
    let git_dir_exists = git_dir.exists();

    if git_dir_exists {
        if refresh {
            info(&format!("Fetching dependency: {dep_name}"));
            run_git(
                Some(&checkout_dir),
                &["fetch", "--quiet", "--tags", "--prune", "origin"],
                "failed to fetch dependency",
            )?;
            success(&format!("Fetched dependency: {dep_name}"));
        }
    } else if checkout_dir.exists() {
        return Err(eyre::eyre!(
            "dependency cache path {} exists but is not a git repository; remove it and retry",
            checkout_dir.display()
        ));
    } else {
        info(&format!("Cloning dependency: {dep_name}"));
        run_git(
            None,
            &[
                "clone",
                "--quiet",
                "--",
                dep_spec.git.as_str(),
                checkout_dir
                    .to_str()
                    .ok_or_else(|| eyre::eyre!("invalid checkout path"))?,
            ],
            "failed to clone dependency",
        )?;

        success(&format!("Cloned dependency: {dep_name}"));
    }

    if let Err(err) = checkout_dependency_reference(dep_name, dep_spec, &checkout_dir) {
        if !refresh && git_dir_exists {
            info(&format!(
                "Refreshing dependency after checkout failed: {dep_name}"
            ));
            run_git(
                Some(&checkout_dir),
                &["fetch", "--quiet", "--tags", "--prune", "origin"],
                "failed to fetch dependency",
            )?;
            success(&format!("Fetched dependency: {dep_name}"));
            checkout_dependency_reference(dep_name, dep_spec, &checkout_dir).map_err(
                |retry_err| {
                    eyre::eyre!("{retry_err}\ninitial checkout failed before refresh: {err}")
                },
            )?;
        } else {
            return Err(err);
        }
    }

    validate_dependency_manifest_name(dep_name, &checkout_dir)?;

    Ok(checkout_dir)
}

fn checkout_dependency_reference(
    dep_name: &str,
    dep_spec: &manifest::DependencySpec,
    checkout_dir: &Path,
) -> eyre::Result<()> {
    match &dep_spec.reference {
        manifest::GitReference::Tag(tag) => {
            info(&format!(
                "Checking out dependency: {dep_name} using tag: {tag}"
            ));
            run_git(
                Some(checkout_dir),
                &["checkout", "--quiet", &format!("refs/tags/{tag}")],
                "failed to checkout dependency tag",
            )?;
            success(&format!(
                "Checked out dependency: {dep_name} using tag: {tag}"
            ));
        }
        manifest::GitReference::Branch(branch) => {
            info(&format!(
                "Checking out dependency: {dep_name} using branch: {branch}"
            ));

            run_git(
                Some(checkout_dir),
                &[
                    "checkout",
                    "--quiet",
                    "-B",
                    branch,
                    &format!("origin/{branch}"),
                ],
                "failed to checkout dependency branch",
            )?;

            success(&format!(
                "Checked out dependency: {dep_name} using branch: {branch}"
            ));
        }
        manifest::GitReference::Rev(rev) => {
            if let Ok(current_rev) = current_dependency_rev(checkout_dir)
                && current_rev == *rev
            {
                return Ok(());
            }

            info(&format!(
                "Checking out dependency: {dep_name} using rev: {rev}"
            ));

            run_git(
                Some(checkout_dir),
                &["checkout", "--quiet", rev],
                "failed to checkout dependency revision",
            )?;
            success(&format!(
                "Checked out dependency: {dep_name} using rev: {rev}"
            ));
        }
    }
    Ok(())
}

fn run_git(cwd: Option<&Path>, args: &[&str], context: &str) -> eyre::Result<()> {
    let mut cmd = Command::new("git");
    cmd.args(["-c", "alias.checkout=", "-c", "alias.switch="]);
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let output = cmd
        .output()
        .with_context(|| format!("{context}: could not run git"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(eyre::eyre!(
        "{context}: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    ))
}

fn run_git_output(cwd: Option<&Path>, args: &[&str], context: &str) -> eyre::Result<String> {
    let mut cmd = Command::new("git");
    cmd.args(["-c", "alias.checkout=", "-c", "alias.switch="]);
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let output = cmd
        .output()
        .with_context(|| format!("{context}: could not run git"))?;
    if !output.status.success() {
        return Err(eyre::eyre!(
            "{context}: {}\n{}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        return Err(eyre::eyre!("{context}: command returned empty output"));
    }
    Ok(stdout)
}

fn load_dependency_from_checkout(
    dep_name: &str,
    checkout_dir: &Path,
) -> eyre::Result<LoadedDependencies> {
    let src_dir = checkout_dir.join(SOURCE_DIR);
    let modules = mondc::load_dependency_modules_from_checkout(dep_name, checkout_dir)
        .map_err(|err| eyre::eyre!(err))?;

    let mut helper_erls: Vec<HelperErlFile> = Vec::new();
    for entry in WalkDir::new(&src_dir)
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
        let module_name = parse_erlang_module_name(&contents).unwrap_or(file_stem);
        helper_erls.push(HelperErlFile {
            file_name,
            module_name,
            contents,
        });
    }
    helper_erls.sort_by(|a, b| a.file_name.cmp(&b.file_name));

    Ok(LoadedDependencies {
        modules,
        helper_erls,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use semver::Version;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_root() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!("mond-deps-test-{}-{nanos}", std::process::id()))
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

    #[test]
    fn load_dependencies_returns_empty_without_dependencies() {
        let manifest = manifest::BahnManifest {
            package: manifest::Package {
                name: "app".to_string(),
                version: Version::new(0, 1, 0),
                min_mond_version: None,
            },
            dependencies: std::collections::HashMap::new(),
        };
        let root = unique_temp_root();
        std::fs::create_dir_all(&root).expect("create temp root");
        let loaded = load_dependencies(&root, &manifest).expect("load deps");
        assert!(loaded.modules.is_empty());
        assert!(loaded.helper_erls.is_empty());
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn load_dependencies_load_from_git_tag() {
        let root = unique_temp_root();
        std::fs::create_dir_all(&root).expect("create root");
        let std_repo = root.join("std-src");
        let std_src_dir = std_repo.join("src");
        std::fs::create_dir_all(&std_src_dir).expect("create std src");
        std::fs::write(
            std_repo.join(crate::MANIFEST_NAME),
            r#"[package]
name = "std"
version = "0.0.1"
mond_version = "0.1.0"

[dependencies]
"#,
        )
        .expect("write std manifest");
        std::fs::write(std_src_dir.join("lib.mond"), "(pub let hello {} \"hello\")")
            .expect("write lib.mond");
        std::fs::write(std_src_dir.join("io.mond"), "(pub let println {x} x)")
            .expect("write io.mond");
        std::fs::write(
            std_src_dir.join("mond_std_helpers.erl"),
            "-module(mond_std_helpers).\n",
        )
        .expect("write helper");

        run_ok(Command::new("git").arg("init").current_dir(&std_repo));
        run_ok(
            Command::new("git")
                .args(["add", "."])
                .current_dir(&std_repo),
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
                .current_dir(&std_repo),
        );
        run_ok(
            Command::new("git")
                .args(["tag", "0.0.1"])
                .current_dir(&std_repo),
        );

        let project_dir = root.join("app");
        std::fs::create_dir_all(&project_dir).expect("create project");
        let manifest = manifest::BahnManifest {
            package: manifest::Package {
                name: "app".to_string(),
                version: Version::new(0, 1, 0),
                min_mond_version: None,
            },
            dependencies: std::collections::HashMap::from([(
                "std".to_string(),
                manifest::DependencySpec {
                    git: format!("file://{}", std_repo.display()),
                    reference: manifest::GitReference::Tag("0.0.1".to_string()),
                },
            )]),
        };

        let loaded = load_dependencies(&project_dir, &manifest).expect("load dependency");
        let names: std::collections::HashSet<String> = loaded
            .modules
            .iter()
            .map(|(name, _, _)| name.clone())
            .collect();
        assert!(names.contains("std"));
        assert!(names.contains("io"));
        assert!(
            loaded
                .modules
                .iter()
                .any(|(name, erl, _)| name == "std" && erl == "d_std_std")
        );
        assert!(
            loaded
                .modules
                .iter()
                .any(|(name, erl, _)| name == "io" && erl == "d_std_io")
        );
        assert!(
            loaded
                .helper_erls
                .iter()
                .any(|h| h.file_name == "mond_std_helpers.erl")
        );

        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn load_dependencies_load_non_std_dependency_from_git_tag() {
        let root = unique_temp_root();
        std::fs::create_dir_all(&root).expect("create root");
        let dep_repo = root.join("time-src");
        let dep_src_dir = dep_repo.join("src");
        std::fs::create_dir_all(&dep_src_dir).expect("create dependency src");
        std::fs::write(
            dep_repo.join(crate::MANIFEST_NAME),
            r#"[package]
name = "time"
version = "0.0.1"
mond_version = "0.1.0"

[dependencies]
"#,
        )
        .expect("write dependency manifest");
        std::fs::write(dep_src_dir.join("lib.mond"), "(pub let now {} 1)").expect("write lib");
        std::fs::write(dep_src_dir.join("format.mond"), "(pub let iso {x} x)")
            .expect("write format module");

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
        std::fs::create_dir_all(&project_dir).expect("create project");
        let manifest = manifest::BahnManifest {
            package: manifest::Package {
                name: "app".to_string(),
                version: Version::new(0, 1, 0),
                min_mond_version: None,
            },
            dependencies: std::collections::HashMap::from([(
                "time".to_string(),
                manifest::DependencySpec {
                    git: format!("file://{}", dep_repo.display()),
                    reference: manifest::GitReference::Tag("0.0.1".to_string()),
                },
            )]),
        };

        let loaded = load_dependencies(&project_dir, &manifest).expect("load dependency");
        assert!(
            loaded
                .modules
                .iter()
                .any(|(name, erl, _)| name == "time" && erl == "d_time_time")
        );
        assert!(
            loaded
                .modules
                .iter()
                .any(|(name, erl, _)| name == "format" && erl == "d_time_format")
        );

        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn load_dependencies_use_cached_checkout_without_fetching() {
        let root = unique_temp_root();
        std::fs::create_dir_all(&root).expect("create root");
        let std_repo = root.join("std-src");
        let std_src_dir = std_repo.join("src");
        std::fs::create_dir_all(&std_src_dir).expect("create std src");
        std::fs::write(
            std_repo.join(crate::MANIFEST_NAME),
            r#"[package]
name = "std"
version = "0.0.1"
mond_version = "0.1.0"

[dependencies]
"#,
        )
        .expect("write std manifest");
        std::fs::write(std_src_dir.join("lib.mond"), "(pub let hello {} \"hello\")")
            .expect("write lib.mond");

        run_ok(Command::new("git").arg("init").current_dir(&std_repo));
        run_ok(
            Command::new("git")
                .args(["add", "."])
                .current_dir(&std_repo),
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
                .current_dir(&std_repo),
        );
        run_ok(
            Command::new("git")
                .args(["tag", "0.0.1"])
                .current_dir(&std_repo),
        );

        let project_dir = root.join("app");
        std::fs::create_dir_all(&project_dir).expect("create project");
        let manifest = manifest::BahnManifest {
            package: manifest::Package {
                name: "app".to_string(),
                version: Version::new(0, 1, 0),
                min_mond_version: None,
            },
            dependencies: std::collections::HashMap::from([(
                "std".to_string(),
                manifest::DependencySpec {
                    git: format!("file://{}", std_repo.display()),
                    reference: manifest::GitReference::Tag("0.0.1".to_string()),
                },
            )]),
        };

        let initial = load_dependencies(&project_dir, &manifest).expect("initial load");
        assert!(
            initial.modules.iter().any(|(name, _, _)| name == "std"),
            "expected initial clone to load std"
        );

        std::fs::remove_dir_all(&std_repo).expect("remove remote repo");

        let cached = load_dependencies(&project_dir, &manifest).expect("cached load");
        assert!(
            cached.modules.iter().any(|(name, _, _)| name == "std"),
            "expected cached checkout to be used without fetching"
        );

        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn load_dependencies_fetches_when_reference_changes_to_new_tag() {
        let root = unique_temp_root();
        std::fs::create_dir_all(&root).expect("create root");
        let std_repo = root.join("std-src");
        let std_src_dir = std_repo.join("src");
        std::fs::create_dir_all(&std_src_dir).expect("create std src");
        std::fs::write(
            std_repo.join(crate::MANIFEST_NAME),
            r#"[package]
name = "std"
version = "0.0.1"
mond_version = "0.1.0"

[dependencies]
"#,
        )
        .expect("write std manifest");
        std::fs::write(std_src_dir.join("lib.mond"), "(pub let hello {} \"hello\")")
            .expect("write lib.mond");

        run_ok(Command::new("git").arg("init").current_dir(&std_repo));
        run_ok(
            Command::new("git")
                .args(["add", "."])
                .current_dir(&std_repo),
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
                .current_dir(&std_repo),
        );
        run_ok(
            Command::new("git")
                .args(["tag", "0.0.1"])
                .current_dir(&std_repo),
        );

        let project_dir = root.join("app");
        std::fs::create_dir_all(&project_dir).expect("create project");
        let mut manifest = manifest::BahnManifest {
            package: manifest::Package {
                name: "app".to_string(),
                version: Version::new(0, 1, 0),
                min_mond_version: None,
            },
            dependencies: std::collections::HashMap::from([(
                "std".to_string(),
                manifest::DependencySpec {
                    git: format!("file://{}", std_repo.display()),
                    reference: manifest::GitReference::Tag("0.0.1".to_string()),
                },
            )]),
        };

        let initial = load_dependencies(&project_dir, &manifest).expect("initial load");
        assert!(
            initial.modules.iter().any(|(name, _, _)| name == "std"),
            "expected initial clone to load std"
        );

        std::fs::write(
            std_src_dir.join("lib.mond"),
            "(pub let hello {} \"hello-2\")",
        )
        .expect("write updated lib.mond");
        run_ok(
            Command::new("git")
                .args(["add", "."])
                .current_dir(&std_repo),
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
                    "second",
                ])
                .current_dir(&std_repo),
        );
        run_ok(
            Command::new("git")
                .args(["tag", "0.0.2"])
                .current_dir(&std_repo),
        );
        manifest.dependencies.insert(
            "std".to_string(),
            manifest::DependencySpec {
                git: format!("file://{}", std_repo.display()),
                reference: manifest::GitReference::Tag("0.0.2".to_string()),
            },
        );

        let reloaded = load_dependencies(&project_dir, &manifest)
            .expect("reload after switching manifest to newly created tag");
        assert!(
            reloaded.modules.iter().any(|(name, _, _)| name == "std"),
            "expected dependency reload after automatic fetch"
        );

        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn load_dependencies_write_lockfile_for_rev_dependency() {
        let root = unique_temp_root();
        std::fs::create_dir_all(&root).expect("create root");
        let std_repo = root.join("std-src");
        let std_src_dir = std_repo.join("src");
        std::fs::create_dir_all(&std_src_dir).expect("create std src");
        std::fs::write(
            std_repo.join(crate::MANIFEST_NAME),
            r#"[package]
name = "std"
version = "0.0.1"
mond_version = "0.1.0"

[dependencies]
"#,
        )
        .expect("write std manifest");
        std::fs::write(std_src_dir.join("lib.mond"), "(pub let hello {} \"hello\")")
            .expect("write lib.mond");

        run_ok(Command::new("git").arg("init").current_dir(&std_repo));
        run_ok(
            Command::new("git")
                .args(["add", "."])
                .current_dir(&std_repo),
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
                .current_dir(&std_repo),
        );
        let initial_rev = run_git_output(
            Some(&std_repo),
            &["rev-parse", "HEAD"],
            "resolve initial rev",
        )
        .expect("initial rev");

        let project_dir = root.join("app");
        std::fs::create_dir_all(&project_dir).expect("create project");
        let manifest = manifest::BahnManifest {
            package: manifest::Package {
                name: "app".to_string(),
                version: Version::new(0, 1, 0),
                min_mond_version: None,
            },
            dependencies: std::collections::HashMap::from([(
                "std".to_string(),
                manifest::DependencySpec {
                    git: format!("file://{}", std_repo.display()),
                    reference: manifest::GitReference::Rev(initial_rev.clone()),
                },
            )]),
        };

        let loaded = load_dependencies(&project_dir, &manifest).expect("load rev dependency");
        assert!(
            loaded.modules.iter().any(|(name, _, _)| name == "std"),
            "expected rev dependency to load"
        );

        let lock_src =
            std::fs::read_to_string(project_dir.join(crate::LOCKFILE_NAME)).expect("read lock");
        assert!(
            lock_src.contains("resolved_rev = "),
            "expected lockfile to write resolved_rev field: {lock_src}"
        );
        let lock: MondLock = toml::from_str(&lock_src).expect("parse lock");
        let std_package = lock
            .package
            .iter()
            .find(|p| p.name == "std")
            .expect("std package in lock");
        assert_eq!(std_package.resolved_rev, initial_rev);
        assert!(matches!(
            &std_package.reference,
            manifest::GitReference::Rev(rev) if rev == &std_package.resolved_rev
        ));

        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn load_dependencies_reject_duplicate_module_names_across_dependencies() {
        let root = unique_temp_root();
        std::fs::create_dir_all(&root).expect("create root");

        let make_dep = |name: &str| {
            let dep_repo = root.join(format!("{name}-src"));
            let dep_src_dir = dep_repo.join("src");
            std::fs::create_dir_all(&dep_src_dir).expect("create dependency src");
            std::fs::write(
                dep_repo.join(crate::MANIFEST_NAME),
                format!(
                    "[package]\nname = \"{name}\"\nversion = \"0.0.1\"\nmond_version = \"0.1.0\"\n\n[dependencies]\n"
                ),
            )
            .expect("write dependency manifest");
            std::fs::write(dep_src_dir.join("lib.mond"), "(pub let root {} 1)").expect("write lib");
            std::fs::write(dep_src_dir.join("io.mond"), "(pub let println {x} x)")
                .expect("write io module");

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
            dep_repo
        };

        let dep_a = make_dep("a");
        let dep_b = make_dep("b");

        let project_dir = root.join("app");
        std::fs::create_dir_all(&project_dir).expect("create project");
        let manifest = manifest::BahnManifest {
            package: manifest::Package {
                name: "app".to_string(),
                version: Version::new(0, 1, 0),
                min_mond_version: None,
            },
            dependencies: std::collections::HashMap::from([
                (
                    "a".to_string(),
                    manifest::DependencySpec {
                        git: format!("file://{}", dep_a.display()),
                        reference: manifest::GitReference::Tag("0.0.1".to_string()),
                    },
                ),
                (
                    "b".to_string(),
                    manifest::DependencySpec {
                        git: format!("file://{}", dep_b.display()),
                        reference: manifest::GitReference::Tag("0.0.1".to_string()),
                    },
                ),
            ]),
        };

        let err = load_dependencies(&project_dir, &manifest).expect_err("expected collision");
        assert!(
            err.to_string().contains(
                "dependency module name collision: module `io` is provided by both `a` and `b`"
            ),
            "unexpected error: {err}"
        );

        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn load_dependencies_reject_mismatched_dependency_package_name() {
        let root = unique_temp_root();
        std::fs::create_dir_all(&root).expect("create root");
        let std_repo = root.join("std-src");
        let std_src_dir = std_repo.join("src");
        std::fs::create_dir_all(&std_src_dir).expect("create std src");
        std::fs::write(
            std_repo.join(crate::MANIFEST_NAME),
            r#"[package]
name = "not_std"
version = "0.0.1"
mond_version = "0.1.0"

[dependencies]
"#,
        )
        .expect("write std manifest");
        std::fs::write(std_src_dir.join("lib.mond"), "(pub let hello {} \"hello\")")
            .expect("write lib.mond");

        run_ok(Command::new("git").arg("init").current_dir(&std_repo));
        run_ok(
            Command::new("git")
                .args(["add", "."])
                .current_dir(&std_repo),
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
                .current_dir(&std_repo),
        );
        run_ok(
            Command::new("git")
                .args(["tag", "0.0.1"])
                .current_dir(&std_repo),
        );

        let project_dir = root.join("app");
        std::fs::create_dir_all(&project_dir).expect("create project");
        let manifest = manifest::BahnManifest {
            package: manifest::Package {
                name: "app".to_string(),
                version: Version::new(0, 1, 0),
                min_mond_version: None,
            },
            dependencies: std::collections::HashMap::from([(
                "std".to_string(),
                manifest::DependencySpec {
                    git: format!("file://{}", std_repo.display()),
                    reference: manifest::GitReference::Tag("0.0.1".to_string()),
                },
            )]),
        };

        let err = load_dependencies(&project_dir, &manifest).expect_err("expected mismatch");
        assert!(
            err.to_string()
                .contains("dependency `std` points to package `not_std`; expected `std`"),
            "unexpected error: {err}"
        );

        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn load_dependencies_resolve_transitive_dependencies_and_write_lockfile() {
        let root = unique_temp_root();
        std::fs::create_dir_all(&root).expect("create root");

        let std_repo = root.join("std-src");
        let std_src_dir = std_repo.join("src");
        std::fs::create_dir_all(&std_src_dir).expect("create std src");
        std::fs::write(
            std_repo.join(crate::MANIFEST_NAME),
            r#"[package]
name = "std"
version = "0.0.1"
mond_version = "0.1.0"

[dependencies]
"#,
        )
        .expect("write std manifest");
        std::fs::write(std_src_dir.join("lib.mond"), "(pub let hello {} \"hello\")")
            .expect("write std lib");
        std::fs::write(std_src_dir.join("list.mond"), "(pub let map {f xs} xs)")
            .expect("write std list");
        run_ok(Command::new("git").arg("init").current_dir(&std_repo));
        run_ok(
            Command::new("git")
                .args(["add", "."])
                .current_dir(&std_repo),
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
                .current_dir(&std_repo),
        );
        run_ok(
            Command::new("git")
                .args(["tag", "0.0.1"])
                .current_dir(&std_repo),
        );

        let time_repo = root.join("time-src");
        let time_src_dir = time_repo.join("src");
        std::fs::create_dir_all(&time_src_dir).expect("create time src");
        std::fs::write(
            time_repo.join(crate::MANIFEST_NAME),
            format!(
                r#"[package]
name = "time"
version = "0.0.1"
mond_version = "0.1.0"

[dependencies]
std = {{ git = "file://{}", tag = "0.0.1" }}
"#,
                std_repo.display()
            ),
        )
        .expect("write time manifest");
        std::fs::write(time_src_dir.join("lib.mond"), "(pub let now {} 1)").expect("write lib");
        run_ok(Command::new("git").arg("init").current_dir(&time_repo));
        run_ok(
            Command::new("git")
                .args(["add", "."])
                .current_dir(&time_repo),
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
                .current_dir(&time_repo),
        );
        run_ok(
            Command::new("git")
                .args(["tag", "0.0.1"])
                .current_dir(&time_repo),
        );

        let project_dir = root.join("app");
        std::fs::create_dir_all(&project_dir).expect("create project");
        let manifest = manifest::BahnManifest {
            package: manifest::Package {
                name: "app".to_string(),
                version: Version::new(0, 1, 0),
                min_mond_version: None,
            },
            dependencies: std::collections::HashMap::from([(
                "time".to_string(),
                manifest::DependencySpec {
                    git: format!("file://{}", time_repo.display()),
                    reference: manifest::GitReference::Tag("0.0.1".to_string()),
                },
            )]),
        };

        let loaded = load_dependencies(&project_dir, &manifest).expect("load dependencies");
        let names: std::collections::HashSet<String> = loaded
            .modules
            .iter()
            .map(|(name, _, _)| name.clone())
            .collect();
        assert!(
            names.contains("time"),
            "expected transitive graph to include time"
        );
        assert!(
            names.contains("std"),
            "expected transitive graph to include std"
        );
        assert!(
            names.contains("list"),
            "expected std submodules to be available through transitive resolution"
        );

        let lock_src =
            std::fs::read_to_string(project_dir.join(crate::LOCKFILE_NAME)).expect("read lock");
        let lock: MondLock = toml::from_str(&lock_src).expect("parse lock");
        assert_eq!(lock.version, LOCKFILE_FORMAT_VERSION);
        assert_eq!(lock.root.len(), 1);
        assert_eq!(lock.root[0].alias, "time");

        let mut lock_names: Vec<String> = lock.package.iter().map(|p| p.name.clone()).collect();
        lock_names.sort();
        assert_eq!(lock_names, vec!["std".to_string(), "time".to_string()]);

        let package_by_name: std::collections::BTreeMap<String, LockedPackage> = lock
            .package
            .iter()
            .cloned()
            .map(|p| (p.name.clone(), p))
            .collect();
        let time_package = package_by_name.get("time").expect("time package");
        let std_package = package_by_name.get("std").expect("std package");
        assert_eq!(lock.root[0].package, time_package.id);
        assert_eq!(time_package.dependencies, vec![std_package.id.clone()]);
        assert!(std_package.dependencies.is_empty());

        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn load_dependencies_reject_conflicting_transitive_dependency_specs() {
        let root = unique_temp_root();
        std::fs::create_dir_all(&root).expect("create root");

        let util_repo = root.join("util-src");
        let util_src_dir = util_repo.join("src");
        std::fs::create_dir_all(&util_src_dir).expect("create util src");
        std::fs::write(
            util_repo.join(crate::MANIFEST_NAME),
            r#"[package]
name = "util"
version = "0.0.1"
mond_version = "0.1.0"

[dependencies]
"#,
        )
        .expect("write util manifest");
        std::fs::write(util_src_dir.join("lib.mond"), "(pub let version {} 1)").expect("write v1");
        run_ok(Command::new("git").arg("init").current_dir(&util_repo));
        run_ok(
            Command::new("git")
                .args(["add", "."])
                .current_dir(&util_repo),
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
                    "v1",
                ])
                .current_dir(&util_repo),
        );
        run_ok(
            Command::new("git")
                .args(["tag", "0.0.1"])
                .current_dir(&util_repo),
        );
        std::fs::write(util_src_dir.join("lib.mond"), "(pub let version {} 2)").expect("write v2");
        run_ok(
            Command::new("git")
                .args(["add", "."])
                .current_dir(&util_repo),
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
                    "v2",
                ])
                .current_dir(&util_repo),
        );
        run_ok(
            Command::new("git")
                .args(["tag", "0.0.2"])
                .current_dir(&util_repo),
        );

        let make_dep = |name: &str, util_tag: &str| {
            let dep_repo = root.join(format!("{name}-src"));
            let dep_src_dir = dep_repo.join("src");
            std::fs::create_dir_all(&dep_src_dir).expect("create dep src");
            std::fs::write(
                dep_repo.join(crate::MANIFEST_NAME),
                format!(
                    r#"[package]
name = "{name}"
version = "0.0.1"
mond_version = "0.1.0"

[dependencies]
util = {{ git = "file://{}", tag = "{util_tag}" }}
"#,
                    util_repo.display()
                ),
            )
            .expect("write dep manifest");
            std::fs::write(dep_src_dir.join("lib.mond"), "(pub let root {} 1)").expect("write lib");
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
            dep_repo
        };

        let dep_a = make_dep("a", "0.0.1");
        let dep_b = make_dep("b", "0.0.2");

        let project_dir = root.join("app");
        std::fs::create_dir_all(&project_dir).expect("create project");
        let manifest = manifest::BahnManifest {
            package: manifest::Package {
                name: "app".to_string(),
                version: Version::new(0, 1, 0),
                min_mond_version: None,
            },
            dependencies: std::collections::HashMap::from([
                (
                    "a".to_string(),
                    manifest::DependencySpec {
                        git: format!("file://{}", dep_a.display()),
                        reference: manifest::GitReference::Tag("0.0.1".to_string()),
                    },
                ),
                (
                    "b".to_string(),
                    manifest::DependencySpec {
                        git: format!("file://{}", dep_b.display()),
                        reference: manifest::GitReference::Tag("0.0.1".to_string()),
                    },
                ),
            ]),
        };

        let err = load_dependencies(&project_dir, &manifest).expect_err("expected conflict");
        assert!(
            err.to_string()
                .contains("dependency `util` has conflicting requirements"),
            "unexpected error: {err}"
        );

        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn update_dependencies_includes_transitive_dependencies_and_updates_lockfile() {
        let root = unique_temp_root();
        std::fs::create_dir_all(&root).expect("create root");

        let std_repo = root.join("std-src");
        let std_src_dir = std_repo.join("src");
        std::fs::create_dir_all(&std_src_dir).expect("create std src");
        std::fs::write(
            std_repo.join(crate::MANIFEST_NAME),
            r#"[package]
name = "std"
version = "0.0.1"
mond_version = "0.1.0"

[dependencies]
"#,
        )
        .expect("write std manifest");
        std::fs::write(std_src_dir.join("lib.mond"), "(pub let hello {} \"hello\")")
            .expect("write std lib");
        run_ok(Command::new("git").arg("init").current_dir(&std_repo));
        run_ok(
            Command::new("git")
                .args(["add", "."])
                .current_dir(&std_repo),
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
                .current_dir(&std_repo),
        );
        run_ok(
            Command::new("git")
                .args(["tag", "0.0.1"])
                .current_dir(&std_repo),
        );

        let time_repo = root.join("time-src");
        let time_src_dir = time_repo.join("src");
        std::fs::create_dir_all(&time_src_dir).expect("create time src");
        std::fs::write(
            time_repo.join(crate::MANIFEST_NAME),
            format!(
                r#"[package]
name = "time"
version = "0.0.1"
mond_version = "0.1.0"

[dependencies]
std = {{ git = "file://{}", tag = "0.0.1" }}
"#,
                std_repo.display()
            ),
        )
        .expect("write time manifest");
        std::fs::write(time_src_dir.join("lib.mond"), "(pub let now {} 1)").expect("write lib");
        run_ok(Command::new("git").arg("init").current_dir(&time_repo));
        run_ok(
            Command::new("git")
                .args(["add", "."])
                .current_dir(&time_repo),
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
                .current_dir(&time_repo),
        );
        run_ok(
            Command::new("git")
                .args(["tag", "0.0.1"])
                .current_dir(&time_repo),
        );

        let project_dir = root.join("app");
        std::fs::create_dir_all(&project_dir).expect("create project");
        let manifest = manifest::BahnManifest {
            package: manifest::Package {
                name: "app".to_string(),
                version: Version::new(0, 1, 0),
                min_mond_version: None,
            },
            dependencies: std::collections::HashMap::from([(
                "time".to_string(),
                manifest::DependencySpec {
                    git: format!("file://{}", time_repo.display()),
                    reference: manifest::GitReference::Tag("0.0.1".to_string()),
                },
            )]),
        };
        manifest::write_manifest(&manifest, &project_dir.join(crate::MANIFEST_NAME))
            .expect("write project manifest");

        let updated = update_dependencies(&project_dir).expect("update dependencies");
        assert_eq!(updated, vec!["std".to_string(), "time".to_string()]);

        let lock_src =
            std::fs::read_to_string(project_dir.join(crate::LOCKFILE_NAME)).expect("read lock");
        let lock: MondLock = toml::from_str(&lock_src).expect("parse lock");
        let mut lock_names: Vec<String> = lock.package.iter().map(|p| p.name.clone()).collect();
        lock_names.sort();
        assert_eq!(lock_names, vec!["std".to_string(), "time".to_string()]);
        assert_eq!(lock.root.len(), 1);
        assert_eq!(lock.root[0].alias, "time");

        std::fs::remove_dir_all(root).expect("cleanup");
    }
}
