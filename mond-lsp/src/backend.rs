use std::{
    collections::HashMap,
    fs,
    sync::{Arc, Mutex},
};

use tower_lsp::{
    Client, LanguageServer,
    jsonrpc::Result,
    lsp_types::{
        CompletionOptions, CompletionParams, CompletionResponse, DidChangeTextDocumentParams,
        DidCloseTextDocumentParams, DidOpenTextDocumentParams, DidSaveTextDocumentParams,
        DocumentFormattingParams, DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse,
        GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverContents, HoverParams,
        InitializeParams, InitializeResult, InitializedParams, Location, MarkedString,
        MarkupContent, MarkupKind, MessageType, OneOf, ParameterInformation, ParameterLabel,
        ReferenceParams, RenameParams, SemanticTokensParams, SemanticTokensResult,
        ServerCapabilities, SignatureHelp, SignatureHelpParams, SignatureInformation,
        SymbolInformation, TextDocumentSyncCapability, TextDocumentSyncKind, TextEdit, Url,
        WorkspaceEdit, WorkspaceSymbolParams,
    },
};

use crate::{
    CompletionContext, DocumentAnalysis, HoverTarget, OccurrenceKind, Symbol,
    analysis::project_diagnostic_batches_for_uri,
    best_expr_type_at_offset, byte_range_to_lsp_range, collect_local_occurrences,
    completion_context, find_hover_target, find_project_root, full_document_range,
    full_text_change, function_arity, local_symbol_at, lsp_documentation, lsp_error_diagnostic,
    position_to_offset,
    project::Project,
    scheme_for_symbol,
    semantic_tokens::{compute_semantic_tokens_full, semantic_tokens_capabilities},
    signature_target_at,
    state::{DocumentState, ServerState},
    symbol_at, symbol_documentation_for_symbol, top_level_symbols,
};

pub struct Backend {
    client: Client,
    state: Arc<Mutex<ServerState>>,
}

impl Backend {
    pub(crate) fn new(client: Client) -> Self {
        Self {
            client,
            state: Arc::new(Mutex::new(ServerState::default())),
        }
    }

    async fn publish_document_diagnostics(&self, uri: Url) {
        let diagnostics = match self.analyze_document(&uri) {
            Ok(Some(analysis)) => analysis.diagnostics,
            Ok(None) => Vec::new(),
            Err(err) => vec![lsp_error_diagnostic(err)],
        };
        self.client
            .publish_diagnostics(uri, diagnostics, None)
            .await;
    }

    async fn publish_project_diagnostics(&self, focus_uri: Url) {
        let path = match focus_uri.to_file_path() {
            Ok(path) => path,
            Err(_) => {
                self.publish_document_diagnostics(focus_uri).await;
                return;
            }
        };
        let root = find_project_root(&path);
        let batches =
            match project_diagnostic_batches_for_uri(root.as_deref(), &self.state, &focus_uri) {
                Ok(Some(batches)) => batches,
                Ok(None) => {
                    self.publish_document_diagnostics(focus_uri).await;
                    return;
                }
                Err(err) => {
                    self.client
                        .publish_diagnostics(focus_uri, vec![lsp_error_diagnostic(err)], None)
                        .await;
                    return;
                }
            };

        for (module, diagnostics) in batches {
            if let Ok(uri) = Url::from_file_path(&module.path) {
                self.client
                    .publish_diagnostics(uri, diagnostics, None)
                    .await;
            }
        }
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
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![":".to_string(), "/".to_string()]),
                    ..CompletionOptions::default()
                }),
                document_symbol_provider: Some(OneOf::Left(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                signature_help_provider: Some(tower_lsp::lsp_types::SignatureHelpOptions {
                    trigger_characters: Some(vec![" ".to_string(), "(".to_string()]),
                    retrigger_characters: None,
                    work_done_progress_options: Default::default(),
                }),
                document_formatting_provider: Some(OneOf::Left(true)),
                semantic_tokens_provider: Some(semantic_tokens_capabilities()),
                ..ServerCapabilities::default()
            },
            ..InitializeResult::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "mond-lsp initialized")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri;
        let Some(source) = self.document_text(&uri) else {
            return Ok(None);
        };
        let Ok(path) = uri.to_file_path() else {
            return Ok(None);
        };
        let imported_type_decls = self
            .analyze_document(&uri)
            .ok()
            .flatten()
            .map(|analysis| analysis.imports.imported_type_decls)
            .unwrap_or_default();
        let tokens = compute_semantic_tokens_full(&path, &source, &imported_type_decls)
            .map_err(tower_lsp::jsonrpc::Error::invalid_params)?;
        Ok(Some(SemanticTokensResult::Tokens(tokens)))
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
        self.publish_project_diagnostics(params.text_document.uri)
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
        self.publish_project_diagnostics(params.text_document.uri)
            .await;
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        self.publish_project_diagnostics(params.text_document.uri)
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
        let scheme = if let Some(target) = find_hover_target(&doc.path, &doc.source, offset) {
            match target {
                HoverTarget::Unqualified(name) => {
                    if let Some(scheme) = analysis.bindings.get(&name).cloned() {
                        Some((
                            name.clone(),
                            scheme,
                            Some(Symbol {
                                module: doc.name.clone(),
                                function: name,
                            }),
                        ))
                    } else if let Some(scheme) =
                        analysis.imports.imported_schemes.get(&name).cloned()
                    {
                        analysis
                            .imports
                            .import_origins
                            .get(&name)
                            .cloned()
                            .map(|module| {
                                (
                                    name.clone(),
                                    scheme,
                                    Some(Symbol {
                                        module,
                                        function: name,
                                    }),
                                )
                            })
                    } else {
                        mondc::typecheck::primitive_env()
                            .get(&name)
                            .cloned()
                            .map(|scheme| (name, scheme, None))
                    }
                }
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
                    .map(|scheme| {
                        (
                            format!("{module}/{function}"),
                            scheme,
                            Some(Symbol { module, function }),
                        )
                    }),
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
                            .map(|scheme| (name, scheme, Some(symbol)))
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
                            .map(|scheme| (name, scheme, Some(symbol)))
                    }
                })
        };

        let Some((name, scheme, symbol)) = scheme else {
            if let Some(ty) = best_expr_type_at_offset(&analysis.expr_types, offset) {
                return Ok(Some(Hover {
                    contents: HoverContents::Scalar(MarkedString::String(ty)),
                    range: None,
                }));
            }
            return Ok(None);
        };
        let rendered = format!("{name} : {}", mondc::typecheck::type_display(&scheme.ty));
        let docs = symbol
            .and_then(|symbol| symbol_documentation_for_symbol(&project, &doc, &analysis, &symbol));
        let contents = if let Some(docs) = docs {
            HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: format!("```mond\n{rendered}\n```\n\n{docs}"),
            })
        } else {
            HoverContents::Scalar(MarkedString::String(rendered))
        };
        Ok(Some(Hover {
            contents,
            range: None,
        }))
    }

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let Some(source) = self.document_text(&params.text_document.uri) else {
            return Ok(None);
        };
        let formatted = mond_format::format_default(&source);
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
            CompletionContext::ImportPath { root, prefix } => {
                project.import_path_completion_items(&root, &prefix)
            }
            CompletionContext::UseImportList { module, prefix } => {
                project.use_import_list_completion_items(&module, &prefix)
            }
            CompletionContext::RecordField {
                record_name,
                prefix,
            } => project.record_field_completion_items(
                &doc,
                &analysis,
                record_name.as_deref(),
                &prefix,
            ),
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
            mondc::typecheck::type_display(&scheme.ty)
        );
        let arity = function_arity(&scheme.ty);
        let params = (0..arity)
            .map(|index| ParameterInformation {
                label: ParameterLabel::Simple(format!("arg{}", index + 1)),
                documentation: None,
            })
            .collect();
        let documentation =
            symbol_documentation_for_symbol(&project, &doc, &analysis, &target.symbol)
                .map(lsp_documentation);
        Ok(Some(SignatureHelp {
            signatures: vec![SignatureInformation {
                label,
                documentation,
                parameters: Some(params),
                active_parameter: Some(target.arg_index.min(arity.saturating_sub(1)) as u32),
            }],
            active_signature: Some(0),
            active_parameter: Some(target.arg_index.min(arity.saturating_sub(1)) as u32),
        }))
    }
}
