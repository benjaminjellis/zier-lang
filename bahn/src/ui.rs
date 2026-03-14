use std::io::Write;

use codespan_reporting::term::termcolor::{
    Color, ColorChoice, ColorSpec, StandardStream, WriteColor,
};

pub(crate) fn diagnostic_color_choice() -> ColorChoice {
    ColorChoice::Auto
}

fn status(label: &str, message: &str, color: Color) {
    let mut stderr = StandardStream::stderr(diagnostic_color_choice());
    let mut spec = ColorSpec::new();
    spec.set_fg(Some(color)).set_bold(true);
    let _ = stderr.set_color(&spec);
    let _ = write!(&mut stderr, "{label}");
    let _ = stderr.reset();
    let _ = writeln!(&mut stderr, " {message}");
}

pub(crate) fn info(message: &str) {
    status("info:", message, Color::Cyan);
}

pub(crate) fn success(message: &str) {
    status("success:", message, Color::Green);
}

pub(crate) fn warn(message: &str) {
    status("warning:", message, Color::Yellow);
}

pub(crate) fn error(message: &str) {
    status("error:", message, Color::Red);
}
