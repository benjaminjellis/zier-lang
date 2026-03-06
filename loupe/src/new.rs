use crate::{BIN_ENTRY_POINT, LIB_ROOT};
use std::path::Path;

use eyre::Context;

use crate::{MANIFEST_NAME, manifest};

pub(crate) fn create_new_project(name: String, root: &Path, lib: bool) -> eyre::Result<()> {
    let project_dir = root.join(&name);

    // create dir
    std::fs::create_dir_all(&project_dir)
        .context(format!("could not create {project_dir:?} dir"))?;

    // create loupe.toml with default
    let manliest_path = project_dir.join(MANIFEST_NAME);

    let manifest = manifest::create_new_manifest(name.clone());
    manifest::write_manifest(&manifest, &manliest_path)?;

    let src_dir = project_dir.join("src");
    std::fs::create_dir_all(&src_dir)
        .context(format!("could not create source dir {src_dir:?}"))?;

    if lib {
        let lib_root = src_dir.join(LIB_ROOT);
        std::fs::write(lib_root, OPAL_LIB)?;
    } else {
        let bin_root = src_dir.join(BIN_ENTRY_POINT);
        std::fs::write(bin_root, OPAL_HELLO_WORLD)?;
    }

    Ok(())
}

const OPAL_HELLO_WORLD: &str = r#"(use std)

(let main {}
  (io/println "hello world~n"))
"#;

const OPAL_LIB: &str = r#"(pub extern let println ~ (String -> Unit) io/format)"#;
