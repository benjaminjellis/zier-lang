use std::{path::Path, process::Command};

use eyre::Context;

use crate::{
    ProjectType, TARGET_DIR,
    build::{ErlSources, generate_erl_sources},
    manifest, ui,
};

fn sanitize_erlang_component(name: &str) -> String {
    let mut sanitized = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            sanitized.push(ch.to_ascii_lowercase());
        } else {
            sanitized.push('_');
        }
    }
    if sanitized.is_empty() || sanitized.starts_with(|c: char| c.is_ascii_digit()) {
        sanitized.insert(0, '_');
    }
    sanitized
}

pub(crate) fn release(project_dir: &Path) -> eyre::Result<()> {
    // Stage the rebar3 project under target/release/
    // Wipe first to prevent stale .erl files from previous builds causing conflicts.
    let staging = project_dir.join(TARGET_DIR).join("release");
    if staging.exists() {
        std::fs::remove_dir_all(&staging).context("could not clean release staging dir")?;
    }
    let src_dir = staging.join("src");
    std::fs::create_dir_all(&src_dir).context("could not create release staging dir")?;

    let manifest = manifest::read_manifest(project_dir.into())?;

    let ErlSources {
        erl_paths: _,
        manifest,
        project_type,
        module_aliases,
        ..
    } = generate_erl_sources(manifest, project_dir, &src_dir)?;

    if matches!(project_type, ProjectType::Lib) {
        return Err(eyre::eyre!("bahn cannot release a library project"));
    }

    // Sanitise project name to a valid Erlang atom component.
    let app_name = sanitize_erlang_component(&manifest.package.name);
    let version = manifest.package.version.to_string();

    let main_module = module_aliases
        .get("main")
        .map(String::as_str)
        .unwrap_or("main");

    // Generate the escript entry-point shim: <app_name>:main/1 calls the compiled main module.
    let shim_path = src_dir.join(format!("{app_name}.erl"));
    if shim_path.exists() {
        return Err(eyre::eyre!(
            "Erlang module name collision: release shim `{app_name}` would overwrite {}",
            shim_path.display()
        ));
    }
    let shim = format!(
        "-module({app_name}).\n\
         -export([main/1]).\n\
         \n\
         main(_Args) ->\n\
             {main_module}:main(unit).\n"
    );
    std::fs::write(&shim_path, shim).context("could not write escript shim")?;

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

    crate::utils::verify_otp_28()?;
    crate::utils::verify_rebar3_installed()?;

    // Run rebar3 escriptize
    let rebar3 = Command::new("rebar3")
        .arg("escriptize")
        .current_dir(&staging)
        .output()
        .context("could not run rebar3 — is it installed?")?;

    if !rebar3.status.success() {
        ui::error("rebar3 failed:");
        eprintln!("{}", String::from_utf8_lossy(&rebar3.stderr));
        std::process::exit(1);
    }

    let bin = staging
        .join("_build")
        .join("default")
        .join("bin")
        .join(&app_name);

    ui::success(&format!(
        "released {} to {}",
        manifest.package.name,
        bin.display()
    ));

    Ok(())
}
