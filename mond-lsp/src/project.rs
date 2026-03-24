use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use tower_lsp::lsp_types::{
    CompletionItem, CompletionItemKind, Location, Range, SymbolInformation, SymbolKind, Url,
};

use crate::{
    ModuleSource, OccurrenceKind, Symbol, analysis::DocumentAnalysis, build_project_analysis,
    byte_range_to_lsp_range, collect_dependency_modules, collect_modules, collect_record_fields,
    collect_std_modules, collect_symbol_occurrences, completion_item, contains_path,
    dependency_name_for_module_path, diagnostic_to_lsp, find_top_level_definition_range,
    is_test_path, local_names_at_offset, local_type_decls, module_name_for_path,
    package_name_from_manifest, push_completion_item, source_path_for_compile, state::ServerState,
    top_level_docs, top_level_symbols, visible_exports,
};

pub(crate) struct Project {
    pub(crate) root: Option<PathBuf>,
    pub(crate) std_modules: BTreeMap<String, ModuleSource>,
    pub(crate) dep_modules: BTreeMap<String, ModuleSource>,
    pub(crate) src_modules: BTreeMap<String, ModuleSource>,
    pub(crate) test_modules: BTreeMap<String, ModuleSource>,
    pub(crate) analysis: mondc::ProjectAnalysis,
}

impl Project {
    pub(crate) fn load(
        root: Option<&Path>,
        state: &Arc<Mutex<ServerState>>,
        focus_uri: &Url,
    ) -> std::result::Result<Self, String> {
        let overlays = state.lock().unwrap().open_docs.clone();
        let std_modules = collect_std_modules(root);
        let dep_modules = collect_dependency_modules(root);
        let src_modules = collect_modules(root, "src", &overlays);
        let test_modules = collect_modules(root, "tests", &overlays);
        let package_name = package_name_from_manifest(root);
        let analysis = build_project_analysis(
            &std_modules,
            &dep_modules,
            &src_modules,
            package_name.as_deref(),
        )?;

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
                    dep_modules,
                    src_modules,
                    test_modules,
                    analysis,
                });
            }
        }

        Ok(Self {
            root: root.map(Path::to_path_buf),
            std_modules,
            dep_modules,
            src_modules,
            test_modules,
            analysis,
        })
    }

    pub(crate) fn document_for_path(&self, path: &Path) -> Option<ModuleSource> {
        let module_name = module_name_for_path(path);
        self.src_modules
            .get(&module_name)
            .cloned()
            .or_else(|| self.test_modules.get(&module_name).cloned())
            .or_else(|| self.dep_modules.get(&module_name).cloned())
            .or_else(|| self.std_modules.get(&module_name).cloned())
    }

    pub(crate) fn module_named(&self, module_name: &str) -> Option<&ModuleSource> {
        self.src_modules
            .get(module_name)
            .or_else(|| self.test_modules.get(module_name))
            .or_else(|| self.dep_modules.get(module_name))
            .or_else(|| self.std_modules.get(module_name))
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
        self.std_modules
            .values()
            .chain(self.dep_modules.values())
            .chain(self.src_modules.values())
            .chain(self.test_modules.values())
            .cloned()
            .collect()
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
            for name in extern_types {
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
        let local_names = local_names_at_offset(&doc.path, &doc.source, offset)?;
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

        if root == "std" {
            modules.extend(
                self.std_modules
                    .keys()
                    .filter(|name| name.as_str() != "std")
                    .cloned(),
            );
        }

        modules.extend(
            self.dep_modules
                .values()
                .filter(|module| dependency_name_for_module_path(&module.path) == Some(root))
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
        let visible_exports = visible_exports(&self.analysis, &self.test_modules, &doc.name);
        let imports = mondc::resolve_imports_for_source(
            doc.source.as_str(),
            &visible_exports,
            &self.analysis,
        );
        let bindings = mondc::infer_module_bindings(
            &doc.name,
            &doc.source,
            imports.imports.clone(),
            &visible_exports,
            &imports.imported_type_decls,
            &imports.imported_extern_types,
            &imports.imported_schemes,
        );
        let expr_types = mondc::infer_module_expr_types(
            &doc.name,
            &doc.source,
            imports.imports.clone(),
            &visible_exports,
            &imports.imported_type_decls,
            &imports.imported_extern_types,
            &imports.imported_schemes,
        );
        Ok(DocumentAnalysis {
            bindings,
            expr_types,
            imports,
        })
    }

    pub(crate) fn diagnostics_for_document(
        &self,
        doc: &ModuleSource,
    ) -> std::result::Result<Vec<tower_lsp::lsp_types::Diagnostic>, String> {
        let visible_exports = visible_exports(&self.analysis, &self.test_modules, &doc.name);
        let imports = mondc::resolve_imports_for_source(
            doc.source.as_str(),
            &visible_exports,
            &self.analysis,
        );
        let report = mondc::compile_with_imports_report_with_private_records(
            &doc.name,
            &doc.source,
            &source_path_for_compile(self.root.as_deref(), &doc.path),
            imports.imports,
            &visible_exports,
            imports.module_aliases,
            &imports.imported_type_decls,
            &imports.imported_extern_types,
            &imports.imported_field_indices,
            &imports.imported_private_records,
            &imports.imported_schemes,
        );
        Ok(report
            .diagnostics
            .iter()
            .map(|diag| diagnostic_to_lsp(&doc.source, diag))
            .collect())
    }
}
