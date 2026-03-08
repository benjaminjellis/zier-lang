use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{BIN_ENTRY_POINT, LIB_ROOT, TARGET_DIR, gitignore};
use clap::builder::OsStr;
use eyre::Context;

use crate::{DEBUG_BUILD_DIR, ProjectType, SOURCE_DIR, manifest, utils::find_opal_files};

// opal-std is embedded at compile time — std ships with loupe,
use include_dir::{Dir, include_dir};
static STD_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../opal-std/src");

pub(crate) fn std_dir() -> &'static Dir<'static> {
    &STD_DIR
}

/// Return `(user_name, erlang_name, source)` for each std module:
///   - user_name:   the name users write in `(use std/io)` → "io"
///   - erlang_name: the compiled Erlang module name → "opal_io"
///     Prefixed with "opal_" to avoid shadowing Erlang/OTP built-in modules.
pub(crate) fn std_modules() -> Vec<(String, String, String)> {
    let lib_src = STD_DIR
        .get_file("lib.opal")
        .and_then(|f| f.contents_utf8())
        .unwrap_or("");

    let mut result = Vec::new();
    result.push((
        "std".to_string(),
        "opal_std".to_string(),
        lib_src.to_string(),
    ));

    for mod_name in opalc::pub_reexports(lib_src) {
        let file_name = format!("{mod_name}.opal");
        if let Some(src) = STD_DIR.get_file(&file_name).and_then(|f| f.contents_utf8()) {
            let erlang_name = format!("opal_{mod_name}");
            result.push((mod_name, erlang_name, src.to_string()));
        }
    }
    result
}

pub(crate) struct ErlSources {
    pub erl_paths: Vec<PathBuf>,
    pub manifest: manifest::LoupeManifest,
    pub project_type: ProjectType,
    // Compilation state exposed for `loupe test`
    pub module_exports: HashMap<String, Vec<String>>,
    pub module_type_decls: HashMap<String, Vec<opalc::ast::TypeDecl>>,
    pub all_module_schemes: HashMap<String, opalc::typecheck::TypeEnv>,
    pub std_mods: Vec<(String, String, String)>,
    pub std_aliases: HashMap<String, String>,
}

/// Compile all Opal source files and write `.erl` output into `erl_dir`.
/// Returns the generated file paths, the project manifest, and detected project type.
/// Exits with code 1 on any compilation error.
pub(crate) fn generate_erl_sources(project_dir: &Path, erl_dir: &Path) -> eyre::Result<ErlSources> {
    let manifest = manifest::read_manifest(project_dir.into())?;

    let src_dir = project_dir.join(SOURCE_DIR);
    let opal_files = find_opal_files(&src_dir);

    if opal_files.is_empty() {
        return Err(eyre::eyre!(
            "loupe found no .opal files in {}",
            src_dir.display()
        ));
    }

    let project_type = verify_project_type(&opal_files).ok_or_else(|| {
        eyre::eyre!("loupe failed to find one of {BIN_ENTRY_POINT} or {LIB_ROOT}")
    })?;

    // Phase 1: scan each module's source to collect its exported function names and type decls
    let mut module_exports: HashMap<String, Vec<String>> = HashMap::new();
    let mut module_type_decls: HashMap<String, Vec<opalc::ast::TypeDecl>> = HashMap::new();
    let mut module_sources: Vec<(String, String)> = Vec::new(); // (module_name, source)

    for opal_path in &opal_files {
        let module_name = opal_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let source = std::fs::read_to_string(opal_path).unwrap_or_else(|e| {
            eprintln!("error: could not read {}: {e}", opal_path.display());
            std::process::exit(1);
        });

        let exports = opalc::exported_names(&source);
        let type_decls = opalc::exported_type_decls(&source);
        module_exports.insert(module_name.clone(), exports);
        module_type_decls.insert(module_name.clone(), type_decls);
        module_sources.push((module_name, source));
    }

    // Phase 1b: seed module_exports with embedded std modules so the compiler's
    // `use` validation and import building treats them identically to local modules.
    let std_mods = std_modules();
    for (user_name, _, source) in &std_mods {
        let exports = opalc::exported_names(source);
        let type_decls = opalc::exported_type_decls(source);
        module_exports.insert(user_name.clone(), exports);
        module_type_decls.insert(user_name.clone(), type_decls);
    }

    // Phase 1c: infer real type schemes for each std module in order so that
    // dependent modules (including user code) get proper type-checking.
    let mut all_module_schemes: HashMap<String, opalc::typecheck::TypeEnv> = HashMap::new();
    let std_aliases: HashMap<String, String> = std_mods
        .iter()
        .map(|(u, e, _)| (u.clone(), e.clone()))
        .collect();

    for (user_name, _erlang_name, source) in &std_mods {
        let mut std_imports: HashMap<String, String> = HashMap::new();
        let mut std_imported_type_decls: Vec<opalc::ast::TypeDecl> = Vec::new();
        let mut std_imported_schemes: opalc::typecheck::TypeEnv = HashMap::new();

        for (_, mod_name, unqualified) in opalc::used_modules(source) {
            let erl_name = std_aliases
                .get(&mod_name)
                .cloned()
                .unwrap_or_else(|| mod_name.clone());
            if let Some(exports) = module_exports.get(&mod_name) {
                for fn_name in exports {
                    if unqualified.includes(fn_name) {
                        std_imports.insert(fn_name.clone(), erl_name.clone());
                    }
                }
            }
            if let Some(type_decls) = module_type_decls.get(&mod_name) {
                std_imported_type_decls.extend(type_decls.clone());
            }
            if let Some(dep_schemes) = all_module_schemes.get(&mod_name) {
                for (fn_name, scheme) in dep_schemes {
                    if unqualified.includes(fn_name) {
                        std_imported_schemes.insert(fn_name.clone(), scheme.clone());
                    }
                    std_imported_schemes.insert(format!("{mod_name}/{fn_name}"), scheme.clone());
                }
            }
        }

        let std_module_exports: HashMap<String, Vec<String>> = std_mods
            .iter()
            .map(|(u, _, src)| (u.clone(), opalc::exported_names(src)))
            .collect();

        let schemes = opalc::infer_module_exports(
            user_name,
            source,
            std_imports,
            &std_module_exports,
            &std_imported_type_decls,
            &std_imported_schemes,
        );
        all_module_schemes.insert(user_name.clone(), schemes);
    }

    // Phase 2: compile each user file with its resolved import map
    let mut erl_paths: Vec<PathBuf> = Vec::new();
    let mut had_error = false;

    for (module_name, source) in &module_sources {
        let mut imports: HashMap<String, String> = HashMap::new();
        let mut imported_schemes: opalc::typecheck::TypeEnv = HashMap::new();

        for (_, mod_name, unqualified) in opalc::used_modules(source) {
            let erlang_name = std_mods
                .iter()
                .find(|(user, _, _)| user == &mod_name)
                .map(|(_, erl, _)| erl.clone())
                .unwrap_or_else(|| mod_name.clone());

            if let Some(exports) = module_exports.get(&mod_name) {
                for fn_name in exports {
                    if unqualified.includes(fn_name) {
                        imports.insert(fn_name.clone(), erlang_name.clone());
                    }
                }
            }

            if let Some(mod_schemes) = all_module_schemes.get(&mod_name) {
                for (fn_name, scheme) in mod_schemes {
                    if unqualified.includes(fn_name) {
                        imported_schemes.insert(fn_name.clone(), scheme.clone());
                    }
                    imported_schemes.insert(format!("{mod_name}/{fn_name}"), scheme.clone());
                }
            }
        }

        // Inject qualified-name schemes for ALL known modules (handles transitive re-exports)
        for (user_name, schemes) in &all_module_schemes {
            for (fn_name, scheme) in schemes {
                let qualified = format!("{user_name}/{fn_name}");
                imported_schemes
                    .entry(qualified)
                    .or_insert_with(|| scheme.clone());
            }
        }

        let module_aliases: HashMap<String, String> = std_mods
            .iter()
            .map(|(user, erlang, _)| (user.clone(), erlang.clone()))
            .collect();

        // Type decls (constructors, field accessors) come into scope only for
        // modules the user explicitly names — either via `(use mod)` or by
        // writing a qualified call `mod/fn`.  No transitive propagation: if you
        // want `Some`/`None` you write `(use std/option)`; if you want
        // `TakeResult` field access you write `(use std/map)` or call `map/take`.
        let mut referenced_modules: std::collections::HashSet<String> = opalc::used_modules(source)
            .into_iter()
            .map(|(_, mod_name, _)| mod_name)
            .collect();
        for tok in opalc::lexer::Lexer::new(source).lex() {
            if let opalc::lexer::TokenKind::QualifiedIdent((module, _)) = tok.kind {
                referenced_modules.insert(module);
            }
        }
        let imported_type_decls: Vec<opalc::ast::TypeDecl> = referenced_modules
            .iter()
            .flat_map(|mod_name| module_type_decls.get(mod_name).cloned().unwrap_or_default())
            .collect();

        match opalc::compile_with_imports(
            module_name,
            source,
            &format!("{module_name}.opal"),
            imports,
            &module_exports,
            module_aliases,
            &imported_type_decls,
            &imported_schemes,
        ) {
            Some(erl_src) => {
                let erl_path = erl_dir.join(format!("{module_name}.erl"));
                std::fs::write(&erl_path, erl_src).expect("could not write .erl");
                erl_paths.push(erl_path);
            }
            None => {
                had_error = true;
            }
        }
    }

    if had_error {
        std::process::exit(1);
    }

    // Compile only std modules that are actually used
    let used_std_names: std::collections::HashSet<String> = module_sources
        .iter()
        .flat_map(|(_, src)| opalc::used_modules(src))
        .map(|(_, m, _)| m)
        .collect();

    let std_module_exports: HashMap<String, Vec<String>> = std_mods
        .iter()
        .map(|(user_name, _, source)| (user_name.clone(), opalc::exported_names(source)))
        .collect();

    // Also compile std sub-modules referenced via qualified idents (e.g. `io/println`)
    let std_sub_names: std::collections::HashSet<&str> =
        std_mods.iter().map(|(u, _, _)| u.as_str()).collect();
    let mut expanded_std_names = used_std_names.clone();
    for (_, src) in &module_sources {
        for tok in opalc::lexer::Lexer::new(src).lex() {
            if let opalc::lexer::TokenKind::QualifiedIdent((module, _)) = tok.kind
                && std_sub_names.contains(module.as_str())
            {
                expanded_std_names.insert(module);
            }
        }
    }

    for (user_name, erlang_name, source) in &std_mods {
        if !expanded_std_names.contains(user_name.as_str()) {
            continue;
        }

        let mut std_imports: HashMap<String, String> = HashMap::new();
        let mut std_imported_schemes: opalc::typecheck::TypeEnv = HashMap::new();

        for (_, mod_name, unqualified) in opalc::used_modules(source) {
            let erl_name = std_aliases
                .get(&mod_name)
                .cloned()
                .unwrap_or_else(|| mod_name.clone());
            if let Some(exports) = std_module_exports.get(&mod_name) {
                for fn_name in exports {
                    if unqualified.includes(fn_name) {
                        std_imports.insert(fn_name.clone(), erl_name.clone());
                    }
                }
            }
            if let Some(dep_schemes) = all_module_schemes.get(&mod_name) {
                for (fn_name, scheme) in dep_schemes {
                    if unqualified.includes(fn_name) {
                        std_imported_schemes.insert(fn_name.clone(), scheme.clone());
                    }
                    std_imported_schemes.insert(format!("{mod_name}/{fn_name}"), scheme.clone());
                }
            }
        }

        let std_imported_type_decls: Vec<opalc::ast::TypeDecl> = opalc::used_modules(source)
            .into_iter()
            .flat_map(|(_, mod_name, _)| {
                module_type_decls
                    .get(&mod_name)
                    .cloned()
                    .unwrap_or_default()
            })
            .collect();

        match opalc::compile_with_imports(
            erlang_name,
            source,
            &format!("{erlang_name}.opal"),
            std_imports,
            &std_module_exports,
            std_aliases.clone(),
            &std_imported_type_decls,
            &std_imported_schemes,
        ) {
            Some(erl_src) => {
                let erl_path = erl_dir.join(format!("{erlang_name}.erl"));
                std::fs::write(&erl_path, erl_src).expect("could not write .erl");
                erl_paths.push(erl_path);
            }
            None => std::process::exit(1),
        }
    }

    // Copy any hand-written .erl files from opal-std/src/ into the build dir.
    // These are embedded alongside the .opal sources via include_dir! and are
    // written verbatim — useful for helpers that are awkward to express in Opal
    // (e.g. functions that return Erlang atoms like `nomatch`).
    for file in STD_DIR.files() {
        if file.path().extension().and_then(|e| e.to_str()) == Some("erl") {
            let file_name = file.path().file_name().unwrap();
            let dest = erl_dir.join(file_name);
            std::fs::write(&dest, file.contents()).expect("could not write std .erl file");
            erl_paths.push(dest);
        }
    }

    Ok(ErlSources {
        erl_paths,
        manifest,
        project_type,
        module_exports,
        module_type_decls,
        all_module_schemes,
        std_mods,
        std_aliases,
    })
}

pub(crate) fn build(project_dir: &Path, run: bool) -> eyre::Result<()> {
    let build_dir = project_dir.join(TARGET_DIR).join(DEBUG_BUILD_DIR);
    std::fs::create_dir_all(&build_dir)
        .context(format!("could not create {DEBUG_BUILD_DIR} dir"))?;
    gitignore::write_gitignore(project_dir.into())?;

    let ErlSources {
        erl_paths,
        manifest,
        project_type,
        ..
    } = generate_erl_sources(project_dir, &build_dir)?;

    if matches!(project_type, ProjectType::Lib) && run {
        return Err(eyre::eyre!("loupe cannot run a library project"));
    }

    // Run erlc on all .erl files at once
    let erlc = Command::new("erlc")
        .arg("-o")
        .arg(&build_dir)
        .args(&erl_paths)
        .output()
        .unwrap_or_else(|e| {
            eprintln!("error: could not run erlc: {e}");
            std::process::exit(1);
        });

    if !erlc.status.success() {
        eprintln!("erlc failed:");
        eprintln!("{}", String::from_utf8_lossy(&erlc.stderr));
        std::process::exit(1);
    }
    if run {
        let status = Command::new("erl")
            .arg("-noinput")
            .arg("-pa")
            .arg(&build_dir)
            .arg("-eval")
            .arg("main:main(unit), init:stop().")
            .status()
            .unwrap_or_else(|e| {
                eprintln!("error: could not run erl: {e}");
                std::process::exit(1);
            });

        std::process::exit(status.code().unwrap_or(1));
    } else {
        println!(
            "built {} ({} module(s))",
            manifest.package.name,
            erl_paths.len()
        );
    }

    Ok(())
}

fn verify_project_type(source_files: &[PathBuf]) -> Option<ProjectType> {
    let entry_point = OsStr::from(BIN_ENTRY_POINT);
    let lib_root = OsStr::from(LIB_ROOT);
    for file in source_files {
        if file.file_name() == Some(&entry_point) {
            return Some(ProjectType::Bin);
        } else if file.file_name() == Some(&lib_root) {
            return Some(ProjectType::Lib);
        }
    }
    None
}
