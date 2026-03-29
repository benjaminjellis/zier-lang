use super::*;

impl Lowerer {
    pub(super) fn lower_lambda(
        &mut self,
        file_id: usize,
        items: &[SExpr],
        span: Range<usize>,
    ) -> Option<Expr> {
        if items.len() != 4 {
            self.error(
                Diagnostic::error()
                    .with_message("invalid anonymous function syntax")
                    .with_labels(vec![Label::primary(file_id, span.clone())])
                    .with_notes(vec!["syntax: (f {arg1 arg2} -> body)".into()]),
            );
            return None;
        }

        let (args, arg_spans): (Vec<_>, Vec<_>) = match &items[1] {
            SExpr::Curly(params, _) => params
                .iter()
                .filter_map(|p| {
                    if let SExpr::Atom(t) = p {
                        Some((
                            self.source_at(file_id, t.span.clone()).to_string(),
                            t.span.clone(),
                        ))
                    } else {
                        None
                    }
                })
                .unzip(),
            _ => {
                self.error(
                    Diagnostic::error()
                        .with_message("expected `{args}` in anonymous function")
                        .with_labels(vec![
                            Label::primary(file_id, items[1].span())
                                .with_message("expected curly braces here"),
                        ])
                        .with_notes(vec!["syntax: (f {arg1 arg2} -> body)".into()]),
                );
                return None;
            }
        };

        for (arg, span) in args.iter().zip(&arg_spans) {
            if arg == "f" {
                self.error(
                        Diagnostic::error()
                            .with_message("invalid argument name, 'f' is a reserved keyword for anonymous functions")
                            .with_labels(vec![Label::primary(file_id, span.clone())]),
                    )
            }
        }

        if !matches!(
            items.get(2),
            Some(SExpr::Atom(Token {
                kind: TokenKind::ThinArrow,
                ..
            }))
        ) {
            self.error(
                Diagnostic::error()
                    .with_message("invalid anonymous function syntax")
                    .with_labels(vec![Label::primary(file_id, span.clone())])
                    .with_notes(vec!["syntax: (f {arg1 arg2} -> body)".into()]),
            );
            return None;
        }

        let body = self.lower_expr(file_id, &items[3])?;
        Some(Expr::Lambda {
            args,
            arg_spans,
            body: Box::new(body),
            span,
        })
    }

    pub(super) fn lower_let_bind(
        &mut self,
        file_id: usize,
        items: &[SExpr],
        span: Range<usize>,
    ) -> Option<Expr> {
        // (let? [a expr1 b expr2] body)
        // items[0] = let?, items[1] = Square([name val ...]), items[2] = body
        if items.len() < 3 {
            self.error(
                Diagnostic::error()
                    .with_message("let? requires a body expression")
                    .with_labels(vec![Label::primary(file_id, span.clone())])
                    .with_notes(vec!["syntax: (let? [name expr ...] body)".into()]),
            );
            return None;
        }
        if items.len() > 3 {
            self.error(
                Diagnostic::error()
                    .with_message("invalid let? syntax")
                    .with_labels(vec![Label::primary(file_id, span.clone())])
                    .with_notes(vec!["syntax: (let? [name expr ...] body)".into()]),
            );
            return None;
        }

        let (bindings, b_span) = match &items[1] {
            SExpr::Square(b, s) => (b, s.clone()),
            _ => {
                self.error(
                    Diagnostic::error()
                        .with_message("expected `[bindings]` in let?")
                        .with_labels(vec![
                            Label::primary(file_id, items[1].span())
                                .with_message("expected square brackets here"),
                        ])
                        .with_notes(vec!["syntax: (let? [name expr ...] body)".into()]),
                );
                return None;
            }
        };

        if bindings.len() % 2 != 0 || bindings.is_empty() {
            self.error(
                Diagnostic::error()
                    .with_message("let? bindings must be non-empty pairs of name and expression")
                    .with_labels(vec![Label::primary(file_id, b_span)])
                    .with_notes(vec!["syntax: (let? [name expr ...] body)".into()]),
            );
            return None;
        }

        let body = self.lower_expr(file_id, &items[2])?;

        // Desugar right-to-left into nested Result matches:
        // (match expr
        //   (Ok name) ~> inner
        //   (Error e) ~> (Error e))
        let mut pairs: Vec<(String, Range<usize>, Expr)> = Vec::new();
        for chunk in bindings.chunks(2) {
            let (name, name_span) = match &chunk[0] {
                SExpr::Atom(t) => (
                    self.source_at(file_id, t.span.clone()).to_string(),
                    t.span.clone(),
                ),
                other => {
                    self.error(
                        Diagnostic::error()
                            .with_message("expected identifier in let? binding")
                            .with_labels(vec![Label::primary(file_id, other.span())]),
                    );
                    return None;
                }
            };
            let val = self.lower_expr(file_id, &chunk[1])?;
            pairs.push((name, name_span, val));
        }

        // Fold right: innermost binding wraps the body
        let result = pairs
            .into_iter()
            .rev()
            .fold(body, |inner, (name, name_span, val)| {
                let error_name = "__letq_error".to_string();
                let ok_pat = Pattern::Constructor(
                    "Ok".to_string(),
                    vec![Pattern::Variable(name, name_span)],
                    span.clone(),
                );
                let error_pat = Pattern::Constructor(
                    "Error".to_string(),
                    vec![Pattern::Variable(error_name.clone(), span.clone())],
                    span.clone(),
                );
                let error_expr = Expr::Call {
                    func: Box::new(Expr::Variable("Error".to_string(), span.clone())),
                    args: vec![Expr::Variable(error_name, span.clone())],
                    span: span.clone(),
                };

                Expr::Match {
                    targets: vec![val],
                    arms: vec![
                        MatchArm {
                            patterns: vec![ok_pat],
                            guard: None,
                            body: inner,
                        },
                        MatchArm {
                            patterns: vec![error_pat],
                            guard: None,
                            body: error_expr,
                        },
                    ],
                    span: span.clone(),
                }
            });

        Some(result)
    }

    pub(super) fn lower_if(
        &mut self,
        file_id: usize,
        items: &[SExpr],
        span: Range<usize>,
    ) -> Option<Expr> {
        // if-let form:
        //   (if let [<pattern> <value>] <then> <else>)
        // Desugars to:
        //   (match <value> <pattern> ~> <then> _ ~> <else>)
        let is_if_let = matches!(
            items.get(1),
            Some(SExpr::Atom(t)) if matches!(t.kind, TokenKind::Let)
        );
        if is_if_let {
            match items {
                // Canonical form: (if let [pattern value] then else)
                [
                    _,
                    SExpr::Atom(let_tok),
                    SExpr::Square(binding, binding_span),
                    then_expr,
                    else_expr,
                ] if let_tok.kind == TokenKind::Let => {
                    if binding.len() != 2 {
                        self.error(
                            Diagnostic::error()
                                .with_code("E005")
                                .with_message("invalid if let binding")
                                .with_labels(vec![Label::primary(file_id, binding_span.clone())
                                    .with_message("expected [<pattern> <value>]")])
                                .with_notes(vec![
                                    "help: the syntax is (if let [<pattern> <value>] <then-branch> <else-branch>)"
                                        .into(),
                                ]),
                        );
                        return None;
                    }

                    let pattern = self.lower_pattern(file_id, &binding[0])?;
                    let target = self.lower_expr(file_id, &binding[1])?;
                    let then = self.lower_expr(file_id, then_expr)?;
                    let els = self.lower_expr(file_id, else_expr)?;

                    return Some(Expr::Match {
                        targets: vec![target],
                        arms: vec![
                            MatchArm {
                                patterns: vec![pattern],
                                guard: None,
                                body: then,
                            },
                            MatchArm {
                                patterns: vec![Pattern::Any(span.clone())],
                                guard: None,
                                body: els,
                            },
                        ],
                        span,
                    });
                }

                // Legacy form (supported for compatibility):
                //   (if let <pattern> <value> <then> <else>)
                [
                    _,
                    SExpr::Atom(let_tok),
                    pattern_expr,
                    target_expr,
                    then_expr,
                    else_expr,
                ] if let_tok.kind == TokenKind::Let
                    && !matches!(pattern_expr, SExpr::Square(_, _)) =>
                {
                    let pattern = self.lower_pattern(file_id, pattern_expr)?;
                    let target = self.lower_expr(file_id, target_expr)?;
                    let then = self.lower_expr(file_id, then_expr)?;
                    let els = self.lower_expr(file_id, else_expr)?;

                    return Some(Expr::Match {
                        targets: vec![target],
                        arms: vec![
                            MatchArm {
                                patterns: vec![pattern],
                                guard: None,
                                body: then,
                            },
                            MatchArm {
                                patterns: vec![Pattern::Any(span.clone())],
                                guard: None,
                                body: els,
                            },
                        ],
                        span,
                    });
                }

                [_, SExpr::Atom(let_tok), binding, _, _] if let_tok.kind == TokenKind::Let => {
                    self.error(
                        Diagnostic::error()
                            .with_code("E005")
                            .with_message("invalid if let binding")
                            .with_labels(vec![Label::primary(file_id, binding.span())
                                .with_message("expected [<pattern> <value>]")])
                            .with_notes(vec![
                                "help: the syntax is (if let [<pattern> <value>] <then-branch> <else-branch>)"
                                    .into(),
                            ]),
                    );
                    return None;
                }

                _ => {
                    let actual_args = if items.is_empty() { 0 } else { items.len() - 1 };
                    self.error(
                        Diagnostic::error()
                            .with_code("E005")
                            .with_message("wrong number of arguments for 'if let'")
                            .with_labels(vec![Label::primary(file_id, span.clone()).with_message(
                                format!("expected 4 arguments, found {}", actual_args),
                            )])
                            .with_notes(vec![
                                "help: the syntax is (if let [<pattern> <value>] <then-branch> <else-branch>)".into(),
                            ]),
                    );
                    return None;
                }
            }
        }

        // 1. Validation: (if cond then else) has 4 elements total
        if items.len() != 4 {
            let actual_args = if items.is_empty() { 0 } else { items.len() - 1 };

            self.error(
                Diagnostic::error()
                    .with_code("E005")
                    .with_message("wrong number of arguments for 'if'")
                    .with_labels(vec![Label::primary(file_id, span.clone()).with_message(
                        format!("expected 3 arguments, found {}", actual_args),
                    )])
                    .with_notes(vec![
                        "help: the syntax is (if <condition> <then-branch> <else-branch>)".into(),
                    ]),
            );
            return None;
        }

        // 2. Recursively lower the three parts
        // Using ? ensures that if the condition or branches are broken,
        // we stop building this 'If' node.
        let cond = self.lower_expr(file_id, &items[1])?;
        let then = self.lower_expr(file_id, &items[2])?;
        let els = self.lower_expr(file_id, &items[3])?;

        Some(Expr::If {
            cond: Box::new(cond),
            then: Box::new(then),
            els: Box::new(els),
            span,
        })
    }

    pub(super) fn lower_do(
        &mut self,
        file_id: usize,
        items: &[SExpr],
        span: Range<usize>,
    ) -> Option<Expr> {
        // (do e1 e2 ... eN) — must have at least one expression after `do`
        let exprs = &items[1..];
        if exprs.is_empty() {
            self.error(Diagnostic {
                severity: Severity::Error,
                code: Some("E003".to_string()),
                message: "`do` requires at least one expression".to_string(),
                labels: vec![Label {
                    style: LabelStyle::Primary,
                    file_id,
                    range: span,
                    message: "".to_string(),
                }],
                notes: vec![],
            });
            return None;
        }
        let mut lowered: Vec<Expr> = exprs
            .iter()
            .map(|s| self.lower_expr(file_id, s))
            .collect::<Option<Vec<_>>>()?;
        // Fold all but the last into LetLocal "_" bindings
        let last = lowered.pop().unwrap();
        Some(
            lowered
                .into_iter()
                .rev()
                .fold(last, |body, expr| Expr::LetLocal {
                    name: "_".to_string(),
                    name_span: span.clone(),
                    value: Box::new(expr),
                    body: Box::new(body),
                    span: span.clone(),
                }),
        )
    }

    pub fn lower_let(
        &mut self,
        file_id: usize,
        items: &[SExpr],
        span: Range<usize>,
        is_pub: bool,
    ) -> Option<Expr> {
        let mut cursor = 1; // Skip the 'let' keyword

        match items.get(cursor) {
            // CASE A: Sequential Variable Bindings -> (let [x 10 y 20] body)
            Some(SExpr::Square(bindings, b_span)) => {
                if bindings.len() % 2 != 0 {
                    self.error(
                        Diagnostic::error()
                            .with_message("let-binding bracket must contain pairs")
                            .with_labels(vec![
                                Label::primary(file_id, b_span.clone())
                                    .with_message("expected [name value ...]"),
                            ]),
                    );
                    return None;
                }

                cursor += 1;

                // Collect all body expressions — implicit sequencing in let body
                let body_sexprs = &items[cursor..];
                if self.reject_ambiguous_constructor_sequence(file_id, body_sexprs) {
                    return None;
                }
                let mut current_expr = if body_sexprs.is_empty() {
                    Expr::Literal(Literal::Unit, span.clone())
                } else {
                    let mut body_exprs = body_sexprs
                        .iter()
                        .map(|s| self.lower_expr(file_id, s))
                        .collect::<Option<Vec<_>>>()?;
                    if body_exprs.len() == 1 {
                        body_exprs.remove(0)
                    } else {
                        let last = body_exprs.pop().unwrap();
                        body_exprs
                            .into_iter()
                            .rev()
                            .fold(last, |body, expr| Expr::LetLocal {
                                name: "_".to_string(),
                                name_span: span.clone(),
                                value: Box::new(expr),
                                body: Box::new(body),
                                span: span.clone(),
                            })
                    }
                };

                // We fold the bindings backwards to create nested LetLocal expressions
                for chunk in bindings.chunks(2).rev() {
                    let (name, name_span) = match &chunk[0] {
                        SExpr::Atom(t) => (
                            self.source_at(file_id, t.span.clone()).to_string(),
                            t.span.clone(),
                        ),
                        other => {
                            let mut diag = Diagnostic::error()
                                .with_message("expected identifier in let-binding name position")
                                .with_labels(vec![
                                    Label::primary(file_id, other.span())
                                        .with_message("this is not a valid binding name"),
                                ]);
                            if matches!(other, SExpr::Round(..)) {
                                diag = diag.with_notes(vec![
                                    "hint: each binding is `name value` — the value must be a single expression; wrap multiple tokens in parentheses".into(),
                                ]);
                            } else {
                                diag = diag.with_notes(vec![
                                    "hint: did you forget to wrap a function call in parentheses? e.g. `[x (f a b)]` not `[x f a b]`".into(),
                                ]);
                            }
                            self.error(diag);
                            return None;
                        }
                    };

                    let value = self.lower_expr(file_id, &chunk[1])?;

                    current_expr = Expr::LetLocal {
                        name,
                        name_span,
                        value: Box::new(value),
                        body: Box::new(current_expr),
                        span: span.clone(),
                    };
                }
                Some(current_expr)
            }

            // CASE B: Function Definition -> (let f {a b} body)
            Some(SExpr::Atom(name_token)) => {
                let name = self.source_at(file_id, name_token.span.clone()).to_string();
                let name_span = name_token.span.clone();
                cursor += 1;

                // STRICT ENFORCEMENT: The next item MUST be Curly brackets {}
                let (args, arg_spans) = if let Some(SExpr::Curly(params, _)) = items.get(cursor) {
                    let mut arg_names = Vec::new();

                    let mut arg_spans = Vec::new();
                    for p in params {
                        if let SExpr::Atom(t) = p {
                            arg_names.push(self.source_at(file_id, t.span.clone()).to_string());
                            arg_spans.push(t.span.clone());
                        }
                    }
                    cursor += 1;
                    (arg_names, arg_spans)
                } else {
                    // This catches (let x 42 ...) and rejects it.
                    self.error(
                    Diagnostic::error()
                        .with_message("invalid let syntax")
                        .with_labels(vec![
                            Label::primary(file_id, span.clone())
                                .with_message("If you were trying to write a function the syntax is 'let my_func {args}', if you were trying to define some variable binding it's 'let [name variable]'")
                        ]),
                );
                    return None;
                };

                for (arg, span) in args.iter().zip(&arg_spans) {
                    // we should not allow 'f' as a arg name because it's reserve for anon
                    // functions
                    if arg == "f" {
                        self.error(
                            Diagnostic::error()
                                .with_message("invalid argument name, 'f' is a reserved keyword for anonymous functions")
                                .with_labels(vec![Label::primary(file_id, span.clone())]),
                        )
                    }
                }

                // Collect all body expressions — implicit sequencing like Clojure
                let body_sexprs = &items[cursor..];
                if body_sexprs.is_empty() {
                    self.error(
                        Diagnostic::error()
                            .with_message("missing function body")
                            .with_labels(vec![Label::primary(file_id, span.clone())]),
                    );
                    return None;
                }
                if self.reject_ambiguous_constructor_sequence(file_id, body_sexprs) {
                    return None;
                }

                let mut body_exprs = body_sexprs
                    .iter()
                    .map(|s| self.lower_expr(file_id, s))
                    .collect::<Option<Vec<_>>>()?;

                // Multiple expressions desugar to nested LetLocal with "_" bindings
                let value = if body_exprs.len() == 1 {
                    body_exprs.remove(0)
                } else {
                    let last = body_exprs.pop().unwrap();
                    body_exprs
                        .into_iter()
                        .rev()
                        .fold(last, |body, expr| Expr::LetLocal {
                            name: "_".to_string(),
                            name_span: span.clone(),
                            value: Box::new(expr),
                            body: Box::new(body),
                            span: span.clone(),
                        })
                };

                Some(Expr::LetFunc {
                    is_pub,
                    name,
                    args,
                    arg_spans,
                    name_span,
                    value: Box::new(value),
                    span,
                })
            }

            _ => {
                self.error(
                    Diagnostic::error()
                        .with_message("invalid let syntax")
                        .with_labels(vec![Label::primary(file_id, span.clone())])
                        .with_notes(vec![
                            "valid forms: (let [x 1] ...) or (let f {x} ...)".to_string(),
                        ]),
                );
                None
            }
        }
    }
}
