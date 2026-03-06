//! Opal source code formatter.
//!
//! Uses the Wadler-Lindig pretty-printing algorithm to format Opal source.
//!
//! # Example
//! ```
//! let src = "(let add {a b} (+ a b))";
//! let formatted = opal_format::format(src, 80);
//! ```

mod doc;
mod pretty;
#[cfg(test)]
mod tests;

/// Format Opal source code with the given line `width`.
///
/// Returns the formatted source, or the original source unchanged if parsing
/// fails (so the formatter is safe to call unconditionally).
pub fn format(source: &str, width: usize) -> String {
    let mut lowerer = opalc::lower::Lowerer::new();
    // Lex once — the full token stream includes Comment tokens.
    let tokens = opalc::lexer::Lexer::new(source).lex();
    let file_id = lowerer.add_file("fmt.opal".into(), source.into());

    // SExprParser::new filters Comment tokens out internally, so the parser
    // sees a clean stream. We keep the full `tokens` for the formatter.
    let sexprs = match opalc::sexpr::SExprParser::new(tokens.clone(), file_id).parse() {
        Ok(s) => s,
        Err(_) => return source.to_string(),
    };

    pretty::format_sexprs(&sexprs, &tokens, source, width)
}

/// Format with the default line width (100 columns).
pub fn format_default(source: &str) -> String {
    format(source, 100)
}
