use opalc::{
    lexer::{Token, TokenKind},
    sexpr::SExpr,
};

use crate::doc::*;

// ── Public entry point ────────────────────────────────────────────────────────

/// Format a sequence of top-level SExprs to a string.
///
/// `tokens` is the full raw token stream (including `Comment` tokens) from the
/// lexer. Comments are attached to the form they immediately precede, preserving
/// blank lines between consecutive comments.
///
/// Comments *inside* a form (within its span) are dropped — this is a known
/// limitation of operating at the SExpr level.
pub fn format_sexprs(sexprs: &[SExpr], tokens: &[Token], source: &str, width: usize) -> String {
    // Collect only comment tokens for fast lookup.
    let comments: Vec<&Token> = tokens
        .iter()
        .filter(|t| t.kind == TokenKind::Comment)
        .collect();

    if sexprs.is_empty() {
        // Source is all comments — emit them as-is.
        let mut out = String::new();
        for c in &comments {
            out.push_str(&source[c.span.clone()]);
            out.push('\n');
        }
        return out;
    }

    let mut output = String::new();

    for (i, sexpr) in sexprs.iter().enumerate() {
        // Byte range that "belongs to" this form's leading comments:
        // everything from the end of the previous form (or start of file) up to
        // the start of this form.
        let region_start = if i == 0 { 0 } else { sexprs[i - 1].span().end };
        let region_end = sexpr.span().start;

        let leading: Vec<&Token> = comments
            .iter()
            .copied()
            .filter(|t| t.span.start >= region_start && t.span.end <= region_end)
            .collect();

        if i > 0 {
            // Suppress the blank line between consecutive import-like forms
            // (use, extern) when there are no comments between them.
            let prev = &sexprs[i - 1];
            let both_imports = is_import_form(prev) && is_import_form(sexpr);
            if !both_imports || !leading.is_empty() {
                output.push('\n'); // blank line between top-level forms
            }
        }

        // Emit leading comments, preserving blank lines between consecutive ones.
        emit_comments(&leading, source, &mut output);

        // Emit the formatted form.
        let doc = fmt(sexpr, source);
        output.push_str(render(&doc, width).trim_end_matches('\n'));
        output.push('\n');
    }

    // Trailing comments (after the last form).
    let last_end = sexprs.last().unwrap().span().end;
    let trailing: Vec<&Token> = comments
        .iter()
        .copied()
        .filter(|t| t.span.start >= last_end)
        .collect();

    if !trailing.is_empty() {
        output.push('\n');
        emit_comments(&trailing, source, &mut output);
    }

    let trimmed = output.trim_end_matches('\n');
    format!("{trimmed}\n")
}

/// Returns true if this top-level form is a `use` or `extern` declaration.
/// Consecutive import forms are kept together without a blank line between them.
fn is_import_form(sexpr: &SExpr) -> bool {
    if let SExpr::Round(items, _) = sexpr {
        items.iter().any(|item| {
            matches!(item, SExpr::Atom(t) if matches!(t.kind, TokenKind::Use | TokenKind::Extern))
        })
    } else {
        false
    }
}

/// Emit a sequence of comment tokens, preserving blank lines that appear between
/// consecutive comments in the original source.
fn emit_comments(comments: &[&Token], source: &str, out: &mut String) {
    for (i, comment) in comments.iter().enumerate() {
        if i > 0 {
            let prev_end = comments[i - 1].span.end;
            let this_start = comment.span.start;
            // Count newlines between end of previous comment and start of this one.
            // ≥2 newlines means there was a blank line in the original.
            let newlines = source[prev_end..this_start]
                .chars()
                .filter(|&c| c == '\n')
                .count();
            if newlines >= 2 {
                out.push('\n');
            }
        }
        out.push_str(&source[comment.span.clone()]);
        out.push('\n');
    }
}

// ── Core dispatch ─────────────────────────────────────────────────────────────

fn fmt(expr: &SExpr, source: &str) -> Doc {
    match expr {
        SExpr::Atom(token) => text(atom_text(token, source)),
        SExpr::Curly(items, _) => fmt_curly(items, source),
        SExpr::Square(items, _) => fmt_square(items, source),
        SExpr::Round(items, _) => fmt_round(items, source),
    }
}

fn atom_text<'a>(token: &opalc::lexer::Token, source: &'a str) -> &'a str {
    &source[token.span.clone()]
}

// ── Bracket forms ─────────────────────────────────────────────────────────────

fn fmt_curly(items: &[SExpr], source: &str) -> Doc {
    if items.is_empty() {
        return text("{}");
    }
    let inner = join(line(), items.iter().map(|i| fmt(i, source)).collect());
    group(concat_all([text("{"), nest(1, inner), text("}")]))
}

fn fmt_square(items: &[SExpr], source: &str) -> Doc {
    if items.is_empty() {
        return text("[]");
    }
    let inner = join(line(), items.iter().map(|i| fmt(i, source)).collect());
    group(concat_all([text("["), nest(1, inner), text("]")]))
}

// ── Round — special form dispatch ─────────────────────────────────────────────

fn fmt_round(items: &[SExpr], source: &str) -> Doc {
    if items.is_empty() {
        return text("()");
    }

    // Strip leading modifier keywords (pub) to find the governing keyword.
    let mod_count = items
        .iter()
        .take_while(|i| matches!(i, SExpr::Atom(t) if matches!(t.kind, TokenKind::Pub)))
        .count();

    let tail = &items[mod_count..];

    match tail {
        [SExpr::Atom(kw_tok), rest @ ..] => match kw_tok.kind {
            TokenKind::Let | TokenKind::LetBind => fmt_let(items, mod_count, rest, source),
            TokenKind::Type => fmt_type(items, mod_count, rest, source),
            TokenKind::If => fmt_if(rest, source),
            TokenKind::Match => fmt_match(rest, source),
            TokenKind::Fn => fmt_fn(rest, source),
            TokenKind::Do => fmt_do(rest, source),
            // use / extern — always stay on one line
            TokenKind::Use | TokenKind::Extern => fmt_inline(items, source),
            _ => fmt_generic(items, source),
        },
        _ => fmt_generic(items, source),
    }
}

// ── let ───────────────────────────────────────────────────────────────────────

fn fmt_let(all: &[SExpr], mod_count: usize, rest: &[SExpr], source: &str) -> Doc {
    // Build the prefix: "let " or "pub let " etc.
    let mods_and_kw: Vec<Doc> = all[..mod_count + 1]
        .iter()
        .map(|s| fmt(s, source))
        .collect();
    let prefix = concat(join(text(" "), mods_and_kw), text(" "));

    match rest {
        // (let name {args} body...)  — function definition, 1 or more body exprs
        [name @ SExpr::Atom(_), args @ SExpr::Curly(..), bodies @ ..] if !bodies.is_empty() => {
            if bodies.len() == 1 {
                // Single body: try inline first
                group(concat_all([
                    text("("),
                    prefix,
                    fmt(name, source),
                    text(" "),
                    fmt(args, source),
                    nest(2, concat(line(), fmt(&bodies[0], source))),
                    text(")"),
                ]))
            } else {
                // Multiple body exprs: always break, each on its own line
                let body_docs: Vec<Doc> = bodies
                    .iter()
                    .map(|b| concat(hardline(), fmt(b, source)))
                    .collect();
                concat_all([
                    text("("),
                    prefix,
                    fmt(name, source),
                    text(" "),
                    fmt(args, source),
                    nest(2, concat_all(body_docs)),
                    text(")"),
                ])
            }
        }

        // (let [x v ...] body...) — sequential local bindings, 0 or more body exprs
        [SExpr::Square(bindings, _), bodies @ ..] => {
            let pairs = fmt_let_bindings(bindings, source);
            if bodies.is_empty() {
                group(concat_all([text("("), prefix, pairs, text(")")]))
            } else if bodies.len() == 1 {
                group(concat_all([
                    text("("),
                    prefix,
                    pairs,
                    nest(2, concat(line(), fmt(&bodies[0], source))),
                    text(")"),
                ]))
            } else {
                let body_docs: Vec<Doc> = bodies
                    .iter()
                    .map(|b| concat(line(), fmt(b, source)))
                    .collect();
                concat_all([
                    text("("),
                    prefix,
                    pairs,
                    nest(2, concat_all(body_docs)),
                    text(")"),
                ])
            }
        }

        _ => fmt_generic(all, source),
    }
}

/// Format let-bindings `[name val name val ...]` as a group of pairs.
///
/// When broken, subsequent pairs align with the first pair right after `[`,
/// matching Clojure-style let layout:
///
/// ```text
/// (let [x   expr1
///       foo expr2]
///   body)
/// ```
fn fmt_let_bindings(bindings: &[SExpr], source: &str) -> Doc {
    if bindings.is_empty() {
        return text("[]");
    }

    // Compute max name width so values align across all pairs.
    let name_width: usize = bindings
        .chunks(2)
        .filter_map(|chunk| chunk.first())
        .map(|s| match s {
            SExpr::Atom(t) => source[t.span.clone()].len(),
            _ => 0,
        })
        .max()
        .unwrap_or(0);

    let mut pairs: Vec<Doc> = Vec::new();
    let mut i = 0;
    while i + 1 < bindings.len() {
        let name_sexpr = &bindings[i];
        let name_len = match name_sexpr {
            SExpr::Atom(t) => source[t.span.clone()].len(),
            _ => 0,
        };
        let name_doc = fmt(name_sexpr, source);
        let val_doc = fmt(&bindings[i + 1], source);
        let padding = " ".repeat(name_width - name_len + 1);
        pairs.push(concat_all([name_doc, text(padding), val_doc]));
        i += 2;
    }
    if i < bindings.len() {
        pairs.push(fmt(&bindings[i], source));
    }

    // align: when broken, subsequent pairs start at the column right after `[`
    let inner = join(line(), pairs);
    group(concat_all([text("["), align(inner), text("]")]))
}

// ── type ──────────────────────────────────────────────────────────────────────

fn fmt_type(all: &[SExpr], mod_count: usize, rest: &[SExpr], source: &str) -> Doc {
    let mods_and_kw: Vec<Doc> = all[..mod_count + 1]
        .iter()
        .map(|s| fmt(s, source))
        .collect();
    let prefix = concat(join(text(" "), mods_and_kw), text(" "));

    match rest {
        // (type ['a 'b] Name (body...))
        [
            params @ SExpr::Square(..),
            SExpr::Atom(name),
            SExpr::Round(body, _),
        ] => concat_all([
            text("("),
            prefix,
            fmt(params, source),
            text(" "),
            text(atom_text(name, source)),
            text(" "),
            fmt_type_body(body, source),
            text(")"),
        ]),

        // (type Name (body...))
        [SExpr::Atom(name), SExpr::Round(body, _)] => concat_all([
            text("("),
            prefix,
            text(atom_text(name, source)),
            text(" "),
            fmt_type_body(body, source),
            text(")"),
        ]),

        _ => fmt_generic(all, source),
    }
}

/// Format the body of a type declaration, including the outer `()`.
///
/// Flat:  `(entry1 entry2)`
/// Break: `(\n  entry1\n  entry2\n)`
///
/// Preserving the outer parens is essential for idempotency: without them the
/// SExpr structure changes on re-parse and the second pass would produce
/// different output.
fn fmt_type_body(items: &[SExpr], source: &str) -> Doc {
    if items.is_empty() {
        return text("()");
    }
    let entries: Vec<Doc> = items.iter().map(|i| fmt(i, source)).collect();
    let inner = join(line(), entries);
    group(concat_all([
        text("("),
        nest(2, concat(line(), inner)),
        line(),
        text(")"),
    ]))
}

// ── if ────────────────────────────────────────────────────────────────────────

fn fmt_if(rest: &[SExpr], source: &str) -> Doc {
    match rest {
        [cond, then, els] => group(concat_all([
            text("(if "),
            fmt(cond, source),
            nest(
                2,
                concat_all([line(), fmt(then, source), line(), fmt(els, source)]),
            ),
            text(")"),
        ])),
        _ => fmt_generic_with_head("if", rest, source),
    }
}

// ── fn (lambda) ───────────────────────────────────────────────────────────────

fn fmt_fn(rest: &[SExpr], source: &str) -> Doc {
    match rest {
        [args @ SExpr::Curly(..), body] => group(concat_all([
            text("(fn "),
            fmt(args, source),
            nest(2, concat(line(), fmt(body, source))),
            text(")"),
        ])),
        _ => fmt_generic_with_head("fn", rest, source),
    }
}

// ── do ────────────────────────────────────────────────────────────────────────

fn fmt_do(rest: &[SExpr], source: &str) -> Doc {
    match rest {
        [] => text("(do)"),
        [single] => group(concat_all([text("(do "), fmt(single, source), text(")")])),
        [first, tail @ ..] => {
            // Multiple expressions always break — `do` is for sequencing side
            // effects and is never flat. First expr stays inline after `do`,
            // subsequent ones align to the same column.
            let tail_docs: Vec<Doc> = tail
                .iter()
                .map(|e| concat(hardline(), fmt(e, source)))
                .collect();
            concat_all([
                text("(do "),
                align(concat_all([fmt(first, source), concat_all(tail_docs)])),
                text(")"),
            ])
        }
    }
}

// ── match ─────────────────────────────────────────────────────────────────────

fn fmt_match(rest: &[SExpr], source: &str) -> Doc {
    // rest = [target, pat... ~> expr, pat... ~> expr, ...]
    // Assume single target (first item); the rest are arms.
    if rest.is_empty() {
        return fmt_generic_with_head("match", rest, source);
    }

    let target = &rest[0];
    let arms_raw = &rest[1..];
    let arms = collect_match_arms(arms_raw);

    if arms.is_empty() {
        return fmt_generic_with_head("match", rest, source);
    }

    let arm_docs: Vec<Doc> = arms
        .into_iter()
        .map(|(pats, body)| {
            let pat_doc = join(text(" "), pats.iter().map(|s| fmt(s, source)).collect());
            concat_all([pat_doc, text(" ~> "), fmt(body, source)])
        })
        .collect();

    concat_all([
        text("(match "),
        fmt(target, source),
        nest(
            2,
            concat_all(
                arm_docs
                    .into_iter()
                    .map(|a| concat(line(), a))
                    .collect::<Vec<_>>(),
            ),
        ),
        text(")"),
    ])
}

/// Split a flat SExpr sequence into match arms: `(patterns, body)`.
fn collect_match_arms(items: &[SExpr]) -> Vec<(Vec<&SExpr>, &SExpr)> {
    let mut arms = Vec::new();
    let mut i = 0;
    while i < items.len() {
        let mut pat = Vec::new();
        while i < items.len() {
            if matches!(&items[i], SExpr::Atom(t) if t.kind == TokenKind::Arrow) {
                break;
            }
            pat.push(&items[i]);
            i += 1;
        }
        if i >= items.len() {
            break;
        }
        i += 1; // skip `~>`
        if i >= items.len() {
            break;
        }
        let body = &items[i];
        i += 1;
        if !pat.is_empty() {
            arms.push((pat, body));
        }
    }
    arms
}

// ── use / extern (always inline) ─────────────────────────────────────────────

fn fmt_inline(items: &[SExpr], source: &str) -> Doc {
    let inner = join(text(" "), items.iter().map(|s| fmt(s, source)).collect());
    concat_all([text("("), inner, text(")")])
}

// ── Generic fallback ──────────────────────────────────────────────────────────

/// Generic round form: try inline; if it doesn't fit, break with each arg indented.
fn fmt_generic(items: &[SExpr], source: &str) -> Doc {
    if items.is_empty() {
        return text("()");
    }
    let head = fmt(&items[0], source);
    if items.len() == 1 {
        return concat_all([text("("), head, text(")")]);
    }
    let args: Vec<Doc> = items[1..].iter().map(|s| fmt(s, source)).collect();
    let args_doc = join(line(), args);
    group(concat_all([
        text("("),
        head,
        nest(2, concat(line(), args_doc)),
        text(")"),
    ]))
}

fn fmt_generic_with_head(kw: &str, rest: &[SExpr], source: &str) -> Doc {
    if rest.is_empty() {
        return concat_all([text("("), text(kw), text(")")]);
    }
    let args: Vec<Doc> = rest.iter().map(|s| fmt(s, source)).collect();
    let args_doc = join(line(), args);
    group(concat_all([
        text("("),
        text(kw),
        nest(2, concat(line(), args_doc)),
        text(")"),
    ]))
}
