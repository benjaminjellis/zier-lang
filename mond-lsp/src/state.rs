use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::SystemTime,
};

use tower_lsp::lsp_types::{Diagnostic, Url};

use crate::{DocumentAnalysis, ModuleSource};

#[derive(Clone, Debug)]
pub(crate) struct DocumentState {
    pub(crate) version: i32,
    pub(crate) text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct AnalysisCacheKey {
    pub(crate) include_bindings: bool,
    pub(crate) include_expr_types: bool,
}

#[derive(Clone)]
pub(crate) struct CachedDocumentState {
    pub(crate) source_revision: u64,
    pub(crate) workspace_generation: u64,
    pub(crate) analyses: HashMap<AnalysisCacheKey, Arc<DocumentAnalysis>>,
    pub(crate) diagnostics: Option<Arc<Vec<Diagnostic>>>,
}

#[derive(Clone, Debug)]
pub(crate) struct IndexedModuleFile {
    pub(crate) module_name: String,
    pub(crate) modified: Option<SystemTime>,
}

pub(crate) struct WorkspaceState {
    pub(crate) external_modules: Arc<BTreeMap<String, ModuleSource>>,
    pub(crate) src_modules: Arc<BTreeMap<String, ModuleSource>>,
    pub(crate) test_modules: Arc<BTreeMap<String, ModuleSource>>,
    pub(crate) package_name: Option<String>,
    pub(crate) analysis: Option<Arc<mondc::ProjectAnalysis>>,
    pub(crate) analysis_generation: u64,
    pub(crate) workspace_generation: u64,
    pub(crate) next_revision: u64,
    pub(crate) document_revisions: Arc<HashMap<PathBuf, u64>>,
    pub(crate) document_cache: HashMap<PathBuf, CachedDocumentState>,
    pub(crate) src_files: HashMap<PathBuf, IndexedModuleFile>,
    pub(crate) test_files: HashMap<PathBuf, IndexedModuleFile>,
    pub(crate) external_files: HashMap<PathBuf, IndexedModuleFile>,
    pub(crate) overlay_paths: HashSet<PathBuf>,
    pub(crate) manifest_modified: Option<SystemTime>,
    pub(crate) seeded: bool,
}

impl Default for WorkspaceState {
    fn default() -> Self {
        Self {
            external_modules: Arc::new(BTreeMap::new()),
            src_modules: Arc::new(BTreeMap::new()),
            test_modules: Arc::new(BTreeMap::new()),
            package_name: None,
            analysis: None,
            analysis_generation: 0,
            workspace_generation: 0,
            next_revision: 1,
            document_revisions: Arc::new(HashMap::new()),
            document_cache: HashMap::new(),
            src_files: HashMap::new(),
            test_files: HashMap::new(),
            external_files: HashMap::new(),
            overlay_paths: HashSet::new(),
            manifest_modified: None,
            seeded: false,
        }
    }
}

#[derive(Default)]
pub(crate) struct ServerState {
    pub(crate) open_docs: HashMap<Url, DocumentState>,
    pub(crate) workspaces: HashMap<PathBuf, Arc<Mutex<WorkspaceState>>>,
    pub(crate) watched_files_dynamic_registration: bool,
    pub(crate) watched_files_registered: bool,
}
