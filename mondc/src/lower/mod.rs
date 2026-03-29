use codespan_reporting::{
    diagnostic::{Diagnostic, Label, LabelStyle, Severity},
    files::SimpleFiles,
};

use crate::{
    ast::{
        Declaration, Expr, Literal, MatchArm, Pattern, TypeDecl, TypeSig, TypeUsage,
        UnqualifiedImports,
    },
    lexer::{Token, TokenKind},
    sexpr::SExpr,
};
use std::{collections::HashMap, ops::Range};

mod bindings;
mod declarations;
mod expr;
mod matching;
mod pipe;
mod records;
mod types;

#[cfg(test)]
mod tests;

#[derive(Default)]
pub struct Lowerer {
    pub files: SimpleFiles<String, String>,
    pub diagnostics: Vec<Diagnostic<usize>>,
}

impl Lowerer {
    pub fn new() -> Self {
        Self {
            files: SimpleFiles::new(),
            diagnostics: Vec::new(),
        }
    }

    fn error(&mut self, diagnostic: Diagnostic<usize>) {
        self.diagnostics.push(diagnostic);
    }

    /// Add a new file to the compiler's memory
    pub fn add_file(&mut self, name: String, source: String) -> usize {
        self.files.add(name, source)
    }

    pub(super) fn source_at(&self, file_id: usize, span: Range<usize>) -> &str {
        let file = self
            .files
            .get(file_id)
            .expect("Invalid file_id in source_at");

        &file.source()[span]
    }

    pub(super) fn reject_ambiguous_constructor_sequence(
        &mut self,
        file_id: usize,
        body_sexprs: &[SExpr],
    ) -> bool {
        if body_sexprs.len() <= 1 {
            return false;
        }

        for (idx, sexpr) in body_sexprs.iter().enumerate().take(body_sexprs.len() - 1) {
            let SExpr::Atom(token) = sexpr else {
                continue;
            };
            let (name, is_constructor_like) = match &token.kind {
                TokenKind::Ident => {
                    let name = self.source_at(file_id, token.span.clone()).to_string();
                    let is_constructor_like = name
                        .chars()
                        .next()
                        .map(|c| c.is_ascii_uppercase())
                        .unwrap_or(false);
                    (name, is_constructor_like)
                }
                TokenKind::QualifiedIdent((module, constructor)) => (
                    format!("{module}/{constructor}"),
                    constructor
                        .chars()
                        .next()
                        .map(|c| c.is_ascii_uppercase())
                        .unwrap_or(false),
                ),
                _ => continue,
            };
            if !is_constructor_like {
                continue;
            }

            let next_span = body_sexprs[idx + 1].span();
            self.error(
                Diagnostic::error()
                    .with_message(format!(
                        "constructor `{name}` cannot be followed by a separate body expression"
                    ))
                    .with_labels(vec![
                        Label::primary(file_id, token.span.clone())
                            .with_message("this constructor expression is complete on its own"),
                        Label::secondary(file_id, next_span)
                            .with_message("this is parsed as a separate expression"),
                    ])
                    .with_notes(vec![
                        "if you intended constructor application, write it in one expression: `(Some x)`".into(),
                        "if you intended sequencing, make it explicit with `do`".into(),
                    ]),
            );
            return true;
        }

        false
    }

    pub fn lower_file(&mut self, file_id: usize, sexprs: &[SExpr]) -> Vec<Declaration> {
        let mut lowered_declarations = Vec::new();

        for sexpr in sexprs {
            match sexpr {
                SExpr::Round(items, span) => {
                    // Strip optional leading `pub` and record visibility
                    let (is_pub, effective_items) = if let Some(SExpr::Atom(t)) = items.first() {
                        if t.kind == TokenKind::Pub {
                            (true, &items[1..])
                        } else {
                            (false, items.as_slice())
                        }
                    } else {
                        (false, items.as_slice())
                    };

                    if let Some(SExpr::Atom(token)) = effective_items.first() {
                        match token.kind {
                            TokenKind::Type => {
                                if let Some(t) = self.lower_type_decl(
                                    file_id,
                                    effective_items,
                                    span.clone(),
                                    is_pub,
                                ) {
                                    lowered_declarations.push(Declaration::Type(t));
                                }
                            }
                            TokenKind::Extern => {
                                if let Some(d) = self.lower_extern_dispatch(
                                    file_id,
                                    effective_items,
                                    span.clone(),
                                    is_pub,
                                ) {
                                    lowered_declarations.push(d);
                                }
                            }
                            TokenKind::Use => {
                                if let Some(d) =
                                    self.lower_use(file_id, effective_items, span.clone(), is_pub)
                                {
                                    lowered_declarations.push(d);
                                }
                            }
                            TokenKind::Test => {
                                if let Some(d) =
                                    self.lower_test(file_id, effective_items, span.clone())
                                {
                                    lowered_declarations.push(d);
                                }
                            }
                            TokenKind::Let => {
                                // At the top level, only function definitions are allowed.
                                // (let [x 10] body) is a local binding — only valid inside a function.
                                if let Some(SExpr::Square(_, sq_span)) = effective_items.get(1) {
                                    self.error(
                                        Diagnostic::error()
                                            .with_message("local let binding is not valid at the top level")
                                            .with_labels(vec![Label::primary(file_id, sq_span.clone())
                                                .with_message("local bindings must be inside a function body")])
                                            .with_notes(vec![
                                                "top-level definitions must be functions: (let name {args} body)".into()
                                            ]),
                                    );
                                } else if let Some(e) =
                                    self.lower_let(file_id, effective_items, span.clone(), is_pub)
                                {
                                    lowered_declarations.push(Declaration::Expression(e));
                                }
                            }
                            _ => {
                                self.error(
                                    Diagnostic::error()
                                        .with_message("only function and type declarations are valid at the top level")
                                        .with_labels(vec![Label::primary(file_id, span.clone())])
                                        .with_notes(vec![
                                            "hint: move this into a function body, e.g. `(let main {} ...)`".into(),
                                        ]),
                                );
                            }
                        }
                    } else {
                        self.error(
                            Diagnostic::error()
                                .with_message("only function and type declarations are valid at the top level")
                                .with_labels(vec![Label::primary(file_id, span.clone())])
                                .with_notes(vec![
                                    "hint: move this into a function body, e.g. `(let main {} ...)`".into(),
                                ]),
                        );
                    }
                }

                _ => {
                    self.error(
                        Diagnostic::error()
                            .with_message(
                                "only function and type declarations are valid at the top level",
                            )
                            .with_labels(vec![Label::primary(file_id, sexpr.span())])
                            .with_notes(vec![
                                "hint: move this into a function body, e.g. `(let main {} ...)`"
                                    .into(),
                            ]),
                    );
                }
            }
        }

        lowered_declarations
    }
}
