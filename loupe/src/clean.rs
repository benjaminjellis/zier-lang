use std::path::Path;

use eyre::Context;

use crate::TARGET_DIR;

pub(crate) fn clean(project_dir: &Path) -> eyre::Result<()> {
    let target = project_dir.join(TARGET_DIR);
    if target.exists() {
        std::fs::remove_dir_all(&target).context("could not remove target dir")?;
    }
    println!("cleaned");
    Ok(())
}
