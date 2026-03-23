use std::{
    path::{Path, PathBuf},
    process::Command,
};

use clap::builder::styling::{AnsiColor, Color, Style};
use walkdir::WalkDir;

const REQUIRED_OTP_MAJOR: u32 = 28;

fn check_dep(dep_name: &str) -> Option<bool> {
    Command::new("which")
        .args([dep_name])
        .output()
        .ok()
        .map(|ouput| !ouput.stdout.is_empty())
}

pub(crate) fn verify_erlc_installed() -> Result<(), eyre::Report> {
    let is_installed = check_dep("erlc").unwrap_or(false);
    if !is_installed {
        Err(eyre::eyre!(
            "erlc is not installed, to compile and run mond code please install it"
        ))
    } else {
        verify_otp_28()?;
        Ok(())
    }
}

pub(crate) fn verify_otp_28() -> Result<(), eyre::Report> {
    let release = otp_release()?;
    let major = parse_otp_major(&release)
        .ok_or_else(|| eyre::eyre!("failed to parse OTP release `{release}`"))?;

    if major != REQUIRED_OTP_MAJOR {
        return Err(eyre::eyre!(
            "unsupported Erlang/OTP version `{release}`; bahn requires OTP {REQUIRED_OTP_MAJOR}"
        ));
    }

    Ok(())
}

fn otp_release() -> Result<String, eyre::Report> {
    let output = Command::new("erl")
        .args([
            "-noshell",
            "-eval",
            "io:format(\"~s\", [erlang:system_info(otp_release)]), halt().",
        ])
        .output()
        .map_err(|err| eyre::eyre!("failed to run `erl`: {err}"))?;

    if !output.status.success() {
        return Err(eyre::eyre!(
            "`erl` failed while checking OTP version:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let release = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if release.is_empty() {
        return Err(eyre::eyre!("`erl` returned an empty OTP version"));
    }
    Ok(release)
}

fn parse_otp_major(release: &str) -> Option<u32> {
    let digits: String = release
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse::<u32>().ok()
    }
}

pub(crate) fn verify_rebar3_installed() -> Result<(), eyre::Report> {
    let is_installed = check_dep("rebar3").unwrap_or(false);
    if !is_installed {
        Err(eyre::eyre!(
            "rebar3 is not installed, to create a bahn release please install it"
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn find_mond_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("mond"))
        .map(|e| e.path().to_path_buf())
        .collect();
    files.sort();
    files
}
pub(crate) fn get_styles() -> clap::builder::Styles {
    clap::builder::Styles::styled()
        .usage(
            Style::new()
                .bold()
                .underline()
                .fg_color(Some(Color::Ansi(AnsiColor::Yellow))),
        )
        .header(
            Style::new()
                .bold()
                .underline()
                .fg_color(Some(Color::Ansi(AnsiColor::Yellow))),
        )
        .literal(Style::new().fg_color(Some(Color::Ansi(AnsiColor::Green))))
        .invalid(
            Style::new()
                .bold()
                .fg_color(Some(Color::Ansi(AnsiColor::Red))),
        )
        .error(
            Style::new()
                .bold()
                .fg_color(Some(Color::Ansi(AnsiColor::Red))),
        )
        .valid(
            Style::new()
                .bold()
                .underline()
                .fg_color(Some(Color::Ansi(AnsiColor::Green))),
        )
        .placeholder(Style::new().fg_color(Some(Color::Ansi(AnsiColor::White))))
}

#[cfg(test)]
mod tests {
    use crate::utils::{check_dep, parse_otp_major};

    #[test]
    fn test_for_made_up_dep() {
        let is_present = check_dep("florp");
        assert_eq!(is_present, Some(false));
    }

    #[test]
    fn test_for_made_present_dep() {
        let is_present = check_dep("pwd");
        assert_eq!(is_present, Some(true));
    }

    #[test]
    fn parse_otp_major_handles_common_release_formats() {
        assert_eq!(parse_otp_major("28"), Some(28));
        assert_eq!(parse_otp_major("28.0.2"), Some(28));
        assert_eq!(parse_otp_major("25"), Some(25));
        assert_eq!(parse_otp_major("otp-28"), None);
        assert_eq!(parse_otp_major(""), None);
    }
}
