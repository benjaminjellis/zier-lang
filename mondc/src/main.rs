use std::path::{Path, PathBuf};

use clap::{Parser, ValueEnum};
use codespan_reporting::term::termcolor::ColorChoice;
use mondc::{CompilePipeline, ModuleInput, PassContext};

#[derive(Copy, Clone, Debug, ValueEnum)]
enum ColorMode {
    Auto,
    Always,
    Never,
}

impl ColorMode {
    fn as_color_choice(self) -> ColorChoice {
        match self {
            Self::Auto => ColorChoice::Auto,
            Self::Always => ColorChoice::Always,
            Self::Never => ColorChoice::Never,
        }
    }
}

#[derive(Parser, Debug)]
#[command(name = "mondc")]
#[command(about = "the mond compiler (single-module mode)")]
#[command(version = env!("CARGO_PKG_VERSION"))]
struct Cli {
    /// Input .mond source file
    input: PathBuf,

    /// Output .erl path (default: <input-stem>.erl in current directory)
    #[arg(short = 'o', long)]
    output: Option<PathBuf>,

    /// Override generated module name (default: file stem)
    #[arg(long)]
    module_name: Option<String>,

    /// Disable warning diagnostics
    #[arg(long)]
    no_warnings: bool,

    /// Diagnostics color mode
    #[arg(long, value_enum, default_value_t = ColorMode::Auto)]
    color: ColorMode,
}

fn module_name_from_input(path: &Path) -> eyre::Result<String> {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| eyre::eyre!("could not infer module name from input path"))?;
    Ok(stem.to_string())
}

fn default_output_path(module_name: &str) -> PathBuf {
    PathBuf::from(format!("{module_name}.erl"))
}

fn main() -> eyre::Result<()> {
    let cli = Cli::parse();

    let module_name = match cli.module_name {
        Some(name) => name,
        None => module_name_from_input(&cli.input)?,
    };

    let source = std::fs::read_to_string(&cli.input)
        .map_err(|err| eyre::eyre!("could not read {}: {err}", cli.input.display()))?;

    let source_label = cli.input.to_string_lossy().to_string();
    let analysis = mondc::build_project_analysis(&[], &[])
        .map_err(|err| eyre::eyre!("could not initialize compile pipeline: {err}"))?;
    let visible_exports = std::collections::HashMap::new();
    let pipeline = CompilePipeline::new(PassContext {
        visible_exports: &visible_exports,
        analysis: &analysis,
    });
    let report = pipeline.compile_module_report(ModuleInput {
        output_module_name: &module_name,
        source: &source,
        source_path: &source_label,
    });

    mondc::session::emit_compile_report_with_color(
        &report,
        !cli.no_warnings,
        cli.color.as_color_choice(),
    );

    if report.has_errors() {
        return Err(eyre::eyre!("compilation failed"));
    }

    let erl_output = report
        .output
        .ok_or_else(|| eyre::eyre!("compilation produced no output"))?;
    let output_path = cli
        .output
        .unwrap_or_else(|| default_output_path(&module_name));

    std::fs::write(&output_path, erl_output)
        .map_err(|err| eyre::eyre!("could not write {}: {err}", output_path.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{default_output_path, module_name_from_input};
    use std::path::Path;

    #[test]
    fn module_name_from_input_uses_file_stem() {
        let name = module_name_from_input(Path::new("src/example.mond")).expect("module name");
        assert_eq!(name, "example");
    }

    #[test]
    fn default_output_path_uses_erl_extension() {
        let output = default_output_path("example");
        assert_eq!(output.to_string_lossy(), "example.erl");
    }
}
