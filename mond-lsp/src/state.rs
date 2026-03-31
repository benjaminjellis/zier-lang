use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::Arc,
};

use tower_lsp::lsp_types::Url;

use crate::ModuleSource;

#[derive(Clone, Debug)]
pub(crate) struct DocumentState {
    pub(crate) version: i32,
    pub(crate) text: String,
}

#[derive(Clone)]
pub(crate) struct WorkspaceCacheEntry {
    pub(crate) external_modules: Arc<BTreeMap<String, ModuleSource>>,
    pub(crate) src_modules: Arc<BTreeMap<String, ModuleSource>>,
    pub(crate) test_modules: Arc<BTreeMap<String, ModuleSource>>,
    pub(crate) package_name: Option<String>,
    pub(crate) analysis: Arc<mondc::ProjectAnalysis>,
}

#[derive(Default)]
pub(crate) struct ServerState {
    pub(crate) open_docs: HashMap<Url, DocumentState>,
    pub(crate) workspace_cache: HashMap<PathBuf, WorkspaceCacheEntry>,
}
