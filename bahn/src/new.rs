use crate::{BIN_ENTRY_POINT, LIB_ROOT};
use std::path::Path;

use eyre::Context;

use crate::{MANIFEST_NAME, manifest};

pub(crate) fn create_new_project(name: String, root: &Path, lib: bool) -> eyre::Result<()> {
    let project_dir = root.join(&name);

    // create dir
    std::fs::create_dir_all(&project_dir)
        .context(format!("could not create {project_dir:?} dir"))?;

    // create bahn.toml with default
    let manliest_path = project_dir.join(MANIFEST_NAME);

    let manifest = manifest::create_new_manifest(name.clone());
    manifest::write_manifest(&manifest, &manliest_path)?;

    let src_dir = project_dir.join("src");
    std::fs::create_dir_all(&src_dir)
        .context(format!("could not create source dir {src_dir:?}"))?;
    let tests_dir = project_dir.join("tests");
    std::fs::create_dir_all(&tests_dir)
        .context(format!("could not create tests dir {tests_dir:?}"))?;

    if lib {
        let lib_root = src_dir.join(LIB_ROOT);
        std::fs::write(lib_root, MOND_LIB)?;
    } else {
        let bin_root = src_dir.join(BIN_ENTRY_POINT);
        std::fs::write(bin_root, MOND_HELLO_WORLD)?;
    }

    Ok(())
}

const MOND_HELLO_WORLD: &str = r#"(use std/io)

(let main {}
  (io/println "hello world"))"#;

const MOND_LIB: &str = r#""#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_root() -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!("mond-new-test-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn create_new_lib_project_creates_tests_dir() {
        let root = unique_temp_root();
        std::fs::create_dir_all(&root).expect("create temp root");

        let project_name = "my_lib".to_string();
        create_new_project(project_name.clone(), &root, true).expect("create project");

        let project_dir = root.join(project_name);
        assert!(project_dir.join("tests").is_dir());
        assert!(project_dir.join("src").join(LIB_ROOT).is_file());

        std::fs::remove_dir_all(&root).expect("cleanup temp root");
    }

    #[test]
    fn create_new_bin_project_creates_tests_dir() {
        let root = unique_temp_root();
        std::fs::create_dir_all(&root).expect("create temp root");

        let project_name = "my_bin".to_string();
        create_new_project(project_name.clone(), &root, false).expect("create project");

        let project_dir = root.join(project_name);
        assert!(project_dir.join("tests").is_dir());
        assert!(project_dir.join("src").join(BIN_ENTRY_POINT).is_file());

        std::fs::remove_dir_all(&root).expect("cleanup temp root");
    }
}
