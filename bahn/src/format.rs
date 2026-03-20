use eyre::Context;

use crate::{MANIFEST_NAME, SOURCE_DIR, TEST_DIR, utils::find_mond_files};
use std::path::{Path, PathBuf};

const LINE_WIDTH: usize = 80;

pub(crate) fn format_fie(source_file_path: &PathBuf) -> eyre::Result<()> {
    let source_file = std::fs::read_to_string(source_file_path)
        .context(format!("failed to read {source_file_path:?}"))?;
    let formatted_file = mond_format::format(&source_file, LINE_WIDTH);

    std::fs::write(source_file_path, &formatted_file).context(format!(
        "failed to write formatted file to {source_file_path:?}"
    ))?;

    Ok(())
}

pub(crate) fn format_dir(dir: &Path, check: bool) -> eyre::Result<Vec<String>> {
    let mond_files = find_mond_files(dir);
    let mut check_file_errors = vec![];

    for source_file_path in mond_files {
        let source_file = std::fs::read_to_string(&source_file_path).context(format!(
            "failed to read {MANIFEST_NAME} at {source_file_path:?}"
        ))?;

        let formatted_file = mond_format::format(&source_file, LINE_WIDTH);
        if source_file != formatted_file && check {
            check_file_errors.push(format!("{source_file_path:?} not formatted correctly"));
            continue;
        }

        std::fs::write(&source_file_path, &formatted_file).context(format!(
            "failed to read {MANIFEST_NAME} at {source_file_path:?}"
        ))?;
    }

    Ok(check_file_errors)
}

pub(crate) fn format_project_dir(project_dir: &Path, check: bool) -> eyre::Result<()> {
    let src_dir = project_dir.join(SOURCE_DIR);
    let mut source_errors = format_dir(&src_dir, check)?;

    let tests_dir = project_dir.join(TEST_DIR);
    let test_errors = format_dir(&tests_dir, check)?;
    source_errors.extend_from_slice(&test_errors);

    if !source_errors.is_empty() {
        return Err(eyre::eyre!("{source_errors:?}"));
    }

    Ok(())
}
