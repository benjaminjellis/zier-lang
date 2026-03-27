use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    path::Path,
};

#[derive(Clone, Debug)]
pub struct ProjectAnalysis {
    pub module_exports: HashMap<String, Vec<String>>,
    pub module_type_decls: HashMap<String, Vec<crate::ast::TypeDecl>>,
    pub module_private_record_types: HashMap<String, Vec<String>>,
    pub module_extern_types: HashMap<String, Vec<String>>,
    pub all_module_schemes: HashMap<String, crate::typecheck::TypeEnv>,
    pub module_aliases: HashMap<String, String>,
}

#[derive(Clone, Debug, Default)]
pub struct ResolvedImports {
    pub imports: HashMap<String, String>,
    pub import_origins: HashMap<String, String>,
    pub imported_schemes: crate::typecheck::TypeEnv,
    pub imported_type_decls: Vec<crate::ast::TypeDecl>,
    pub imported_extern_types: Vec<String>,
    pub imported_field_indices: HashMap<(String, String), usize>,
    pub imported_private_records: HashMap<String, Vec<String>>,
    pub module_aliases: HashMap<String, String>,
}

fn type_decl_name(type_decl: &crate::ast::TypeDecl) -> &str {
    match type_decl {
        crate::ast::TypeDecl::Record { name, .. } => name,
        crate::ast::TypeDecl::Variant { name, .. } => name,
    }
}

fn clone_type_decl_with_name(
    type_decl: &crate::ast::TypeDecl,
    name: String,
) -> crate::ast::TypeDecl {
    match type_decl {
        crate::ast::TypeDecl::Record {
            is_pub,
            params,
            fields,
            span,
            ..
        } => crate::ast::TypeDecl::Record {
            is_pub: *is_pub,
            name,
            params: params.clone(),
            fields: fields.clone(),
            span: span.clone(),
        },
        crate::ast::TypeDecl::Variant {
            is_pub,
            params,
            constructors,
            span,
            ..
        } => crate::ast::TypeDecl::Variant {
            is_pub: *is_pub,
            name,
            params: params.clone(),
            constructors: constructors.clone(),
            span: span.clone(),
        },
    }
}

pub fn build_project_analysis(
    std_mods: &[(String, String, String)],
    src_module_sources: &[(String, String)],
) -> Result<ProjectAnalysis, String> {
    build_project_analysis_with_modules(std_mods, src_module_sources)
}

pub fn build_project_analysis_with_modules(
    external_mods: &[(String, String, String)],
    src_module_sources: &[(String, String)],
) -> Result<ProjectAnalysis, String> {
    build_project_analysis_with_modules_and_package(external_mods, src_module_sources, None)
}

pub fn build_project_analysis_with_modules_and_package(
    external_mods: &[(String, String, String)],
    src_module_sources: &[(String, String)],
    package_name: Option<&str>,
) -> Result<ProjectAnalysis, String> {
    let mut module_exports = HashMap::new();
    let mut module_type_decls = HashMap::new();
    let mut module_private_record_types = HashMap::new();
    let mut module_extern_types = HashMap::new();

    for (user_name, _, source) in external_mods {
        module_exports.insert(user_name.clone(), crate::exported_names(source));
        module_type_decls.insert(user_name.clone(), crate::exported_type_decls(source));
        module_private_record_types.insert(
            user_name.clone(),
            crate::query::private_record_type_names(source),
        );
        module_extern_types.insert(user_name.clone(), crate::exported_extern_types(source));
    }
    for (module_name, source) in src_module_sources {
        module_exports.insert(module_name.clone(), crate::exported_names(source));
        module_type_decls.insert(module_name.clone(), crate::exported_type_decls(source));
        module_private_record_types.insert(
            module_name.clone(),
            crate::query::private_record_type_names(source),
        );
        module_extern_types.insert(module_name.clone(), crate::exported_extern_types(source));
    }

    let mut module_aliases: HashMap<String, String> = external_mods
        .iter()
        .map(|(user_name, erlang_name, _)| (user_name.clone(), erlang_name.clone()))
        .collect();

    apply_package_root_alias_to_metadata(
        &mut module_exports,
        &mut module_type_decls,
        &mut module_private_record_types,
        &mut module_extern_types,
        &mut module_aliases,
        package_name,
    )?;

    let mut src_module_sources_for_inference = src_module_sources.to_vec();
    if let Some(package_name) = package_name
        && package_name != "lib"
        && !src_module_sources_for_inference
            .iter()
            .any(|(module_name, _)| module_name == package_name)
        && let Some((_, lib_source)) = src_module_sources
            .iter()
            .find(|(module_name, _)| module_name == "lib")
    {
        // Add a synthetic root-module source (package name -> lib source) so dependency
        // ordering and export inference both understand `(use <package>)` imports.
        src_module_sources_for_inference.push((package_name.to_string(), lib_source.clone()));
    }

    let mut all_module_schemes: HashMap<String, crate::typecheck::TypeEnv> = HashMap::new();
    for (user_name, _, source) in external_mods {
        let imports = resolve_imports_for_source(
            source,
            &module_exports,
            &ProjectAnalysis {
                module_exports: module_exports.clone(),
                module_type_decls: module_type_decls.clone(),
                module_private_record_types: module_private_record_types.clone(),
                module_extern_types: module_extern_types.clone(),
                all_module_schemes: all_module_schemes.clone(),
                module_aliases: module_aliases.clone(),
            },
        );
        let schemes = crate::infer_module_exports(
            user_name,
            source,
            imports.imports,
            &module_exports,
            &imports.imported_type_decls,
            &imports.imported_extern_types,
            &imports.imported_schemes,
        );
        all_module_schemes.insert(user_name.clone(), schemes);
    }

    let ordered_module_sources = ordered_module_sources(&src_module_sources_for_inference)?;
    for (module_name, source) in &ordered_module_sources {
        let imports = resolve_imports_for_source(
            source,
            &module_exports,
            &ProjectAnalysis {
                module_exports: module_exports.clone(),
                module_type_decls: module_type_decls.clone(),
                module_private_record_types: module_private_record_types.clone(),
                module_extern_types: module_extern_types.clone(),
                all_module_schemes: all_module_schemes.clone(),
                module_aliases: module_aliases.clone(),
            },
        );
        let schemes = crate::infer_module_exports(
            module_name,
            source,
            imports.imports,
            &module_exports,
            &imports.imported_type_decls,
            &imports.imported_extern_types,
            &imports.imported_schemes,
        );
        all_module_schemes.insert(module_name.clone(), schemes);
    }

    Ok(ProjectAnalysis {
        module_exports,
        module_type_decls,
        module_private_record_types,
        module_extern_types,
        all_module_schemes,
        module_aliases,
    })
}

fn apply_package_root_alias_to_metadata(
    module_exports: &mut HashMap<String, Vec<String>>,
    module_type_decls: &mut HashMap<String, Vec<crate::ast::TypeDecl>>,
    module_private_record_types: &mut HashMap<String, Vec<String>>,
    module_extern_types: &mut HashMap<String, Vec<String>>,
    module_aliases: &mut HashMap<String, String>,
    package_name: Option<&str>,
) -> Result<(), String> {
    const LIB_MODULE_NAME: &str = "lib";

    let Some(package_name) = package_name else {
        return Ok(());
    };
    if package_name == LIB_MODULE_NAME {
        return Ok(());
    }

    let Some(lib_exports) = module_exports.get(LIB_MODULE_NAME).cloned() else {
        return Ok(());
    };

    if module_exports.contains_key(package_name)
        || module_type_decls.contains_key(package_name)
        || module_extern_types.contains_key(package_name)
        || module_aliases.contains_key(package_name)
    {
        return Err(format!(
            "module name collision: package `{package_name}` conflicts with an existing module name; cannot alias `src/lib.mond` as `{package_name}`"
        ));
    }

    module_exports.insert(package_name.to_string(), lib_exports);
    module_type_decls.insert(
        package_name.to_string(),
        module_type_decls
            .get(LIB_MODULE_NAME)
            .cloned()
            .unwrap_or_default(),
    );
    module_private_record_types.insert(
        package_name.to_string(),
        module_private_record_types
            .get(LIB_MODULE_NAME)
            .cloned()
            .unwrap_or_default(),
    );
    module_extern_types.insert(
        package_name.to_string(),
        module_extern_types
            .get(LIB_MODULE_NAME)
            .cloned()
            .unwrap_or_default(),
    );
    module_aliases.insert(package_name.to_string(), LIB_MODULE_NAME.to_string());

    Ok(())
}

pub fn alias_package_root_module(
    analysis: &mut ProjectAnalysis,
    package_name: &str,
) -> Result<(), String> {
    const LIB_MODULE_NAME: &str = "lib";

    if package_name == LIB_MODULE_NAME {
        return Ok(());
    }

    let Some(lib_exports) = analysis.module_exports.get(LIB_MODULE_NAME).cloned() else {
        return Ok(());
    };

    if analysis.module_exports.contains_key(package_name)
        || analysis.module_type_decls.contains_key(package_name)
        || analysis.module_extern_types.contains_key(package_name)
        || analysis.all_module_schemes.contains_key(package_name)
    {
        return Err(format!(
            "module name collision: package `{package_name}` conflicts with an existing module name; cannot alias `src/lib.mond` as `{package_name}`"
        ));
    }

    analysis
        .module_exports
        .insert(package_name.to_string(), lib_exports);
    analysis.module_type_decls.insert(
        package_name.to_string(),
        analysis
            .module_type_decls
            .get(LIB_MODULE_NAME)
            .cloned()
            .unwrap_or_default(),
    );
    analysis.module_private_record_types.insert(
        package_name.to_string(),
        analysis
            .module_private_record_types
            .get(LIB_MODULE_NAME)
            .cloned()
            .unwrap_or_default(),
    );
    analysis.module_extern_types.insert(
        package_name.to_string(),
        analysis
            .module_extern_types
            .get(LIB_MODULE_NAME)
            .cloned()
            .unwrap_or_default(),
    );
    analysis.all_module_schemes.insert(
        package_name.to_string(),
        analysis
            .all_module_schemes
            .get(LIB_MODULE_NAME)
            .cloned()
            .unwrap_or_default(),
    );
    analysis
        .module_aliases
        .insert(package_name.to_string(), LIB_MODULE_NAME.to_string());

    Ok(())
}

pub fn resolve_imports_for_source(
    source: &str,
    visible_exports: &HashMap<String, Vec<String>>,
    project: &ProjectAnalysis,
) -> ResolvedImports {
    let mut imports = HashMap::new();
    let mut import_origins = HashMap::new();
    let mut imported_schemes = HashMap::new();
    let mut imported_type_decls = Vec::new();
    let mut imported_extern_types = Vec::new();
    let mut imported_field_indices: HashMap<(String, String), usize> = HashMap::new();
    let mut imported_private_records: HashMap<String, Vec<String>> = HashMap::new();
    let mut imported_type_keys: HashSet<(String, String)> = HashSet::new();
    let mut imported_extern_type_keys: HashSet<(String, String)> = HashSet::new();
    let mut imported_qualified_type_keys: HashSet<(String, String)> = HashSet::new();
    let mut imported_qualified_extern_type_keys: HashSet<(String, String)> = HashSet::new();

    for (_, mod_name, unqualified) in crate::used_modules(source) {
        let erlang_name = project
            .module_aliases
            .get(&mod_name)
            .cloned()
            .unwrap_or_else(|| mod_name.clone());

        if let Some(exports) = visible_exports.get(&mod_name) {
            for fn_name in exports {
                if unqualified.includes(fn_name) {
                    imports.insert(fn_name.clone(), erlang_name.clone());
                    import_origins.insert(fn_name.clone(), mod_name.clone());
                }
            }
        }

        let mod_schemes = project.all_module_schemes.get(&mod_name).or_else(|| {
            project
                .module_aliases
                .get(&mod_name)
                .and_then(|alias_target| project.all_module_schemes.get(alias_target))
        });
        if let Some(mod_schemes) = mod_schemes {
            for (fn_name, scheme) in mod_schemes {
                if unqualified.includes(fn_name) {
                    imported_schemes.insert(fn_name.clone(), scheme.clone());
                }
                imported_schemes.insert(format!("{mod_name}/{fn_name}"), scheme.clone());
            }
        }

        if let Some(type_decls) = project.module_type_decls.get(&mod_name) {
            // Always add qualified type aliases (module/TypeName) for modules in scope.
            for type_decl in type_decls {
                let type_name = type_decl_name(type_decl).to_string();
                if imported_qualified_type_keys.insert((mod_name.clone(), type_name.clone())) {
                    imported_type_decls.push(clone_type_decl_with_name(
                        type_decl,
                        format!("{mod_name}/{type_name}"),
                    ));
                }
            }

            // Qualified constructors should be available whenever the module is in scope,
            // even if types are not imported unqualified.
            let qualified_type_aliases_for_module: HashMap<String, String> = type_decls
                .iter()
                .map(|type_decl| {
                    let type_name = type_decl_name(type_decl).to_string();
                    (format!("{mod_name}/{type_name}"), type_name)
                })
                .collect();
            for type_decl in type_decls {
                let constructor_schemes = crate::typecheck::constructor_schemes_with_aliases(
                    type_decl,
                    &qualified_type_aliases_for_module,
                );
                for (name, scheme) in constructor_schemes {
                    if name.starts_with(':') {
                        continue;
                    }
                    imported_schemes
                        .entry(format!("{mod_name}/{name}"))
                        .or_insert(scheme);
                }
            }

            // Field accessors (for example `:value`) should remain usable when a module is
            // referenced, even if constructors require explicit unqualified type import.
            for type_decl in type_decls {
                if matches!(type_decl, crate::ast::TypeDecl::Record { .. }) {
                    let accessor_schemes = crate::typecheck::constructor_schemes(type_decl);
                    for (name, scheme) in accessor_schemes {
                        if name.starts_with(':') {
                            imported_schemes.insert(name, scheme);
                        }
                    }
                    if let crate::ast::TypeDecl::Record { fields, .. } = type_decl {
                        for (i, (field_name, _)) in fields.iter().enumerate() {
                            imported_field_indices.insert(
                                (type_decl_name(type_decl).to_string(), field_name.clone()),
                                i + 2,
                            );
                        }
                    }
                }
            }

            match &unqualified {
                crate::ast::UnqualifiedImports::None => {}
                crate::ast::UnqualifiedImports::Wildcard => {
                    for type_decl in type_decls {
                        let type_name = type_decl_name(type_decl).to_string();
                        let key = (mod_name.clone(), type_name);
                        if imported_type_keys.insert(key) {
                            imported_type_decls.push(type_decl.clone());
                        }
                    }
                }
                crate::ast::UnqualifiedImports::Specific(names) => {
                    for type_decl in type_decls {
                        let type_name = type_decl_name(type_decl);
                        if !names.iter().any(|n| n == type_name) {
                            continue;
                        }
                        let key = (mod_name.clone(), type_name.to_string());
                        if imported_type_keys.insert(key) {
                            imported_type_decls.push(type_decl.clone());
                        }
                    }
                }
            }
        }

        if let Some(private_records) = project.module_private_record_types.get(&mod_name) {
            for record_name in private_records {
                let unqualified = imported_private_records
                    .entry(record_name.clone())
                    .or_default();
                if !unqualified.iter().any(|m| m == &mod_name) {
                    unqualified.push(mod_name.clone());
                    unqualified.sort();
                }

                let qualified_key = format!("{mod_name}/{record_name}");
                let qualified = imported_private_records.entry(qualified_key).or_default();
                if !qualified.iter().any(|m| m == &mod_name) {
                    qualified.push(mod_name.clone());
                    qualified.sort();
                }
            }
        }

        if let Some(extern_types) = project.module_extern_types.get(&mod_name) {
            // Always add qualified extern type aliases (module/TypeName) for modules in scope.
            for type_name in extern_types {
                if imported_qualified_extern_type_keys.insert((mod_name.clone(), type_name.clone()))
                {
                    imported_extern_types.push(format!("{mod_name}/{type_name}"));
                }
            }

            match &unqualified {
                crate::ast::UnqualifiedImports::None => {}
                crate::ast::UnqualifiedImports::Wildcard => {
                    for type_name in extern_types {
                        let key = (mod_name.clone(), type_name.clone());
                        if imported_extern_type_keys.insert(key) {
                            imported_extern_types.push(type_name.clone());
                        }
                    }
                }
                crate::ast::UnqualifiedImports::Specific(names) => {
                    for type_name in extern_types {
                        if !names.iter().any(|name| name == type_name) {
                            continue;
                        }
                        let key = (mod_name.clone(), type_name.clone());
                        if imported_extern_type_keys.insert(key) {
                            imported_extern_types.push(type_name.clone());
                        }
                    }
                }
            }
        }
    }

    ResolvedImports {
        imports,
        import_origins,
        imported_schemes,
        imported_type_decls,
        imported_extern_types,
        imported_field_indices,
        imported_private_records,
        module_aliases: project.module_aliases.clone(),
    }
}

pub fn referenced_modules(source: &str) -> HashSet<String> {
    let mut referenced: HashSet<String> = crate::used_modules(source)
        .into_iter()
        .map(|(_, mod_name, _)| mod_name)
        .collect();
    for tok in crate::lexer::Lexer::new(source).lex() {
        if let crate::lexer::TokenKind::QualifiedIdent((module, _)) = tok.kind {
            referenced.insert(module);
        }
    }
    referenced
}

pub fn ordered_module_sources(
    module_sources: &[(String, String)],
) -> Result<Vec<(String, String)>, String> {
    let source_by_name: BTreeMap<String, String> = module_sources
        .iter()
        .map(|(name, src)| (name.clone(), src.clone()))
        .collect();
    if source_by_name.len() != module_sources.len() {
        return Err(
            "duplicate module names found in src/: module file stems must be unique".into(),
        );
    }

    let graph = local_module_graph(&source_by_name);

    let order = topo_sort_modules(&graph)?;
    Ok(order
        .into_iter()
        .filter_map(|name| source_by_name.get(&name).cloned().map(|src| (name, src)))
        .collect())
}

pub fn reachable_module_sources(
    module_sources: &[(String, String)],
    roots: &[String],
) -> Result<Vec<(String, String)>, String> {
    let source_by_name: BTreeMap<String, String> = module_sources
        .iter()
        .map(|(name, src)| (name.clone(), src.clone()))
        .collect();
    if source_by_name.len() != module_sources.len() {
        return Err(
            "duplicate module names found in src/: module file stems must be unique".into(),
        );
    }

    let graph = local_module_graph(&source_by_name);
    let order = topo_sort_modules(&graph)?;
    let reachable = reachable_modules(&graph, roots)?;

    Ok(order
        .into_iter()
        .filter(|name| reachable.contains(name))
        .filter_map(|name| source_by_name.get(&name).cloned().map(|src| (name, src)))
        .collect())
}

pub fn std_modules_from_sources(
    module_sources: &[(String, String)],
) -> Result<Vec<(String, String, String)>, String> {
    let ordered = ordered_module_sources(module_sources)?;
    Ok(ordered
        .into_iter()
        .map(|(user_name, source)| {
            let erlang_name = format!("mond_{user_name}");
            (user_name, erlang_name, source)
        })
        .collect())
}

pub fn dependency_erlang_module_name(dep_name: &str, module_name: &str) -> String {
    format!("d_{}_{}", sanitize_erlang_prefix(dep_name), module_name)
}

pub fn load_dependency_modules_from_checkout(
    dep_name: &str,
    checkout_dir: &Path,
) -> Result<Vec<(String, String, String)>, String> {
    let src_dir = checkout_dir.join("src");
    if !src_dir.exists() {
        return Err(format!(
            "dependency `{dep_name}` is missing `src` at {}",
            src_dir.display()
        ));
    }

    let mut dep_sources: Vec<(String, String)> = Vec::new();
    let mut lib_source: Option<String> = None;
    collect_named_module_sources(&src_dir, &mut dep_sources, &mut lib_source)?;
    if let Some(lib_src) = lib_source {
        dep_sources.push((dep_name.to_string(), lib_src));
    }

    std_modules_from_sources(&dep_sources)?
        .into_iter()
        .map(|(user_name, _, source)| {
            let erlang_name = dependency_erlang_module_name(dep_name, &user_name);
            Ok((user_name, erlang_name, source))
        })
        .collect()
}

fn topo_sort_modules(graph: &BTreeMap<String, Vec<String>>) -> Result<Vec<String>, String> {
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
    ) -> Result<(), String> {
        match marks.get(node).copied() {
            Some(Mark::Done) => return Ok(()),
            Some(Mark::Visiting) => {
                let start = stack.iter().position(|n| n == node).unwrap_or(0);
                let mut cycle: Vec<String> = stack[start..].to_vec();
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
    let mut out = Vec::new();
    let mut stack = Vec::new();
    for node in graph.keys() {
        dfs(node, graph, &mut marks, &mut stack, &mut out)?;
    }
    Ok(out)
}

fn sanitize_erlang_prefix(name: &str) -> String {
    let mut sanitized = String::with_capacity(name.len());
    for ch in name.chars() {
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

fn collect_named_module_sources(
    dir: &Path,
    dep_sources: &mut Vec<(String, String)>,
    lib_source: &mut Option<String>,
) -> Result<(), String> {
    let entries =
        fs::read_dir(dir).map_err(|err| format!("failed to read {}: {err}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|err| format!("failed to read {}: {err}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_named_module_sources(&path, dep_sources, lib_source)?;
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("mond") {
            continue;
        }
        let module_name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        let source = fs::read_to_string(&path)
            .map_err(|err| format!("could not read {}: {err}", path.display()))?;
        if module_name == "lib" {
            *lib_source = Some(source);
        } else {
            dep_sources.push((module_name, source));
        }
    }
    Ok(())
}

fn local_module_graph(source_by_name: &BTreeMap<String, String>) -> BTreeMap<String, Vec<String>> {
    let mut graph: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (module_name, source) in source_by_name {
        let mut deps: BTreeSet<String> = BTreeSet::new();
        for (namespace, dep, _) in crate::used_modules(source) {
            if namespace.is_empty() && source_by_name.contains_key(&dep) {
                deps.insert(dep);
            }
        }
        graph.insert(module_name.clone(), deps.into_iter().collect());
    }
    graph
}

fn reachable_modules(
    graph: &BTreeMap<String, Vec<String>>,
    roots: &[String],
) -> Result<BTreeSet<String>, String> {
    let mut reachable = BTreeSet::new();
    let mut stack: Vec<String> = Vec::new();

    for root in roots {
        if !graph.contains_key(root) {
            return Err(format!(
                "module `{root}` was selected as a build root but does not exist in src/"
            ));
        }
        stack.push(root.clone());
    }

    while let Some(module_name) = stack.pop() {
        if !reachable.insert(module_name.clone()) {
            continue;
        }
        for dep in graph.get(&module_name).into_iter().flatten() {
            stack.push(dep.clone());
        }
    }

    Ok(reachable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn unique_temp_root() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!("mondc-project-test-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn ordered_module_sources_respects_dependencies() {
        let modules = vec![
            (
                "main".to_string(),
                "(use util)\n(let main {} (util_fn))".to_string(),
            ),
            ("util".to_string(), "(let util_fn {} 1)".to_string()),
            ("other".to_string(), "(let other {} 2)".to_string()),
        ];
        let ordered = ordered_module_sources(&modules).expect("topo order");
        let names: Vec<String> = ordered.into_iter().map(|(n, _)| n).collect();
        let pos_main = names.iter().position(|n| n == "main").expect("main");
        let pos_util = names.iter().position(|n| n == "util").expect("util");
        assert!(pos_util < pos_main, "dependency must come first: {names:?}");
    }

    #[test]
    fn ordered_module_sources_rejects_cycles() {
        let modules = vec![
            ("a".to_string(), "(use b)\n(let a {} 1)".to_string()),
            ("b".to_string(), "(use a)\n(let b {} 2)".to_string()),
        ];
        let err = ordered_module_sources(&modules).expect_err("expected cycle error");
        assert!(err.contains("cyclic module dependency detected"));
        assert!(err.contains("a -> b -> a") || err.contains("b -> a -> b"));
    }

    #[test]
    fn reachable_module_sources_only_keeps_transitive_dependencies_of_roots() {
        let modules = vec![
            (
                "main".to_string(),
                "(use util)\n(let main {} (util_fn))".to_string(),
            ),
            (
                "util".to_string(),
                "(use helper)\n(let util_fn {} (helper_fn))".to_string(),
            ),
            ("helper".to_string(), "(let helper_fn {} 1)".to_string()),
            ("unused".to_string(), "(let ignore_me {} 2)".to_string()),
        ];

        let ordered = reachable_module_sources(&modules, &["main".to_string()]).expect("roots");
        let names: Vec<String> = ordered.into_iter().map(|(n, _)| n).collect();

        assert_eq!(names, vec!["helper", "util", "main"]);
    }

    #[test]
    fn std_modules_from_sources_discovers_files_without_root_reexports() {
        let modules = vec![
            ("io".to_string(), "(let println {x} x)".to_string()),
            ("extra".to_string(), "(let helper {} 1)".to_string()),
            ("std".to_string(), "(let hello {} 1)".to_string()),
        ];
        let discovered = std_modules_from_sources(&modules).expect("std modules");
        let names: Vec<String> = discovered.into_iter().map(|(name, _, _)| name).collect();
        assert!(names.contains(&"io".to_string()));
        assert!(names.contains(&"extra".to_string()));
        assert!(names.contains(&"std".to_string()));
    }

    #[test]
    fn load_dependency_modules_from_checkout_aliases_lib_to_package_name() {
        let root = unique_temp_root();
        let src_dir = root.join("src");
        fs::create_dir_all(&src_dir).expect("create src");
        fs::write(src_dir.join("lib.mond"), "(pub let now {} 1)").expect("write lib");
        fs::write(src_dir.join("duration.mond"), "(pub let seconds {} 1)").expect("write duration");

        let modules =
            load_dependency_modules_from_checkout("time", &root).expect("load dependency");

        assert!(
            modules
                .iter()
                .any(|(name, erl, _)| name == "time" && erl == "d_time_time"),
            "expected aliased root module, got {modules:?}"
        );
        assert!(
            modules
                .iter()
                .any(|(name, erl, _)| name == "duration" && erl == "d_time_duration"),
            "expected submodule alias, got {modules:?}"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolve_imports_supports_root_and_submodule_imports() {
        let mut exports = HashMap::new();
        exports.insert("std".to_string(), vec!["hello".to_string()]);
        exports.insert("io".to_string(), vec!["println".to_string()]);

        let mut module_aliases = HashMap::new();
        module_aliases.insert("std".to_string(), "mond_std".to_string());
        module_aliases.insert("io".to_string(), "mond_io".to_string());

        let resolved = resolve_imports_for_source(
            "(use std [hello])\n(use std/io)\n(let main {} (hello))",
            &exports,
            &ProjectAnalysis {
                module_exports: exports.clone(),
                module_type_decls: HashMap::new(),
                module_private_record_types: HashMap::new(),
                module_extern_types: HashMap::new(),
                all_module_schemes: HashMap::new(),
                module_aliases,
            },
        );

        assert_eq!(resolved.imports.get("hello"), Some(&"mond_std".to_string()));
        assert!(!resolved.imports.contains_key("println"));
    }

    #[test]
    fn alias_package_root_module_maps_package_name_to_lib() {
        let mut analysis = ProjectAnalysis {
            module_exports: HashMap::from([
                ("lib".to_string(), vec!["now".to_string()]),
                ("util".to_string(), vec!["helper".to_string()]),
            ]),
            module_type_decls: HashMap::new(),
            module_private_record_types: HashMap::new(),
            module_extern_types: HashMap::new(),
            all_module_schemes: HashMap::new(),
            module_aliases: HashMap::new(),
        };

        alias_package_root_module(&mut analysis, "time").expect("alias package root module");

        assert!(analysis.module_exports.contains_key("time"));
        assert_eq!(
            analysis.module_aliases.get("time").map(String::as_str),
            Some("lib")
        );
    }

    #[test]
    fn resolve_imports_falls_back_to_alias_target_schemes() {
        let project = ProjectAnalysis {
            module_exports: HashMap::from([("time".to_string(), vec!["now".to_string()])]),
            module_type_decls: HashMap::new(),
            module_private_record_types: HashMap::new(),
            module_extern_types: HashMap::new(),
            all_module_schemes: HashMap::from([(
                "lib".to_string(),
                HashMap::from([(
                    "now".to_string(),
                    crate::typecheck::Scheme {
                        vars: vec![],
                        preds: vec![],
                        ty: crate::typecheck::Type::int(),
                    },
                )]),
            )]),
            module_aliases: HashMap::from([("time".to_string(), "lib".to_string())]),
        };

        let resolved = resolve_imports_for_source(
            "(use time)\n(let main {} (time/now))",
            &project.module_exports,
            &project,
        );
        let scheme = resolved
            .imported_schemes
            .get("time/now")
            .expect("time/now scheme");
        assert_eq!(crate::typecheck::scheme_display(scheme), "Int");
    }

    #[test]
    fn build_project_analysis_with_package_infers_precise_types_for_package_imports() {
        let src_modules = vec![
            (
                "lib".to_string(),
                "(pub type Header [(:name ~ String)])\n\
                 (pub let to_header {value} (Header :name value))"
                    .to_string(),
            ),
            (
                "cookie".to_string(),
                "(use time)\n\
                 (pub let parse {value}\n\
                   [(time/to_header value)])"
                    .to_string(),
            ),
        ];

        let analysis =
            build_project_analysis_with_modules_and_package(&[], &src_modules, Some("time"))
                .expect("project analysis with package alias");
        let cookie_schemes = analysis
            .all_module_schemes
            .get("cookie")
            .expect("cookie schemes");
        let parse_scheme = cookie_schemes.get("parse").expect("parse scheme");
        assert_eq!(
            crate::typecheck::scheme_display(parse_scheme),
            "String -> List Header"
        );
    }

    #[test]
    fn alias_package_root_module_rejects_name_collisions() {
        let mut analysis = ProjectAnalysis {
            module_exports: HashMap::from([
                ("lib".to_string(), vec!["now".to_string()]),
                ("time".to_string(), vec!["from_time".to_string()]),
            ]),
            module_type_decls: HashMap::new(),
            module_private_record_types: HashMap::new(),
            module_extern_types: HashMap::new(),
            all_module_schemes: HashMap::new(),
            module_aliases: HashMap::new(),
        };

        let err =
            alias_package_root_module(&mut analysis, "time").expect_err("expected alias collision");
        assert!(
            err.contains("module name collision"),
            "unexpected error: {err}"
        );
    }
}
