use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::SystemTime,
};

use tower_lsp::lsp_types::{
    CompletionItem, Diagnostic, DocumentSymbol, SemanticTokens, SignatureHelp, Url,
};

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
pub(crate) struct CachedSemanticTokens {
    pub(crate) version: i32,
    pub(crate) tokens: SemanticTokens,
}

#[derive(Clone)]
pub(crate) struct CachedDocumentSymbols {
    pub(crate) version: i32,
    pub(crate) symbols: Vec<DocumentSymbol>,
}

#[derive(Clone, Default)]
pub(crate) struct FrontendSnapshot {
    pub(crate) fast_diagnostics: Option<Vec<Diagnostic>>,
    pub(crate) semantic_tokens: Option<SemanticTokens>,
    pub(crate) document_symbols: Option<Vec<DocumentSymbol>>,
}

#[derive(Clone)]
pub(crate) struct CachedFrontendSnapshot {
    pub(crate) version: i32,
    pub(crate) snapshot: FrontendSnapshot,
}

#[derive(Clone)]
pub(crate) struct CachedCompletionItems {
    pub(crate) version: i32,
    pub(crate) context_hash: u64,
    pub(crate) line: u32,
    pub(crate) character: u32,
    pub(crate) items: Vec<CompletionItem>,
}

#[derive(Clone)]
pub(crate) struct CachedSignatureHelp {
    pub(crate) version: i32,
    pub(crate) context_hash: u64,
    pub(crate) line: u32,
    pub(crate) character: u32,
    pub(crate) help: Option<SignatureHelp>,
}

#[derive(Clone)]
pub(crate) struct CachedDocumentState {
    pub(crate) source_revision: u64,
    pub(crate) workspace_generation: u64,
    pub(crate) analyses: HashMap<AnalysisCacheKey, Arc<DocumentAnalysis>>,
    pub(crate) diagnostics: Option<Arc<Vec<Diagnostic>>>,
}

#[derive(Clone)]
pub(crate) struct CachedModuleDiagnostics {
    pub(crate) source_revision: u64,
    pub(crate) analysis_generation: u64,
    pub(crate) diagnostics: Arc<Vec<Diagnostic>>,
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
    pub(crate) module_import_graph: HashMap<String, HashSet<String>>,
    pub(crate) module_reverse_import_graph: HashMap<String, HashSet<String>>,
    pub(crate) module_diagnostics_cache: HashMap<PathBuf, CachedModuleDiagnostics>,
    pub(crate) diagnostics_reconcile_generation: u64,
    pub(crate) src_files: HashMap<PathBuf, IndexedModuleFile>,
    pub(crate) test_files: HashMap<PathBuf, IndexedModuleFile>,
    pub(crate) external_files: HashMap<PathBuf, IndexedModuleFile>,
    pub(crate) overlay_paths: HashSet<PathBuf>,
    pub(crate) manifest_modified: Option<SystemTime>,
    pub(crate) last_unwatched_full_refresh: Option<SystemTime>,
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
            module_import_graph: HashMap::new(),
            module_reverse_import_graph: HashMap::new(),
            module_diagnostics_cache: HashMap::new(),
            diagnostics_reconcile_generation: 0,
            src_files: HashMap::new(),
            test_files: HashMap::new(),
            external_files: HashMap::new(),
            overlay_paths: HashSet::new(),
            manifest_modified: None,
            last_unwatched_full_refresh: None,
            seeded: false,
        }
    }
}

#[derive(Default)]
pub(crate) struct ServerState {
    pub(crate) open_docs: HashMap<Url, DocumentState>,
    pub(crate) document_diagnostics_generation: HashMap<Url, u64>,
    pub(crate) semantic_tokens_generation: HashMap<Url, u64>,
    pub(crate) document_symbol_generation: HashMap<Url, u64>,
    pub(crate) frontend_snapshot_cache: HashMap<Url, CachedFrontendSnapshot>,
    pub(crate) semantic_tokens_cache: HashMap<Url, CachedSemanticTokens>,
    pub(crate) document_symbol_cache: HashMap<Url, CachedDocumentSymbols>,
    pub(crate) completion_cache: HashMap<Url, CachedCompletionItems>,
    pub(crate) signature_help_cache: HashMap<Url, CachedSignatureHelp>,
    pub(crate) workspaces: HashMap<PathBuf, Arc<Mutex<WorkspaceState>>>,
    pub(crate) watched_files_dynamic_registration: bool,
    pub(crate) watched_files_registered: bool,
}
