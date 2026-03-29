use super::*;

impl Lowerer {
    pub(super) fn lower_type_decl(
        &mut self,
        file_id: usize,
        items: &[SExpr],
        span: Range<usize>,
        is_pub: bool,
    ) -> Option<TypeDecl> {
        let mut cursor = 1; // Skip the leading `type` keyword

        // 1. Parse optional generics: ['t] or ['e 'a]
        let mut params = Vec::new();
        if let Some(SExpr::Square(gen_items, _)) = items.get(cursor) {
            params = self.lower_type_params(file_id, gen_items)?;
            cursor += 1;
        }

        // 2. Get Type Name: MyType, MyGenericType, Result, etc.
        let name = match items.get(cursor) {
            Some(SExpr::Atom(t)) => self.source_at(file_id, t.span.clone()).to_string(),
            _ => return None, // Error: Missing type name
        };
        cursor += 1;

        // 3. Parse the type body group.
        // Types now require bracket bodies: [ ... ]
        let body_group = match items.get(cursor) {
            Some(group) => group,
            None => {
                self.error(
                    Diagnostic::error()
                        .with_message("type declaration is missing a body")
                        .with_labels(vec![Label::primary(file_id, span.clone())])
                        .with_notes(vec!["expected a bracket body like `[Ctor ...]`".into()]),
                );
                return None;
            }
        };
        let body_items: &[SExpr] = match body_group {
            SExpr::Square(items, _) => items.as_slice(),
            other => {
                self.error(
                    Diagnostic::error()
                        .with_message("type body must be wrapped in square brackets")
                        .with_labels(vec![Label::primary(file_id, other.span())])
                        .with_notes(vec![
                            "example variant: (type ExitReason [Normal Killed (Abnormal ~ Dynamic)])"
                                .into(),
                            "example record:  (type ExitMessage [(:pid ~ Pid) (:reason ~ ExitReason)])"
                                .into(),
                        ]),
                );
                return None;
            }
        };

        // Determine if we are building a Record or a Variant based on the first body item.
        let is_record = matches!(
            body_items.first(),
            Some(SExpr::Round(field_items, _))
                if matches!(field_items.first(), Some(SExpr::Atom(t)) if matches!(t.kind, TokenKind::NamedField(_)))
        );

        if is_record {
            // --- Lowering as a Record (Product Type) ---
            let mut fields = Vec::new();
            let mut seen_fields: HashMap<String, Range<usize>> = HashMap::new();
            for item in body_items {
                let SExpr::Round(field_items, field_span) = item else {
                    self.error(
                        Diagnostic::error()
                            .with_message("each record field must be wrapped in parentheses")
                            .with_labels(vec![
                                Label::primary(file_id, item.span())
                                    .with_message("expected `(:field ~ Type)`"),
                            ])
                            .with_notes(vec!["example: `(:x ~ Int)`".into()]),
                    );
                    return None;
                };
                let Some(SExpr::Atom(name_token)) = field_items.first() else {
                    self.error(
                        Diagnostic::error()
                            .with_message("expected field name as first item in field spec")
                            .with_labels(vec![Label::primary(file_id, field_span.clone())])
                            .with_notes(vec!["field names must start with `:`, e.g. `:x`".into()]),
                    );
                    return None;
                };
                let TokenKind::NamedField(field_name) = &name_token.kind else {
                    self.error(
                        Diagnostic::error()
                            .with_message("field name must start with `:`")
                            .with_labels(vec![
                                Label::primary(file_id, name_token.span.clone())
                                    .with_message("expected `:field_name`"),
                            ])
                            .with_notes(vec!["example: `(:x ~ Int)`".into()]),
                    );
                    return None;
                };
                let Some(SExpr::Atom(tilde_token)) = field_items.get(1) else {
                    self.error(
                        Diagnostic::error()
                            .with_message("expected `~` after field name")
                            .with_labels(vec![Label::primary(file_id, field_span.clone())])
                            .with_notes(vec!["example: `(:x ~ Int)`".into()]),
                    );
                    return None;
                };
                if !matches!(tilde_token.kind, TokenKind::Tilde) {
                    self.error(
                        Diagnostic::error()
                            .with_message("expected `~` after field name")
                            .with_labels(vec![Label::primary(file_id, tilde_token.span.clone())]),
                    );
                    return None;
                }
                let type_usage =
                    self.lower_type_usage_atoms(file_id, &field_items[2..], field_span.clone())?;
                if let Some(first_span) =
                    seen_fields.insert(field_name.clone(), name_token.span.clone())
                {
                    self.error(
                        Diagnostic::error()
                            .with_message(format!("duplicate record field `:{field_name}`"))
                            .with_labels(vec![
                                Label::primary(file_id, name_token.span.clone()).with_message(
                                    format!("`:{field_name}` is declared again here"),
                                ),
                                Label::secondary(file_id, first_span)
                                    .with_message("first declared here"),
                            ]),
                    );
                    return None;
                }
                fields.push((field_name.clone(), type_usage));
            }
            Some(TypeDecl::Record {
                is_pub,
                name,
                params,
                fields,
                span,
            })
        } else {
            // --- Lowering as a Variant (Sum Type) ---
            let mut constructors = Vec::new();
            let mut seen_ctors: HashMap<String, Range<usize>> = HashMap::new();
            for item in body_items {
                match item {
                    // Case: (Some ~ 'a) or (Error ~ 'e) or (Wrap ~ Map 'k 'v)
                    SExpr::Round(inner, inner_span) => {
                        // Expect: (ConstructorName ~ Type [TypeArgs...])
                        let (name_token, tilde_token) = match (inner.first(), inner.get(1)) {
                            (Some(n), Some(t)) if inner.len() >= 3 => (n, t),
                            _ => {
                                self.error(
                                    Diagnostic::error()
                                        .with_message(
                                            "invalid constructor — expected (Name ~ Type)",
                                        )
                                        .with_labels(vec![Label::primary(
                                            file_id,
                                            inner_span.clone(),
                                        )]),
                                );
                                return None;
                            }
                        };

                        let c_name = self.source_at(file_id, name_token.span()).to_string();
                        if !c_name.starts_with(|c: char| c.is_uppercase()) {
                            self.error(
                                Diagnostic::error()
                                    .with_message(
                                        "constructor name must start with an uppercase letter",
                                    )
                                    .with_labels(vec![
                                        Label::primary(file_id, name_token.span()).with_message(
                                            format!("'{c_name}' is not a valid constructor name"),
                                        ),
                                    ]),
                            );
                            return None;
                        }

                        let SExpr::Atom(tilde) = tilde_token else {
                            self.error(
                                Diagnostic::error()
                                    .with_message("expected '~' between constructor name and type")
                                    .with_labels(vec![Label::primary(file_id, tilde_token.span())]),
                            );
                            return None;
                        };
                        if !matches!(tilde.kind, TokenKind::Tilde) {
                            self.error(
                                Diagnostic::error()
                                    .with_message("expected '~' between constructor name and type")
                                    .with_labels(vec![Label::primary(file_id, tilde.span.clone())]),
                            );
                            return None;
                        }

                        let type_usage =
                            self.lower_type_usage_atoms(file_id, &inner[2..], inner_span.clone())?;
                        if let Some(first_span) =
                            seen_ctors.insert(c_name.clone(), name_token.span())
                        {
                            self.error(
                                Diagnostic::error()
                                    .with_message(format!(
                                        "duplicate variant constructor `{c_name}`"
                                    ))
                                    .with_labels(vec![
                                        Label::primary(file_id, name_token.span()).with_message(
                                            format!("`{c_name}` is declared again here"),
                                        ),
                                        Label::secondary(file_id, first_span)
                                            .with_message("first declared here"),
                                    ]),
                            );
                            return None;
                        }
                        constructors.push((c_name, Some(type_usage)));
                    }
                    // Case: None — nullary constructor (no payload)
                    SExpr::Atom(t) => {
                        let c_name = self.source_at(file_id, t.span.clone()).to_string();
                        if !matches!(t.kind, TokenKind::Ident)
                            || !c_name.starts_with(|c: char| c.is_uppercase())
                        {
                            self.error(
                                Diagnostic::error()
                                    .with_message("invalid variant constructor")
                                    .with_labels(vec![Label::primary(file_id, t.span.clone())
                                        .with_message(
                                            "expected a capitalised constructor name (e.g. None) or (Name ~ Type)",
                                        )]),
                            );
                            return None;
                        }
                        if let Some(first_span) = seen_ctors.insert(c_name.clone(), t.span.clone())
                        {
                            self.error(
                                Diagnostic::error()
                                    .with_message(format!(
                                        "duplicate variant constructor `{c_name}`"
                                    ))
                                    .with_labels(vec![
                                        Label::primary(file_id, t.span.clone()).with_message(
                                            format!("`{c_name}` is declared again here"),
                                        ),
                                        Label::secondary(file_id, first_span)
                                            .with_message("first declared here"),
                                    ]),
                            );
                            return None;
                        }
                        constructors.push((c_name, None));
                    }
                    other => {
                        self.error(
                            Diagnostic::error()
                                .with_message("unexpected item in variant body")
                                .with_labels(vec![
                                    Label::primary(file_id, other.span()).with_message(
                                        "expected a constructor: Name or (Name ~ Type)",
                                    ),
                                ]),
                        );
                        return None;
                    }
                }
            }
            Some(TypeDecl::Variant {
                is_pub,
                name,
                params,
                constructors,
                span: span.to_owned(),
            })
        }
    }
    pub(super) fn lower_test(
        &mut self,
        file_id: usize,
        items: &[SExpr],
        span: Range<usize>,
    ) -> Option<Declaration> {
        let file_name = self
            .files
            .get(file_id)
            .expect("invalid file_id")
            .name()
            .clone();
        let is_test_file = file_name.contains("/tests/") || file_name.starts_with("tests/");

        if !is_test_file {
            self.error(
                Diagnostic::error()
                    .with_message("`test` declarations are only allowed in the `tests/` directory")
                    .with_labels(vec![
                        Label::primary(file_id, span)
                            .with_message("move this to a file under `tests/`"),
                    ]),
            );
            return None;
        }

        // items[0] = `test` keyword
        // items[1] = string name literal
        // items[2] = body expression
        let name = match items.get(1) {
            Some(SExpr::Atom(t)) if matches!(t.kind, TokenKind::String) => {
                let raw = self.source_at(file_id, t.span.clone());
                raw[1..raw.len() - 1].to_string()
            }
            _ => {
                self.error(
                    Diagnostic::error()
                        .with_message("expected a string name after `test`")
                        .with_labels(vec![Label::primary(
                            file_id,
                            items.first().map(|s| s.span()).unwrap_or(0..0),
                        )])
                        .with_notes(vec!["example: `(test \"my test\" ...)`".into()]),
                );
                return None;
            }
        };

        let body_sexpr = match items.get(2) {
            Some(b) => b,
            None => {
                self.error(
                    Diagnostic::error()
                        .with_message("expected body expression after test name")
                        .with_labels(vec![Label::primary(
                            file_id,
                            items.get(1).map(|s| s.span()).unwrap_or(0..0),
                        )]),
                );
                return None;
            }
        };

        let body = self.lower_expr(file_id, body_sexpr)?;

        Some(Declaration::Test {
            name,
            body: Box::new(body),
            span: items.first().map(|s| s.span()).unwrap_or(0..0),
        })
    }
}
