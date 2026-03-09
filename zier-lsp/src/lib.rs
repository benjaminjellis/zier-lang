use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use codespan_reporting::diagnostic::{Diagnostic as CodeDiagnostic, LabelStyle, Severity};
use include_dir::{Dir, include_dir};
use tokio::io::{AsyncRead, AsyncWrite};
use tower_lsp::{
    Client, LanguageServer, LspService, Server,
    jsonrpc::Result,
    lsp_types::{
        CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams,
        CompletionResponse, Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams,
        DidCloseTextDocumentParams, DidOpenTextDocumentParams, DidSaveTextDocumentParams,
        DocumentFormattingParams, DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse,
        GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverContents, HoverParams,
        InitializeParams, InitializeResult, InitializedParams, Location, MarkedString, MessageType,
        OneOf, ParameterInformation, ParameterLabel, Position, Range, ReferenceParams,
        RenameParams, ServerCapabilities, SignatureHelp, SignatureHelpParams, SignatureInformation,
        SymbolInformation, SymbolKind, TextDocumentContentChangeEvent, TextDocumentSyncCapability,
        TextDocumentSyncKind, TextEdit, Url, WorkspaceEdit, WorkspaceSymbolParams,
    },
};

static STD_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../zier-std/src");

#[derive(Clone, Debug)]
struct DocumentState {
    version: i32,
    text: String,
}

#[derive(Default)]
struct ServerState {
    open_docs: HashMap<Url, DocumentState>,
}

#[derive(Clone, Debug)]
struct ModuleSource {
    name: String,
    path: PathBuf,
    source: String,
}

#[derive(Clone)]
struct ProjectAnalysis {
    module_exports: HashMap<String, Vec<String>>,
    module_type_decls: HashMap<String, Vec<zierc::ast::TypeDecl>>,
    all_module_schemes: HashMap<String, zierc::typecheck::TypeEnv>,
    std_aliases: HashMap<String, String>,
}

struct ResolvedImports {
    imports: HashMap<String, String>,
    import_origins: HashMap<String, String>,
    imported_schemes: zierc::typecheck::TypeEnv,
    imported_type_decls: Vec<zierc::ast::TypeDecl>,
    module_aliases: HashMap<String, String>,
}

struct DocumentAnalysis {
    diagnostics: Vec<Diagnostic>,
    bindings: zierc::typecheck::TypeEnv,
    expr_types: Vec<(std::ops::Range<usize>, String)>,
    imports: ResolvedImports,
}

#[derive(Debug)]
enum HoverTarget {
    Unqualified(String),
    Qualified { module: String, function: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Symbol {
    module: String,
    function: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OccurrenceKind {
    Definition,
    Reference,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SymbolOccurrence {
    symbol: Symbol,
    range: std::ops::Range<usize>,
    kind: OccurrenceKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TopLevelSymbol {
    name: String,
    kind: SymbolKind,
    selection_range: std::ops::Range<usize>,
    full_range: std::ops::Range<usize>,
}

struct SignatureTarget {
    symbol: Symbol,
    arg_index: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct LocalSymbol {
    name: String,
    def_range: std::ops::Range<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LocalOccurrence {
    symbol: LocalSymbol,
    range: std::ops::Range<usize>,
    kind: OccurrenceKind,
}

#[derive(Debug)]
enum CompletionContext {
    Unqualified { prefix: String },
    Qualified { module: String, prefix: String },
}

pub struct Backend {
    client: Client,
    state: Arc<Mutex<ServerState>>,
}

impl Backend {
    fn new(client: Client) -> Self {
        Self {
            client,
            state: Arc::new(Mutex::new(ServerState::default())),
        }
    }

    async fn publish_document_diagnostics(&self, uri: Url) {
        let diagnostics = match self.analyze_document(&uri) {
            Ok(Some(analysis)) => analysis.diagnostics,
            Ok(None) => Vec::new(),
            Err(err) => vec![Diagnostic {
                range: Range::new(Position::new(0, 0), Position::new(0, 0)),
                severity: Some(DiagnosticSeverity::ERROR),
                message: err,
                ..Diagnostic::default()
            }],
        };
        self.client
            .publish_diagnostics(uri, diagnostics, None)
            .await;
    }

    fn analyze_document(&self, uri: &Url) -> std::result::Result<Option<DocumentAnalysis>, String> {
        let path = match uri.to_file_path() {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        let root = find_project_root(&path);
        let project = Project::load(root.as_deref(), &self.state, uri)?;
        let doc = match project.document_for_path(&path) {
            Some(doc) => doc,
            None => return Ok(None),
        };
        let analysis = project.analyze_document(&doc)?;
        Ok(Some(analysis))
    }

    fn document_text(&self, uri: &Url) -> Option<String> {
        if let Some(doc) = self.state.lock().unwrap().open_docs.get(uri).cloned() {
            return Some(doc.text);
        }
        let path = uri.to_file_path().ok()?;
        fs::read_to_string(path).ok()
    }

    fn workspace_project(&self) -> std::result::Result<Option<Project>, String> {
        let first_uri = self.state.lock().unwrap().open_docs.keys().next().cloned();
        let Some(uri) = first_uri else {
            return Ok(None);
        };
        let path = match uri.to_file_path() {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        let root = find_project_root(&path);
        Project::load(root.as_deref(), &self.state, &uri).map(Some)
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                hover_provider: Some(tower_lsp::lsp_types::HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                rename_provider: Some(OneOf::Left(true)),
                completion_provider: Some(CompletionOptions::default()),
                document_symbol_provider: Some(OneOf::Left(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                signature_help_provider: Some(tower_lsp::lsp_types::SignatureHelpOptions {
                    trigger_characters: Some(vec![" ".to_string(), "(".to_string()]),
                    retrigger_characters: None,
                    work_done_progress_options: Default::default(),
                }),
                document_formatting_provider: Some(OneOf::Left(true)),
                ..ServerCapabilities::default()
            },
            ..InitializeResult::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "zier-lsp initialized")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        {
            let mut state = self.state.lock().unwrap();
            state.open_docs.insert(
                params.text_document.uri.clone(),
                DocumentState {
                    version: params.text_document.version,
                    text: params.text_document.text,
                },
            );
        }
        self.publish_document_diagnostics(params.text_document.uri)
            .await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let text = full_text_change(params.content_changes);
        {
            let mut state = self.state.lock().unwrap();
            if let Some(doc) = state.open_docs.get_mut(&params.text_document.uri) {
                doc.version = params.text_document.version;
                doc.text = text;
            }
        }
        self.publish_document_diagnostics(params.text_document.uri)
            .await;
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        self.publish_document_diagnostics(params.text_document.uri)
            .await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        {
            let mut state = self.state.lock().unwrap();
            state.open_docs.remove(&params.text_document.uri);
        }
        self.client
            .publish_diagnostics(params.text_document.uri, Vec::new(), None)
            .await;
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let Some(analysis) = self
            .analyze_document(&uri)
            .map_err(tower_lsp::jsonrpc::Error::invalid_params)?
        else {
            return Ok(None);
        };
        let path = match uri.to_file_path() {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        let root = find_project_root(&path);
        let project = Project::load(root.as_deref(), &self.state, &uri)
            .map_err(tower_lsp::jsonrpc::Error::invalid_params)?;
        let doc = match project.document_for_path(&path) {
            Some(doc) => doc,
            None => return Ok(None),
        };
        let Some(offset) =
            position_to_offset(&doc.source, params.text_document_position_params.position)
        else {
            return Ok(None);
        };
        if let Some(ty) = best_expr_type_at_offset(&analysis.expr_types, offset) {
            return Ok(Some(Hover {
                contents: HoverContents::Scalar(MarkedString::String(ty)),
                range: None,
            }));
        }
        let scheme = if let Some(target) = find_hover_target(&doc.path, &doc.source, offset) {
            match target {
                HoverTarget::Unqualified(name) => analysis
                    .bindings
                    .get(&name)
                    .cloned()
                    .or_else(|| analysis.imports.imported_schemes.get(&name).cloned())
                    .or_else(|| zierc::typecheck::primitive_env().get(&name).cloned())
                    .map(|scheme| (name, scheme)),
                HoverTarget::Qualified { module, function } => analysis
                    .imports
                    .imported_schemes
                    .get(&format!("{module}/{function}"))
                    .cloned()
                    .or_else(|| {
                        project
                            .analysis
                            .all_module_schemes
                            .get(&module)
                            .and_then(|env| env.get(&function))
                            .cloned()
                    })
                    .map(|scheme| (format!("{module}/{function}"), scheme)),
            }
        } else {
            symbol_at(&doc.path, &doc.source, &doc.name, &analysis.imports, offset)
                .map_err(tower_lsp::jsonrpc::Error::invalid_params)?
                .and_then(|symbol| {
                    let name = if symbol.module == doc.name {
                        symbol.function.clone()
                    } else {
                        format!("{}/{}", symbol.module, symbol.function)
                    };
                    if symbol.module == doc.name {
                        analysis
                            .bindings
                            .get(&symbol.function)
                            .cloned()
                            .map(|scheme| (name, scheme))
                    } else {
                        analysis
                            .imports
                            .imported_schemes
                            .get(&format!("{}/{}", symbol.module, symbol.function))
                            .cloned()
                            .or_else(|| {
                                project
                                    .analysis
                                    .all_module_schemes
                                    .get(&symbol.module)
                                    .and_then(|env| env.get(&symbol.function))
                                    .cloned()
                            })
                            .map(|scheme| (name, scheme))
                    }
                })
        };

        let Some((name, scheme)) = scheme else {
            return Ok(None);
        };
        let rendered = format!("{name} : {}", zierc::typecheck::type_display(&scheme.ty));
        Ok(Some(Hover {
            contents: HoverContents::Scalar(MarkedString::String(rendered)),
            range: None,
        }))
    }

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let Some(source) = self.document_text(&params.text_document.uri) else {
            return Ok(None);
        };
        let formatted = zier_format::format_default(&source);
        if formatted == source {
            return Ok(Some(Vec::new()));
        }
        Ok(Some(vec![TextEdit {
            range: full_document_range(&source),
            new_text: formatted,
        }]))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let path = match uri.to_file_path() {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        let root = find_project_root(&path);
        let project = Project::load(root.as_deref(), &self.state, &uri)
            .map_err(tower_lsp::jsonrpc::Error::invalid_params)?;
        let doc = match project.document_for_path(&path) {
            Some(doc) => doc,
            None => return Ok(None),
        };
        let Some(offset) =
            position_to_offset(&doc.source, params.text_document_position_params.position)
        else {
            return Ok(None);
        };
        let analysis = project
            .analyze_document(&doc)
            .map_err(tower_lsp::jsonrpc::Error::invalid_params)?;
        if let Some(local) = local_symbol_at(&doc.path, &doc.source, offset)
            .map_err(tower_lsp::jsonrpc::Error::invalid_params)?
        {
            let uri = Url::from_file_path(&doc.path)
                .map_err(|_| tower_lsp::jsonrpc::Error::invalid_params("invalid document path"))?;
            let range =
                byte_range_to_lsp_range(&doc.source, local.def_range.start, local.def_range.end);
            return Ok(Some(GotoDefinitionResponse::Scalar(Location::new(
                uri, range,
            ))));
        }
        let Some(symbol) = symbol_at(&doc.path, &doc.source, &doc.name, &analysis.imports, offset)
            .map_err(tower_lsp::jsonrpc::Error::invalid_params)?
        else {
            return Ok(None);
        };
        let location = project
            .definition_location(&symbol.module, &symbol.function)
            .map_err(tower_lsp::jsonrpc::Error::invalid_params)?;

        Ok(location.map(GotoDefinitionResponse::Scalar))
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = params.text_document_position.text_document.uri;
        let path = match uri.to_file_path() {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        let root = find_project_root(&path);
        let project = Project::load(root.as_deref(), &self.state, &uri)
            .map_err(tower_lsp::jsonrpc::Error::invalid_params)?;
        let doc = match project.document_for_path(&path) {
            Some(doc) => doc,
            None => return Ok(None),
        };
        let analysis = project
            .analyze_document(&doc)
            .map_err(tower_lsp::jsonrpc::Error::invalid_params)?;
        let Some(offset) = position_to_offset(&doc.source, params.text_document_position.position)
        else {
            return Ok(None);
        };
        if let Some(local) = local_symbol_at(&doc.path, &doc.source, offset)
            .map_err(tower_lsp::jsonrpc::Error::invalid_params)?
        {
            let uri = Url::from_file_path(&doc.path)
                .map_err(|_| tower_lsp::jsonrpc::Error::invalid_params("invalid document path"))?;
            let locations = collect_local_occurrences(&doc.path, &doc.source)
                .map_err(tower_lsp::jsonrpc::Error::invalid_params)?
                .into_iter()
                .filter(|occ| occ.symbol == local)
                .filter(|occ| {
                    params.context.include_declaration || occ.kind != OccurrenceKind::Definition
                })
                .map(|occ| {
                    Location::new(
                        uri.clone(),
                        byte_range_to_lsp_range(&doc.source, occ.range.start, occ.range.end),
                    )
                })
                .collect();
            return Ok(Some(locations));
        }
        let Some(symbol) = symbol_at(&doc.path, &doc.source, &doc.name, &analysis.imports, offset)
            .map_err(tower_lsp::jsonrpc::Error::invalid_params)?
        else {
            return Ok(None);
        };

        let locations = project
            .reference_locations(&symbol, params.context.include_declaration)
            .map_err(tower_lsp::jsonrpc::Error::invalid_params)?;
        Ok(Some(locations))
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let uri = params.text_document_position.text_document.uri;
        let path = match uri.to_file_path() {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        let root = find_project_root(&path);
        let project = Project::load(root.as_deref(), &self.state, &uri)
            .map_err(tower_lsp::jsonrpc::Error::invalid_params)?;
        let doc = match project.document_for_path(&path) {
            Some(doc) => doc,
            None => return Ok(None),
        };
        let analysis = project
            .analyze_document(&doc)
            .map_err(tower_lsp::jsonrpc::Error::invalid_params)?;
        let Some(offset) = position_to_offset(&doc.source, params.text_document_position.position)
        else {
            return Ok(None);
        };
        if let Some(local) = local_symbol_at(&doc.path, &doc.source, offset)
            .map_err(tower_lsp::jsonrpc::Error::invalid_params)?
        {
            let uri = Url::from_file_path(&doc.path)
                .map_err(|_| tower_lsp::jsonrpc::Error::invalid_params("invalid document path"))?;
            let edits = collect_local_occurrences(&doc.path, &doc.source)
                .map_err(tower_lsp::jsonrpc::Error::invalid_params)?
                .into_iter()
                .filter(|occ| occ.symbol == local)
                .map(|occ| TextEdit {
                    range: byte_range_to_lsp_range(&doc.source, occ.range.start, occ.range.end),
                    new_text: params.new_name.clone(),
                })
                .collect();
            let mut changes = HashMap::new();
            changes.insert(uri, edits);
            return Ok(Some(WorkspaceEdit {
                changes: Some(changes),
                ..WorkspaceEdit::default()
            }));
        }
        let Some(symbol) = symbol_at(&doc.path, &doc.source, &doc.name, &analysis.imports, offset)
            .map_err(tower_lsp::jsonrpc::Error::invalid_params)?
        else {
            return Ok(None);
        };

        let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
        for (uri, range) in project
            .reference_ranges(&symbol, true)
            .map_err(tower_lsp::jsonrpc::Error::invalid_params)?
        {
            changes.entry(uri).or_default().push(TextEdit {
                range,
                new_text: params.new_name.clone(),
            });
        }

        Ok(Some(WorkspaceEdit {
            changes: Some(changes),
            ..WorkspaceEdit::default()
        }))
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let path = match uri.to_file_path() {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        let root = find_project_root(&path);
        let project = Project::load(root.as_deref(), &self.state, &uri)
            .map_err(tower_lsp::jsonrpc::Error::invalid_params)?;
        let doc = match project.document_for_path(&path) {
            Some(doc) => doc,
            None => return Ok(None),
        };
        let analysis = project
            .analyze_document(&doc)
            .map_err(tower_lsp::jsonrpc::Error::invalid_params)?;
        let Some(offset) = position_to_offset(&doc.source, params.text_document_position.position)
        else {
            return Ok(None);
        };
        let Some(ctx) = completion_context(&doc.source, offset) else {
            return Ok(None);
        };

        let items = match ctx {
            CompletionContext::Qualified { module, prefix } => {
                project.qualified_completion_items(&module, &prefix)
            }
            CompletionContext::Unqualified { prefix } => project
                .unqualified_completion_items(&doc, &analysis, offset, &prefix)
                .map_err(tower_lsp::jsonrpc::Error::invalid_params)?,
        };

        Ok(Some(CompletionResponse::Array(items)))
    }

    #[allow(deprecated)]
    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = params.text_document.uri;
        let path = match uri.to_file_path() {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        let root = find_project_root(&path);
        let project = Project::load(root.as_deref(), &self.state, &uri)
            .map_err(tower_lsp::jsonrpc::Error::invalid_params)?;
        let doc = match project.document_for_path(&path) {
            Some(doc) => doc,
            None => return Ok(None),
        };
        let symbols = top_level_symbols(&doc.path, &doc.source)
            .map_err(tower_lsp::jsonrpc::Error::invalid_params)?
            .into_iter()
            .map(|symbol| DocumentSymbol {
                name: symbol.name,
                detail: None,
                kind: symbol.kind,
                tags: None,
                deprecated: None,
                range: byte_range_to_lsp_range(
                    &doc.source,
                    symbol.full_range.start,
                    symbol.full_range.end,
                ),
                selection_range: byte_range_to_lsp_range(
                    &doc.source,
                    symbol.selection_range.start,
                    symbol.selection_range.end,
                ),
                children: None,
            })
            .collect();
        Ok(Some(DocumentSymbolResponse::Nested(symbols)))
    }

    #[allow(deprecated)]
    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<Vec<SymbolInformation>>> {
        let Some(project) = self
            .workspace_project()
            .map_err(tower_lsp::jsonrpc::Error::invalid_params)?
        else {
            return Ok(None);
        };
        let symbols = project
            .workspace_symbols(&params.query)
            .map_err(tower_lsp::jsonrpc::Error::invalid_params)?;
        Ok(Some(symbols))
    }

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        let uri = params.text_document_position_params.text_document.uri;
        let path = match uri.to_file_path() {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        let root = find_project_root(&path);
        let project = Project::load(root.as_deref(), &self.state, &uri)
            .map_err(tower_lsp::jsonrpc::Error::invalid_params)?;
        let doc = match project.document_for_path(&path) {
            Some(doc) => doc,
            None => return Ok(None),
        };
        let analysis = project
            .analyze_document(&doc)
            .map_err(tower_lsp::jsonrpc::Error::invalid_params)?;
        let Some(offset) =
            position_to_offset(&doc.source, params.text_document_position_params.position)
        else {
            return Ok(None);
        };
        let Some(target) =
            signature_target_at(&doc.path, &doc.source, &doc.name, &analysis.imports, offset)
                .map_err(tower_lsp::jsonrpc::Error::invalid_params)?
        else {
            return Ok(None);
        };
        let Some(scheme) = scheme_for_symbol(&project, &doc, &analysis, &target.symbol) else {
            return Ok(None);
        };
        let label = format!(
            "{} : {}",
            if target.symbol.module == doc.name {
                target.symbol.function.clone()
            } else {
                format!("{}/{}", target.symbol.module, target.symbol.function)
            },
            zierc::typecheck::type_display(&scheme.ty)
        );
        let arity = function_arity(&scheme.ty);
        let params = (0..arity)
            .map(|index| ParameterInformation {
                label: ParameterLabel::Simple(format!("arg{}", index + 1)),
                documentation: None,
            })
            .collect();
        Ok(Some(SignatureHelp {
            signatures: vec![SignatureInformation {
                label,
                documentation: None,
                parameters: Some(params),
                active_parameter: Some(target.arg_index.min(arity.saturating_sub(1)) as u32),
            }],
            active_signature: Some(0),
            active_parameter: Some(target.arg_index.min(arity.saturating_sub(1)) as u32),
        }))
    }
}

pub async fn serve<R, W>(stdin: R, stdout: W)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}

struct Project {
    root: Option<PathBuf>,
    std_modules: BTreeMap<String, ModuleSource>,
    src_modules: BTreeMap<String, ModuleSource>,
    test_modules: BTreeMap<String, ModuleSource>,
    analysis: ProjectAnalysis,
}

impl Project {
    fn load(
        root: Option<&Path>,
        state: &Arc<Mutex<ServerState>>,
        focus_uri: &Url,
    ) -> std::result::Result<Self, String> {
        let overlays = state.lock().unwrap().open_docs.clone();
        let std_modules = collect_std_modules();
        let src_modules = collect_modules(root, "src", &overlays);
        let test_modules = collect_modules(root, "tests", &overlays);
        let analysis = build_project_analysis(&src_modules)?;

        // Ensure an open unsaved file outside src/tests still gets analyzed standalone.
        if let Ok(path) = focus_uri.to_file_path()
            && !contains_path(&src_modules, &path)
            && !contains_path(&test_modules, &path)
            && let Some(doc) = overlays.get(focus_uri)
        {
            let module = ModuleSource {
                name: module_name_for_path(&path),
                path: path.clone(),
                source: doc.text.clone(),
            };
            let mut test_modules = test_modules.clone();
            if is_test_path(root, &path) {
                test_modules.insert(module.name.clone(), module);
                return Ok(Self {
                    root: root.map(Path::to_path_buf),
                    std_modules,
                    src_modules,
                    test_modules,
                    analysis,
                });
            }
        }

        Ok(Self {
            root: root.map(Path::to_path_buf),
            std_modules,
            src_modules,
            test_modules,
            analysis,
        })
    }

    fn document_for_path(&self, path: &Path) -> Option<ModuleSource> {
        let module_name = module_name_for_path(path);
        self.src_modules
            .get(&module_name)
            .cloned()
            .or_else(|| self.test_modules.get(&module_name).cloned())
            .or_else(|| self.std_modules.get(&module_name).cloned())
    }

    fn module_named(&self, module_name: &str) -> Option<&ModuleSource> {
        self.src_modules
            .get(module_name)
            .or_else(|| self.test_modules.get(module_name))
            .or_else(|| self.std_modules.get(module_name))
    }

    fn definition_location(
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
        self.std_modules
            .values()
            .chain(self.src_modules.values())
            .chain(self.test_modules.values())
            .cloned()
            .collect()
    }

    fn reference_locations(
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

    fn reference_ranges(
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

    fn qualified_completion_items(&self, module: &str, prefix: &str) -> Vec<CompletionItem> {
        let mut items = Vec::new();
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
                        zierc::typecheck::type_display(&scheme.ty)
                    )),
                ));
            }
        } else if let Some(exports) = self.analysis.module_exports.get(module) {
            items.extend(completion_items_from_names(
                exports.clone(),
                prefix,
                CompletionItemKind::FUNCTION,
            ));
        }
        items.sort_by(|a, b| a.label.cmp(&b.label));
        items
    }

    fn unqualified_completion_items(
        &self,
        doc: &ModuleSource,
        analysis: &DocumentAnalysis,
        offset: usize,
        prefix: &str,
    ) -> std::result::Result<Vec<CompletionItem>, String> {
        let local_names = local_names_at_offset(&doc.path, &doc.source, offset)?;
        let mut items = Vec::new();
        let mut seen = HashSet::new();

        for name in local_names {
            push_completion_item(
                &mut items,
                &mut seen,
                completion_item(
                    name,
                    CompletionItemKind::VARIABLE,
                    Some("local".to_string()),
                ),
            );
        }

        for name in analysis.bindings.keys() {
            let detail = analysis
                .bindings
                .get(name)
                .map(|scheme| zierc::typecheck::type_display(&scheme.ty));
            push_completion_item(
                &mut items,
                &mut seen,
                completion_item(
                    name.clone(),
                    CompletionItemKind::FUNCTION,
                    detail.map(|ty| format!("{} | {ty}", doc.name)),
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
                        zierc::typecheck::type_display(&scheme.ty)
                    )),
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
                ),
            );
        }

        for (name, scheme) in zierc::typecheck::primitive_env() {
            push_completion_item(
                &mut items,
                &mut seen,
                completion_item(
                    name,
                    CompletionItemKind::FUNCTION,
                    Some(format!(
                        "builtin | {}",
                        zierc::typecheck::type_display(&scheme.ty)
                    )),
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

    #[allow(deprecated)]
    fn workspace_symbols(
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

    fn analyze_document(
        &self,
        doc: &ModuleSource,
    ) -> std::result::Result<DocumentAnalysis, String> {
        let visible_exports = visible_exports(&self.analysis, &self.test_modules, &doc.name);
        let imports =
            resolve_imports_for_source(doc.source.as_str(), &visible_exports, &self.analysis);
        let report = zierc::compile_with_imports_report(
            &doc.name,
            &doc.source,
            &source_path_for_compile(self.root.as_deref(), &doc.path),
            imports.imports.clone(),
            &visible_exports,
            imports.module_aliases.clone(),
            &imports.imported_type_decls,
            &imports.imported_schemes,
        );
        let diagnostics = report
            .diagnostics
            .iter()
            .map(|diag| diagnostic_to_lsp(&doc.source, diag))
            .collect();
        let bindings = zierc::infer_module_bindings(
            &doc.name,
            &doc.source,
            imports.imports.clone(),
            &visible_exports,
            &imports.imported_type_decls,
            &imports.imported_schemes,
        );
        let expr_types = zierc::infer_module_expr_types(
            &doc.name,
            &doc.source,
            imports.imports.clone(),
            &visible_exports,
            &imports.imported_type_decls,
            &imports.imported_schemes,
        );
        Ok(DocumentAnalysis {
            diagnostics,
            bindings,
            expr_types,
            imports,
        })
    }
}

fn build_project_analysis(
    src_modules: &BTreeMap<String, ModuleSource>,
) -> std::result::Result<ProjectAnalysis, String> {
    let std_mods = std_modules();
    let mut module_exports = HashMap::new();
    let mut module_type_decls = HashMap::new();

    for (user_name, _, source) in &std_mods {
        module_exports.insert(user_name.clone(), zierc::exported_names(source));
        module_type_decls.insert(user_name.clone(), zierc::exported_type_decls(source));
    }
    for (module_name, module) in src_modules {
        module_exports.insert(module_name.clone(), zierc::exported_names(&module.source));
        module_type_decls.insert(
            module_name.clone(),
            zierc::exported_type_decls(&module.source),
        );
    }

    let std_aliases: HashMap<String, String> = std_mods
        .iter()
        .map(|(user_name, erlang_name, _)| (user_name.clone(), erlang_name.clone()))
        .collect();

    let mut all_module_schemes: HashMap<String, zierc::typecheck::TypeEnv> = HashMap::new();
    for (user_name, _, source) in &std_mods {
        let imports = resolve_imports_for_source(
            source,
            &module_exports,
            &ProjectAnalysis {
                module_exports: module_exports.clone(),
                module_type_decls: module_type_decls.clone(),
                all_module_schemes: all_module_schemes.clone(),
                std_aliases: std_aliases.clone(),
            },
        );
        let schemes = zierc::infer_module_exports(
            user_name,
            source,
            imports.imports,
            &module_exports,
            &imports.imported_type_decls,
            &imports.imported_schemes,
        );
        all_module_schemes.insert(user_name.clone(), schemes);
    }

    for module_name in ordered_module_names(src_modules)? {
        let module = &src_modules[&module_name];
        let imports = resolve_imports_for_source(
            &module.source,
            &module_exports,
            &ProjectAnalysis {
                module_exports: module_exports.clone(),
                module_type_decls: module_type_decls.clone(),
                all_module_schemes: all_module_schemes.clone(),
                std_aliases: std_aliases.clone(),
            },
        );
        let schemes = zierc::infer_module_exports(
            &module.name,
            &module.source,
            imports.imports,
            &module_exports,
            &imports.imported_type_decls,
            &imports.imported_schemes,
        );
        all_module_schemes.insert(module.name.clone(), schemes);
    }

    Ok(ProjectAnalysis {
        module_exports,
        module_type_decls,
        all_module_schemes,
        std_aliases,
    })
}

fn visible_exports(
    analysis: &ProjectAnalysis,
    test_modules: &BTreeMap<String, ModuleSource>,
    current_module: &str,
) -> HashMap<String, Vec<String>> {
    let mut exports = analysis.module_exports.clone();
    for (module_name, module) in test_modules {
        if module_name == current_module {
            continue;
        }
        exports.insert(module_name.clone(), zierc::exported_names(&module.source));
    }
    exports
}

fn resolve_imports_for_source(
    source: &str,
    analysis_exports: &HashMap<String, Vec<String>>,
    project: &ProjectAnalysis,
) -> ResolvedImports {
    let mut imports = HashMap::new();
    let mut import_origins = HashMap::new();
    let mut imported_schemes = HashMap::new();

    for (_, mod_name, unqualified) in zierc::used_modules(source) {
        let erlang_name = project
            .std_aliases
            .get(&mod_name)
            .cloned()
            .unwrap_or_else(|| mod_name.clone());

        if let Some(exports) = analysis_exports.get(&mod_name) {
            for fn_name in exports {
                if unqualified.includes(fn_name) {
                    imports.insert(fn_name.clone(), erlang_name.clone());
                    import_origins.insert(fn_name.clone(), mod_name.clone());
                }
            }
        }

        if let Some(mod_schemes) = project.all_module_schemes.get(&mod_name) {
            for (fn_name, scheme) in mod_schemes {
                if unqualified.includes(fn_name) {
                    imported_schemes.insert(fn_name.clone(), scheme.clone());
                }
                imported_schemes.insert(format!("{mod_name}/{fn_name}"), scheme.clone());
            }
        }
    }

    let imported_type_decls: Vec<zierc::ast::TypeDecl> = referenced_modules(source)
        .into_iter()
        .flat_map(|mod_name| {
            project
                .module_type_decls
                .get(&mod_name)
                .cloned()
                .unwrap_or_default()
        })
        .collect();

    ResolvedImports {
        imports,
        import_origins,
        imported_schemes,
        imported_type_decls,
        module_aliases: project.std_aliases.clone(),
    }
}

fn referenced_modules(source: &str) -> HashSet<String> {
    let mut referenced: HashSet<String> = zierc::used_modules(source)
        .into_iter()
        .map(|(_, mod_name, _)| mod_name)
        .collect();
    for tok in zierc::lexer::Lexer::new(source).lex() {
        if let zierc::lexer::TokenKind::QualifiedIdent((module, _)) = tok.kind {
            referenced.insert(module);
        }
    }
    referenced
}

fn collect_modules(
    root: Option<&Path>,
    subdir: &str,
    overlays: &HashMap<Url, DocumentState>,
) -> BTreeMap<String, ModuleSource> {
    let mut modules = BTreeMap::new();
    if let Some(root) = root {
        let dir = root.join(subdir);
        collect_zier_files_from_dir(&dir, &mut modules);
    }
    for (uri, doc) in overlays {
        let Ok(path) = uri.to_file_path() else {
            continue;
        };
        let Some(root) = root else {
            continue;
        };
        if !path.starts_with(root.join(subdir)) {
            continue;
        }
        let module = ModuleSource {
            name: module_name_for_path(&path),
            path: path.clone(),
            source: doc.text.clone(),
        };
        modules.insert(module.name.clone(), module);
    }
    modules
}

fn collect_zier_files_from_dir(dir: &Path, modules: &mut BTreeMap<String, ModuleSource>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_zier_files_from_dir(&path, modules);
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("zier") {
            continue;
        }
        let Ok(source) = fs::read_to_string(&path) else {
            continue;
        };
        let module = ModuleSource {
            name: module_name_for_path(&path),
            path: path.clone(),
            source,
        };
        modules.insert(module.name.clone(), module);
    }
}

fn ordered_module_names(
    modules: &BTreeMap<String, ModuleSource>,
) -> std::result::Result<Vec<String>, String> {
    let mut graph: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (module_name, module) in modules {
        let mut deps = BTreeSet::new();
        for (namespace, dep, _) in zierc::used_modules(&module.source) {
            if namespace.is_empty() && modules.contains_key(&dep) {
                deps.insert(dep);
            }
        }
        graph.insert(module_name.clone(), deps.into_iter().collect());
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mark {
        Visiting,
        Done,
    }

    fn dfs(
        node: &str,
        graph: &BTreeMap<String, Vec<String>>,
        marks: &mut HashMap<String, Mark>,
        stack: &mut Vec<String>,
        out: &mut Vec<String>,
    ) -> std::result::Result<(), String> {
        match marks.get(node).copied() {
            Some(Mark::Done) => return Ok(()),
            Some(Mark::Visiting) => {
                let start = stack.iter().position(|n| n == node).unwrap_or(0);
                let mut cycle = stack[start..].to_vec();
                cycle.push(node.to_string());
                return Err(format!(
                    "cyclic module dependency detected: {}",
                    cycle.join(" -> ")
                ));
            }
            None => {}
        }
        marks.insert(node.to_string(), Mark::Visiting);
        stack.push(node.to_string());
        for dep in graph.get(node).cloned().unwrap_or_default() {
            dfs(&dep, graph, marks, stack, out)?;
        }
        stack.pop();
        marks.insert(node.to_string(), Mark::Done);
        out.push(node.to_string());
        Ok(())
    }

    let mut marks = HashMap::new();
    let mut stack = Vec::new();
    let mut out = Vec::new();
    for node in graph.keys() {
        dfs(node, &graph, &mut marks, &mut stack, &mut out)?;
    }
    Ok(out)
}

fn std_modules() -> Vec<(String, String, String)> {
    let lib_src = STD_DIR
        .get_file("lib.zier")
        .and_then(|f| f.contents_utf8())
        .unwrap_or("");
    let mut result = vec![(
        "std".to_string(),
        "zier_std".to_string(),
        lib_src.to_string(),
    )];

    for mod_name in zierc::pub_reexports(lib_src) {
        let file_name = format!("{mod_name}.zier");
        if let Some(src) = STD_DIR.get_file(&file_name).and_then(|f| f.contents_utf8()) {
            result.push((
                mod_name.clone(),
                format!("zier_{mod_name}"),
                src.to_string(),
            ));
        }
    }
    result
}

fn collect_std_modules() -> BTreeMap<String, ModuleSource> {
    let std_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../zier-std/src");
    std_modules()
        .into_iter()
        .map(|(name, _, source)| {
            let path = if name == "std" {
                std_root.join("lib.zier")
            } else {
                std_root.join(format!("{name}.zier"))
            };
            let module = ModuleSource {
                name: name.clone(),
                path,
                source,
            };
            (name, module)
        })
        .collect()
}

fn find_top_level_definition_range(
    source_path: &Path,
    source: &str,
    name: &str,
) -> std::result::Result<Option<Range>, String> {
    let mut lowerer = zierc::lower::Lowerer::new();
    let tokens = zierc::lexer::Lexer::new(source).lex();
    let file_id = lowerer.add_file(
        source_path.to_string_lossy().to_string(),
        source.to_string(),
    );
    let sexprs = zierc::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .map_err(|err| err.message.clone())?;
    let decls = lowerer.lower_file(file_id, &sexprs);
    if !lowerer.diagnostics.is_empty() {
        return Ok(None);
    }

    for decl in decls {
        match decl {
            zierc::ast::Declaration::Expression(zierc::ast::Expr::LetFunc {
                name: fn_name,
                name_span,
                ..
            }) if fn_name == name => {
                return Ok(Some(byte_range_to_lsp_range(
                    source,
                    name_span.start,
                    name_span.end,
                )));
            }
            zierc::ast::Declaration::ExternLet {
                name: fn_name,
                name_span,
                ..
            } if fn_name == name => {
                return Ok(Some(byte_range_to_lsp_range(
                    source,
                    name_span.start,
                    name_span.end,
                )));
            }
            _ => {}
        }
    }

    Ok(None)
}

fn find_project_root(path: &Path) -> Option<PathBuf> {
    let mut current = path.parent()?;
    loop {
        if current.join("zier.toml").exists() {
            return Some(current.to_path_buf());
        }
        current = current.parent()?;
    }
}

fn is_test_path(root: Option<&Path>, path: &Path) -> bool {
    root.is_some_and(|root| path.starts_with(root.join("tests")))
}

fn contains_path(modules: &BTreeMap<String, ModuleSource>, path: &Path) -> bool {
    modules.values().any(|module| module.path == path)
}

fn module_name_for_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string()
}

fn source_path_for_compile(root: Option<&Path>, path: &Path) -> String {
    root.and_then(|root| path.strip_prefix(root).ok())
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string())
}

fn full_text_change(changes: Vec<TextDocumentContentChangeEvent>) -> String {
    changes
        .into_iter()
        .last()
        .map(|change| change.text)
        .unwrap_or_default()
}

fn full_document_range(source: &str) -> Range {
    Range::new(
        Position::new(0, 0),
        offset_to_position(source, source.len()),
    )
}

fn best_expr_type_at_offset(
    expr_types: &[(std::ops::Range<usize>, String)],
    offset: usize,
) -> Option<String> {
    expr_types
        .iter()
        .filter(|(span, _)| span.start <= offset && offset <= span.end)
        .min_by_key(|(span, _)| span.end.saturating_sub(span.start))
        .map(|(_, ty)| ty.clone())
}

fn completion_items_from_names(
    names: Vec<String>,
    prefix: &str,
    kind: CompletionItemKind,
) -> Vec<CompletionItem> {
    let mut items: Vec<_> = names
        .into_iter()
        .filter(|name| name.starts_with(prefix))
        .map(|name| completion_item(name, kind, None))
        .collect();
    items.sort_by(|a, b| a.label.cmp(&b.label));
    items
}

fn completion_item(
    label: String,
    kind: CompletionItemKind,
    detail: Option<String>,
) -> CompletionItem {
    CompletionItem {
        label,
        kind: Some(kind),
        detail,
        ..CompletionItem::default()
    }
}

fn push_completion_item(
    items: &mut Vec<CompletionItem>,
    seen: &mut HashSet<String>,
    item: CompletionItem,
) {
    if seen.insert(item.label.clone()) {
        items.push(item);
    }
}

fn diagnostic_to_lsp(source: &str, diag: &CodeDiagnostic<usize>) -> Diagnostic {
    let label = diag
        .labels
        .iter()
        .find(|label| label.style == LabelStyle::Primary)
        .or_else(|| diag.labels.first());
    let range = label
        .map(|label| byte_range_to_lsp_range(source, label.range.start, label.range.end))
        .unwrap_or_else(|| Range::new(Position::new(0, 0), Position::new(0, 0)));

    let mut message = diag.message.clone();
    if !diag.notes.is_empty() {
        message.push('\n');
        message.push_str(&diag.notes.join("\n"));
    }

    Diagnostic {
        range,
        severity: Some(match diag.severity {
            Severity::Bug | Severity::Error => DiagnosticSeverity::ERROR,
            Severity::Warning => DiagnosticSeverity::WARNING,
            Severity::Note => DiagnosticSeverity::INFORMATION,
            Severity::Help => DiagnosticSeverity::HINT,
        }),
        message,
        ..Diagnostic::default()
    }
}

fn byte_range_to_lsp_range(source: &str, start: usize, end: usize) -> Range {
    Range::new(
        offset_to_position(source, start),
        offset_to_position(source, end),
    )
}

fn offset_to_position(source: &str, offset: usize) -> Position {
    let mut line = 0u32;
    let mut col = 0u32;
    let mut seen = 0usize;
    for ch in source.chars() {
        if seen >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += ch.len_utf16() as u32;
        }
        seen += ch.len_utf8();
    }
    Position::new(line, col)
}

fn position_to_offset(source: &str, position: Position) -> Option<usize> {
    let mut line = 0u32;
    let mut col = 0u32;
    let mut offset = 0usize;

    for ch in source.chars() {
        if line == position.line && col == position.character {
            return Some(offset);
        }
        if ch == '\n' {
            if line == position.line {
                return Some(offset);
            }
            line += 1;
            col = 0;
        } else if line == position.line {
            let next = col + ch.len_utf16() as u32;
            if next > position.character {
                return Some(offset);
            }
            col = next;
        }
        offset += ch.len_utf8();
    }

    if line == position.line && col == position.character {
        Some(offset)
    } else {
        None
    }
}

fn completion_context(source: &str, offset: usize) -> Option<CompletionContext> {
    if offset > source.len() {
        return None;
    }

    let prefix_start = scan_ident_start(source, offset);
    let prefix = source[prefix_start..offset].to_string();

    if prefix_start > 0 && source.as_bytes()[prefix_start - 1] == b'/' {
        let module_end = prefix_start - 1;
        let module_start = scan_ident_start(source, module_end);
        if module_start < module_end {
            return Some(CompletionContext::Qualified {
                module: source[module_start..module_end].to_string(),
                prefix,
            });
        }
    }

    Some(CompletionContext::Unqualified { prefix })
}

fn scan_ident_start(source: &str, offset: usize) -> usize {
    let bytes = source.as_bytes();
    let mut idx = offset;
    while idx > 0 && is_ident_byte(bytes[idx - 1]) {
        idx -= 1;
    }
    idx
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn local_names_at_offset(
    source_path: &Path,
    source: &str,
    offset: usize,
) -> std::result::Result<Vec<String>, String> {
    let (_, decls) = parse_module(source_path, source)?;
    for decl in &decls {
        if let Some(names) = local_names_in_decl(decl, offset, &HashSet::new()) {
            let mut names: Vec<_> = names.into_iter().collect();
            names.sort();
            return Ok(names);
        }
    }
    Ok(Vec::new())
}

fn local_names_in_decl(
    decl: &zierc::ast::Declaration,
    offset: usize,
    locals: &HashSet<String>,
) -> Option<HashSet<String>> {
    match decl {
        zierc::ast::Declaration::Expression(expr) => local_names_in_expr(expr, offset, locals),
        zierc::ast::Declaration::Test { body, .. } => local_names_in_expr(body, offset, locals),
        _ => None,
    }
}

fn local_names_in_expr(
    expr: &zierc::ast::Expr,
    offset: usize,
    locals: &HashSet<String>,
) -> Option<HashSet<String>> {
    if !span_contains(&expr.span(), offset) {
        return None;
    }

    use zierc::ast::Expr;
    match expr {
        Expr::Literal(_, _) | Expr::Variable(_, _) => Some(locals.clone()),
        Expr::List(items, _) => items
            .iter()
            .find_map(|item| local_names_in_expr(item, offset, locals))
            .or_else(|| Some(locals.clone())),
        Expr::LetFunc {
            name, args, value, ..
        } => {
            let mut inner = locals.clone();
            inner.insert(name.clone());
            inner.extend(args.iter().cloned());
            local_names_in_expr(value, offset, &inner).or_else(|| Some(inner))
        }
        Expr::LetLocal {
            name, value, body, ..
        } => local_names_in_expr(value, offset, locals).or_else(|| {
            let mut inner = locals.clone();
            inner.insert(name.clone());
            local_names_in_expr(body, offset, &inner).or(Some(inner))
        }),
        Expr::If {
            cond, then, els, ..
        } => local_names_in_expr(cond, offset, locals)
            .or_else(|| local_names_in_expr(then, offset, locals))
            .or_else(|| local_names_in_expr(els, offset, locals))
            .or_else(|| Some(locals.clone())),
        Expr::Call { func, args, .. } => local_names_in_expr(func, offset, locals)
            .or_else(|| {
                args.iter()
                    .find_map(|arg| local_names_in_expr(arg, offset, locals))
            })
            .or_else(|| Some(locals.clone())),
        Expr::Match { targets, arms, .. } => targets
            .iter()
            .find_map(|target| local_names_in_expr(target, offset, locals))
            .or_else(|| {
                arms.iter().find_map(|(pats, body)| {
                    let mut inner = locals.clone();
                    for pat in pats {
                        bind_pattern_names(pat, &mut inner);
                    }
                    local_names_in_expr(body, offset, &inner)
                })
            })
            .or_else(|| Some(locals.clone())),
        Expr::FieldAccess { record, .. } => {
            local_names_in_expr(record, offset, locals).or_else(|| Some(locals.clone()))
        }
        Expr::RecordConstruct { fields, .. } => fields
            .iter()
            .find_map(|(_, value)| local_names_in_expr(value, offset, locals))
            .or_else(|| Some(locals.clone())),
        Expr::Lambda { args, body, .. } => {
            let mut inner = locals.clone();
            inner.extend(args.iter().cloned());
            local_names_in_expr(body, offset, &inner).or(Some(inner))
        }
        Expr::QualifiedCall { args, .. } => args
            .iter()
            .find_map(|arg| local_names_in_expr(arg, offset, locals))
            .or_else(|| Some(locals.clone())),
    }
}

fn span_contains(span: &std::ops::Range<usize>, offset: usize) -> bool {
    span.start <= offset && offset <= span.end
}

fn symbol_at(
    source_path: &Path,
    source: &str,
    current_module: &str,
    imports: &ResolvedImports,
    offset: usize,
) -> std::result::Result<Option<Symbol>, String> {
    let occurrences = collect_symbol_occurrences(source_path, source, current_module, imports)?;
    Ok(occurrences
        .into_iter()
        .filter(|occ| occ.range.start <= offset && offset <= occ.range.end)
        .min_by_key(|occ| occ.range.end.saturating_sub(occ.range.start))
        .map(|occ| occ.symbol))
}

fn scheme_for_symbol(
    project: &Project,
    doc: &ModuleSource,
    analysis: &DocumentAnalysis,
    symbol: &Symbol,
) -> Option<zierc::typecheck::Scheme> {
    if symbol.module == doc.name {
        analysis.bindings.get(&symbol.function).cloned()
    } else {
        analysis
            .imports
            .imported_schemes
            .get(&format!("{}/{}", symbol.module, symbol.function))
            .cloned()
            .or_else(|| {
                project
                    .analysis
                    .all_module_schemes
                    .get(&symbol.module)
                    .and_then(|env| env.get(&symbol.function))
                    .cloned()
            })
    }
}

fn signature_target_at(
    source_path: &Path,
    source: &str,
    current_module: &str,
    imports: &ResolvedImports,
    offset: usize,
) -> std::result::Result<Option<SignatureTarget>, String> {
    let (_, decls) = parse_module(source_path, source)?;
    let top_level = top_level_bindings(&decls);
    for decl in &decls {
        if let Some(target) = signature_target_in_decl(
            decl,
            current_module,
            &top_level,
            imports,
            offset,
            &HashSet::new(),
        ) {
            return Ok(Some(target));
        }
    }
    Ok(None)
}

fn signature_target_in_decl(
    decl: &zierc::ast::Declaration,
    current_module: &str,
    top_level: &HashSet<String>,
    imports: &ResolvedImports,
    offset: usize,
    locals: &HashSet<String>,
) -> Option<SignatureTarget> {
    match decl {
        zierc::ast::Declaration::Expression(expr) => {
            signature_target_in_expr(expr, current_module, top_level, imports, offset, locals)
        }
        zierc::ast::Declaration::Test { body, .. } => {
            signature_target_in_expr(body, current_module, top_level, imports, offset, locals)
        }
        _ => None,
    }
}

fn signature_target_in_expr(
    expr: &zierc::ast::Expr,
    current_module: &str,
    top_level: &HashSet<String>,
    imports: &ResolvedImports,
    offset: usize,
    locals: &HashSet<String>,
) -> Option<SignatureTarget> {
    if !span_contains(&expr.span(), offset) {
        return None;
    }

    use zierc::ast::Expr;
    match expr {
        Expr::Literal(_, _) | Expr::Variable(_, _) => None,
        Expr::List(items, _) => items.iter().find_map(|item| {
            signature_target_in_expr(item, current_module, top_level, imports, offset, locals)
        }),
        Expr::LetFunc {
            name, args, value, ..
        } => {
            let mut inner = locals.clone();
            inner.insert(name.clone());
            inner.extend(args.iter().cloned());
            signature_target_in_expr(value, current_module, top_level, imports, offset, &inner)
        }
        Expr::LetLocal {
            name, value, body, ..
        } => signature_target_in_expr(value, current_module, top_level, imports, offset, locals)
            .or_else(|| {
                let mut inner = locals.clone();
                inner.insert(name.clone());
                signature_target_in_expr(body, current_module, top_level, imports, offset, &inner)
            }),
        Expr::If {
            cond, then, els, ..
        } => signature_target_in_expr(cond, current_module, top_level, imports, offset, locals)
            .or_else(|| {
                signature_target_in_expr(then, current_module, top_level, imports, offset, locals)
            })
            .or_else(|| {
                signature_target_in_expr(els, current_module, top_level, imports, offset, locals)
            }),
        Expr::Call { func, args, .. } => {
            for arg in args {
                if let Some(target) = signature_target_in_expr(
                    arg,
                    current_module,
                    top_level,
                    imports,
                    offset,
                    locals,
                ) {
                    return Some(target);
                }
            }
            if let Some(target) =
                signature_target_in_expr(func, current_module, top_level, imports, offset, locals)
            {
                return Some(target);
            }

            let Expr::Variable(name, span) = func.as_ref() else {
                return None;
            };
            if offset < span.end || locals.contains(name) {
                return None;
            }
            let symbol = if top_level.contains(name) {
                Some(Symbol {
                    module: current_module.to_string(),
                    function: name.clone(),
                })
            } else {
                imports.import_origins.get(name).map(|module| Symbol {
                    module: module.clone(),
                    function: name.clone(),
                })
            }?;
            Some(SignatureTarget {
                symbol,
                arg_index: active_argument_index(args, offset),
            })
        }
        Expr::Match { targets, arms, .. } => targets
            .iter()
            .find_map(|target| {
                signature_target_in_expr(target, current_module, top_level, imports, offset, locals)
            })
            .or_else(|| {
                arms.iter().find_map(|(pats, body)| {
                    let mut inner = locals.clone();
                    for pat in pats {
                        bind_pattern_names(pat, &mut inner);
                    }
                    signature_target_in_expr(
                        body,
                        current_module,
                        top_level,
                        imports,
                        offset,
                        &inner,
                    )
                })
            }),
        Expr::FieldAccess { record, .. } => {
            signature_target_in_expr(record, current_module, top_level, imports, offset, locals)
        }
        Expr::RecordConstruct { fields, .. } => fields.iter().find_map(|(_, value)| {
            signature_target_in_expr(value, current_module, top_level, imports, offset, locals)
        }),
        Expr::Lambda { args, body, .. } => {
            let mut inner = locals.clone();
            inner.extend(args.iter().cloned());
            signature_target_in_expr(body, current_module, top_level, imports, offset, &inner)
        }
        Expr::QualifiedCall {
            module,
            function,
            args,
            fn_span,
            ..
        } => {
            for arg in args {
                if let Some(target) = signature_target_in_expr(
                    arg,
                    current_module,
                    top_level,
                    imports,
                    offset,
                    locals,
                ) {
                    return Some(target);
                }
            }
            if offset < fn_span.end {
                return None;
            }
            Some(SignatureTarget {
                symbol: Symbol {
                    module: module.clone(),
                    function: function.clone(),
                },
                arg_index: active_argument_index(args, offset),
            })
        }
    }
}

fn active_argument_index(args: &[zierc::ast::Expr], offset: usize) -> usize {
    args.iter()
        .enumerate()
        .take_while(|(_, arg)| arg.span().start <= offset)
        .map(|(index, _)| index)
        .last()
        .unwrap_or(0)
}

fn function_arity(ty: &std::rc::Rc<zierc::typecheck::Type>) -> usize {
    let mut count = 0;
    let mut current = ty.as_ref();
    while let zierc::typecheck::Type::Fun(_, ret) = current {
        count += 1;
        current = ret.as_ref();
    }
    count
}

fn collect_symbol_occurrences(
    source_path: &Path,
    source: &str,
    current_module: &str,
    imports: &ResolvedImports,
) -> std::result::Result<Vec<SymbolOccurrence>, String> {
    let (sexprs, decls) = parse_module(source_path, source)?;
    let top_level = top_level_bindings(&decls);
    let mut out = collect_use_import_occurrences(source, &sexprs)?;
    for decl in &decls {
        collect_decl_occurrences(decl, current_module, &top_level, imports, &mut out);
    }
    Ok(out)
}

fn parse_module(
    source_path: &Path,
    source: &str,
) -> std::result::Result<(Vec<zierc::sexpr::SExpr>, Vec<zierc::ast::Declaration>), String> {
    let mut lowerer = zierc::lower::Lowerer::new();
    let tokens = zierc::lexer::Lexer::new(source).lex();
    let file_id = lowerer.add_file(
        source_path.to_string_lossy().to_string(),
        source.to_string(),
    );
    let sexprs = zierc::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .map_err(|err| err.message.clone())?;
    let decls = lowerer.lower_file(file_id, &sexprs);
    if !lowerer.diagnostics.is_empty() {
        return Err(lowerer.diagnostics[0].message.clone());
    }
    Ok((sexprs, decls))
}

fn top_level_symbols(
    source_path: &Path,
    source: &str,
) -> std::result::Result<Vec<TopLevelSymbol>, String> {
    let (_, decls) = parse_module(source_path, source)?;
    let mut out = Vec::new();
    for decl in decls {
        match decl {
            zierc::ast::Declaration::Expression(zierc::ast::Expr::LetFunc {
                name,
                name_span,
                span,
                ..
            }) => out.push(TopLevelSymbol {
                name,
                kind: SymbolKind::FUNCTION,
                selection_range: name_span,
                full_range: span,
            }),
            zierc::ast::Declaration::ExternLet {
                name,
                name_span,
                span,
                ..
            } => out.push(TopLevelSymbol {
                name,
                kind: SymbolKind::FUNCTION,
                selection_range: name_span,
                full_range: span,
            }),
            zierc::ast::Declaration::Type(zierc::ast::TypeDecl::Record { name, span, .. }) => {
                out.push(TopLevelSymbol {
                    name,
                    kind: SymbolKind::STRUCT,
                    selection_range: span.clone(),
                    full_range: span,
                });
            }
            zierc::ast::Declaration::Type(zierc::ast::TypeDecl::Variant { name, span, .. }) => {
                out.push(TopLevelSymbol {
                    name,
                    kind: SymbolKind::ENUM,
                    selection_range: span.clone(),
                    full_range: span,
                });
            }
            zierc::ast::Declaration::Test { name, span, .. } => out.push(TopLevelSymbol {
                name,
                kind: SymbolKind::EVENT,
                selection_range: span.clone(),
                full_range: span,
            }),
            zierc::ast::Declaration::ExternType { name, span, .. } => out.push(TopLevelSymbol {
                name,
                kind: SymbolKind::CLASS,
                selection_range: span.clone(),
                full_range: span,
            }),
            zierc::ast::Declaration::Use { .. } | zierc::ast::Declaration::Expression(_) => {}
        }
    }
    Ok(out)
}

fn top_level_bindings(decls: &[zierc::ast::Declaration]) -> HashSet<String> {
    decls
        .iter()
        .filter_map(|decl| match decl {
            zierc::ast::Declaration::Expression(zierc::ast::Expr::LetFunc { name, .. }) => {
                Some(name.clone())
            }
            zierc::ast::Declaration::ExternLet { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect()
}

fn collect_use_import_occurrences(
    source: &str,
    sexprs: &[zierc::sexpr::SExpr],
) -> std::result::Result<Vec<SymbolOccurrence>, String> {
    let mut out = Vec::new();
    for sexpr in sexprs {
        let zierc::sexpr::SExpr::Round(items, _) = sexpr else {
            continue;
        };
        let (path_item, imports_item) = match items.as_slice() {
            [zierc::sexpr::SExpr::Atom(tok), path] if tok.kind == zierc::lexer::TokenKind::Use => {
                (path, None)
            }
            [
                zierc::sexpr::SExpr::Atom(pub_tok),
                zierc::sexpr::SExpr::Atom(use_tok),
                path,
            ] if pub_tok.kind == zierc::lexer::TokenKind::Pub
                && use_tok.kind == zierc::lexer::TokenKind::Use =>
            {
                (path, None)
            }
            [zierc::sexpr::SExpr::Atom(tok), path, imports]
                if tok.kind == zierc::lexer::TokenKind::Use =>
            {
                (path, Some(imports))
            }
            [
                zierc::sexpr::SExpr::Atom(pub_tok),
                zierc::sexpr::SExpr::Atom(use_tok),
                path,
                imports,
            ] if pub_tok.kind == zierc::lexer::TokenKind::Pub
                && use_tok.kind == zierc::lexer::TokenKind::Use =>
            {
                (path, Some(imports))
            }
            _ => continue,
        };

        let module = match path_item {
            zierc::sexpr::SExpr::Atom(tok) => match &tok.kind {
                zierc::lexer::TokenKind::QualifiedIdent((_, module)) => module.clone(),
                zierc::lexer::TokenKind::Ident => source[tok.span.clone()].to_string(),
                _ => continue,
            },
            _ => continue,
        };

        let Some(zierc::sexpr::SExpr::Square(items, _)) = imports_item else {
            continue;
        };
        for item in items {
            let zierc::sexpr::SExpr::Atom(tok) = item else {
                continue;
            };
            if !matches!(tok.kind, zierc::lexer::TokenKind::Ident) {
                continue;
            }
            out.push(SymbolOccurrence {
                symbol: Symbol {
                    module: module.clone(),
                    function: source[tok.span.clone()].to_string(),
                },
                range: tok.span.clone(),
                kind: OccurrenceKind::Reference,
            });
        }
    }
    Ok(out)
}

fn collect_decl_occurrences(
    decl: &zierc::ast::Declaration,
    current_module: &str,
    top_level: &HashSet<String>,
    imports: &ResolvedImports,
    out: &mut Vec<SymbolOccurrence>,
) {
    match decl {
        zierc::ast::Declaration::Expression(expr) => {
            collect_expr_occurrences(
                expr,
                current_module,
                top_level,
                imports,
                &HashSet::new(),
                out,
            );
        }
        zierc::ast::Declaration::ExternLet {
            name, name_span, ..
        } => out.push(SymbolOccurrence {
            symbol: Symbol {
                module: current_module.to_string(),
                function: name.clone(),
            },
            range: name_span.clone(),
            kind: OccurrenceKind::Definition,
        }),
        zierc::ast::Declaration::Test { body, .. } => {
            collect_expr_occurrences(
                body,
                current_module,
                top_level,
                imports,
                &HashSet::new(),
                out,
            );
        }
        _ => {}
    }
}

fn collect_expr_occurrences(
    expr: &zierc::ast::Expr,
    current_module: &str,
    top_level: &HashSet<String>,
    imports: &ResolvedImports,
    locals: &HashSet<String>,
    out: &mut Vec<SymbolOccurrence>,
) {
    use zierc::ast::Expr;

    match expr {
        Expr::Literal(_, _) => {}
        Expr::Variable(name, span) => {
            if locals.contains(name) {
                return;
            }
            let symbol = if top_level.contains(name) {
                Some(Symbol {
                    module: current_module.to_string(),
                    function: name.clone(),
                })
            } else {
                imports.import_origins.get(name).map(|module| Symbol {
                    module: module.clone(),
                    function: name.clone(),
                })
            };
            if let Some(symbol) = symbol {
                out.push(SymbolOccurrence {
                    symbol,
                    range: span.clone(),
                    kind: OccurrenceKind::Reference,
                });
            }
        }
        Expr::List(items, _) => {
            for item in items {
                collect_expr_occurrences(item, current_module, top_level, imports, locals, out);
            }
        }
        Expr::LetFunc {
            name,
            name_span,
            args,
            value,
            ..
        } => {
            out.push(SymbolOccurrence {
                symbol: Symbol {
                    module: current_module.to_string(),
                    function: name.clone(),
                },
                range: name_span.clone(),
                kind: OccurrenceKind::Definition,
            });
            let mut inner = locals.clone();
            inner.insert(name.clone());
            inner.extend(args.iter().cloned());
            collect_expr_occurrences(value, current_module, top_level, imports, &inner, out);
        }
        Expr::LetLocal {
            name, value, body, ..
        } => {
            collect_expr_occurrences(value, current_module, top_level, imports, locals, out);
            let mut inner = locals.clone();
            inner.insert(name.clone());
            collect_expr_occurrences(body, current_module, top_level, imports, &inner, out);
        }
        Expr::If {
            cond, then, els, ..
        } => {
            collect_expr_occurrences(cond, current_module, top_level, imports, locals, out);
            collect_expr_occurrences(then, current_module, top_level, imports, locals, out);
            collect_expr_occurrences(els, current_module, top_level, imports, locals, out);
        }
        Expr::Call { func, args, .. } => {
            collect_expr_occurrences(func, current_module, top_level, imports, locals, out);
            for arg in args {
                collect_expr_occurrences(arg, current_module, top_level, imports, locals, out);
            }
        }
        Expr::Match { targets, arms, .. } => {
            for target in targets {
                collect_expr_occurrences(target, current_module, top_level, imports, locals, out);
            }
            for (pats, body) in arms {
                let mut inner = locals.clone();
                for pat in pats {
                    bind_pattern_names(pat, &mut inner);
                }
                collect_expr_occurrences(body, current_module, top_level, imports, &inner, out);
            }
        }
        Expr::FieldAccess { record, .. } => {
            collect_expr_occurrences(record, current_module, top_level, imports, locals, out);
        }
        Expr::RecordConstruct { fields, .. } => {
            for (_, value) in fields {
                collect_expr_occurrences(value, current_module, top_level, imports, locals, out);
            }
        }
        Expr::Lambda { args, body, .. } => {
            let mut inner = locals.clone();
            inner.extend(args.iter().cloned());
            collect_expr_occurrences(body, current_module, top_level, imports, &inner, out);
        }
        Expr::QualifiedCall {
            module,
            function,
            args,
            fn_span,
            ..
        } => {
            out.push(SymbolOccurrence {
                symbol: Symbol {
                    module: module.clone(),
                    function: function.clone(),
                },
                range: qualified_function_range(fn_span, function),
                kind: OccurrenceKind::Reference,
            });
            for arg in args {
                collect_expr_occurrences(arg, current_module, top_level, imports, locals, out);
            }
        }
    }
}

fn qualified_function_range(
    span: &std::ops::Range<usize>,
    function: &str,
) -> std::ops::Range<usize> {
    let end = span.end;
    let start = end.saturating_sub(function.len());
    start..end
}

fn find_hover_target(source_path: &Path, source: &str, offset: usize) -> Option<HoverTarget> {
    let mut lowerer = zierc::lower::Lowerer::new();
    let tokens = zierc::lexer::Lexer::new(source).lex();
    let file_id = lowerer.add_file(
        source_path.to_string_lossy().to_string(),
        source.to_string(),
    );
    let sexprs = zierc::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .ok()?;
    let decls = lowerer.lower_file(file_id, &sexprs);
    if !lowerer.diagnostics.is_empty() {
        return None;
    }

    for decl in &decls {
        if let Some(target) = hover_target_in_decl(decl, offset, &HashSet::new()) {
            return Some(target);
        }
    }
    None
}

fn hover_target_in_decl(
    decl: &zierc::ast::Declaration,
    offset: usize,
    locals: &HashSet<String>,
) -> Option<HoverTarget> {
    match decl {
        zierc::ast::Declaration::Expression(expr) => hover_target_in_expr(expr, offset, locals),
        zierc::ast::Declaration::Test { body, .. } => hover_target_in_expr(body, offset, locals),
        _ => None,
    }
}

fn hover_target_in_expr(
    expr: &zierc::ast::Expr,
    offset: usize,
    locals: &HashSet<String>,
) -> Option<HoverTarget> {
    use zierc::ast::Expr;
    match expr {
        Expr::Literal(_, _) => None,
        Expr::Variable(name, span) => {
            if span.start <= offset && offset <= span.end && !locals.contains(name) {
                Some(HoverTarget::Unqualified(name.clone()))
            } else {
                None
            }
        }
        Expr::List(items, _) => items
            .iter()
            .find_map(|item| hover_target_in_expr(item, offset, locals)),
        Expr::LetFunc {
            name, args, value, ..
        } => {
            let mut inner = locals.clone();
            inner.insert(name.clone());
            inner.extend(args.iter().cloned());
            hover_target_in_expr(value, offset, &inner)
        }
        Expr::LetLocal {
            name, value, body, ..
        } => hover_target_in_expr(value, offset, locals).or_else(|| {
            let mut inner = locals.clone();
            inner.insert(name.clone());
            hover_target_in_expr(body, offset, &inner)
        }),
        Expr::If {
            cond, then, els, ..
        } => hover_target_in_expr(cond, offset, locals)
            .or_else(|| hover_target_in_expr(then, offset, locals))
            .or_else(|| hover_target_in_expr(els, offset, locals)),
        Expr::Call { func, args, .. } => hover_target_in_expr(func, offset, locals).or_else(|| {
            args.iter()
                .find_map(|arg| hover_target_in_expr(arg, offset, locals))
        }),
        Expr::Match { targets, arms, .. } => targets
            .iter()
            .find_map(|target| hover_target_in_expr(target, offset, locals))
            .or_else(|| {
                arms.iter().find_map(|(pats, body)| {
                    let mut inner = locals.clone();
                    for pat in pats {
                        bind_pattern_names(pat, &mut inner);
                    }
                    hover_target_in_expr(body, offset, &inner)
                })
            }),
        Expr::FieldAccess { record, .. } => hover_target_in_expr(record, offset, locals),
        Expr::RecordConstruct { fields, .. } => fields
            .iter()
            .find_map(|(_, value)| hover_target_in_expr(value, offset, locals)),
        Expr::Lambda { args, body, .. } => {
            let mut inner = locals.clone();
            inner.extend(args.iter().cloned());
            hover_target_in_expr(body, offset, &inner)
        }
        Expr::QualifiedCall {
            module,
            function,
            args,
            fn_span,
            ..
        } => {
            if fn_span.start <= offset && offset <= fn_span.end {
                Some(HoverTarget::Qualified {
                    module: module.clone(),
                    function: function.clone(),
                })
            } else {
                args.iter()
                    .find_map(|arg| hover_target_in_expr(arg, offset, locals))
            }
        }
    }
}

fn local_symbol_at(
    source_path: &Path,
    source: &str,
    offset: usize,
) -> std::result::Result<Option<LocalSymbol>, String> {
    let occurrences = collect_local_occurrences(source_path, source)?;
    Ok(occurrences
        .into_iter()
        .filter(|occ| occ.range.start <= offset && offset <= occ.range.end)
        .min_by_key(|occ| occ.range.end.saturating_sub(occ.range.start))
        .map(|occ| occ.symbol))
}

fn collect_local_occurrences(
    source_path: &Path,
    source: &str,
) -> std::result::Result<Vec<LocalOccurrence>, String> {
    let (_, decls) = parse_module(source_path, source)?;
    let mut out = Vec::new();
    for decl in &decls {
        collect_local_occurrences_in_decl(decl, &HashMap::new(), &mut out);
    }
    Ok(out)
}

fn collect_local_occurrences_in_decl(
    decl: &zierc::ast::Declaration,
    locals: &HashMap<String, LocalSymbol>,
    out: &mut Vec<LocalOccurrence>,
) {
    match decl {
        zierc::ast::Declaration::Expression(expr) => {
            collect_local_occurrences_in_expr(expr, locals, out);
        }
        zierc::ast::Declaration::Test { body, .. } => {
            collect_local_occurrences_in_expr(body, locals, out);
        }
        _ => {}
    }
}

fn collect_local_occurrences_in_expr(
    expr: &zierc::ast::Expr,
    locals: &HashMap<String, LocalSymbol>,
    out: &mut Vec<LocalOccurrence>,
) {
    use zierc::ast::Expr;

    match expr {
        Expr::Literal(_, _) => {}
        Expr::Variable(name, span) => {
            if let Some(symbol) = locals.get(name) {
                out.push(LocalOccurrence {
                    symbol: symbol.clone(),
                    range: span.clone(),
                    kind: OccurrenceKind::Reference,
                });
            }
        }
        Expr::List(items, _) => {
            for item in items {
                collect_local_occurrences_in_expr(item, locals, out);
            }
        }
        Expr::LetFunc {
            args,
            arg_spans,
            value,
            ..
        } => {
            let mut inner = locals.clone();
            for (arg, span) in args.iter().zip(arg_spans.iter()) {
                let symbol = LocalSymbol {
                    name: arg.clone(),
                    def_range: span.clone(),
                };
                out.push(LocalOccurrence {
                    symbol: symbol.clone(),
                    range: span.clone(),
                    kind: OccurrenceKind::Definition,
                });
                inner.insert(arg.clone(), symbol);
            }
            collect_local_occurrences_in_expr(value, &inner, out);
        }
        Expr::LetLocal {
            name,
            name_span,
            value,
            body,
            ..
        } => {
            collect_local_occurrences_in_expr(value, locals, out);
            let mut inner = locals.clone();
            let symbol = LocalSymbol {
                name: name.clone(),
                def_range: name_span.clone(),
            };
            out.push(LocalOccurrence {
                symbol: symbol.clone(),
                range: name_span.clone(),
                kind: OccurrenceKind::Definition,
            });
            inner.insert(name.clone(), symbol);
            collect_local_occurrences_in_expr(body, &inner, out);
        }
        Expr::If {
            cond, then, els, ..
        } => {
            collect_local_occurrences_in_expr(cond, locals, out);
            collect_local_occurrences_in_expr(then, locals, out);
            collect_local_occurrences_in_expr(els, locals, out);
        }
        Expr::Call { func, args, .. } => {
            collect_local_occurrences_in_expr(func, locals, out);
            for arg in args {
                collect_local_occurrences_in_expr(arg, locals, out);
            }
        }
        Expr::Match { targets, arms, .. } => {
            for target in targets {
                collect_local_occurrences_in_expr(target, locals, out);
            }
            for (pats, body) in arms {
                let mut inner = locals.clone();
                for pat in pats {
                    bind_pattern_locals(pat, &mut inner, out);
                }
                collect_local_occurrences_in_expr(body, &inner, out);
            }
        }
        Expr::FieldAccess { record, .. } => {
            collect_local_occurrences_in_expr(record, locals, out);
        }
        Expr::RecordConstruct { fields, .. } => {
            for (_, value) in fields {
                collect_local_occurrences_in_expr(value, locals, out);
            }
        }
        Expr::Lambda {
            args,
            arg_spans,
            body,
            ..
        } => {
            let mut inner = locals.clone();
            for (arg, span) in args.iter().zip(arg_spans.iter()) {
                let symbol = LocalSymbol {
                    name: arg.clone(),
                    def_range: span.clone(),
                };
                out.push(LocalOccurrence {
                    symbol: symbol.clone(),
                    range: span.clone(),
                    kind: OccurrenceKind::Definition,
                });
                inner.insert(arg.clone(), symbol);
            }
            collect_local_occurrences_in_expr(body, &inner, out);
        }
        Expr::QualifiedCall { args, .. } => {
            for arg in args {
                collect_local_occurrences_in_expr(arg, locals, out);
            }
        }
    }
}

fn bind_pattern_locals(
    pat: &zierc::ast::Pattern,
    locals: &mut HashMap<String, LocalSymbol>,
    out: &mut Vec<LocalOccurrence>,
) {
    match pat {
        zierc::ast::Pattern::Variable(name, span) => {
            let symbol = LocalSymbol {
                name: name.clone(),
                def_range: span.clone(),
            };
            out.push(LocalOccurrence {
                symbol: symbol.clone(),
                range: span.clone(),
                kind: OccurrenceKind::Definition,
            });
            locals.insert(name.clone(), symbol);
        }
        zierc::ast::Pattern::Constructor(_, args, _) | zierc::ast::Pattern::Or(args, _) => {
            for arg in args {
                bind_pattern_locals(arg, locals, out);
            }
        }
        zierc::ast::Pattern::Cons(head, tail, _) => {
            bind_pattern_locals(head, locals, out);
            bind_pattern_locals(tail, locals, out);
        }
        zierc::ast::Pattern::Any(_)
        | zierc::ast::Pattern::Literal(_, _)
        | zierc::ast::Pattern::EmptyList(_) => {}
    }
}

fn bind_pattern_names(pat: &zierc::ast::Pattern, out: &mut HashSet<String>) {
    match pat {
        zierc::ast::Pattern::Variable(name, _) => {
            out.insert(name.clone());
        }
        zierc::ast::Pattern::Constructor(_, args, _) => {
            for arg in args {
                bind_pattern_names(arg, out);
            }
        }
        zierc::ast::Pattern::Or(pats, _) => {
            for pat in pats {
                bind_pattern_names(pat, out);
            }
        }
        zierc::ast::Pattern::Cons(head, tail, _) => {
            bind_pattern_names(head, out);
            bind_pattern_names(tail, out);
        }
        zierc::ast::Pattern::Any(_)
        | zierc::ast::Pattern::Literal(_, _)
        | zierc::ast::Pattern::EmptyList(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_offset_roundtrip_handles_ascii() {
        let src = "(let main {} (io/debug 1))\n";
        let pos = Position::new(0, 14);
        let offset = position_to_offset(src, pos).unwrap();
        assert_eq!(offset_to_position(src, offset), pos);
    }

    #[test]
    fn hover_target_finds_imported_function_reference() {
        let src = "(use std/testing [assert_eq])\n(let main {} (assert_eq 1 1))";
        let offset = src.rfind("assert_eq").unwrap();
        let target = find_hover_target(Path::new("src/main.zier"), src, offset);
        match target {
            Some(HoverTarget::Unqualified(name)) => assert_eq!(name, "assert_eq"),
            other => panic!("unexpected target: {other:?}"),
        }
    }

    #[test]
    fn hover_target_ignores_local_bindings() {
        let src = "(let main {} (let [assert_eq 1] assert_eq))";
        let offset = src.rfind("assert_eq").unwrap();
        assert!(find_hover_target(Path::new("src/main.zier"), src, offset).is_none());
    }

    #[test]
    fn full_document_range_covers_entire_source() {
        let src = "(let add {a b} (+ a b))\n";
        let range = full_document_range(src);
        assert_eq!(range.start, Position::new(0, 0));
        assert_eq!(range.end, Position::new(1, 0));
    }

    #[test]
    fn best_expr_type_prefers_smallest_matching_span() {
        let expr_types = vec![(0..10, "Int".to_string()), (4..7, "String".to_string())];
        assert_eq!(
            best_expr_type_at_offset(&expr_types, 5),
            Some("String".to_string())
        );
    }

    #[test]
    fn find_top_level_definition_range_points_at_function_name() {
        let src = "(let add_one {x} (+ x 1))\n";
        let range =
            find_top_level_definition_range(Path::new("src/main.zier"), src, "add_one").unwrap();
        assert_eq!(
            range,
            Some(Range::new(Position::new(0, 5), Position::new(0, 12)))
        );
    }

    #[test]
    fn symbol_at_resolves_top_level_definition_site() {
        let src = "(let add_one {x} (+ x 1))\n(let main {} (add_one 2))";
        let imports = ResolvedImports {
            imports: HashMap::new(),
            import_origins: HashMap::new(),
            imported_schemes: HashMap::new(),
            imported_type_decls: Vec::new(),
            module_aliases: HashMap::new(),
        };
        let offset = src.find("add_one").unwrap();
        let symbol = symbol_at(Path::new("src/main.zier"), src, "main", &imports, offset)
            .unwrap()
            .unwrap();
        assert_eq!(
            symbol,
            Symbol {
                module: "main".to_string(),
                function: "add_one".to_string(),
            }
        );
    }

    #[test]
    fn symbol_at_resolves_import_list_entries() {
        let src = "(use std/testing [assert_eq])\n(let main {} (assert_eq 1 1))";
        let mut import_origins = HashMap::new();
        import_origins.insert("assert_eq".to_string(), "testing".to_string());
        let imports = ResolvedImports {
            imports: HashMap::new(),
            import_origins,
            imported_schemes: HashMap::new(),
            imported_type_decls: Vec::new(),
            module_aliases: HashMap::new(),
        };
        let offset = src.find("assert_eq").unwrap();
        let symbol = symbol_at(Path::new("src/main.zier"), src, "main", &imports, offset)
            .unwrap()
            .unwrap();
        assert_eq!(
            symbol,
            Symbol {
                module: "testing".to_string(),
                function: "assert_eq".to_string(),
            }
        );
    }

    #[test]
    fn collect_symbol_occurrences_includes_imports_defs_and_refs() {
        let src = "(use util [map])\n(let map {x} x)\n(let main {} (util/map (map 1)))";
        let mut import_origins = HashMap::new();
        import_origins.insert("map".to_string(), "util".to_string());
        let imports = ResolvedImports {
            imports: HashMap::new(),
            import_origins,
            imported_schemes: HashMap::new(),
            imported_type_decls: Vec::new(),
            module_aliases: HashMap::new(),
        };
        let occurrences =
            collect_symbol_occurrences(Path::new("src/main.zier"), src, "main", &imports).unwrap();

        let main_map = occurrences
            .iter()
            .filter(|occ| occ.symbol.module == "main" && occ.symbol.function == "map")
            .count();
        let util_map = occurrences
            .iter()
            .filter(|occ| occ.symbol.module == "util" && occ.symbol.function == "map")
            .count();

        assert_eq!(main_map, 2);
        assert_eq!(util_map, 2);
    }

    #[test]
    fn completion_context_detects_qualified_prefix() {
        let src = "(io/pri)";
        let offset = src.find("pri").unwrap() + 3;
        match completion_context(src, offset) {
            Some(CompletionContext::Qualified { module, prefix }) => {
                assert_eq!(module, "io");
                assert_eq!(prefix, "pri");
            }
            other => panic!("unexpected completion context: {other:?}"),
        }
    }

    #[test]
    fn completion_context_detects_unqualified_prefix() {
        let src = "(prin)";
        let offset = src.find("prin").unwrap() + 4;
        match completion_context(src, offset) {
            Some(CompletionContext::Unqualified { prefix }) => assert_eq!(prefix, "prin"),
            other => panic!("unexpected completion context: {other:?}"),
        }
    }

    #[test]
    fn local_names_at_offset_includes_let_match_and_lambda_bindings() {
        let src = "(let main {arg}\n\
                     (let [local 1]\n\
                       (match local\n\
                         value ~> (f {inner} -> (+ arg (+ local (+ value inner)))))))";
        let offset = src.find("inner").unwrap() + 2;
        let names = local_names_at_offset(Path::new("src/main.zier"), src, offset).unwrap();
        assert!(names.contains(&"arg".to_string()));
        assert!(names.contains(&"local".to_string()));
        assert!(names.contains(&"value".to_string()));
        assert!(names.contains(&"inner".to_string()));
    }

    #[test]
    fn completion_items_filter_by_prefix() {
        let items = completion_items_from_names(
            vec![
                "println".to_string(),
                "print".to_string(),
                "debug".to_string(),
            ],
            "pri",
            CompletionItemKind::FUNCTION,
        );
        let labels: Vec<_> = items.into_iter().map(|item| item.label).collect();
        assert_eq!(labels, vec!["print".to_string(), "println".to_string()]);
    }

    #[test]
    fn completion_item_can_describe_modules() {
        let item = completion_item(
            "io/".to_string(),
            CompletionItemKind::MODULE,
            Some("module".to_string()),
        );
        assert_eq!(item.label, "io/");
        assert_eq!(item.kind, Some(CompletionItemKind::MODULE));
        assert_eq!(item.detail.as_deref(), Some("module"));
    }

    #[test]
    fn top_level_symbols_collect_functions_and_types() {
        let src = "(type Option (None))\n\
                   (extern let debug {} ~ String io/debug)\n\
                   (let main {} (debug))";
        let symbols = top_level_symbols(Path::new("src/main.zier"), src).unwrap();
        let names: Vec<_> = symbols.into_iter().map(|symbol| symbol.name).collect();
        assert_eq!(
            names,
            vec![
                "Option".to_string(),
                "debug".to_string(),
                "main".to_string()
            ]
        );
    }

    #[test]
    fn signature_target_finds_unqualified_call_argument_index() {
        let src = "(let add {a b} (+ a b))\n(let main {} (add 1 2))";
        let imports = ResolvedImports {
            imports: HashMap::new(),
            import_origins: HashMap::new(),
            imported_schemes: HashMap::new(),
            imported_type_decls: Vec::new(),
            module_aliases: HashMap::new(),
        };
        let offset = src.rfind('2').unwrap();
        let target = signature_target_at(Path::new("src/main.zier"), src, "main", &imports, offset)
            .unwrap()
            .unwrap();
        assert_eq!(target.symbol.module, "main");
        assert_eq!(target.symbol.function, "add");
        assert_eq!(target.arg_index, 1);
    }

    #[test]
    fn signature_target_finds_qualified_call_argument_index() {
        let src = "(use std/io)\n(let main {} (io/println \"hello\"))";
        let imports = ResolvedImports {
            imports: HashMap::new(),
            import_origins: HashMap::new(),
            imported_schemes: HashMap::new(),
            imported_type_decls: Vec::new(),
            module_aliases: HashMap::new(),
        };
        let offset = src.find("hello").unwrap();
        let target = signature_target_at(Path::new("src/main.zier"), src, "main", &imports, offset)
            .unwrap()
            .unwrap();
        assert_eq!(target.symbol.module, "io");
        assert_eq!(target.symbol.function, "println");
        assert_eq!(target.arg_index, 0);
    }

    #[test]
    fn local_symbol_at_resolves_let_binding_and_use() {
        let src = "(let main {}\n  (let [x 1]\n    (+ x x)))";
        let offset = src.rfind("x").unwrap();
        let symbol = local_symbol_at(Path::new("src/main.zier"), src, offset)
            .unwrap()
            .unwrap();
        let def_start = src.find("[x").unwrap() + 1;
        assert_eq!(symbol.name, "x");
        assert_eq!(symbol.def_range, def_start..def_start + 1);
    }

    #[test]
    fn local_symbol_at_resolves_match_binding() {
        let src = "(let main {x}\n  (match x\n    value ~> (+ value 1)))";
        let offset = src.rfind("value").unwrap();
        let symbol = local_symbol_at(Path::new("src/main.zier"), src, offset)
            .unwrap()
            .unwrap();
        let def_start = src.find("value").unwrap();
        assert_eq!(symbol.name, "value");
        assert_eq!(symbol.def_range, def_start..def_start + "value".len());
    }

    #[test]
    fn collect_local_occurrences_respects_shadowing() {
        let src = "(let main {x}\n  (let [x 1]\n    (+ x ((f {x} -> x)))))";
        let occurrences = collect_local_occurrences(Path::new("src/main.zier"), src).unwrap();

        let outer_x = src.find("{x}").unwrap() + 1;
        let let_x = src.find("[x").unwrap() + 1;
        let lambda_x = src.rfind("{x}").unwrap() + 1;

        let outer_refs = occurrences
            .iter()
            .filter(|occ| occ.symbol.def_range == (outer_x..outer_x + 1))
            .count();
        let let_refs = occurrences
            .iter()
            .filter(|occ| occ.symbol.def_range == (let_x..let_x + 1))
            .count();
        let lambda_refs = occurrences
            .iter()
            .filter(|occ| occ.symbol.def_range == (lambda_x..lambda_x + 1))
            .count();

        assert_eq!(outer_refs, 1);
        assert_eq!(let_refs, 2);
        assert_eq!(lambda_refs, 2);
    }
}
