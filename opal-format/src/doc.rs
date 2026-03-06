/// Wadler-Lindig pretty-printing Doc IR.
///
/// A Doc describes a layout with two modes:
/// - `Flat`: Line breaks become single spaces; used when the doc fits on one line.
/// - `Break`: Line breaks become actual newlines + indentation.
///
/// `Group(d)` is the key combinator: it tries `Flat` mode. If the doc fits within
/// the remaining column budget, it stays flat; otherwise it falls through to `Break`.
#[derive(Debug, Clone)]
pub enum Doc {
    Nil,
    Text(String),
    /// Soft break: space in Flat mode, newline + indent in Break mode.
    Line,
    /// Hard break: always a newline + indent, regardless of mode.
    HardLine,
    Concat(Box<Doc>, Box<Doc>),
    /// Increase indentation by `n` for the inner doc.
    Nest(usize, Box<Doc>),
    /// Set indentation to the current column for the inner doc.
    /// Produces Clojure-style alignment where wrapped lines line up with
    /// the first character of the content, not the surrounding block indent.
    Align(Box<Doc>),
    /// Try Flat; fall back to Break if the flat rendering exceeds the column budget.
    Group(Box<Doc>),
}

// ── Constructors ─────────────────────────────────────────────────────────────

pub fn nil() -> Doc {
    Doc::Nil
}

pub fn text(s: impl Into<String>) -> Doc {
    Doc::Text(s.into())
}

pub fn line() -> Doc {
    Doc::Line
}

pub fn hardline() -> Doc {
    Doc::HardLine
}

pub fn group(d: Doc) -> Doc {
    Doc::Group(Box::new(d))
}

pub fn nest(n: usize, d: Doc) -> Doc {
    Doc::Nest(n, Box::new(d))
}

pub fn align(d: Doc) -> Doc {
    Doc::Align(Box::new(d))
}

pub fn concat(a: Doc, b: Doc) -> Doc {
    match (a, b) {
        (Doc::Nil, b) => b,
        (a, Doc::Nil) => a,
        (a, b) => Doc::Concat(Box::new(a), Box::new(b)),
    }
}

/// Concatenate an iterator of docs left-to-right.
pub fn concat_all(docs: impl IntoIterator<Item = Doc>) -> Doc {
    docs.into_iter().fold(nil(), concat)
}

/// Join docs with a separator doc between each pair.
pub fn join(sep: Doc, docs: Vec<Doc>) -> Doc {
    docs.into_iter().enumerate().fold(nil(), |acc, (i, d)| {
        if i == 0 {
            d
        } else {
            concat(acc, concat(sep.clone(), d))
        }
    })
}

// ── Renderer ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum Mode {
    Flat,
    Break,
}

/// Render a Doc to a String within the given column `width`.
pub fn render(doc: &Doc, width: usize) -> String {
    let mut out = String::new();
    let mut col = 0usize;
    render_doc(doc, &mut out, &mut col, 0, Mode::Break, width);
    out
}

fn render_doc(
    doc: &Doc,
    out: &mut String,
    col: &mut usize,
    indent: usize,
    mode: Mode,
    width: usize,
) {
    match doc {
        Doc::Nil => {}

        Doc::Text(s) => {
            out.push_str(s);
            *col += s.len();
        }

        Doc::Line => match mode {
            Mode::Flat => {
                out.push(' ');
                *col += 1;
            }
            Mode::Break => {
                out.push('\n');
                for _ in 0..indent {
                    out.push(' ');
                }
                *col = indent;
            }
        },

        Doc::HardLine => {
            out.push('\n');
            for _ in 0..indent {
                out.push(' ');
            }
            *col = indent;
        }

        Doc::Concat(a, b) => {
            render_doc(a, out, col, indent, mode, width);
            render_doc(b, out, col, indent, mode, width);
        }

        Doc::Nest(n, inner) => {
            render_doc(inner, out, col, indent + n, mode, width);
        }

        Doc::Align(inner) => {
            render_doc(inner, out, col, *col, mode, width);
        }

        Doc::Group(inner) => match mode {
            // Already flat — stay flat.
            Mode::Flat => render_doc(inner, out, col, indent, Mode::Flat, width),
            // Break mode — decide based on whether the flat form fits.
            Mode::Break => {
                let remaining = width as isize - *col as isize;
                if flat_size(inner) <= remaining {
                    render_doc(inner, out, col, indent, Mode::Flat, width);
                } else {
                    render_doc(inner, out, col, indent, Mode::Break, width);
                }
            }
        },
    }
}

/// Compute the width of a doc rendered flat (all Lines become spaces).
/// Returns `isize::MAX` if the doc contains a HardLine (i.e. can never be flat).
fn flat_size(doc: &Doc) -> isize {
    match doc {
        Doc::Nil => 0,
        Doc::Text(s) => s.len() as isize,
        Doc::Line => 1,
        Doc::HardLine => isize::MAX / 2,
        Doc::Concat(a, b) => flat_size(a).saturating_add(flat_size(b)),
        Doc::Nest(_, inner) => flat_size(inner),
        Doc::Align(inner) => flat_size(inner),
        Doc::Group(inner) => flat_size(inner),
    }
}
