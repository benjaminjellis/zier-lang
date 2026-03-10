use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

#[derive(Clone, Debug)]
pub struct ProjectAnalysis {
    pub module_exports: HashMap<String, Vec<String>>,
    pub module_type_decls: HashMap<String, Vec<crate::ast::TypeDecl>>,
    pub all_module_schemes: HashMap<String, crate::typecheck::TypeEnv>,
    pub std_aliases: HashMap<String, String>,
}

#[derive(Clone, Debug, Default)]
pub struct ResolvedImports {
    pub imports: HashMap<String, String>,
    pub import_origins: HashMap<String, String>,
    pub imported_schemes: crate::typecheck::TypeEnv,
    pub imported_type_decls: Vec<crate::ast::TypeDecl>,
    pub module_aliases: HashMap<String, String>,
}

pub fn build_project_analysis(
    std_mods: &[(String, String, String)],
    src_module_sources: &[(String, String)],
) -> Result<ProjectAnalysis, String> {
    let mut module_exports = HashMap::new();
    let mut module_type_decls = HashMap::new();

    for (user_name, _, source) in std_mods {
        module_exports.insert(user_name.clone(), crate::exported_names(source));
        module_type_decls.insert(user_name.clone(), crate::exported_type_decls(source));
    }
    for (module_name, source) in src_module_sources {
        module_exports.insert(module_name.clone(), crate::exported_names(source));
        module_type_decls.insert(module_name.clone(), crate::exported_type_decls(source));
    }

    let std_aliases: HashMap<String, String> = std_mods
        .iter()
        .map(|(user_name, erlang_name, _)| (user_name.clone(), erlang_name.clone()))
        .collect();

    let mut all_module_schemes: HashMap<String, crate::typecheck::TypeEnv> = HashMap::new();
    for (user_name, _, source) in std_mods {
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
        let schemes = crate::infer_module_exports(
            user_name,
            source,
            imports.imports,
            &module_exports,
            &imports.imported_type_decls,
            &imports.imported_schemes,
        );
        all_module_schemes.insert(user_name.clone(), schemes);
    }

    let ordered_module_sources = ordered_module_sources(src_module_sources)?;
    for (module_name, source) in &ordered_module_sources {
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
        let schemes = crate::infer_module_exports(
            module_name,
            source,
            imports.imports,
            &module_exports,
            &imports.imported_type_decls,
            &imports.imported_schemes,
        );
        all_module_schemes.insert(module_name.clone(), schemes);
    }

    Ok(ProjectAnalysis {
        module_exports,
        module_type_decls,
        all_module_schemes,
        std_aliases,
    })
}

pub fn resolve_imports_for_source(
    source: &str,
    visible_exports: &HashMap<String, Vec<String>>,
    project: &ProjectAnalysis,
) -> ResolvedImports {
    let mut imports = HashMap::new();
    let mut import_origins = HashMap::new();
    let mut imported_schemes = HashMap::new();

    for (_, mod_name, unqualified) in crate::used_modules(source) {
        let erlang_name = project
            .std_aliases
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

        if let Some(mod_schemes) = project.all_module_schemes.get(&mod_name) {
            for (fn_name, scheme) in mod_schemes {
                if unqualified.includes(fn_name) {
                    imported_schemes.insert(fn_name.clone(), scheme.clone());
                }
                imported_schemes.insert(format!("{mod_name}/{fn_name}"), scheme.clone());
            }
        }
    }

    let imported_type_decls: Vec<crate::ast::TypeDecl> = referenced_modules(source)
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

    let mut graph: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (module_name, source) in &source_by_name {
        let mut deps: BTreeSet<String> = BTreeSet::new();
        for (namespace, dep, _) in crate::used_modules(source) {
            if namespace.is_empty() && source_by_name.contains_key(&dep) {
                deps.insert(dep);
            }
        }
        graph.insert(module_name.clone(), deps.into_iter().collect());
    }

    let order = topo_sort_modules(&graph)?;
    Ok(order
        .into_iter()
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
            let erlang_name = if user_name == "std" {
                "mond_std".to_string()
            } else {
                format!("mond_{user_name}")
            };
            (user_name, erlang_name, source)
        })
        .collect())
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn resolve_imports_supports_root_and_submodule_imports() {
        let mut exports = HashMap::new();
        exports.insert("std".to_string(), vec!["hello".to_string()]);
        exports.insert("io".to_string(), vec!["println".to_string()]);

        let mut std_aliases = HashMap::new();
        std_aliases.insert("std".to_string(), "mond_std".to_string());
        std_aliases.insert("io".to_string(), "mond_io".to_string());

        let resolved = resolve_imports_for_source(
            "(use std [hello])\n(use std/io)\n(let main {} (hello))",
            &exports,
            &ProjectAnalysis {
                module_exports: exports.clone(),
                module_type_decls: HashMap::new(),
                all_module_schemes: HashMap::new(),
                std_aliases,
            },
        );

        assert_eq!(resolved.imports.get("hello"), Some(&"mond_std".to_string()));
        assert!(!resolved.imports.contains_key("println"));
    }
}
