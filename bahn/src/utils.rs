use std::{
    path::{Path, PathBuf},
    process::Command,
};

use walkdir::WalkDir;

fn check_dep(dep_name: &str) -> Option<bool> {
    Command::new("which")
        .args([dep_name])
        .output()
        .ok()
        .map(|ouput| !ouput.stdout.is_empty())
}

pub(crate) fn verify_erlc_installed() -> Result<(), eyre::Report> {
    let is_installed = check_dep("erlc").unwrap_or(false);
    if !is_installed {
        Err(eyre::eyre!(
            "erlc is not installed, to compile and run mond code please install it"
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn verify_rebar3_installed() -> Result<(), eyre::Report> {
    let is_installed = check_dep("rebar3").unwrap_or(false);
    if !is_installed {
        Err(eyre::eyre!(
            "rebar3 is not installed, to create a bahn release please install it"
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn find_mond_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("mond"))
        .map(|e| e.path().to_path_buf())
        .collect();
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use crate::utils::check_dep;

    #[test]
    fn test_for_made_up_dep() {
        let is_present = check_dep("florp");
        assert_eq!(is_present, Some(false));
    }

    #[test]
    fn test_for_made_present_dep() {
        let is_present = check_dep("pwd");
        assert_eq!(is_present, Some(true));
    }
}
