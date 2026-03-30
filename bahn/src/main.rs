mod build;
mod clean;
mod compile_flow;
mod deps;
mod format;
mod gitignore;
mod manifest;
mod new;
mod release;
mod test;
mod ui;
mod utils;

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

pub(crate) const STD_GIT_URL: &str = "git@github.com:benjaminjellis/mond-std.git";
pub(crate) const STD_GIT_TAG: &str = "0.0.9";
pub(crate) const MANIFEST_NAME: &str = "bahn.toml";
pub(crate) const LOCKFILE_NAME: &str = "bahn.lock";
pub(crate) const TARGET_DIR: &str = "target";
pub(crate) const DEBUG_BUILD_DIR: &str = "debug";
pub(crate) const TEST_BUILD_DIR: &str = "tests";
pub(crate) const SOURCE_DIR: &str = "src";
pub(crate) const TEST_DIR: &str = "tests";
pub(crate) const BIN_ENTRY_POINT: &str = "main.mond";
pub(crate) const LIB_ROOT: &str = "lib.mond";
/// banh and mond version
const VERSION: &str = env!("CARGO_PKG_VERSION");

// TODO: cargo calls them crates, what are they for mond?
pub(crate) enum ProjectType {
    Bin,
    Lib,
}

#[derive(Parser)]
#[command(name = "bahn")]
#[command(version = VERSION)]
#[command(styles= utils::get_styles())]
#[command(about = "the build tool for the mond programming language")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the current binary project
    Run,
    /// Test the current project
    Test,
    /// Mange the current project's dependencies
    Deps {
        #[arg(long)]
        update: bool,
    },
    /// Run the LSP
    Lsp,
    /// Format the current project or provided path
    Format {
        #[arg(long)]
        path: Option<PathBuf>,
        #[arg(long)]
        check: bool,
    },
    /// Create a new project in the provided directory
    New {
        name: String,
        #[arg(long)]
        lib: bool,
    },
    /// Build the current project
    Build,
    /// Create a release for the current project
    Release,
    /// Clean the /target directory for the current project
    Clean,
}

fn main() -> eyre::Result<()> {
    let cli = Cli::parse();

    let root = Path::new(".");
    match cli.command {
        Commands::Build => {
            utils::run_async(build::build(root, false))?;
        }
        Commands::Format { path, check } => {
            if let Some(path) = path {
                if path.is_file() {
                    format::format_fie(&path)?;
                } else {
                    format::format_dir(&path, check)?;
                }
            } else {
                format::format_project_dir(root, check)?;
            }
        }
        Commands::New { name, lib } => {
            new::create_new_project(&name, root, lib)?;
            let kind = if lib { "library" } else { "binary" };
            ui::success(&format!("created {kind} project `{name}`"));
        }
        Commands::Run => {
            utils::run_async(build::build(root, true))?;
        }
        Commands::Test => {
            utils::run_async(test::test(root))?;
        }
        Commands::Deps { update } => {
            if update {
                let updated = deps::update_dependencies(root)?;
                if updated.is_empty() {
                    ui::success("no dependencies to update");
                } else {
                    ui::success(&format!("updated {}", updated.join(", ")));
                }
            } else {
                ui::info(
                    "dependency cache is offline by default; run `bahn deps --update` to refresh",
                );
            }
        }
        Commands::Lsp => {
            utils::run_async(async {
                mond_lsp::serve(tokio::io::stdin(), tokio::io::stdout()).await;
                Ok(())
            })?;
        }
        Commands::Release => {
            utils::run_async(release::release(root))?;
        }
        Commands::Clean => {
            clean::clean(root)?;
        }
    }
    Ok(())
}
