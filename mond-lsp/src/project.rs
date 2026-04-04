use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use tower_lsp::lsp_types::{
    CompletionItem, CompletionItemKind, Location, Range, SymbolInformation, SymbolKind, Url,
};

use crate::{
    ModuleSource, OccurrenceKind, Symbol,
    analysis::DocumentAnalysis,
    build_project_analysis, byte_range_to_lsp_range, collect_record_fields,
    collect_symbol_occurrences, completion_item, diagnostic_to_lsp,
    external_package_name_for_module_path, find_top_level_definition_range, local_names_at_offset,
    local_type_decls, module_name_for_path, package_name_from_manifest, push_completion_item,
    source_path_for_compile,
    state::{
        AnalysisCacheKey, CachedDocumentState, CachedModuleDiagnostics, DocumentState,
        IndexedModuleFile, ServerState, WorkspaceState,
    },
    top_level_docs, top_level_symbols, visible_exports,
};

#[derive(Clone)]
pub(crate) struct Project {
    pub(crate) root: Option<PathBuf>,
    pub(crate) external_modules: Arc<BTreeMap<String, ModuleSource>>,
    pub(crate) src_modules: Arc<BTreeMap<String, ModuleSource>>,
    pub(crate) test_modules: Arc<BTreeMap<String, ModuleSource>>,
    pub(crate) analysis: Arc<mondc::ProjectAnalysis>,
    pub(crate) analysis_generation: u64,
    pub(crate) workspace_generation: u64,
    document_revisions: Arc<HashMap<PathBuf, u64>>,
    workspace: Option<Arc<Mutex<WorkspaceState>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModuleSetKind {
    Src,
    Test,
    External,
}

const UNWATCHED_FULL_REFRESH_INTERVAL: Duration = Duration::from_millis(750);

#[derive(Debug, Default)]
pub(crate) struct WorkspaceRefreshResult {
    pub(crate) dirty_modules: HashSet<String>,
    pub(crate) force_full_reconcile: bool,
}

fn file_modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
}

fn is_mond_file(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()) == Some("mond")
}

fn external_module_name_for_path(path: &Path) -> String {
    let module_name = module_name_for_path(path);
    if module_name == "lib" {
        external_package_name_for_module_path(path)
            .unwrap_or("lib")
            .to_string()
    } else {
        module_name
    }
}

fn module_source_for_path(kind: ModuleSetKind, path: &Path, source: String) -> ModuleSource {
    let name = match kind {
        ModuleSetKind::External => external_module_name_for_path(path),
        ModuleSetKind::Src | ModuleSetKind::Test => module_name_for_path(path),
    };
    ModuleSource {
        name,
        path: path.to_path_buf(),
        source,
    }
}

fn collect_mond_paths_from_dir(dir: &Path, paths: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_mond_paths_from_dir(&path, paths);
        } else if is_mond_file(&path) {
            paths.push(path);
        }
    }
}

fn disk_module_paths(root: &Path, kind: ModuleSetKind) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    match kind {
        ModuleSetKind::Src => collect_mond_paths_from_dir(&root.join("src"), &mut paths),
        ModuleSetKind::Test => collect_mond_paths_from_dir(&root.join("tests"), &mut paths),
        ModuleSetKind::External => {
            let deps_root = root.join("target").join("deps");
            let Ok(entries) = fs::read_dir(&deps_root) else {
                return paths;
            };
            for entry in entries.flatten() {
                let dep_dir = entry.path();
                if dep_dir.is_dir() {
                    collect_mond_paths_from_dir(&dep_dir.join("src"), &mut paths);
                }
            }
        }
    }
    paths
}

fn path_kind(root: &Path, path: &Path) -> Option<ModuleSetKind> {
    if path.starts_with(root.join("src")) && is_mond_file(path) {
        return Some(ModuleSetKind::Src);
    }
    if path.starts_with(root.join("tests")) && is_mond_file(path) {
        return Some(ModuleSetKind::Test);
    }
    if path.starts_with(root.join("target").join("deps")) && is_mond_file(path) {
        return Some(ModuleSetKind::External);
    }
    None
}

fn module_set_refs(
    workspace: &WorkspaceState,
    kind: ModuleSetKind,
) -> (
    &Arc<BTreeMap<String, ModuleSource>>,
    &HashMap<PathBuf, IndexedModuleFile>,
) {
    match kind {
        ModuleSetKind::Src => (&workspace.src_modules, &workspace.src_files),
        ModuleSetKind::Test => (&workspace.test_modules, &workspace.test_files),
        ModuleSetKind::External => (&workspace.external_modules, &workspace.external_files),
    }
}

fn module_set_mut(
    workspace: &mut WorkspaceState,
    kind: ModuleSetKind,
) -> (
    &mut Arc<BTreeMap<String, ModuleSource>>,
    &mut HashMap<PathBuf, IndexedModuleFile>,
) {
    match kind {
        ModuleSetKind::Src => (&mut workspace.src_modules, &mut workspace.src_files),
        ModuleSetKind::Test => (&mut workspace.test_modules, &mut workspace.test_files),
        ModuleSetKind::External => (
            &mut workspace.external_modules,
            &mut workspace.external_files,
        ),
    }
}

fn module_name_for_workspace_path(
    workspace: &WorkspaceState,
    kind: ModuleSetKind,
    path: &Path,
) -> Option<String> {
    match kind {
        ModuleSetKind::Src => workspace
            .src_files
            .get(path)
            .map(|file| file.module_name.clone()),
        ModuleSetKind::Test => workspace
            .test_files
            .get(path)
            .map(|file| file.module_name.clone()),
        ModuleSetKind::External => workspace
            .external_files
            .get(path)
            .map(|file| file.module_name.clone()),
    }
}

fn refresh_workspace_dependency_graph(workspace: &mut WorkspaceState) {
    let mut import_graph: HashMap<String, HashSet<String>> = HashMap::new();
    let mut reverse_graph: HashMap<String, HashSet<String>> = HashMap::new();
    let module_aliases = workspace
        .analysis
        .as_ref()
        .map(|analysis| analysis.module_aliases.clone())
        .unwrap_or_default();
    let local_module_names: HashSet<String> = workspace
        .src_modules
        .keys()
        .chain(workspace.test_modules.keys())
        .cloned()
        .collect();

    for module in workspace
        .src_modules
        .values()
        .chain(workspace.test_modules.values())
    {
        let imports = import_graph.entry(module.name.clone()).or_default();
        reverse_graph.entry(module.name.clone()).or_default();
        for (_, imported_module, _) in mondc::used_modules(&module.source) {
            let resolved_module = module_aliases
                .get(&imported_module)
                .cloned()
                .unwrap_or(imported_module);
            if resolved_module == module.name {
                continue;
            }
            imports.insert(resolved_module.clone());
            reverse_graph
                .entry(resolved_module)
                .or_default()
                .insert(module.name.clone());
        }
    }

    for module_name in local_module_names {
        import_graph.entry(module_name.clone()).or_default();
        reverse_graph.entry(module_name).or_default();
    }

    workspace.module_import_graph = import_graph;
    workspace.module_reverse_import_graph = reverse_graph;
}

fn prune_module_diagnostics_cache(workspace: &mut WorkspaceState) {
    let active_module_paths: HashSet<&Path> = workspace
        .src_modules
        .values()
        .chain(workspace.test_modules.values())
        .map(|module| module.path.as_path())
        .collect();
    workspace
        .module_diagnostics_cache
        .retain(|module_path, _| active_module_paths.contains(module_path.as_path()));
}

fn tracked_module_source<'a>(
    modules: &'a Arc<BTreeMap<String, ModuleSource>>,
    files: &HashMap<PathBuf, IndexedModuleFile>,
    path: &Path,
) -> Option<&'a ModuleSource> {
    files
        .get(path)
        .and_then(|file| modules.get(&file.module_name))
        .filter(|module| module.path == path)
}

fn next_revision(workspace: &mut WorkspaceState) -> u64 {
    let revision = workspace.next_revision;
    workspace.next_revision += 1;
    revision
}

fn upsert_module(
    workspace: &mut WorkspaceState,
    kind: ModuleSetKind,
    path: &Path,
    source: String,
    modified: Option<SystemTime>,
) -> bool {
    let module = module_source_for_path(kind, path, source);
    let (previous_name, previous_source) = {
        let (modules, files) = module_set_refs(workspace, kind);
        let previous_name = files.get(path).map(|file| file.module_name.clone());
        let previous_source =
            tracked_module_source(modules, files, path).map(|module| module.source.clone());
        (previous_name, previous_source)
    };
    let changed = previous_source.as_deref() != Some(module.source.as_str())
        || previous_name.as_deref() != Some(module.name.as_str());

    let (modules, files) = module_set_mut(workspace, kind);
    if let Some(previous_name) = previous_name
        && previous_name != module.name
    {
        Arc::make_mut(modules).remove(&previous_name);
    }
    Arc::make_mut(modules).insert(module.name.clone(), module.clone());
    files.insert(
        path.to_path_buf(),
        IndexedModuleFile {
            module_name: module.name.clone(),
            modified,
        },
    );

    if changed {
        let revision = next_revision(workspace);
        Arc::make_mut(&mut workspace.document_revisions).insert(path.to_path_buf(), revision);
    }
    changed
}

fn update_file_metadata(
    workspace: &mut WorkspaceState,
    kind: ModuleSetKind,
    path: &Path,
    modified: Option<SystemTime>,
) {
    let module_name = {
        let (modules, files) = module_set_refs(workspace, kind);
        files
            .get(path)
            .map(|file| file.module_name.clone())
            .or_else(|| {
                tracked_module_source(modules, files, path).map(|module| module.name.clone())
            })
            .unwrap_or_else(|| match kind {
                ModuleSetKind::External => external_module_name_for_path(path),
                ModuleSetKind::Src | ModuleSetKind::Test => module_name_for_path(path),
            })
    };
    let (_, files) = module_set_mut(workspace, kind);
    files.insert(
        path.to_path_buf(),
        IndexedModuleFile {
            module_name,
            modified,
        },
    );
}

fn remove_module(workspace: &mut WorkspaceState, kind: ModuleSetKind, path: &Path) -> bool {
    let previous_name = {
        let (_, files) = module_set_refs(workspace, kind);
        files.get(path).map(|file| file.module_name.clone())
    };
    let Some(previous_name) = previous_name else {
        Arc::make_mut(&mut workspace.document_revisions).remove(path);
        return false;
    };
    let (modules, files) = module_set_mut(workspace, kind);
    files.remove(path);
    Arc::make_mut(modules).remove(&previous_name);
    Arc::make_mut(&mut workspace.document_revisions).remove(path);
    true
}

fn refresh_manifest(workspace: &mut WorkspaceState, root: &Path) -> bool {
    let manifest_path = root.join("bahn.toml");
    let package_name = package_name_from_manifest(Some(root));
    let changed = workspace.package_name != package_name;
    workspace.package_name = package_name;
    workspace.manifest_modified = file_modified(&manifest_path);
    changed
}

fn refresh_disk_modules(workspace: &mut WorkspaceState, root: &Path, kind: ModuleSetKind) -> bool {
    let overlay_paths = workspace.overlay_paths.clone();
    let disk_paths = disk_module_paths(root, kind);
    let mut changed = false;
    let mut seen = HashSet::new();

    for path in disk_paths {
        seen.insert(path.clone());
        let modified = file_modified(&path);
        if overlay_paths.contains(&path) {
            update_file_metadata(workspace, kind, &path, modified);
            continue;
        }

        let unchanged = {
            let (modules, files) = module_set_refs(workspace, kind);
            files
                .get(&path)
                .is_some_and(|file| file.modified == modified)
                && tracked_module_source(modules, files, &path).is_some()
        };
        if unchanged {
            continue;
        }

        let Ok(source) = fs::read_to_string(&path) else {
            continue;
        };
        changed |= upsert_module(workspace, kind, &path, source, modified);
    }

    let existing_paths = {
        let (_, files) = module_set_refs(workspace, kind);
        files.keys().cloned().collect::<Vec<_>>()
    };
    for path in existing_paths {
        if seen.contains(&path) {
            continue;
        }
        if overlay_paths.contains(&path) {
            update_file_metadata(workspace, kind, &path, None);
            continue;
        }
        changed |= remove_module(workspace, kind, &path);
    }

    changed
}

fn refresh_disk_module_path(workspace: &mut WorkspaceState, root: &Path, path: &Path) -> bool {
    let Some(kind) = path_kind(root, path) else {
        return false;
    };
    if kind != ModuleSetKind::External && workspace.overlay_paths.contains(path) {
        update_file_metadata(workspace, kind, path, file_modified(path));
        return false;
    }
    if path.exists() {
        let modified = file_modified(path);
        let unchanged = {
            let (modules, files) = module_set_refs(workspace, kind);
            files
                .get(path)
                .is_some_and(|file| file.modified == modified)
                && tracked_module_source(modules, files, path).is_some()
        };
        if unchanged {
            return false;
        }
        let Ok(source) = fs::read_to_string(path) else {
            return false;
        };
        return upsert_module(workspace, kind, path, source, modified);
    }
    if workspace.overlay_paths.contains(path) && kind != ModuleSetKind::External {
        update_file_metadata(workspace, kind, path, None);
        return false;
    }
    remove_module(workspace, kind, path)
}

fn should_full_refresh_without_watchers(workspace: &WorkspaceState) -> bool {
    let now = SystemTime::now();
    match workspace
        .last_unwatched_full_refresh
        .and_then(|last| now.duration_since(last).ok())
    {
        Some(elapsed) => elapsed >= UNWATCHED_FULL_REFRESH_INTERVAL,
        None => true,
    }
}

fn restore_disk_module_or_remove(workspace: &mut WorkspaceState, root: &Path, path: &Path) -> bool {
    let Some(kind) = path_kind(root, path) else {
        return false;
    };
    if path.exists() {
        let Ok(source) = fs::read_to_string(path) else {
            return false;
        };
        return upsert_module(workspace, kind, path, source, file_modified(path));
    }
    remove_module(workspace, kind, path)
}

fn reconcile_open_overlays(
    workspace: &mut WorkspaceState,
    root: &Path,
    overlays: &HashMap<Url, DocumentState>,
) -> bool {
    let mut changed = false;
    let mut current_overlay_paths = HashSet::new();

    for (uri, doc) in overlays {
        let Ok(path) = uri.to_file_path() else {
            continue;
        };
        let Some(kind) = path_kind(root, &path) else {
            continue;
        };
        if kind == ModuleSetKind::External {
            continue;
        }
        current_overlay_paths.insert(path.clone());
        changed |= upsert_module(
            workspace,
            kind,
            &path,
            doc.text.clone(),
            file_modified(&path),
        );
    }

    let previous_overlay_paths = workspace.overlay_paths.iter().cloned().collect::<Vec<_>>();
    for path in previous_overlay_paths {
        if !current_overlay_paths.contains(&path) {
            changed |= restore_disk_module_or_remove(workspace, root, &path);
        }
    }

    workspace.overlay_paths = current_overlay_paths;
    changed
}

fn rebuild_analysis_if_needed(
    workspace: &mut WorkspaceState,
    previous_external: &Arc<BTreeMap<String, ModuleSource>>,
    previous_src: &Arc<BTreeMap<String, ModuleSource>>,
    previous_package_name: &Option<String>,
    force: bool,
) -> std::result::Result<(), String> {
    let analysis_inputs_changed = force
        || previous_external.as_ref() != workspace.external_modules.as_ref()
        || previous_src.as_ref() != workspace.src_modules.as_ref()
        || previous_package_name != &workspace.package_name;
    if analysis_inputs_changed || workspace.analysis.is_none() {
        workspace.analysis = Some(Arc::new(build_project_analysis(
            workspace.external_modules.as_ref(),
            workspace.src_modules.as_ref(),
            workspace.package_name.as_deref(),
        )?));
        workspace.analysis_generation += 1;
    }
    refresh_workspace_dependency_graph(workspace);
    prune_module_diagnostics_cache(workspace);
    Ok(())
}

fn invalidate_document_cache(workspace: &mut WorkspaceState) {
    workspace.workspace_generation += 1;
    workspace.document_cache.clear();
}

#[cfg(test)]
pub(crate) fn reconcile_workspace_overlays(
    root: Option<&Path>,
    state: &Arc<Mutex<ServerState>>,
) -> std::result::Result<(), String> {
    let Some(root) = root else {
        return Ok(());
    };
    let (workspace, overlays) = {
        let state = state.lock().unwrap();
        let Some(workspace) = state.workspaces.get(root).cloned() else {
            return Ok(());
        };
        (workspace, state.open_docs.clone())
    };

    let mut workspace = workspace.lock().unwrap();
    let previous_external = workspace.external_modules.clone();
    let previous_src = workspace.src_modules.clone();
    let previous_package_name = workspace.package_name.clone();
    let changed = reconcile_open_overlays(&mut workspace, root, &overlays);
    rebuild_analysis_if_needed(
        &mut workspace,
        &previous_external,
        &previous_src,
        &previous_package_name,
        false,
    )?;
    if changed {
        invalidate_document_cache(&mut workspace);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn refresh_workspace_path(
    root: Option<&Path>,
    state: &Arc<Mutex<ServerState>>,
    path: &Path,
) -> std::result::Result<(), String> {
    let _ = refresh_workspace_paths(root, state, &[path.to_path_buf()])?;
    Ok(())
}

pub(crate) fn refresh_workspace_paths(
    root: Option<&Path>,
    state: &Arc<Mutex<ServerState>>,
    paths: &[PathBuf],
) -> std::result::Result<WorkspaceRefreshResult, String> {
    let Some(root) = root else {
        return Ok(WorkspaceRefreshResult::default());
    };
    let workspace = {
        let state = state.lock().unwrap();
        let Some(workspace) = state.workspaces.get(root).cloned() else {
            return Ok(WorkspaceRefreshResult::default());
        };
        workspace
    };

    let mut workspace = workspace.lock().unwrap();
    let previous_external = workspace.external_modules.clone();
    let previous_src = workspace.src_modules.clone();
    let previous_package_name = workspace.package_name.clone();
    let mut result = WorkspaceRefreshResult::default();
    let mut changed = false;

    for path in paths {
        if path == &root.join("bahn.toml") {
            changed |= refresh_manifest(&mut workspace, root);
            result.force_full_reconcile = true;
            continue;
        }

        let kind = path_kind(root, path);
        if matches!(kind, Some(ModuleSetKind::External)) {
            result.force_full_reconcile = true;
        }

        if let Some(kind @ (ModuleSetKind::Src | ModuleSetKind::Test)) = kind {
            if let Some(module_name) = module_name_for_workspace_path(&workspace, kind, path) {
                result.dirty_modules.insert(module_name);
            } else {
                result.dirty_modules.insert(module_name_for_path(path));
            }
        }

        changed |= refresh_disk_module_path(&mut workspace, root, path);

        if let Some(kind @ (ModuleSetKind::Src | ModuleSetKind::Test)) = kind
            && let Some(module_name) = module_name_for_workspace_path(&workspace, kind, path)
        {
            result.dirty_modules.insert(module_name);
        }
    }

    rebuild_analysis_if_needed(
        &mut workspace,
        &previous_external,
        &previous_src,
        &previous_package_name,
        false,
    )?;
    if changed {
        invalidate_document_cache(&mut workspace);
    }
    Ok(result)
}

pub(crate) fn bump_workspace_diagnostics_generation(
    root: Option<&Path>,
    state: &Arc<Mutex<ServerState>>,
) -> Option<u64> {
    let root = root?;
    let workspace = {
        let mut state = state.lock().unwrap();
        state
            .workspaces
            .entry(root.to_path_buf())
            .or_insert_with(|| Arc::new(Mutex::new(WorkspaceState::default())))
            .clone()
    };
    let mut workspace = workspace.lock().unwrap();
    workspace.diagnostics_reconcile_generation += 1;
    Some(workspace.diagnostics_reconcile_generation)
}

pub(crate) fn workspace_diagnostics_generation(
    root: Option<&Path>,
    state: &Arc<Mutex<ServerState>>,
) -> Option<u64> {
    let root = root?;
    let workspace = {
        let state = state.lock().unwrap();
        state.workspaces.get(root).cloned()
    }?;
    Some(workspace.lock().unwrap().diagnostics_reconcile_generation)
}

impl Project {
    pub(crate) fn fast_diagnostics_for_source(
        source_path: &Path,
        source: &str,
    ) -> Vec<tower_lsp::lsp_types::Diagnostic> {
        let report = mondc::quick_diagnostics_report(&source_path.to_string_lossy(), source);
        report
            .diagnostics
            .iter()
            // Fast diagnostics are intentionally single-file only; warning-level checks
            // (notably import usage) can be temporarily inaccurate until full project
            // analysis resolves dependencies/imports. Surface only errors here to avoid
            // flicker, then replace with full diagnostics shortly after.
            .filter(|diag| diag.severity == codespan_reporting::diagnostic::Severity::Error)
            .map(|diag| diagnostic_to_lsp(source, diag))
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(
        external_modules: BTreeMap<String, ModuleSource>,
        src_modules: BTreeMap<String, ModuleSource>,
        test_modules: BTreeMap<String, ModuleSource>,
        package_name: Option<&str>,
    ) -> Self {
        Self {
            root: None,
            external_modules: Arc::new(external_modules.clone()),
            src_modules: Arc::new(src_modules.clone()),
            test_modules: Arc::new(test_modules),
            analysis: Arc::new(
                build_project_analysis(&external_modules, &src_modules, package_name)
                    .expect("project analysis"),
            ),
            analysis_generation: 0,
            workspace_generation: 0,
            document_revisions: Arc::new(HashMap::new()),
            workspace: None,
        }
    }

    pub(crate) fn load(
        root: Option<&Path>,
        state: &Arc<Mutex<ServerState>>,
        focus_uri: &Url,
    ) -> std::result::Result<Self, String> {
        let root = root.map(Path::to_path_buf);
        let Some(root) = root else {
            let overlays = state.lock().unwrap().open_docs.clone();
            let mut src_modules = BTreeMap::new();
            let test_modules = BTreeMap::new();
            if let Ok(path) = focus_uri.to_file_path()
                && let Some(doc) = overlays.get(focus_uri)
            {
                let module = ModuleSource {
                    name: module_name_for_path(&path),
                    path: path.clone(),
                    source: doc.text.clone(),
                };
                src_modules.insert(module.name.clone(), module);
            }
            let analysis = Arc::new(build_project_analysis(
                &BTreeMap::new(),
                &src_modules,
                None,
            )?);
            return Ok(Self {
                root: None,
                external_modules: Arc::new(BTreeMap::new()),
                src_modules: Arc::new(src_modules),
                test_modules: Arc::new(test_modules),
                analysis,
                analysis_generation: 0,
                workspace_generation: 0,
                document_revisions: Arc::new(HashMap::new()),
                workspace: None,
            });
        };

        let (workspace, overlays, watched_files_registered) = {
            let mut state = state.lock().unwrap();
            let workspace = state
                .workspaces
                .entry(root.clone())
                .or_insert_with(|| Arc::new(Mutex::new(WorkspaceState::default())))
                .clone();
            (
                workspace,
                state.open_docs.clone(),
                state.watched_files_registered,
            )
        };

        {
            let mut workspace_state = workspace.lock().unwrap();
            let previous_external = workspace_state.external_modules.clone();
            let previous_src = workspace_state.src_modules.clone();
            let previous_package_name = workspace_state.package_name.clone();
            let focus_path = focus_uri.to_file_path().ok();

            if !workspace_state.seeded {
                refresh_manifest(&mut workspace_state, &root);
                refresh_disk_modules(&mut workspace_state, &root, ModuleSetKind::External);
                refresh_disk_modules(&mut workspace_state, &root, ModuleSetKind::Src);
                refresh_disk_modules(&mut workspace_state, &root, ModuleSetKind::Test);
                reconcile_open_overlays(&mut workspace_state, &root, &overlays);
                workspace_state.last_unwatched_full_refresh = Some(SystemTime::now());
                rebuild_analysis_if_needed(
                    &mut workspace_state,
                    &previous_external,
                    &previous_src,
                    &previous_package_name,
                    true,
                )?;
                invalidate_document_cache(&mut workspace_state);
                workspace_state.seeded = true;
            } else {
                let mut changed = reconcile_open_overlays(&mut workspace_state, &root, &overlays);
                if !watched_files_registered {
                    changed |= refresh_manifest(&mut workspace_state, &root);
                    if let Some(path) = focus_path.as_deref() {
                        changed |= refresh_disk_module_path(&mut workspace_state, &root, path);
                    }
                    if should_full_refresh_without_watchers(&workspace_state) {
                        changed |= refresh_disk_modules(
                            &mut workspace_state,
                            &root,
                            ModuleSetKind::External,
                        );
                        changed |=
                            refresh_disk_modules(&mut workspace_state, &root, ModuleSetKind::Src);
                        changed |=
                            refresh_disk_modules(&mut workspace_state, &root, ModuleSetKind::Test);
                        changed |= reconcile_open_overlays(&mut workspace_state, &root, &overlays);
                        workspace_state.last_unwatched_full_refresh = Some(SystemTime::now());
                    }
                }
                rebuild_analysis_if_needed(
                    &mut workspace_state,
                    &previous_external,
                    &previous_src,
                    &previous_package_name,
                    false,
                )?;
                if changed {
                    invalidate_document_cache(&mut workspace_state);
                }
            }
        }

        let workspace_state = workspace.lock().unwrap();
        Ok(Self {
            root: Some(root),
            external_modules: workspace_state.external_modules.clone(),
            src_modules: workspace_state.src_modules.clone(),
            test_modules: workspace_state.test_modules.clone(),
            analysis: workspace_state
                .analysis
                .clone()
                .expect("workspace analysis is seeded"),
            analysis_generation: workspace_state.analysis_generation,
            workspace_generation: workspace_state.workspace_generation,
            document_revisions: workspace_state.document_revisions.clone(),
            workspace: Some(workspace.clone()),
        })
    }

    pub(crate) fn document_for_path(&self, path: &Path) -> Option<ModuleSource> {
        let module_name = module_name_for_path(path);
        self.src_modules
            .get(&module_name)
            .cloned()
            .or_else(|| self.test_modules.get(&module_name).cloned())
            .or_else(|| self.external_modules.get(&module_name).cloned())
    }

    pub(crate) fn module_named(&self, module_name: &str) -> Option<&ModuleSource> {
        let direct = self
            .src_modules
            .get(module_name)
            .or_else(|| self.test_modules.get(module_name))
            .or_else(|| self.external_modules.get(module_name));
        if direct.is_some() {
            return direct;
        }

        let alias_target = self.analysis.module_aliases.get(module_name)?;
        if alias_target == module_name {
            return None;
        }

        self.src_modules
            .get(alias_target)
            .or_else(|| self.test_modules.get(alias_target))
            .or_else(|| self.external_modules.get(alias_target))
    }

    pub(crate) fn definition_location(
        &self,
        module_name: &str,
        symbol: &str,
    ) -> std::result::Result<Option<Location>, String> {
        let Some(module) = self.module_named(module_name) else {
            return Ok(None);
        };
        let Some(range) = find_top_level_definition_range(&module.path, &module.source, symbol)?
        else {
            return Ok(None);
        };
        let uri = Url::from_file_path(&module.path)
            .map_err(|_| format!("invalid module path: {}", module.path.display()))?;
        Ok(Some(Location::new(uri, range)))
    }

    fn all_modules(&self) -> Vec<ModuleSource> {
        self.external_modules
            .values()
            .chain(self.src_modules.values())
            .chain(self.test_modules.values())
            .cloned()
            .collect()
    }

    pub(crate) fn diagnostic_modules(&self) -> Vec<ModuleSource> {
        self.src_modules
            .values()
            .chain(self.test_modules.values())
            .cloned()
            .collect()
    }

    pub(crate) fn diagnostic_module_names(&self) -> HashSet<String> {
        self.diagnostic_modules()
            .into_iter()
            .map(|module| module.name)
            .collect()
    }

    pub(crate) fn diagnostic_modules_for_names(
        &self,
        module_names: &HashSet<String>,
    ) -> Vec<ModuleSource> {
        self.diagnostic_modules()
            .into_iter()
            .filter(|module| module_names.contains(&module.name))
            .collect()
    }

    pub(crate) fn affected_diagnostic_module_names(
        &self,
        dirty_modules: &HashSet<String>,
    ) -> HashSet<String> {
        let local_module_names = self.diagnostic_module_names();
        let Some(workspace) = self.workspace.as_ref() else {
            return dirty_modules
                .iter()
                .filter(|module_name| local_module_names.contains(*module_name))
                .cloned()
                .collect();
        };

        let reverse_graph = {
            let workspace = workspace.lock().unwrap();
            workspace.module_reverse_import_graph.clone()
        };

        let mut affected: HashSet<String> = dirty_modules
            .iter()
            .filter(|module_name| local_module_names.contains(*module_name))
            .cloned()
            .collect();
        let mut queue: Vec<String> = dirty_modules.iter().cloned().collect();

        while let Some(module_name) = queue.pop() {
            if let Some(dependents) = reverse_graph.get(&module_name) {
                for dependent in dependents {
                    if !local_module_names.contains(dependent) {
                        continue;
                    }
                    if affected.insert(dependent.clone()) {
                        queue.push(dependent.clone());
                    }
                }
            }
        }

        affected
    }

    pub(crate) fn stale_diagnostic_module_names(&self) -> HashSet<String> {
        let modules = self.diagnostic_modules();
        let Some(workspace) = self.workspace.as_ref() else {
            return modules.into_iter().map(|module| module.name).collect();
        };

        let workspace = workspace.lock().unwrap();
        let mut stale = HashSet::new();

        for module in modules {
            let Some(revision) = self.cacheable_document_revision(&module.path) else {
                stale.insert(module.name);
                continue;
            };
            let is_fresh = workspace
                .module_diagnostics_cache
                .get(&module.path)
                .is_some_and(|entry| {
                    entry.source_revision == revision
                        && entry.analysis_generation <= self.analysis_generation
                });
            if !is_fresh {
                stale.insert(module.name);
            }
        }

        stale
    }

    pub(crate) fn cache_module_diagnostics(
        &self,
        module: &ModuleSource,
        diagnostics: Vec<tower_lsp::lsp_types::Diagnostic>,
    ) {
        let Some(revision) = self.cacheable_document_revision(&module.path) else {
            return;
        };
        let Some(workspace) = self.workspace.as_ref() else {
            return;
        };

        let mut workspace = workspace.lock().unwrap();
        if workspace.document_revisions.get(&module.path).copied() != Some(revision) {
            return;
        }
        workspace.module_diagnostics_cache.insert(
            module.path.clone(),
            CachedModuleDiagnostics {
                source_revision: revision,
                analysis_generation: self.analysis_generation,
                diagnostics: Arc::new(diagnostics),
            },
        );
    }

    pub(crate) fn cached_module_diagnostics(
        &self,
        module: &ModuleSource,
    ) -> Option<Vec<tower_lsp::lsp_types::Diagnostic>> {
        let revision = self.cacheable_document_revision(&module.path)?;
        let workspace = self.workspace.as_ref()?;
        let workspace = workspace.lock().unwrap();
        let entry = workspace.module_diagnostics_cache.get(&module.path)?;
        if entry.source_revision != revision || entry.analysis_generation > self.analysis_generation
        {
            return None;
        }
        Some(entry.diagnostics.as_ref().clone())
    }

    pub(crate) fn reference_locations(
        &self,
        symbol: &Symbol,
        include_definition: bool,
    ) -> std::result::Result<Vec<Location>, String> {
        self.reference_ranges(symbol, include_definition)
            .map(|refs| {
                refs.into_iter()
                    .map(|(uri, range)| Location::new(uri, range))
                    .collect()
            })
    }

    pub(crate) fn reference_ranges(
        &self,
        symbol: &Symbol,
        include_definition: bool,
    ) -> std::result::Result<Vec<(Url, Range)>, String> {
        let mut refs = Vec::new();
        for module in self.all_modules() {
            let analysis = self.analyze_document(&module)?;
            let occurrences = collect_symbol_occurrences(
                &module.path,
                &module.source,
                &module.name,
                &analysis.imports,
            )?;
            let uri = Url::from_file_path(&module.path)
                .map_err(|_| format!("invalid module path: {}", module.path.display()))?;
            for occ in occurrences {
                if &occ.symbol != symbol {
                    continue;
                }
                if !include_definition && occ.kind == OccurrenceKind::Definition {
                    continue;
                }
                refs.push((
                    uri.clone(),
                    byte_range_to_lsp_range(&module.source, occ.range.start, occ.range.end),
                ));
            }
        }
        Ok(refs)
    }

    pub(crate) fn qualified_completion_items(
        &self,
        module: &str,
        prefix: &str,
    ) -> Vec<CompletionItem> {
        let mut items = Vec::new();
        let docs = self.top_level_docs_for_module(module);
        if let Some(schemes) = self.analysis.all_module_schemes.get(module) {
            for (name, scheme) in schemes {
                if !name.starts_with(prefix) {
                    continue;
                }
                items.push(completion_item(
                    name.clone(),
                    CompletionItemKind::FUNCTION,
                    Some(format!(
                        "{module} | {}",
                        mondc::typecheck::type_display(&scheme.ty)
                    )),
                    docs.get(name).cloned(),
                ));
            }
        } else if let Some(exports) = self.analysis.module_exports.get(module) {
            items.extend(
                exports
                    .iter()
                    .filter(|name| name.starts_with(prefix))
                    .map(|name| {
                        completion_item(
                            name.clone(),
                            CompletionItemKind::FUNCTION,
                            None,
                            docs.get(name.as_str()).cloned(),
                        )
                    }),
            );
        }
        items.sort_by(|a, b| a.label.cmp(&b.label));
        items
    }

    pub(crate) fn import_path_completion_items(
        &self,
        root: &str,
        prefix: &str,
    ) -> Vec<CompletionItem> {
        let mut items = Vec::new();
        let mut seen = HashSet::new();

        for module_name in self.importable_module_names(root) {
            if !module_name.starts_with(prefix) {
                continue;
            }
            push_completion_item(
                &mut items,
                &mut seen,
                completion_item(
                    module_name,
                    CompletionItemKind::MODULE,
                    Some(format!("{root} module")),
                    None,
                ),
            );
        }

        items.sort_by(|a, b| a.label.cmp(&b.label));
        items
    }

    pub(crate) fn use_import_list_completion_items(
        &self,
        module: &str,
        prefix: &str,
    ) -> Vec<CompletionItem> {
        let mut items = Vec::new();
        let mut seen = HashSet::new();
        let docs = self.top_level_docs_for_module(module);

        if let Some(schemes) = self.analysis.all_module_schemes.get(module) {
            for (name, scheme) in schemes {
                if !name.starts_with(prefix) {
                    continue;
                }
                push_completion_item(
                    &mut items,
                    &mut seen,
                    completion_item(
                        name.clone(),
                        CompletionItemKind::FUNCTION,
                        Some(format!(
                            "{module} | {}",
                            mondc::typecheck::type_display(&scheme.ty)
                        )),
                        docs.get(name).cloned(),
                    ),
                );
            }
        }

        if let Some(exports) = self.analysis.module_exports.get(module) {
            for name in exports {
                if !name.starts_with(prefix) {
                    continue;
                }
                push_completion_item(
                    &mut items,
                    &mut seen,
                    completion_item(
                        name.clone(),
                        CompletionItemKind::FUNCTION,
                        Some(format!("{module} export")),
                        docs.get(name).cloned(),
                    ),
                );
            }
        }

        if let Some(type_decls) = self.analysis.module_type_decls.get(module) {
            for type_decl in type_decls {
                let (name, kind) = match type_decl {
                    mondc::ast::TypeDecl::Record { name, .. } => {
                        (name.clone(), CompletionItemKind::STRUCT)
                    }
                    mondc::ast::TypeDecl::Variant { name, .. } => {
                        (name.clone(), CompletionItemKind::ENUM)
                    }
                };
                if !name.starts_with(prefix) {
                    continue;
                }
                push_completion_item(
                    &mut items,
                    &mut seen,
                    completion_item(
                        name.clone(),
                        kind,
                        Some(format!("{module} type")),
                        docs.get(&name).cloned(),
                    ),
                );
            }
        }

        if let Some(extern_types) = self.analysis.module_extern_types.get(module) {
            for extern_type in extern_types {
                let name = &extern_type.name;
                if !name.starts_with(prefix) {
                    continue;
                }
                push_completion_item(
                    &mut items,
                    &mut seen,
                    completion_item(
                        name.clone(),
                        CompletionItemKind::CLASS,
                        Some(format!("{module} extern type")),
                        docs.get(name).cloned(),
                    ),
                );
            }
        }

        items.sort_by(|a, b| a.label.cmp(&b.label));
        items
    }

    pub(crate) fn record_field_completion_items(
        &self,
        doc: &ModuleSource,
        analysis: &DocumentAnalysis,
        record_name: Option<&str>,
        prefix: &str,
    ) -> Vec<CompletionItem> {
        let mut record_fields: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

        if let Some(type_decls) = local_type_decls(&doc.path, &doc.source) {
            for type_decl in &type_decls {
                collect_record_fields(type_decl, &mut record_fields);
            }
        }

        if let Some(type_decls) = self.analysis.module_type_decls.get(&doc.name) {
            for type_decl in type_decls {
                collect_record_fields(type_decl, &mut record_fields);
            }
        }

        for type_decl in &analysis.imports.imported_type_decls {
            collect_record_fields(type_decl, &mut record_fields);
        }

        let mut items = Vec::new();
        let mut seen = HashSet::new();

        if let Some(record_name) = record_name {
            if let Some(fields) = record_fields.get(record_name) {
                for field in fields {
                    push_completion_item(
                        &mut items,
                        &mut seen,
                        completion_item(
                            field.clone(),
                            CompletionItemKind::FIELD,
                            Some(format!("{record_name} field")),
                            None,
                        ),
                    );
                }
            }
        } else {
            let mut by_field: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
            for (record, fields) in &record_fields {
                for field in fields {
                    by_field
                        .entry(field.clone())
                        .or_default()
                        .insert(record.clone());
                }
            }
            for (field, records) in by_field {
                let detail = if records.len() == 1 {
                    Some(format!(
                        "{} field",
                        records.iter().next().expect("one item")
                    ))
                } else {
                    Some("record field".to_string())
                };
                push_completion_item(
                    &mut items,
                    &mut seen,
                    completion_item(field, CompletionItemKind::FIELD, detail, None),
                );
            }
        }

        items.retain(|item| item.label.starts_with(prefix));
        items.sort_by(|a, b| a.label.cmp(&b.label));
        items
    }

    pub(crate) fn unqualified_completion_items(
        &self,
        doc: &ModuleSource,
        analysis: &DocumentAnalysis,
        offset: usize,
        prefix: &str,
    ) -> std::result::Result<Vec<CompletionItem>, String> {
        let local_names = local_names_at_offset(&doc.path, &doc.source, offset).unwrap_or_default();
        let mut items = Vec::new();
        let mut seen = HashSet::new();
        let local_top_levels = top_level_docs(&doc.path, &doc.source).unwrap_or_default();
        let local_top_level_data = local_top_levels
            .into_iter()
            .filter_map(|symbol| {
                let kind = match symbol.kind {
                    SymbolKind::FUNCTION => Some(CompletionItemKind::FUNCTION),
                    SymbolKind::STRUCT => Some(CompletionItemKind::STRUCT),
                    SymbolKind::ENUM => Some(CompletionItemKind::ENUM),
                    SymbolKind::CLASS => Some(CompletionItemKind::CLASS),
                    _ => None,
                }?;
                Some((symbol.name, (kind, symbol.documentation)))
            })
            .collect::<HashMap<_, _>>();

        for name in local_names {
            push_completion_item(
                &mut items,
                &mut seen,
                completion_item(
                    name,
                    CompletionItemKind::VARIABLE,
                    Some("local".to_string()),
                    None,
                ),
            );
        }

        for (name, (kind, documentation)) in &local_top_level_data {
            let detail = if *kind == CompletionItemKind::FUNCTION {
                analysis
                    .bindings
                    .get(name)
                    .map(|scheme| mondc::typecheck::type_display(&scheme.ty))
                    .map(|ty| format!("{} | {ty}", doc.name))
            } else {
                Some(doc.name.clone())
            };
            push_completion_item(
                &mut items,
                &mut seen,
                completion_item(name.clone(), *kind, detail, documentation.clone()),
            );
        }

        for name in analysis.bindings.keys() {
            let detail = analysis
                .bindings
                .get(name)
                .map(|scheme| mondc::typecheck::type_display(&scheme.ty));
            push_completion_item(
                &mut items,
                &mut seen,
                completion_item(
                    name.clone(),
                    CompletionItemKind::FUNCTION,
                    detail.map(|ty| format!("{} | {ty}", doc.name)),
                    local_top_level_data
                        .get(name)
                        .and_then(|(_, documentation)| documentation.clone()),
                ),
            );
        }

        for (name, scheme) in &analysis.imports.imported_schemes {
            if name.contains('/') {
                continue;
            }
            let origin = analysis
                .imports
                .import_origins
                .get(name)
                .cloned()
                .unwrap_or_else(|| "import".to_string());
            push_completion_item(
                &mut items,
                &mut seen,
                completion_item(
                    name.clone(),
                    CompletionItemKind::FUNCTION,
                    Some(format!(
                        "{origin} | {}",
                        mondc::typecheck::type_display(&scheme.ty)
                    )),
                    self.top_level_docs_for_module(&origin).get(name).cloned(),
                ),
            );
        }

        for module_name in self.visible_module_names(doc) {
            let label = format!("{module_name}/");
            push_completion_item(
                &mut items,
                &mut seen,
                completion_item(
                    label,
                    CompletionItemKind::MODULE,
                    Some("module".to_string()),
                    None,
                ),
            );
        }

        for (name, scheme) in mondc::typecheck::primitive_env() {
            push_completion_item(
                &mut items,
                &mut seen,
                completion_item(
                    name,
                    CompletionItemKind::FUNCTION,
                    Some(format!(
                        "builtin | {}",
                        mondc::typecheck::type_display(&scheme.ty)
                    )),
                    None,
                ),
            );
        }

        items.retain(|item| item.label.starts_with(prefix));
        items.sort_by(|a, b| a.label.cmp(&b.label));
        Ok(items)
    }

    fn visible_module_names(&self, doc: &ModuleSource) -> Vec<String> {
        let mut modules: Vec<String> = self
            .analysis
            .module_exports
            .keys()
            .filter(|name| *name != &doc.name)
            .cloned()
            .collect();
        modules.sort();
        modules
    }

    fn importable_module_names(&self, root: &str) -> Vec<String> {
        let mut modules = Vec::new();

        modules.extend(
            self.external_modules
                .values()
                .filter(|module| external_package_name_for_module_path(&module.path) == Some(root))
                .filter(|module| module.name != root)
                .map(|module| module.name.clone()),
        );

        if self.root.is_some()
            && package_name_from_manifest(self.root.as_deref()).as_deref() == Some(root)
        {
            modules.extend(
                self.src_modules
                    .keys()
                    .filter(|name| name.as_str() != "lib")
                    .cloned(),
            );
        }

        modules.sort();
        modules.dedup();
        modules
    }

    fn top_level_docs_for_module(&self, module: &str) -> HashMap<String, String> {
        self.module_named(module)
            .and_then(|module| top_level_docs(&module.path, &module.source).ok())
            .map(|symbols| {
                symbols
                    .into_iter()
                    .filter_map(|symbol| symbol.documentation.map(|doc| (symbol.name, doc)))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[allow(deprecated)]
    pub(crate) fn workspace_symbols(
        &self,
        query: &str,
    ) -> std::result::Result<Vec<SymbolInformation>, String> {
        let mut symbols = Vec::new();
        for module in self.all_modules() {
            let uri = Url::from_file_path(&module.path)
                .map_err(|_| format!("invalid module path: {}", module.path.display()))?;
            for symbol in top_level_symbols(&module.path, &module.source)? {
                if !query.is_empty() && !symbol.name.contains(query) && !module.name.contains(query)
                {
                    continue;
                }
                symbols.push(SymbolInformation {
                    name: symbol.name,
                    kind: symbol.kind,
                    tags: None,
                    deprecated: None,
                    location: Location::new(
                        uri.clone(),
                        byte_range_to_lsp_range(
                            &module.source,
                            symbol.selection_range.start,
                            symbol.selection_range.end,
                        ),
                    ),
                    container_name: Some(module.name.clone()),
                });
            }
        }
        Ok(symbols)
    }

    pub(crate) fn analyze_document(
        &self,
        doc: &ModuleSource,
    ) -> std::result::Result<DocumentAnalysis, String> {
        self.analyze_document_with_options(doc, true, true)
    }

    fn compute_document_analysis(
        &self,
        doc: &ModuleSource,
        include_bindings: bool,
        include_expr_types: bool,
        imports: Option<mondc::ResolvedImports>,
    ) -> DocumentAnalysis {
        let visible_exports = visible_exports(&self.analysis, &self.test_modules, &doc.name);
        let imports = imports.unwrap_or_else(|| {
            mondc::resolve_imports_for_source(doc.source.as_str(), &visible_exports, &self.analysis)
        });
        let bindings = if include_bindings {
            mondc::infer_module_bindings(
                &doc.name,
                &doc.source,
                imports.imports.clone(),
                &visible_exports,
                &imports.imported_type_decls,
                &imports.imported_extern_types,
                &imports.imported_schemes,
            )
        } else {
            HashMap::new()
        };
        let expr_types = if include_expr_types {
            mondc::infer_module_expr_types(
                &doc.name,
                &doc.source,
                imports.imports.clone(),
                &visible_exports,
                &imports.imported_type_decls,
                &imports.imported_extern_types,
                &imports.imported_schemes,
            )
        } else {
            Vec::new()
        };
        DocumentAnalysis {
            bindings,
            expr_types,
            imports,
        }
    }

    fn cacheable_document_revision(&self, path: &Path) -> Option<u64> {
        self.document_revisions.get(path).copied()
    }

    pub(crate) fn analyze_document_with_options(
        &self,
        doc: &ModuleSource,
        include_bindings: bool,
        include_expr_types: bool,
    ) -> std::result::Result<DocumentAnalysis, String> {
        let cache_key = AnalysisCacheKey {
            include_bindings,
            include_expr_types,
        };
        let Some(revision) = self.cacheable_document_revision(&doc.path) else {
            return Ok(self.compute_document_analysis(
                doc,
                include_bindings,
                include_expr_types,
                None,
            ));
        };
        let Some(workspace) = self.workspace.as_ref() else {
            return Ok(self.compute_document_analysis(
                doc,
                include_bindings,
                include_expr_types,
                None,
            ));
        };

        let cached_imports = {
            let workspace = workspace.lock().unwrap();
            if let Some(entry) = workspace.document_cache.get(&doc.path)
                && entry.source_revision == revision
                && entry.workspace_generation == self.workspace_generation
            {
                if let Some(analysis) = entry.analyses.get(&cache_key) {
                    return Ok(analysis.as_ref().clone());
                }
                entry
                    .analyses
                    .values()
                    .next()
                    .map(|analysis| analysis.imports.clone())
            } else {
                None
            }
        };

        let analysis = self.compute_document_analysis(
            doc,
            include_bindings,
            include_expr_types,
            cached_imports,
        );

        let mut workspace = workspace.lock().unwrap();
        if workspace.document_revisions.get(&doc.path).copied() == Some(revision)
            && workspace.workspace_generation == self.workspace_generation
        {
            let entry = workspace
                .document_cache
                .entry(doc.path.clone())
                .or_insert_with(|| CachedDocumentState {
                    source_revision: revision,
                    workspace_generation: self.workspace_generation,
                    analyses: HashMap::new(),
                    diagnostics: None,
                });
            if entry.source_revision != revision
                || entry.workspace_generation != self.workspace_generation
            {
                *entry = CachedDocumentState {
                    source_revision: revision,
                    workspace_generation: self.workspace_generation,
                    analyses: HashMap::new(),
                    diagnostics: None,
                };
            }
            entry.analyses.insert(cache_key, Arc::new(analysis.clone()));
        }

        Ok(analysis)
    }

    pub(crate) fn diagnostics_for_document(
        &self,
        doc: &ModuleSource,
    ) -> std::result::Result<Vec<tower_lsp::lsp_types::Diagnostic>, String> {
        let Some(revision) = self.cacheable_document_revision(&doc.path) else {
            return self.compute_document_diagnostics(doc);
        };
        let Some(workspace) = self.workspace.as_ref() else {
            return self.compute_document_diagnostics(doc);
        };

        {
            let workspace = workspace.lock().unwrap();
            if let Some(entry) = workspace.document_cache.get(&doc.path)
                && entry.source_revision == revision
                && entry.workspace_generation == self.workspace_generation
                && let Some(diagnostics) = entry.diagnostics.as_ref()
            {
                return Ok(diagnostics.as_ref().clone());
            }
        }

        let diagnostics = self.compute_document_diagnostics(doc)?;

        let mut workspace = workspace.lock().unwrap();
        if workspace.document_revisions.get(&doc.path).copied() == Some(revision)
            && workspace.workspace_generation == self.workspace_generation
        {
            let entry = workspace
                .document_cache
                .entry(doc.path.clone())
                .or_insert_with(|| CachedDocumentState {
                    source_revision: revision,
                    workspace_generation: self.workspace_generation,
                    analyses: HashMap::new(),
                    diagnostics: None,
                });
            if entry.source_revision != revision
                || entry.workspace_generation != self.workspace_generation
            {
                *entry = CachedDocumentState {
                    source_revision: revision,
                    workspace_generation: self.workspace_generation,
                    analyses: HashMap::new(),
                    diagnostics: None,
                };
            }
            entry.diagnostics = Some(Arc::new(diagnostics.clone()));
        }

        Ok(diagnostics)
    }

    fn compute_document_diagnostics(
        &self,
        doc: &ModuleSource,
    ) -> std::result::Result<Vec<tower_lsp::lsp_types::Diagnostic>, String> {
        let visible_exports = visible_exports(&self.analysis, &self.test_modules, &doc.name);
        let pipeline = mondc::CompilePipeline::new(mondc::PassContext {
            visible_exports: &visible_exports,
            analysis: &self.analysis,
            compile_target: mondc::CompileTarget::Dev,
        });
        let source_path = source_path_for_compile(self.root.as_deref(), &doc.path);
        let report = pipeline.compile_module_report(mondc::ModuleInput {
            output_module_name: &doc.name,
            source: &doc.source,
            source_path: &source_path,
        });
        Ok(report
            .diagnostics
            .iter()
            .map(|diag| diagnostic_to_lsp(&doc.source, diag))
            .collect())
    }
}
