use std::{path::Path, process::Command};

use eyre::Context;

use crate::{ProjectType, TARGET_DIR};
use crate::build::{ErlSources, generate_erl_sources};

pub(crate) fn release(project_dir: &Path) -> eyre::Result<()> {
    // Stage the rebar3 project under target/release/
    // Wipe first to prevent stale .erl files from previous builds causing conflicts.
    let staging = project_dir.join(TARGET_DIR).join("release");
    if staging.exists() {
        std::fs::remove_dir_all(&staging).context("could not clean release staging dir")?;
    }
    let src_dir = staging.join("src");
    std::fs::create_dir_all(&src_dir).context("could not create release staging dir")?;

    let ErlSources { erl_paths: _, manifest, project_type } =
        generate_erl_sources(project_dir, &src_dir)?;

    if matches!(project_type, ProjectType::Lib) {
        return Err(eyre::eyre!("loupe cannot release a library project"));
    }

    // Sanitise project name to a valid Erlang atom (replace hyphens with underscores)
    let app_name = manifest.package.name.replace('-', "_");
    let version = manifest.package.version.to_string();

    // Generate the escript entry-point shim: <app_name>:main/1 calls our main:main(unit)
    let shim = format!(
        "-module({app_name}).\n\
         -export([main/1]).\n\
         \n\
         main(_Args) ->\n\
             main:main(unit).\n"
    );
    std::fs::write(src_dir.join(format!("{app_name}.erl")), shim)
        .context("could not write escript shim")?;

    // Generate <app_name>.app.src
    let app_src = format!(
        "{{application, {app_name}, [\n\
             {{description, \"\"}},\n\
             {{vsn, \"{version}\"}},\n\
             {{modules, []}},\n\
             {{registered, []}},\n\
             {{applications, [kernel, stdlib]}},\n\
             {{env, []}}\n\
         ]}}.\n"
    );
    std::fs::write(src_dir.join(format!("{app_name}.app.src")), app_src)
        .context("could not write app.src")?;

    // Generate rebar.config
    let rebar_config = format!(
        "{{erl_opts, [no_debug_info]}}.\n\
         {{deps, []}}.\n\
         \n\
         {{escript_main_app, {app_name}}}.\n\
         {{escript_name, \"{app_name}\"}}.\n\
         {{escript_emu_args, \"%%! -noinput\\n\"}}.\n"
    );
    std::fs::write(staging.join("rebar.config"), rebar_config)
        .context("could not write rebar.config")?;

    // Run rebar3 escriptize
    let rebar3 = Command::new("rebar3")
        .arg("escriptize")
        .current_dir(&staging)
        .output()
        .context("could not run rebar3 — is it installed?")?;

    if !rebar3.status.success() {
        eprintln!("rebar3 failed:");
        eprintln!("{}", String::from_utf8_lossy(&rebar3.stderr));
        std::process::exit(1);
    }

    let bin = staging
        .join("_build")
        .join("default")
        .join("bin")
        .join(&app_name);

    println!("released {} to {}", manifest.package.name, bin.display());

    Ok(())
}
