use super::*;

impl Lowerer {
    pub(super) fn lower_record_construct(
        &mut self,
        file_id: usize,
        items: &[SExpr],
        span: Range<usize>,
    ) -> Option<Expr> {
        // items[0] = TypeName (Ident), items[1..] = :field val pairs
        let name = match &items[0] {
            SExpr::Atom(t) => self.source_at(file_id, t.span.clone()).to_string(),
            _ => unreachable!("record construct name must be an atom"),
        };

        let mut fields = Vec::new();
        let mut cursor = 1;

        while cursor < items.len() {
            // Expect a NamedField token
            let field_name = match items.get(cursor) {
                Some(SExpr::Atom(t)) => match &t.kind {
                    TokenKind::NamedField(f) => f.clone(),
                    _ => {
                        self.error(
                            Diagnostic::error()
                                .with_message("expected a field name")
                                .with_labels(vec![
                                    Label::primary(file_id, t.span.clone())
                                        .with_message("expected `:field_name` here"),
                                ])
                                .with_notes(vec![
                                    "syntax: (TypeName :field1 val1 :field2 val2)".into(),
                                ]),
                        );
                        return None;
                    }
                },
                Some(other) => {
                    self.error(
                        Diagnostic::error()
                            .with_message("expected a field name")
                            .with_labels(vec![
                                Label::primary(file_id, other.span())
                                    .with_message("expected `:field_name` here"),
                            ])
                            .with_notes(vec![
                                "syntax: (TypeName :field1 val1 :field2 val2)".into(),
                            ]),
                    );
                    return None;
                }
                None => break,
            };
            cursor += 1;

            // Expect a value expression
            let value = match items.get(cursor) {
                Some(sexpr) => self.lower_expr(file_id, sexpr)?,
                None => {
                    self.error(
                        Diagnostic::error()
                            .with_message(format!("missing value for field `:{field_name}`"))
                            .with_labels(vec![Label::primary(file_id, span.clone())])
                            .with_notes(vec![
                                "syntax: (TypeName :field1 val1 :field2 val2)".into(),
                            ]),
                    );
                    return None;
                }
            };
            cursor += 1;

            fields.push((field_name, value));
        }

        if fields.is_empty() {
            self.error(
                Diagnostic::error()
                    .with_message(format!("record construction of `{name}` has no fields"))
                    .with_labels(vec![Label::primary(file_id, span.clone())])
                    .with_notes(vec!["syntax: (TypeName :field1 val1 :field2 val2)".into()]),
            );
            return None;
        }

        Some(Expr::RecordConstruct { name, fields, span })
    }

    pub(super) fn lower_record_update(
        &mut self,
        file_id: usize,
        items: &[SExpr],
        span: Range<usize>,
    ) -> Option<Expr> {
        // Syntax: (with record_expr :field1 val1 :field2 val2 ...)
        if items.len() < 4 {
            self.error(
                Diagnostic::error()
                    .with_message("record update requires a record and at least one field update")
                    .with_labels(vec![Label::primary(file_id, span.clone())])
                    .with_notes(vec![
                        "syntax: (with record :field1 val1 :field2 val2)".into(),
                    ]),
            );
            return None;
        }

        let record = Box::new(self.lower_expr(file_id, &items[1])?);
        let mut updates = Vec::new();
        let mut cursor = 2;

        while cursor < items.len() {
            let field_name = match items.get(cursor) {
                Some(SExpr::Atom(t)) => match &t.kind {
                    TokenKind::NamedField(f) => f.clone(),
                    _ => {
                        self.error(
                            Diagnostic::error()
                                .with_message("expected a field name in record update")
                                .with_labels(vec![
                                    Label::primary(file_id, t.span.clone())
                                        .with_message("expected `:field_name` here"),
                                ])
                                .with_notes(vec![
                                    "syntax: (with record :field1 val1 :field2 val2)".into(),
                                ]),
                        );
                        return None;
                    }
                },
                Some(other) => {
                    self.error(
                        Diagnostic::error()
                            .with_message("expected a field name in record update")
                            .with_labels(vec![
                                Label::primary(file_id, other.span())
                                    .with_message("expected `:field_name` here"),
                            ])
                            .with_notes(vec![
                                "syntax: (with record :field1 val1 :field2 val2)".into(),
                            ]),
                    );
                    return None;
                }
                None => break,
            };
            cursor += 1;

            let value = match items.get(cursor) {
                Some(sexpr) => self.lower_expr(file_id, sexpr)?,
                None => {
                    self.error(
                        Diagnostic::error()
                            .with_message(format!(
                                "missing value for field `:{field_name}` in record update"
                            ))
                            .with_labels(vec![Label::primary(file_id, span.clone())])
                            .with_notes(vec![
                                "syntax: (with record :field1 val1 :field2 val2)".into(),
                            ]),
                    );
                    return None;
                }
            };
            cursor += 1;

            updates.push((field_name, value));
        }

        if updates.is_empty() {
            self.error(
                Diagnostic::error()
                    .with_message("record update has no fields")
                    .with_labels(vec![Label::primary(file_id, span.clone())])
                    .with_notes(vec![
                        "syntax: (with record :field1 val1 :field2 val2)".into(),
                    ]),
            );
            return None;
        }

        Some(Expr::RecordUpdate {
            record,
            updates,
            span,
        })
    }

    pub(super) fn lower_field_access(
        &mut self,
        file_id: usize,
        items: &[SExpr],
        span: Range<usize>,
    ) -> Option<Expr> {
        // items[0] is the NamedField token, items[1..] are the args
        let field_token = match &items[0] {
            SExpr::Atom(t) => t,
            _ => unreachable!("lower_field_access called without NamedField token"),
        };
        let field = match &field_token.kind {
            TokenKind::NamedField(name) => name.clone(),
            _ => unreachable!("lower_field_access called without NamedField token"),
        };

        let args = &items[1..];
        if args.len() != 1 {
            self.error(
                Diagnostic::error()
                    .with_message(format!(
                        "field accessor ':{field}' expects exactly 1 argument, found {}",
                        args.len()
                    ))
                    .with_labels(vec![
                        Label::primary(file_id, span.clone())
                            .with_message("expected (:{field} record)"),
                    ])
                    .with_notes(vec![format!("syntax: (:{field} <record>)")]),
            );
            return None;
        }

        let record = Box::new(self.lower_expr(file_id, &args[0])?);
        Some(Expr::FieldAccess {
            field,
            record,
            span,
        })
    }
}
