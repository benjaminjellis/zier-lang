mod build;
mod gitignore;
mod manifest;
mod new;

use std::path::Path;

use clap::{Parser, Subcommand};

pub(crate) const MANIFEST_NAME: &str = "loupe.toml";
pub(crate) const TARGET_DIR: &str = "target";
pub(crate) const SOURCE_DIR: &str = "src";
pub(crate) const BIN_ENTRY_POINT: &str = "main.opal";
pub(crate) const LIB_ROOT: &str = "lib.opal";

// TODO: cargo calls them crates, what are they for opal?
pub(crate) enum ProjectType {
    Bin,
    Lib,
}

#[derive(Parser)]
#[command(name = "loupe")]
#[command(version = "0.1")]
#[command(about = "build tool for the opal programming language")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Run,
    New {
        name: String,
        #[arg(long)]
        lib: bool,
    },
    Build,
}

fn main() -> eyre::Result<()> {
    let cli = Cli::parse();

    let root = Path::new(".");
    match cli.command {
        Commands::Build => {
            build::build(root, false)?;
        }
        Commands::New { name, lib } => {
            new::create_new_project(name, root, lib)?;
        }
        Commands::Run => {
            build::build(root, true)?;
        }
    }
    Ok(())
}
