use super::*;

impl Lowerer {
    pub(super) fn lower_match(
        &mut self,
        file_id: usize,
        items: &[SExpr],
        span: Range<usize>,
    ) -> Option<Expr> {
        // items[0] = match keyword; items[1..] = targets followed by arms.
        // Minimum valid: (match x pat ~> res) = 5 items.
        if items.len() < 5 {
            self.error(
                Diagnostic::error()
                    .with_message("match expression is too short")
                    .with_labels(vec![Label::primary(file_id, span.clone())])
                    .with_notes(vec![
                        "syntax: (match <target> <pattern> ~> <expr> ...)".into(),
                    ]),
            );
            return None;
        }

        // Find the position of the first '~>' arrow in items[1..].
        let first_arrow = items[1..]
            .iter()
            .position(|s| matches!(s, SExpr::Atom(t) if matches!(t.kind, TokenKind::Arrow)));
        let Some(first_arrow) = first_arrow else {
            self.error(
                Diagnostic::error()
                    .with_message("match expression has no arms")
                    .with_labels(vec![Label::primary(file_id, span.clone())])
                    .with_notes(vec![
                        "syntax: (match <target> <pattern> ~> <expr> ...)".into(),
                    ]),
            );
            return None;
        };
        // first_arrow is index within items[1..], so absolute index = first_arrow + 1.
        let first_arrow_abs = first_arrow + 1;

        // items[1..first_arm_head_end] are targets + first-arm patterns.
        // If the first arm has a guard (`... if <expr> ~>`), strip the guard
        // marker and expression from this prefix before inferring target count.
        let mut first_arm_head_end = first_arrow_abs;
        if first_arm_head_end >= 2
            && matches!(
                items.get(first_arm_head_end - 1),
                Some(SExpr::Atom(t)) if matches!(t.kind, TokenKind::If)
            )
        {
            let if_span = items[first_arm_head_end - 1].span();
            self.error(
                Diagnostic::error()
                    .with_message("missing guard expression after `if` in match arm")
                    .with_labels(vec![Label::primary(file_id, if_span)]),
            );
            return None;
        }
        if first_arm_head_end >= 3
            && matches!(
                items.get(first_arm_head_end - 2),
                Some(SExpr::Atom(t)) if matches!(t.kind, TokenKind::If)
            )
        {
            first_arm_head_end -= 2;
        }

        // Detect or-patterns: if any match-alternative separator appears
        // before the first arrow, single-target mode.
        let has_or = items[1..first_arm_head_end]
            .iter()
            .any(|s| self.is_match_alt_separator(file_id, s));

        let n_targets = if has_or {
            1
        } else {
            // items between 'match' and first arm arrow, excluding optional guard prefix
            let n_items = first_arm_head_end - 1;
            if n_items == 0 || n_items % 2 != 0 {
                self.error(
                    Diagnostic::error()
                        .with_message("match has an unequal number of targets and patterns")
                        .with_labels(vec![Label::primary(file_id, span.clone())])
                        .with_notes(vec![
                            "each arm must have the same number of patterns as there are targets"
                                .into(),
                        ]),
                );
                return None;
            }
            n_items / 2
        };

        // Parse targets.
        let mut targets = Vec::with_capacity(n_targets);
        for i in 0..n_targets {
            targets.push(self.lower_expr(file_id, &items[1 + i])?);
        }

        // Parse arms. cursor starts just after the targets.
        let mut cursor = 1 + n_targets;
        let mut arms = Vec::new();

        while cursor < items.len() {
            let mut arm_patterns = Vec::with_capacity(n_targets);

            for _i in 0..n_targets {
                if cursor >= items.len() {
                    self.error(
                        Diagnostic::error()
                            .with_message("incomplete match arm: missing pattern")
                            .with_labels(vec![Label::primary(file_id, span.clone())]),
                    );
                    return None;
                }

                let pat = self.lower_pattern(file_id, &items[cursor])?;
                cursor += 1;

                // In single-target mode, collect `|`-separated alternatives.
                let final_pat = if n_targets == 1 {
                    let mut or_pats = vec![pat];
                    while let Some(SExpr::Atom(t)) = items.get(cursor) {
                        if self.is_match_alt_separator(file_id, &items[cursor]) {
                            cursor += 1; // consume separator
                            if cursor >= items.len() {
                                self.error(
                                    Diagnostic::error()
                                        .with_message("expected a pattern after '|'")
                                        .with_labels(vec![Label::primary(file_id, t.span.clone())]),
                                );
                                return None;
                            }
                            let next = self.lower_pattern(file_id, &items[cursor])?;
                            cursor += 1;
                            or_pats.push(next);
                        } else {
                            break;
                        }
                    }
                    if or_pats.len() == 1 {
                        or_pats.remove(0)
                    } else {
                        Pattern::Or(or_pats, span.clone())
                    }
                } else {
                    pat
                };

                arm_patterns.push(final_pat);
            }

            // Optional guard: `... if <guard-expr> ~> ...`
            let guard = match items.get(cursor) {
                Some(SExpr::Atom(token)) if matches!(token.kind, TokenKind::If) => {
                    let if_span = token.span.clone();
                    cursor += 1;
                    let Some(guard_sexpr) = items.get(cursor) else {
                        self.error(
                            Diagnostic::error()
                                .with_message("missing guard expression after `if` in match arm")
                                .with_labels(vec![Label::primary(file_id, if_span)]),
                        );
                        return None;
                    };
                    let guard = self.lower_expr(file_id, guard_sexpr)?;
                    cursor += 1;
                    Some(guard)
                }
                _ => None,
            };

            // Expect and consume '~>'.
            match items.get(cursor) {
                Some(SExpr::Atom(token)) if matches!(token.kind, TokenKind::Arrow) => {
                    cursor += 1;
                }
                _ => {
                    self.error(
                        Diagnostic::error()
                            .with_message(
                                "match arms must use '~>' to separate the pattern and result",
                            )
                            .with_notes(vec!["help: write `pattern ~> expression`".to_string()])
                            .with_labels(vec![Label::primary(file_id, span.clone())]),
                    );
                    return None;
                }
            }

            // Expect the result expression.
            let result_sexpr = match items.get(cursor) {
                Some(s) => s,
                None => {
                    self.error(
                        Diagnostic::error()
                            .with_message("missing result expression after '~>'")
                            .with_labels(vec![Label::primary(file_id, span.clone())]),
                    );
                    return None;
                }
            };
            let body = self.lower_expr(file_id, result_sexpr)?;
            cursor += 1;

            // Check for the common mistake of writing multiple expressions in a match arm
            // without `do`. The next item is a Round (call expression) but there's no `~>`
            // n_targets positions after it, meaning it can't be a pattern-arrow pair.
            if let Some(next) = items.get(cursor)
                && matches!(next, SExpr::Round(..))
            {
                let next_has_arrow = self.looks_like_match_arm_start(items, cursor, n_targets);
                if !next_has_arrow {
                    self.error(
                        Diagnostic::error()
                            .with_message(
                                "match arm has multiple expressions — use `do` to sequence them",
                            )
                            .with_labels(vec![Label::primary(file_id, next.span())])
                            .with_notes(vec![
                                "help: wrap the expressions in `do`: `pat ~> (do expr1 expr2 ...)`"
                                    .to_string(),
                            ]),
                    );
                    return None;
                }
            }

            arms.push(MatchArm {
                patterns: arm_patterns,
                guard,
                body,
            });
        }

        Some(Expr::Match {
            targets,
            arms,
            span,
        })
    }

    fn is_match_alt_separator(&self, file_id: usize, sexpr: &SExpr) -> bool {
        let SExpr::Atom(token) = sexpr else {
            return false;
        };
        matches!(token.kind, TokenKind::Operator)
            && self.source_at(file_id, token.span.clone()) == "|"
    }

    fn looks_like_match_arm_start(&self, items: &[SExpr], cursor: usize, n_targets: usize) -> bool {
        let mut idx = cursor;

        for _ in 0..n_targets {
            if idx >= items.len() {
                return false;
            }
            idx += 1;
        }

        if matches!(
            items.get(idx),
            Some(SExpr::Atom(token)) if matches!(token.kind, TokenKind::If)
        ) {
            idx += 1;
            if idx >= items.len() {
                return false;
            }
            idx += 1;
        }

        matches!(
            items.get(idx),
            Some(SExpr::Atom(token)) if matches!(token.kind, TokenKind::Arrow)
        )
    }

    pub(super) fn lower_pattern(&mut self, file_id: usize, sexpr: &SExpr) -> Option<Pattern> {
        match sexpr {
            SExpr::Atom(token) => {
                let text = self.source_at(file_id, token.span.clone());
                let span = token.span.clone();

                match &token.kind {
                    // Handle "_" -> Pattern::Any
                    TokenKind::Ident if text == "_" => Some(Pattern::Any(span)),

                    // Capitalised identifier -> nullary constructor (e.g. None, True variant)
                    TokenKind::Ident if text.starts_with(|c: char| c.is_uppercase()) => {
                        Some(Pattern::Constructor(text.to_string(), vec![], span))
                    }

                    // Qualified constructor in pattern (e.g. option/None)
                    TokenKind::QualifiedIdent((module, constructor))
                        if constructor.starts_with(|c: char| c.is_uppercase()) =>
                    {
                        Some(Pattern::Constructor(
                            format!("{module}/{constructor}"),
                            vec![],
                            span,
                        ))
                    }

                    // Lower-case identifier -> Pattern::Variable binding
                    TokenKind::Ident => Some(Pattern::Variable(text.to_string(), span)),

                    // Handle literals
                    TokenKind::Int(v) => Some(Pattern::Literal(Literal::Int(*v), span)),
                    TokenKind::Float(v) => Some(Pattern::Literal(Literal::Float(*v), span)),
                    TokenKind::Bool(v) => Some(Pattern::Literal(Literal::Bool(*v), span)),
                    TokenKind::String => {
                        let raw = self.source_at(file_id, token.span.clone());
                        let s = raw[1..raw.len() - 1].to_string();
                        Some(Pattern::Literal(Literal::String(s), span))
                    }

                    TokenKind::QualifiedIdent(_) => {
                        self.error(
                            Diagnostic::error()
                                .with_code("E005")
                                .with_message("invalid pattern")
                                .with_labels(vec![Label::primary(file_id, span).with_message(
                                    "qualified patterns must reference constructors like `option/Some`",
                                )]),
                        );
                        None
                    }

                    _ => {
                        self.error(
                            Diagnostic::error()
                                .with_code("E005")
                                .with_message("invalid pattern")
                                .with_labels(vec![Label::primary(file_id, span).with_message(
                                    format!(
                                        "found {:?}, expected identifier, literal, or '_'",
                                        token.kind
                                    ),
                                )]),
                        );
                        None
                    }
                }
            }

            SExpr::Round(items, span) => {
                if let [SExpr::Atom(token), rest @ ..] = items.as_slice() {
                    let name = match &token.kind {
                        TokenKind::Ident => self.source_at(file_id, token.span.clone()).to_string(),
                        TokenKind::QualifiedIdent((module, constructor)) => {
                            format!("{module}/{constructor}")
                        }
                        _ => {
                            self.error(
                                Diagnostic::error()
                                    .with_code("E005")
                                    .with_message("invalid constructor pattern")
                                    .with_labels(vec![
                                        Label::primary(file_id, span.clone()).with_message(
                                            "expected (ConstructorName <pattern>...)",
                                        ),
                                    ]),
                            );
                            return None;
                        }
                    };
                    let is_record_pattern = !rest.is_empty()
                        && rest.iter().step_by(2).all(|item| {
                            matches!(item, SExpr::Atom(t) if matches!(t.kind, TokenKind::NamedField(_)))
                        });

                    if is_record_pattern {
                        if rest.len() % 2 != 0 {
                            self.error(
                                Diagnostic::error()
                                    .with_code("E005")
                                    .with_message("invalid record pattern")
                                    .with_labels(vec![
                                        Label::primary(file_id, span.clone())
                                            .with_message("expected `:field pattern` pairs"),
                                    ]),
                            );
                            return None;
                        }

                        let mut fields = Vec::new();
                        let mut cursor = 0;
                        while cursor < rest.len() {
                            let (field_name, field_span) = match &rest[cursor] {
                                SExpr::Atom(t) => match &t.kind {
                                    TokenKind::NamedField(field) => (field.clone(), t.span.clone()),
                                    _ => unreachable!("validated record field token"),
                                },
                                _ => unreachable!("validated record field token"),
                            };
                            let pat = self.lower_pattern(file_id, &rest[cursor + 1])?;
                            fields.push((field_name, pat, field_span));
                            cursor += 2;
                        }

                        return Some(Pattern::Record {
                            name,
                            fields,
                            span: span.clone(),
                        });
                    }

                    let mut lowered_args = Vec::new();
                    for arg in rest {
                        lowered_args.push(self.lower_pattern(file_id, arg)?);
                    }

                    return Some(Pattern::Constructor(name, lowered_args, span.clone()));
                }

                self.error(
                    Diagnostic::error()
                        .with_code("E005")
                        .with_message("invalid constructor pattern")
                        .with_labels(vec![
                            Label::primary(file_id, span.clone())
                                .with_message("expected (ConstructorName <pattern>...)"),
                        ]),
                );
                None
            }

            // List patterns:
            // - `[]`            => empty list
            // - `[h | t]`       => cons
            // - `[a b c]`       => sugar for `[a | [b | [c | []]]]`
            // - `[a b | tail]`  => sugar for `[a | [b | tail]]`
            SExpr::Square(items, span) => {
                if items.is_empty() {
                    return Some(Pattern::EmptyList(span.clone()));
                }
                // Find all `|` separators, if any.
                let pipe_positions: Vec<usize> = items
                    .iter()
                    .enumerate()
                    .filter_map(|(idx, item)| {
                        if let SExpr::Atom(t) = item
                            && t.kind == TokenKind::Operator
                            && self.source_at(file_id, t.span.clone()) == "|"
                        {
                            return Some(idx);
                        }
                        None
                    })
                    .collect();

                match pipe_positions.as_slice() {
                    // `[a b c]` -> `[a | [b | [c | []]]]`
                    [] => {
                        let mut acc = Pattern::EmptyList(span.clone());
                        for head_expr in items.iter().rev() {
                            let head = self.lower_pattern(file_id, head_expr)?;
                            acc = Pattern::Cons(Box::new(head), Box::new(acc), span.clone());
                        }
                        Some(acc)
                    }

                    // `[h1 h2 ... | tail]`
                    [pipe_pos] if *pipe_pos > 0 && (*pipe_pos + 2) == items.len() => {
                        let heads = &items[..*pipe_pos];
                        let tail = self.lower_pattern(file_id, &items[*pipe_pos + 1])?;
                        let mut acc = tail;
                        for head_expr in heads.iter().rev() {
                            let head = self.lower_pattern(file_id, head_expr)?;
                            acc = Pattern::Cons(Box::new(head), Box::new(acc), span.clone());
                        }
                        Some(acc)
                    }

                    _ => {
                        self.error(
                            Diagnostic::error()
                                .with_code("E005")
                                .with_message("invalid list pattern")
                                .with_labels(vec![
                                    Label::primary(file_id, span.clone()).with_message(
                                        "expected `[]`, `[h1 ... hn]`, or `[h1 ... hn | tail]`",
                                    ),
                                ])
                                .with_notes(vec![
                                    "hint: examples: `[x]`, `[x y]`, `[h | t]`, `[h1 h2 | t]`"
                                        .into(),
                                ]),
                        );
                        None
                    }
                }
            }

            // Rejection of syntax variants not valid in patterns
            other => {
                let span = other.span();
                self.error(
                    Diagnostic::error()
                        .with_code("E005")
                        .with_message("unsupported pattern structure")
                        .with_labels(vec![
                            Label::primary(file_id, span)
                                .with_message("this structure cannot be used as a pattern"),
                        ])
                        .with_notes(vec![
                            "hint: patterns only support atoms and constructor lists".into(),
                        ]),
                );
                None
            }
        }
    }
}
