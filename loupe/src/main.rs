mod build;
mod clean;
mod format;
mod gitignore;
mod manifest;
mod new;
mod release;
mod utils;

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

pub(crate) const MANIFEST_NAME: &str = "loupe.toml";
pub(crate) const TARGET_DIR: &str = "target";
pub(crate) const DEBUG_DIR: &str = "target/debug";
pub(crate) const SOURCE_DIR: &str = "src";
pub(crate) const BIN_ENTRY_POINT: &str = "main.opal";
pub(crate) const LIB_ROOT: &str = "lib.opal";
const VERSION: &str = env!("CARGO_PKG_VERSION");

// TODO: cargo calls them crates, what are they for opal?
pub(crate) enum ProjectType {
    Bin,
    Lib,
}

#[derive(Parser)]
#[command(name = "loupe")]
#[command(version = VERSION)]
#[command(about = "build tool for the opal programming language")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Run,
    Format {
        /// File to format (formats all source files if omitted)
        #[arg(long)]
        path: Option<PathBuf>,
    },
    New {
        name: String,
        #[arg(long)]
        lib: bool,
    },
    Build,
    Release,
    Clean,
}

fn main() -> eyre::Result<()> {
    let cli = Cli::parse();

    let root = Path::new(".");
    match cli.command {
        Commands::Build => {
            build::build(root, false)?;
        }
        Commands::Format { path } => {
            if let Some(path) = path {
                if path.is_file() {
                    format::format_fie(&path)?;
                } else {
                    format::format_dir(&path)?;
                }
            } else {
                format::format_project_dir(root)?;
            }
        }
        Commands::New { name, lib } => {
            new::create_new_project(name, root, lib)?;
        }
        Commands::Run => {
            build::build(root, true)?;
        }
        Commands::Release => {
            release::release(root)?;
        }
        Commands::Clean => {
            clean::clean(root)?;
        }
    }
    Ok(())
}
