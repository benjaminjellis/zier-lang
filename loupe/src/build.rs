use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{BIN_ENTRY_POINT, LIB_ROOT, gitignore};
use clap::builder::OsStr;
use eyre::Context;

use crate::{ProjectType, SOURCE_DIR, TARGET_DIR, manifest};

// opal-std is embedded at compile time — std ships with loupe, no filesystem discovery needed.
const STD_LIB_SRC: &str = include_str!("../../opal-std/src/lib.opal");

/// All submodule sources embedded at compile time.
/// Add a new entry here whenever a new std module is created.
static STD_SUBMODULES: &[(&str, &str)] = &[
    // ("io", include_str!("../../opal-std/src/io.opal")),
];

/// Return (module_name, source) for:
///   - "std" itself → lib.opal
///   - each module pub-reexported by lib.opal
fn std_modules() -> Vec<(String, String)> {
    let mut result = Vec::new();
    result.push(("std".to_string(), STD_LIB_SRC.to_string()));
    for mod_name in opalc::pub_reexports(STD_LIB_SRC) {
        if let Some(src) = STD_SUBMODULES.iter().find(|(n, _)| *n == mod_name.as_str()) {
            result.push((mod_name, src.1.to_string()));
        }
    }
    result
}

pub(crate) fn build(project_dir: &Path, run: bool) -> eyre::Result<()> {
    // Load manifest
    let manifest = manifest::read_manifest(project_dir.into())?;

    // Find all .opal source files in src/
    let src_dir = project_dir.join(SOURCE_DIR);
    let opal_files = find_opal_files(&src_dir);

    if opal_files.is_empty() {
        return Err(eyre::eyre!("loupe found no .opal {}", src_dir.display()));
    }

    if let Some(project_type) = verify_project_type(&opal_files) {
        if matches!(project_type, ProjectType::Lib) && run {
            return Err(eyre::eyre!("loupe cannot run a library project"));
        }
    } else {
        return Err(eyre::eyre!(
            "loupe failed to find one of {BIN_ENTRY_POINT} or {LIB_ROOT}"
        ));
    }

    // Create build directory
    let build_dir = project_dir.join(TARGET_DIR);
    std::fs::create_dir_all(&build_dir).context(format!("could not create {TARGET_DIR} dir"))?;

    // create a .gitignore
    gitignore::write_gitignore(project_dir.into())?;

    // Phase 1: scan each module's source to collect its exported function names
    let mut module_exports: HashMap<String, Vec<String>> = HashMap::new();
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
        module_exports.insert(module_name.clone(), exports);
        module_sources.push((module_name, source));
    }

    // Phase 1b: seed module_exports with embedded std modules so the compiler's
    // `use` validation and import building treats them identically to local modules.
    let std_mods = std_modules();
    for (mod_name, source) in &std_mods {
        let exports = opalc::exported_names(source);
        module_exports.insert(mod_name.clone(), exports);
    }

    // Phase 2: compile each file with its resolved import map
    let mut erl_paths: Vec<PathBuf> = Vec::new();
    let mut had_error = false;

    for (module_name, source) in &module_sources {
        // Build imports: fn_name → module_name for each `use` in this file
        let mut imports: HashMap<String, String> = HashMap::new();
        for (_, mod_name) in opalc::used_modules(source) {
            if let Some(exports) = module_exports.get(&mod_name) {
                for fn_name in exports {
                    imports.insert(fn_name.clone(), mod_name.clone());
                }
            }
            // Unknown module — the compiler will emit a proper codespan diagnostic
        }

        match opalc::compile_with_imports(module_name, source, imports, &module_exports) {
            Some(erl_src) => {
                let erl_path = build_dir.join(format!("{module_name}.erl"));
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
        .map(|(_, m)| m)
        .collect();

    for (mod_name, source) in &std_mods {
        if !used_std_names.contains(mod_name.as_str()) {
            continue;
        }
        match opalc::compile(mod_name, source) {
            Some(erl_src) => {
                let erl_path = build_dir.join(format!("{mod_name}.erl"));
                std::fs::write(&erl_path, erl_src).expect("could not write .erl");
                erl_paths.push(erl_path);
            }
            None => std::process::exit(1),
        }
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
            .arg("-noshell")
            .arg("-pa")
            .arg(&build_dir)
            .arg("-eval")
            // Entry point is always main:main/1 (src/main.opal)
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

fn find_opal_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return files;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("opal") {
            files.push(path);
        }
    }
    files.sort(); // deterministic order
    files
}

fn verify_project_type(source_files: &Vec<PathBuf>) -> Option<ProjectType> {
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
