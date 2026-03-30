use tower_lsp::lsp_types::{
    CompletionItem, CompletionItemKind, DiagnosticSeverity, Documentation, MarkupContent,
    MarkupKind, Position, Range, TextDocumentContentChangeEvent,
};

use crate::{project::Project, state::DocumentState};

use super::*;

#[derive(Clone, Debug)]
pub(crate) struct ModuleSource {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) source: String,
}

#[derive(Debug)]
pub(crate) enum HoverTarget {
    Unqualified(String),
    Qualified { module: String, function: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Symbol {
    pub(crate) module: String,
    pub(crate) function: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OccurrenceKind {
    Definition,
    Reference,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SymbolOccurrence {
    pub(crate) symbol: Symbol,
    pub(crate) range: std::ops::Range<usize>,
    pub(crate) kind: OccurrenceKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TopLevelSymbol {
    pub(crate) name: String,
    pub(crate) kind: SymbolKind,
    pub(crate) selection_range: std::ops::Range<usize>,
    pub(crate) full_range: std::ops::Range<usize>,
    pub(crate) documentation: Option<String>,
}

pub(crate) struct SignatureTarget {
    pub(crate) symbol: Symbol,
    pub(crate) arg_index: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct LocalSymbol {
    pub(crate) name: String,
    pub(crate) def_range: std::ops::Range<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LocalOccurrence {
    pub(crate) symbol: LocalSymbol,
    pub(crate) range: std::ops::Range<usize>,
    pub(crate) kind: OccurrenceKind,
}

#[derive(Debug)]
pub(crate) enum CompletionContext {
    Unqualified {
        prefix: String,
    },
    Qualified {
        module: String,
        prefix: String,
    },
    ImportPath {
        root: String,
        prefix: String,
    },
    UseImportList {
        module: String,
        prefix: String,
    },
    RecordField {
        record_name: Option<String>,
        prefix: String,
    },
}

type ProjectDiagnostic = Vec<(ModuleSource, Vec<Diagnostic>)>;

pub(crate) fn project_diagnostic_batches_for_uri(
    root: Option<&Path>,
    state: &Arc<Mutex<ServerState>>,
    focus_uri: &Url,
) -> std::result::Result<Option<ProjectDiagnostic>, String> {
    let project = Project::load(root, state, focus_uri)?;
    let project_modules = project
        .src_modules
        .values()
        .chain(project.test_modules.values())
        .cloned()
        .collect::<Vec<_>>();
    if project_modules.is_empty() {
        return Ok(None);
    }
    Ok(Some(project_diagnostic_batches(&project, project_modules)))
}

pub(crate) struct DocumentAnalysis {
    pub(crate) bindings: mondc::typecheck::TypeEnv,
    pub(crate) expr_types: Vec<(std::ops::Range<usize>, String)>,
    pub(crate) imports: mondc::ResolvedImports,
}

pub(super) fn build_project_analysis(
    std_modules: &BTreeMap<String, ModuleSource>,
    dep_modules: &BTreeMap<String, ModuleSource>,
    src_modules: &BTreeMap<String, ModuleSource>,
    package_name: Option<&str>,
) -> std::result::Result<mondc::ProjectAnalysis, String> {
    let std_mods = std_modules
        .iter()
        .map(|(module_name, module)| (module_name.clone(), module.source.clone()))
        .collect::<Vec<(String, String)>>();
    let mut external_mods = mondc::std_modules_from_sources(&std_mods)?;
    external_mods.extend(load_dependency_analysis_modules(dep_modules));
    let src_module_sources: Vec<(String, String)> = src_modules
        .iter()
        .map(|(module_name, module)| (module_name.clone(), module.source.clone()))
        .collect();
    mondc::build_project_analysis_with_modules_and_package(
        &external_mods,
        &src_module_sources,
        package_name,
    )
}

pub(super) fn visible_exports(
    analysis: &mondc::ProjectAnalysis,
    test_modules: &BTreeMap<String, ModuleSource>,
    current_module: &str,
) -> HashMap<String, Vec<String>> {
    let mut exports = analysis.module_exports.clone();
    for (module_name, module) in test_modules {
        if module_name == current_module {
            continue;
        }
        exports.insert(module_name.clone(), mondc::exported_names(&module.source));
    }
    exports
}

pub(super) fn collect_modules(
    root: Option<&Path>,
    subdir: &str,
    overlays: &HashMap<Url, DocumentState>,
) -> BTreeMap<String, ModuleSource> {
    let mut modules = BTreeMap::new();
    if let Some(root) = root {
        let dir = root.join(subdir);
        collect_mond_files_from_dir(&dir, &mut modules);
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

pub(super) fn collect_mond_files_from_dir(
    dir: &Path,
    modules: &mut BTreeMap<String, ModuleSource>,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_mond_files_from_dir(&path, modules);
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("mond") {
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

pub(super) fn std_source_root(root: Option<&Path>) -> Option<PathBuf> {
    if let Some(root) = root {
        let dep_root = root.join("target").join("deps").join("std").join("src");
        if dep_root.exists() {
            return Some(dep_root);
        }
    }

    let workspace_std = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../mond-std/src");
    if workspace_std.exists() {
        return Some(workspace_std);
    }

    None
}

pub(super) fn package_name_from_manifest(root: Option<&Path>) -> Option<String> {
    let root = root?;
    let manifest_path = root.join("bahn.toml");
    let manifest_source = fs::read_to_string(manifest_path).ok()?;
    let manifest: toml::Value = toml::from_str(&manifest_source).ok()?;
    manifest
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(toml::Value::as_str)
        .map(str::to_string)
}

pub(super) fn collect_std_modules(root: Option<&Path>) -> BTreeMap<String, ModuleSource> {
    let Some(std_root) = std_source_root(root) else {
        return BTreeMap::new();
    };

    let mut discovered = BTreeMap::new();
    collect_mond_files_from_dir(&std_root, &mut discovered);

    let mut modules = BTreeMap::new();
    for (_, mut module) in discovered {
        let name = if module.name == "lib" {
            "std".to_string()
        } else {
            module.name.clone()
        };
        module.name = name.clone();
        modules.insert(name, module);
    }
    modules
}

pub(super) fn collect_dependency_modules(root: Option<&Path>) -> BTreeMap<String, ModuleSource> {
    let Some(root) = root else {
        return BTreeMap::new();
    };
    let deps_root = root.join("target").join("deps");
    let Ok(entries) = fs::read_dir(&deps_root) else {
        return BTreeMap::new();
    };

    let mut modules = BTreeMap::new();
    for entry in entries.flatten() {
        let dep_dir = entry.path();
        if !dep_dir.is_dir() {
            continue;
        }
        let dep_name = dep_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if dep_name.is_empty() || dep_name == "std" {
            continue;
        }
        let src_dir = dep_dir.join("src");
        let mut discovered = BTreeMap::new();
        collect_mond_files_from_dir(&src_dir, &mut discovered);
        for (_, mut module) in discovered {
            let module_name = if module.name == "lib" {
                dep_name.to_string()
            } else {
                module.name.clone()
            };
            module.name = module_name.clone();
            modules.insert(module_name, module);
        }
    }
    modules
}

pub(super) fn load_dependency_analysis_modules(
    dep_modules: &BTreeMap<String, ModuleSource>,
) -> Vec<(String, String, String)> {
    let mut loaded = Vec::new();
    let mut dep_dirs = BTreeMap::new();
    for module in dep_modules.values() {
        let Some(dep_dir) = module.path.ancestors().find(|ancestor| {
            ancestor.parent().and_then(|parent| parent.file_name())
                == Some(std::ffi::OsStr::new("deps"))
        }) else {
            continue;
        };
        let dep_name = dep_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        if dep_name.is_empty() {
            continue;
        }
        dep_dirs
            .entry(dep_name)
            .or_insert_with(|| dep_dir.to_path_buf());
    }
    for (dep_name, dep_dir) in dep_dirs {
        if let Ok(dep_loaded) = mondc::load_dependency_modules_from_checkout(&dep_name, &dep_dir) {
            loaded.extend(dep_loaded);
        }
    }
    loaded
}

pub(super) fn find_top_level_definition_range(
    source_path: &Path,
    source: &str,
    name: &str,
) -> std::result::Result<Option<Range>, String> {
    let mut lowerer = mondc::lower::Lowerer::new();
    let tokens = mondc::lexer::Lexer::new(source).lex();
    let file_id = lowerer.add_file(
        source_path.to_string_lossy().to_string(),
        source.to_string(),
    );
    let sexprs = mondc::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .map_err(|err| err.message.clone())?;
    let decls = lowerer.lower_file(file_id, &sexprs);
    if !lowerer.diagnostics.is_empty() {
        return Ok(None);
    }

    for decl in decls {
        match decl {
            mondc::ast::Declaration::Expression(mondc::ast::Expr::LetFunc {
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
            mondc::ast::Declaration::ExternLet {
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

pub(super) fn find_project_root(path: &Path) -> Option<PathBuf> {
    let mut current = path.parent()?;
    loop {
        if current.join("bahn.toml").exists() {
            return Some(current.to_path_buf());
        }
        current = current.parent()?;
    }
}

pub(super) fn is_test_path(root: Option<&Path>, path: &Path) -> bool {
    root.is_some_and(|root| path.starts_with(root.join("tests")))
}

pub(super) fn contains_path(modules: &BTreeMap<String, ModuleSource>, path: &Path) -> bool {
    modules.values().any(|module| module.path == path)
}

pub(super) fn module_name_for_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string()
}

pub(super) fn source_path_for_compile(root: Option<&Path>, path: &Path) -> String {
    root.and_then(|root| path.strip_prefix(root).ok())
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string())
}

pub(super) fn full_text_change(changes: Vec<TextDocumentContentChangeEvent>) -> String {
    changes
        .into_iter()
        .last()
        .map(|change| change.text)
        .unwrap_or_default()
}

pub(super) fn full_document_range(source: &str) -> Range {
    Range::new(
        Position::new(0, 0),
        offset_to_position(source, source.len()),
    )
}

pub(super) fn best_expr_type_at_offset(
    expr_types: &[(std::ops::Range<usize>, String)],
    offset: usize,
) -> Option<String> {
    expr_types
        .iter()
        .filter(|(span, _)| span.start <= offset && offset <= span.end)
        .min_by_key(|(span, _)| span.end.saturating_sub(span.start))
        .map(|(_, ty)| ty.clone())
}

#[cfg(test)]
pub(super) fn completion_items_from_names(
    names: Vec<String>,
    prefix: &str,
    kind: CompletionItemKind,
) -> Vec<CompletionItem> {
    let mut items: Vec<_> = names
        .into_iter()
        .filter(|name| name.starts_with(prefix))
        .map(|name| completion_item(name, kind, None, None))
        .collect();
    items.sort_by(|a, b| a.label.cmp(&b.label));
    items
}

pub(super) fn completion_item(
    label: String,
    kind: CompletionItemKind,
    detail: Option<String>,
    documentation: Option<String>,
) -> CompletionItem {
    CompletionItem {
        label,
        kind: Some(kind),
        detail,
        documentation: documentation.map(lsp_documentation),
        ..CompletionItem::default()
    }
}

pub(super) fn lsp_documentation(value: String) -> Documentation {
    Documentation::MarkupContent(MarkupContent {
        kind: MarkupKind::Markdown,
        value,
    })
}

pub(super) fn push_completion_item(
    items: &mut Vec<CompletionItem>,
    seen: &mut HashSet<String>,
    item: CompletionItem,
) {
    if seen.insert(item.label.clone()) {
        items.push(item);
    }
}

pub(super) fn diagnostic_to_lsp(source: &str, diag: &CodeDiagnostic<usize>) -> Diagnostic {
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

pub(super) fn lsp_error_diagnostic(message: String) -> Diagnostic {
    Diagnostic {
        range: Range::new(Position::new(0, 0), Position::new(0, 0)),
        severity: Some(DiagnosticSeverity::ERROR),
        message,
        ..Diagnostic::default()
    }
}

pub(super) fn project_diagnostic_batches(
    project: &Project,
    modules: Vec<ModuleSource>,
) -> Vec<(ModuleSource, Vec<Diagnostic>)> {
    modules
        .into_iter()
        .map(|module| {
            let diagnostics = match project.diagnostics_for_document(&module) {
                Ok(diagnostics) => diagnostics,
                Err(err) => vec![lsp_error_diagnostic(err)],
            };
            (module, diagnostics)
        })
        .collect()
}

pub(super) fn byte_range_to_lsp_range(source: &str, start: usize, end: usize) -> Range {
    Range::new(
        offset_to_position(source, start),
        offset_to_position(source, end),
    )
}

pub(crate) fn offset_to_position(source: &str, offset: usize) -> Position {
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

pub(super) fn position_to_offset(source: &str, position: Position) -> Option<usize> {
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

pub(super) fn completion_context(source: &str, offset: usize) -> Option<CompletionContext> {
    if offset > source.len() {
        return None;
    }

    if let Some((record_name, prefix)) = record_field_context(source, offset) {
        return Some(CompletionContext::RecordField {
            record_name,
            prefix,
        });
    }

    let prefix_start = scan_ident_start(source, offset);
    let prefix = source[prefix_start..offset].to_string();

    if let Some(module) = use_import_list_module_at(source, prefix_start) {
        return Some(CompletionContext::UseImportList { module, prefix });
    }

    if prefix_start > 0 && source.as_bytes()[prefix_start - 1] == b'/' {
        let module_end = prefix_start - 1;
        let module_start = scan_ident_start(source, module_end);
        if module_start < module_end {
            if is_use_import_path_context(source, module_start) {
                return Some(CompletionContext::ImportPath {
                    root: source[module_start..module_end].to_string(),
                    prefix,
                });
            }
            return Some(CompletionContext::Qualified {
                module: source[module_start..module_end].to_string(),
                prefix,
            });
        }
    }

    Some(CompletionContext::Unqualified { prefix })
}

pub(super) fn record_field_context(
    source: &str,
    offset: usize,
) -> Option<(Option<String>, String)> {
    if offset == 0 || offset > source.len() {
        return None;
    }
    let field_start = scan_ident_start(source, offset);
    if field_start == 0 || source.as_bytes()[field_start - 1] != b':' {
        return None;
    }

    let prefix = source[field_start..offset].to_string();
    let list_start = enclosing_round_list_start(source, field_start - 1)?;
    let head = list_head_atom(source, list_start)?;
    if head == "with" {
        return Some((None, prefix));
    }
    if head
        .as_bytes()
        .first()
        .is_some_and(|byte| byte.is_ascii_uppercase())
    {
        return Some((Some(head.to_string()), prefix));
    }
    None
}

pub(super) fn list_head_atom(source: &str, list_start: usize) -> Option<&str> {
    let bytes = source.as_bytes();
    let mut idx = list_start + 1;
    while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
        idx += 1;
    }
    let head_start = idx;
    while idx < bytes.len() && !bytes[idx].is_ascii_whitespace() && bytes[idx] != b')' {
        idx += 1;
    }
    if head_start == idx {
        None
    } else {
        source.get(head_start..idx)
    }
}

pub(super) fn scan_ident_start(source: &str, offset: usize) -> usize {
    let bytes = source.as_bytes();
    let mut idx = offset;
    while idx > 0 && is_ident_byte(bytes[idx - 1]) {
        idx -= 1;
    }
    idx
}

pub(super) fn scan_ident_end(source: &str, offset: usize) -> usize {
    let bytes = source.as_bytes();
    let mut idx = offset;
    while idx < bytes.len() && is_ident_byte(bytes[idx]) {
        idx += 1;
    }
    idx
}

pub(super) fn field_accessor_at_offset(source: &str, offset: usize) -> Option<String> {
    if source.is_empty() || offset > source.len() {
        return None;
    }

    let bytes = source.as_bytes();

    if offset < bytes.len() && bytes[offset] == b':' {
        let start = offset + 1;
        if start >= bytes.len() || !is_ident_byte(bytes[start]) {
            return None;
        }
        let end = scan_ident_end(source, start);
        return source.get(start..end).map(|name| format!(":{name}"));
    }

    let mut ident_offset = if offset == bytes.len() {
        offset.saturating_sub(1)
    } else {
        offset
    };
    if !is_ident_byte(bytes[ident_offset]) {
        if ident_offset == 0 || !is_ident_byte(bytes[ident_offset - 1]) {
            return None;
        }
        ident_offset -= 1;
    }

    let start = scan_ident_start(source, ident_offset + 1);
    if start == 0 || bytes[start - 1] != b':' {
        return None;
    }
    let end = scan_ident_end(source, ident_offset);
    source.get(start..end).map(|name| format!(":{name}"))
}

pub(super) fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

pub(super) fn is_use_import_path_context(source: &str, module_start: usize) -> bool {
    let Some(list_start) = enclosing_round_list_start(source, module_start) else {
        return false;
    };
    let head = source[list_start + 1..module_start]
        .split_whitespace()
        .collect::<Vec<_>>();
    matches!(head.as_slice(), ["use"] | ["pub", "use"])
}

pub(super) fn enclosing_round_list_start(source: &str, offset: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut round_depth = 0usize;

    for idx in (0..offset).rev() {
        match bytes[idx] {
            b')' => round_depth += 1,
            b'(' => {
                if round_depth == 0 {
                    return Some(idx);
                }
                round_depth -= 1;
            }
            _ => {}
        }
    }

    None
}

pub(super) fn dependency_name_for_module_path(path: &Path) -> Option<&str> {
    path.ancestors().find_map(|ancestor| {
        let parent = ancestor.parent()?;
        if parent.file_name()? != std::ffi::OsStr::new("deps") {
            return None;
        }
        ancestor.file_name()?.to_str()
    })
}

pub(super) fn use_import_list_module_at(source: &str, offset: usize) -> Option<String> {
    let list_start = enclosing_round_list_start(source, offset)?;
    let bytes = source.as_bytes();
    let mut idx = list_start + 1;

    while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
        idx += 1;
    }

    if source.get(idx..)?.starts_with("pub") {
        let next = idx + 3;
        if next >= bytes.len() || !bytes[next].is_ascii_whitespace() {
            return None;
        }
        idx = next;
        while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
    }

    if !source.get(idx..)?.starts_with("use") {
        return None;
    }
    let next = idx + 3;
    if next >= bytes.len() || !bytes[next].is_ascii_whitespace() {
        return None;
    }
    idx = next;

    while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
        idx += 1;
    }

    let path_start = idx;
    while idx < bytes.len() && (is_ident_byte(bytes[idx]) || bytes[idx] == b'/') {
        idx += 1;
    }
    if path_start == idx {
        return None;
    }
    let module_path = &source[path_start..idx];

    while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
        idx += 1;
    }
    if idx >= bytes.len() || bytes[idx] != b'[' || offset < idx + 1 {
        return None;
    }

    if source[idx + 1..offset].contains(']') {
        return None;
    }

    Some(
        module_path
            .rsplit_once('/')
            .map(|(_, module)| module)
            .unwrap_or(module_path)
            .to_string(),
    )
}

pub(super) fn local_names_at_offset(
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

pub(super) fn local_names_in_decl(
    decl: &mondc::ast::Declaration,
    offset: usize,
    locals: &HashSet<String>,
) -> Option<HashSet<String>> {
    match decl {
        mondc::ast::Declaration::Expression(expr) => local_names_in_expr(expr, offset, locals),
        mondc::ast::Declaration::Test { body, .. } => local_names_in_expr(body, offset, locals),
        _ => None,
    }
}

pub(super) fn local_names_in_expr(
    expr: &mondc::ast::Expr,
    offset: usize,
    locals: &HashSet<String>,
) -> Option<HashSet<String>> {
    if !span_contains(&expr.span(), offset) {
        return None;
    }

    use mondc::ast::Expr;
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
                arms.iter().find_map(|arm| {
                    let mut inner = locals.clone();
                    for pat in &arm.patterns {
                        bind_pattern_names(pat, &mut inner);
                    }
                    local_names_in_expr(&arm.body, offset, &inner).or_else(|| {
                        arm.guard
                            .as_ref()
                            .and_then(|guard| local_names_in_expr(guard, offset, &inner))
                    })
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
        Expr::RecordUpdate {
            record, updates, ..
        } => local_names_in_expr(record, offset, locals)
            .or_else(|| {
                updates
                    .iter()
                    .find_map(|(_, value)| local_names_in_expr(value, offset, locals))
            })
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

pub(super) fn span_contains(span: &std::ops::Range<usize>, offset: usize) -> bool {
    span.start <= offset && offset <= span.end
}

pub(super) fn symbol_at(
    source_path: &Path,
    source: &str,
    current_module: &str,
    imports: &mondc::ResolvedImports,
    offset: usize,
) -> std::result::Result<Option<Symbol>, String> {
    let occurrences = collect_symbol_occurrences(source_path, source, current_module, imports)?;
    Ok(occurrences
        .into_iter()
        .filter(|occ| occ.range.start <= offset && offset <= occ.range.end)
        .min_by_key(|occ| occ.range.end.saturating_sub(occ.range.start))
        .map(|occ| occ.symbol))
}

pub(super) fn scheme_for_symbol(
    project: &Project,
    doc: &ModuleSource,
    analysis: &DocumentAnalysis,
    symbol: &Symbol,
) -> Option<mondc::typecheck::Scheme> {
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

pub(super) fn symbol_documentation_for_symbol(
    project: &Project,
    doc: &ModuleSource,
    analysis: &DocumentAnalysis,
    symbol: &Symbol,
) -> Option<String> {
    if symbol.module == doc.name {
        return top_level_docs(&doc.path, &doc.source)
            .ok()?
            .into_iter()
            .find(|top| top.name == symbol.function)
            .and_then(|top| top.documentation);
    }

    let module = analysis
        .imports
        .import_origins
        .get(&symbol.function)
        .filter(|origin| *origin == &symbol.module)
        .and_then(|_| project.module_named(&symbol.module))
        .or_else(|| project.module_named(&symbol.module))?;

    top_level_docs(&module.path, &module.source)
        .ok()?
        .into_iter()
        .find(|top| top.name == symbol.function)
        .and_then(|top| top.documentation)
}

pub(super) fn signature_target_at(
    source_path: &Path,
    source: &str,
    current_module: &str,
    imports: &mondc::ResolvedImports,
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

pub(super) fn signature_target_in_decl(
    decl: &mondc::ast::Declaration,
    current_module: &str,
    top_level: &HashSet<String>,
    imports: &mondc::ResolvedImports,
    offset: usize,
    locals: &HashSet<String>,
) -> Option<SignatureTarget> {
    match decl {
        mondc::ast::Declaration::Expression(expr) => {
            signature_target_in_expr(expr, current_module, top_level, imports, offset, locals)
        }
        mondc::ast::Declaration::Test { body, .. } => {
            signature_target_in_expr(body, current_module, top_level, imports, offset, locals)
        }
        _ => None,
    }
}

pub(super) fn signature_target_in_expr(
    expr: &mondc::ast::Expr,
    current_module: &str,
    top_level: &HashSet<String>,
    imports: &mondc::ResolvedImports,
    offset: usize,
    locals: &HashSet<String>,
) -> Option<SignatureTarget> {
    if !span_contains(&expr.span(), offset) {
        return None;
    }

    use mondc::ast::Expr;
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
                arms.iter().find_map(|arm| {
                    let mut inner = locals.clone();
                    for pat in &arm.patterns {
                        bind_pattern_names(pat, &mut inner);
                    }
                    signature_target_in_expr(
                        &arm.body,
                        current_module,
                        top_level,
                        imports,
                        offset,
                        &inner,
                    )
                    .or_else(|| {
                        arm.guard.as_ref().and_then(|guard| {
                            signature_target_in_expr(
                                guard,
                                current_module,
                                top_level,
                                imports,
                                offset,
                                &inner,
                            )
                        })
                    })
                })
            }),
        Expr::FieldAccess { record, .. } => {
            signature_target_in_expr(record, current_module, top_level, imports, offset, locals)
        }
        Expr::RecordConstruct { fields, .. } => fields.iter().find_map(|(_, value)| {
            signature_target_in_expr(value, current_module, top_level, imports, offset, locals)
        }),
        Expr::RecordUpdate {
            record, updates, ..
        } => signature_target_in_expr(record, current_module, top_level, imports, offset, locals)
            .or_else(|| {
                updates.iter().find_map(|(_, value)| {
                    signature_target_in_expr(
                        value,
                        current_module,
                        top_level,
                        imports,
                        offset,
                        locals,
                    )
                })
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

pub(super) fn active_argument_index(args: &[mondc::ast::Expr], offset: usize) -> usize {
    args.iter()
        .enumerate()
        .take_while(|(_, arg)| arg.span().start <= offset)
        .map(|(index, _)| index)
        .last()
        .unwrap_or(0)
}

pub(super) fn function_arity(ty: &std::sync::Arc<mondc::typecheck::Type>) -> usize {
    let mut count = 0;
    let mut current = ty.as_ref();
    while let mondc::typecheck::Type::Fun(_, ret) = current {
        count += 1;
        current = ret.as_ref();
    }
    count
}

pub(super) fn collect_symbol_occurrences(
    source_path: &Path,
    source: &str,
    current_module: &str,
    imports: &mondc::ResolvedImports,
) -> std::result::Result<Vec<SymbolOccurrence>, String> {
    let (sexprs, decls) = parse_module(source_path, source)?;
    let top_level = top_level_bindings(&decls);
    let mut out = collect_use_import_occurrences(source, &sexprs)?;
    for decl in &decls {
        collect_decl_occurrences(decl, current_module, &top_level, imports, &mut out);
    }
    Ok(out)
}

pub(super) fn local_type_decls(
    source_path: &Path,
    source: &str,
) -> Option<Vec<mondc::ast::TypeDecl>> {
    let mut lowerer = mondc::lower::Lowerer::new();
    let tokens = mondc::lexer::Lexer::new(source).lex();
    let file_id = lowerer.add_file(
        source_path.to_string_lossy().to_string(),
        source.to_string(),
    );
    let sexprs = mondc::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .ok()?;
    let decls = lowerer.lower_file(file_id, &sexprs);
    Some(
        decls
            .into_iter()
            .filter_map(|decl| match decl {
                mondc::ast::Declaration::Type(type_decl) => Some(type_decl),
                _ => None,
            })
            .collect(),
    )
}

pub(super) fn collect_record_fields(
    type_decl: &mondc::ast::TypeDecl,
    out: &mut BTreeMap<String, BTreeSet<String>>,
) {
    if let mondc::ast::TypeDecl::Record { name, fields, .. } = type_decl {
        let field_names = out.entry(name.clone()).or_default();
        for (field_name, _) in fields {
            field_names.insert(field_name.clone());
        }
    }
}

pub(crate) fn parse_module(
    source_path: &Path,
    source: &str,
) -> std::result::Result<(Vec<mondc::sexpr::SExpr>, Vec<mondc::ast::Declaration>), String> {
    let mut lowerer = mondc::lower::Lowerer::new();
    let tokens = mondc::lexer::Lexer::new(source).lex();
    let file_id = lowerer.add_file(
        source_path.to_string_lossy().to_string(),
        source.to_string(),
    );
    let sexprs = mondc::sexpr::SExprParser::new(tokens, file_id)
        .parse()
        .map_err(|err| err.message.clone())?;
    let decls = lowerer.lower_file(file_id, &sexprs);
    if !lowerer.diagnostics.is_empty() {
        return Err(lowerer.diagnostics[0].message.clone());
    }
    Ok((sexprs, decls))
}

pub(super) fn top_level_docs(
    source_path: &Path,
    source: &str,
) -> std::result::Result<Vec<TopLevelSymbol>, String> {
    let tokens = mondc::lexer::Lexer::new(source).lex();
    let (_, decls) = parse_module(source_path, source)?;
    let mut out = Vec::new();
    let mut prev_end = 0;
    for decl in decls {
        match decl {
            mondc::ast::Declaration::Expression(mondc::ast::Expr::LetFunc {
                name,
                name_span,
                span,
                ..
            }) => {
                let documentation =
                    extract_leading_doc_comment(&tokens, source, prev_end, span.start);
                prev_end = span.end;
                out.push(TopLevelSymbol {
                    name,
                    kind: SymbolKind::FUNCTION,
                    selection_range: name_span,
                    full_range: span,
                    documentation,
                });
            }
            mondc::ast::Declaration::ExternLet {
                name,
                name_span,
                span,
                ..
            } => {
                let documentation =
                    extract_leading_doc_comment(&tokens, source, prev_end, span.start);
                prev_end = span.end;
                out.push(TopLevelSymbol {
                    name,
                    kind: SymbolKind::FUNCTION,
                    selection_range: name_span,
                    full_range: span,
                    documentation,
                });
            }
            mondc::ast::Declaration::Type(mondc::ast::TypeDecl::Record { name, span, .. }) => {
                let documentation =
                    extract_leading_doc_comment(&tokens, source, prev_end, span.start);
                prev_end = span.end;
                out.push(TopLevelSymbol {
                    name,
                    kind: SymbolKind::STRUCT,
                    selection_range: span.clone(),
                    full_range: span,
                    documentation,
                });
            }
            mondc::ast::Declaration::Type(mondc::ast::TypeDecl::Variant { name, span, .. }) => {
                let documentation =
                    extract_leading_doc_comment(&tokens, source, prev_end, span.start);
                prev_end = span.end;
                out.push(TopLevelSymbol {
                    name,
                    kind: SymbolKind::ENUM,
                    selection_range: span.clone(),
                    full_range: span,
                    documentation,
                });
            }
            mondc::ast::Declaration::Test { name, span, .. } => {
                let documentation =
                    extract_leading_doc_comment(&tokens, source, prev_end, span.start);
                prev_end = span.end;
                out.push(TopLevelSymbol {
                    name,
                    kind: SymbolKind::EVENT,
                    selection_range: span.clone(),
                    full_range: span,
                    documentation,
                });
            }
            mondc::ast::Declaration::ExternType { name, span, .. } => {
                let documentation =
                    extract_leading_doc_comment(&tokens, source, prev_end, span.start);
                prev_end = span.end;
                out.push(TopLevelSymbol {
                    name,
                    kind: SymbolKind::CLASS,
                    selection_range: span.clone(),
                    full_range: span,
                    documentation,
                });
            }
            mondc::ast::Declaration::Use { span, .. } => {
                prev_end = span.end;
            }
            mondc::ast::Declaration::Expression(_) => {}
        }
    }
    Ok(out)
}

pub(super) fn top_level_symbols(
    source_path: &Path,
    source: &str,
) -> std::result::Result<Vec<TopLevelSymbol>, String> {
    top_level_docs(source_path, source)
}

pub(super) fn extract_leading_doc_comment(
    tokens: &[mondc::lexer::Token],
    source: &str,
    region_start: usize,
    region_end: usize,
) -> Option<String> {
    let mut current_block: Vec<&mondc::lexer::Token> = Vec::new();
    let mut last_block: Vec<&mondc::lexer::Token> = Vec::new();

    for token in tokens
        .iter()
        .filter(|token| token.span.start >= region_start && token.span.end <= region_end)
    {
        match token.kind {
            mondc::lexer::TokenKind::DocComment => current_block.push(token),
            mondc::lexer::TokenKind::Comment => {
                current_block.clear();
                last_block.clear();
            }
            _ => {}
        }
        if !current_block.is_empty() {
            last_block = current_block.clone();
        }
    }

    if last_block.is_empty() {
        return None;
    }

    let lines: Vec<String> = last_block
        .into_iter()
        .map(|token| {
            source[token.span.clone()]
                .trim_start_matches(";;;")
                .trim_start()
                .to_string()
        })
        .collect();
    Some(lines.join("\n"))
}

pub(super) fn top_level_bindings(decls: &[mondc::ast::Declaration]) -> HashSet<String> {
    decls
        .iter()
        .filter_map(|decl| match decl {
            mondc::ast::Declaration::Expression(mondc::ast::Expr::LetFunc { name, .. }) => {
                Some(name.clone())
            }
            mondc::ast::Declaration::ExternLet { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect()
}

pub(super) fn collect_use_import_occurrences(
    source: &str,
    sexprs: &[mondc::sexpr::SExpr],
) -> std::result::Result<Vec<SymbolOccurrence>, String> {
    let mut out = Vec::new();
    for sexpr in sexprs {
        let mondc::sexpr::SExpr::Round(items, _) = sexpr else {
            continue;
        };
        let (path_item, imports_item) = match items.as_slice() {
            [mondc::sexpr::SExpr::Atom(tok), path] if tok.kind == mondc::lexer::TokenKind::Use => {
                (path, None)
            }
            [
                mondc::sexpr::SExpr::Atom(pub_tok),
                mondc::sexpr::SExpr::Atom(use_tok),
                path,
            ] if pub_tok.kind == mondc::lexer::TokenKind::Pub
                && use_tok.kind == mondc::lexer::TokenKind::Use =>
            {
                (path, None)
            }
            [mondc::sexpr::SExpr::Atom(tok), path, imports]
                if tok.kind == mondc::lexer::TokenKind::Use =>
            {
                (path, Some(imports))
            }
            [
                mondc::sexpr::SExpr::Atom(pub_tok),
                mondc::sexpr::SExpr::Atom(use_tok),
                path,
                imports,
            ] if pub_tok.kind == mondc::lexer::TokenKind::Pub
                && use_tok.kind == mondc::lexer::TokenKind::Use =>
            {
                (path, Some(imports))
            }
            _ => continue,
        };

        let module = match path_item {
            mondc::sexpr::SExpr::Atom(tok) => match &tok.kind {
                mondc::lexer::TokenKind::QualifiedIdent((_, module)) => module.clone(),
                mondc::lexer::TokenKind::Ident => source[tok.span.clone()].to_string(),
                _ => continue,
            },
            _ => continue,
        };

        let Some(mondc::sexpr::SExpr::Square(items, _)) = imports_item else {
            continue;
        };
        for item in items {
            let mondc::sexpr::SExpr::Atom(tok) = item else {
                continue;
            };
            if !matches!(tok.kind, mondc::lexer::TokenKind::Ident) {
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

pub(super) fn collect_decl_occurrences(
    decl: &mondc::ast::Declaration,
    current_module: &str,
    top_level: &HashSet<String>,
    imports: &mondc::ResolvedImports,
    out: &mut Vec<SymbolOccurrence>,
) {
    match decl {
        mondc::ast::Declaration::Expression(expr) => {
            collect_expr_occurrences(
                expr,
                current_module,
                top_level,
                imports,
                &HashSet::new(),
                out,
            );
        }
        mondc::ast::Declaration::ExternLet {
            name, name_span, ..
        } => out.push(SymbolOccurrence {
            symbol: Symbol {
                module: current_module.to_string(),
                function: name.clone(),
            },
            range: name_span.clone(),
            kind: OccurrenceKind::Definition,
        }),
        mondc::ast::Declaration::Test { body, .. } => {
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

pub(super) fn collect_expr_occurrences(
    expr: &mondc::ast::Expr,
    current_module: &str,
    top_level: &HashSet<String>,
    imports: &mondc::ResolvedImports,
    locals: &HashSet<String>,
    out: &mut Vec<SymbolOccurrence>,
) {
    use mondc::ast::Expr;

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
            for arm in arms {
                let mut inner = locals.clone();
                for pat in &arm.patterns {
                    bind_pattern_names(pat, &mut inner);
                }
                if let Some(guard) = &arm.guard {
                    collect_expr_occurrences(
                        guard,
                        current_module,
                        top_level,
                        imports,
                        &inner,
                        out,
                    );
                }
                collect_expr_occurrences(
                    &arm.body,
                    current_module,
                    top_level,
                    imports,
                    &inner,
                    out,
                );
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
        Expr::RecordUpdate {
            record, updates, ..
        } => {
            collect_expr_occurrences(record, current_module, top_level, imports, locals, out);
            for (_, value) in updates {
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

pub(super) fn qualified_function_range(
    span: &std::ops::Range<usize>,
    function: &str,
) -> std::ops::Range<usize> {
    let end = span.end;
    let start = end.saturating_sub(function.len());
    start..end
}

pub(super) fn find_hover_target(
    source_path: &Path,
    source: &str,
    offset: usize,
) -> Option<HoverTarget> {
    if let Some(field_accessor) = field_accessor_at_offset(source, offset) {
        return Some(HoverTarget::Unqualified(field_accessor));
    }

    let mut lowerer = mondc::lower::Lowerer::new();
    let tokens = mondc::lexer::Lexer::new(source).lex();
    let file_id = lowerer.add_file(
        source_path.to_string_lossy().to_string(),
        source.to_string(),
    );
    let sexprs = mondc::sexpr::SExprParser::new(tokens, file_id)
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

pub(super) fn hover_target_in_decl(
    decl: &mondc::ast::Declaration,
    offset: usize,
    locals: &HashSet<String>,
) -> Option<HoverTarget> {
    match decl {
        mondc::ast::Declaration::Expression(expr) => hover_target_in_expr(expr, offset, locals),
        mondc::ast::Declaration::Test { body, .. } => hover_target_in_expr(body, offset, locals),
        _ => None,
    }
}

pub(super) fn hover_target_in_expr(
    expr: &mondc::ast::Expr,
    offset: usize,
    locals: &HashSet<String>,
) -> Option<HoverTarget> {
    use mondc::ast::Expr;
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
                arms.iter().find_map(|arm| {
                    let mut inner = locals.clone();
                    for pat in &arm.patterns {
                        bind_pattern_names(pat, &mut inner);
                    }
                    hover_target_in_expr(&arm.body, offset, &inner).or_else(|| {
                        arm.guard
                            .as_ref()
                            .and_then(|guard| hover_target_in_expr(guard, offset, &inner))
                    })
                })
            }),
        Expr::FieldAccess { record, .. } => hover_target_in_expr(record, offset, locals),
        Expr::RecordConstruct { fields, .. } => fields
            .iter()
            .find_map(|(_, value)| hover_target_in_expr(value, offset, locals)),
        Expr::RecordUpdate {
            record, updates, ..
        } => hover_target_in_expr(record, offset, locals).or_else(|| {
            updates
                .iter()
                .find_map(|(_, value)| hover_target_in_expr(value, offset, locals))
        }),
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

pub(super) fn local_symbol_at(
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

pub(super) fn collect_local_occurrences(
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

pub(super) fn collect_local_occurrences_in_decl(
    decl: &mondc::ast::Declaration,
    locals: &HashMap<String, LocalSymbol>,
    out: &mut Vec<LocalOccurrence>,
) {
    match decl {
        mondc::ast::Declaration::Expression(expr) => {
            collect_local_occurrences_in_expr(expr, locals, out);
        }
        mondc::ast::Declaration::Test { body, .. } => {
            collect_local_occurrences_in_expr(body, locals, out);
        }
        _ => {}
    }
}

pub(super) fn collect_local_occurrences_in_expr(
    expr: &mondc::ast::Expr,
    locals: &HashMap<String, LocalSymbol>,
    out: &mut Vec<LocalOccurrence>,
) {
    use mondc::ast::Expr;

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
            for arm in arms {
                let mut inner = locals.clone();
                for pat in &arm.patterns {
                    bind_pattern_locals(pat, &mut inner, out);
                }
                if let Some(guard) = &arm.guard {
                    collect_local_occurrences_in_expr(guard, &inner, out);
                }
                collect_local_occurrences_in_expr(&arm.body, &inner, out);
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
        Expr::RecordUpdate {
            record, updates, ..
        } => {
            collect_local_occurrences_in_expr(record, locals, out);
            for (_, value) in updates {
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

pub(super) fn bind_pattern_locals(
    pat: &mondc::ast::Pattern,
    locals: &mut HashMap<String, LocalSymbol>,
    out: &mut Vec<LocalOccurrence>,
) {
    match pat {
        mondc::ast::Pattern::Variable(name, span) => {
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
        mondc::ast::Pattern::Constructor(_, args, _) | mondc::ast::Pattern::Or(args, _) => {
            for arg in args {
                bind_pattern_locals(arg, locals, out);
            }
        }
        mondc::ast::Pattern::Cons(head, tail, _) => {
            bind_pattern_locals(head, locals, out);
            bind_pattern_locals(tail, locals, out);
        }
        mondc::ast::Pattern::Record { fields, .. } => {
            for (_, pat, _) in fields {
                bind_pattern_locals(pat, locals, out);
            }
        }
        mondc::ast::Pattern::Any(_)
        | mondc::ast::Pattern::Literal(_, _)
        | mondc::ast::Pattern::EmptyList(_) => {}
    }
}

pub(super) fn bind_pattern_names(pat: &mondc::ast::Pattern, out: &mut HashSet<String>) {
    match pat {
        mondc::ast::Pattern::Variable(name, _) => {
            out.insert(name.clone());
        }
        mondc::ast::Pattern::Constructor(_, args, _) => {
            for arg in args {
                bind_pattern_names(arg, out);
            }
        }
        mondc::ast::Pattern::Or(pats, _) => {
            for pat in pats {
                bind_pattern_names(pat, out);
            }
        }
        mondc::ast::Pattern::Cons(head, tail, _) => {
            bind_pattern_names(head, out);
            bind_pattern_names(tail, out);
        }
        mondc::ast::Pattern::Record { fields, .. } => {
            for (_, pat, _) in fields {
                bind_pattern_names(pat, out);
            }
        }
        mondc::ast::Pattern::Any(_)
        | mondc::ast::Pattern::Literal(_, _)
        | mondc::ast::Pattern::EmptyList(_) => {}
    }
}
