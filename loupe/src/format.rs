use eyre::Context;

use crate::{MANIFEST_NAME, SOURCE_DIR, utils::find_opal_files};
use std::path::{Path, PathBuf};

const LINE_WIDTH: usize = 80;

pub(crate) fn format_fie(source_file_path: &PathBuf) -> eyre::Result<()> {
    let source_file = std::fs::read_to_string(source_file_path)
        .context(format!("failed to read {source_file_path:?}"))?;
    let formatted_file = opal_format::format(&source_file, LINE_WIDTH);

    std::fs::write(source_file_path, &formatted_file).context(format!(
        "failed to write formatted file to {source_file_path:?}"
    ))?;

    Ok(())
}

pub(crate) fn format_dir(dir: &Path) -> eyre::Result<()> {
    let opal_files = find_opal_files(dir);

    for source_file_path in opal_files {
        let source_file = std::fs::read_to_string(&source_file_path).context(format!(
            "failed to read {MANIFEST_NAME} at {source_file_path:?}"
        ))?;
        let formatted_file = opal_format::format(&source_file, LINE_WIDTH);

        std::fs::write(&source_file_path, &formatted_file).context(format!(
            "failed to read {MANIFEST_NAME} at {source_file_path:?}"
        ))?;
    }

    Ok(())
}

pub(crate) fn format_project_dir(project_dir: &Path) -> eyre::Result<()> {
    let src_dir = project_dir.join(SOURCE_DIR);
    format_dir(&src_dir)?;
    Ok(())
}
