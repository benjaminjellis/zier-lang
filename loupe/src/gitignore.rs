use std::path::PathBuf;

use eyre::Context;

const GITIGNORE_CONTENTS: &str = "# Compiled output
/target

# Dependency lock file for libraries (optional)
# loupe.lock";

pub(crate) fn write_gitignore(root: PathBuf) -> eyre::Result<()> {
    let path = root.join(".gitignore");
    std::fs::write(path, GITIGNORE_CONTENTS).context("failed to write {MANIFEST_NAME} to disk")?;
    Ok(())
}
