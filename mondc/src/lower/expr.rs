use super::*;

impl Lowerer {
    fn lower_atom(&mut self, file_id: usize, token: &Token) -> Option<Expr> {
        match &token.kind {
            TokenKind::Int(val) => Some(Expr::Literal(Literal::Int(*val), token.span.clone())),
            TokenKind::Float(val) => Some(Expr::Literal(Literal::Float(*val), token.span.clone())),
            TokenKind::Bool(val) => Some(Expr::Literal(Literal::Bool(*val), token.span.clone())),
            TokenKind::String => {
                let raw = self.source_at(file_id, token.span.clone());
                // Strip surrounding double quotes
                let s = raw[1..raw.len() - 1].to_string();
                Some(Expr::Literal(Literal::String(s), token.span.clone()))
            }

            // Identifier, operator, or keyword-as-function used as a variable reference
            TokenKind::Ident | TokenKind::Operator | TokenKind::Or | TokenKind::And => {
                let name = self.source_at(file_id, token.span.clone());
                Some(Expr::Variable(name.to_string(), token.span.clone()))
            }

            // Qualified ident in value position — function value (like any other identifier).
            // Calls remain explicit via `(module/function ...)`.
            TokenKind::QualifiedIdent((module, function)) => Some(Expr::Variable(
                format!("{module}/{function}"),
                token.span.clone(),
            )),

            // Field accessor used as a bare atom, not wrapped in parens
            TokenKind::NamedField(name) => {
                self.error(
                    Diagnostic::error()
                        .with_message(format!(
                            "field accessor ':{name}' must be used as '(:{name} record)'"
                        ))
                        .with_labels(vec![
                            Label::primary(file_id, token.span.clone())
                                .with_message("field accessors cannot be used standalone"),
                        ])
                        .with_notes(vec![format!(
                            "hint: use (:{name} <record>) to access a field"
                        )]),
                );
                None
            }

            // Catch keywords that wandered into the wrong place
            TokenKind::Let | TokenKind::If | TokenKind::Match | TokenKind::Do | TokenKind::With => {
                self.error(Diagnostic {
                    severity: Severity::Error,
                    code: Some("E003".to_string()),
                    message: "Unexpected keyword".to_string(),
                    labels: vec![Label {
                        style: LabelStyle::Primary,
                        file_id,
                        range: token.span.to_owned(),
                        message: "".to_string(),
                    }],
                    notes: vec![],
                });

                None
            }

            _ => {
                self.error(
                    Diagnostic::error()
                        .with_message("unexpected token")
                        .with_labels(vec![Label::primary(file_id, token.span.clone())]),
                );
                None
            }
        }
    }

    fn lower_list(&mut self, file_id: usize, items: &[SExpr], span: Range<usize>) -> Option<Expr> {
        let lowered_items = items
            .iter()
            .map(|item| self.lower_expr(file_id, item))
            .collect::<Option<Vec<_>>>()?;
        Some(Expr::List(lowered_items, span))
    }

    pub fn lower_expr(&mut self, file_id: usize, sexpr: &SExpr) -> Option<Expr> {
        match sexpr {
            SExpr::Atom(token) => self.lower_atom(file_id, token),
            SExpr::Square(items, span) => self.lower_list(file_id, items, span.to_owned()),
            SExpr::Curly(_, span) => {
                self.error(Diagnostic {
                    severity: Severity::Error,
                    code: Some("E001".to_string()),
                    message: "Curly brackets are used to define arguments and should only follow name of the function".to_string(),
                    labels: vec![Label{ style: LabelStyle::Primary, file_id, range : span.to_owned(), message: "".to_string() }],
                    notes: vec![],
                 });
                None
            }
            SExpr::Round(items, span) => {
                if items.is_empty() {
                    return Some(Expr::Literal(Literal::Unit, span.clone()));
                }

                // Peek at the first item to see if it's a Keyword or a Call
                if let SExpr::Atom(token) = &items[0] {
                    match &token.kind {
                        TokenKind::Let => {
                            return self.lower_let(file_id, items, span.clone(), false);
                        }
                        TokenKind::LetBind => {
                            return self.lower_let_bind(file_id, items, span.clone());
                        }
                        TokenKind::Fn => {
                            return self.lower_lambda(file_id, items, span.clone());
                        }
                        TokenKind::If => return self.lower_if(file_id, items, span.clone()),
                        TokenKind::Match => return self.lower_match(file_id, items, span.clone()),
                        TokenKind::Do => return self.lower_do(file_id, items, span.clone()),
                        TokenKind::With => {
                            return self.lower_record_update(file_id, items, span.clone());
                        }
                        TokenKind::NamedField(_) => {
                            return self.lower_field_access(file_id, items, span.clone());
                        }
                        TokenKind::Ident => {
                            let name = self.source_at(file_id, token.span.clone());
                            // (TypeName :field val ...) — named-field record construction
                            // Detect: capitalised ident followed by a NamedField token
                            if name.starts_with(|c: char| c.is_uppercase())
                                && let Some(SExpr::Atom(t2)) = items.get(1)
                                && matches!(t2.kind, TokenKind::NamedField(_))
                            {
                                return self.lower_record_construct(file_id, items, span.clone());
                            }
                        }
                        // Cross-module call: (module/function arg1 arg2)
                        TokenKind::QualifiedIdent((module, function)) => {
                            let module = module.clone();
                            let function = function.clone();
                            let fn_span = token.span.clone();
                            let args = items[1..]
                                .iter()
                                .map(|s| self.lower_expr(file_id, s))
                                .collect::<Option<Vec<_>>>()?;
                            return Some(Expr::QualifiedCall {
                                module,
                                function,
                                args,
                                span: span.clone(),
                                fn_span,
                            });
                        }
                        _ => {} // Fall through to function call
                    }
                }

                // If it's not a keyword, it's a function call: (func arg1 arg2)
                self.lower_call(file_id, items, span.clone())
            }
        }
    }

    fn lower_call(&mut self, file_id: usize, items: &[SExpr], span: Range<usize>) -> Option<Expr> {
        if let Some(SExpr::Atom(token)) = items.first()
            && matches!(token.kind, TokenKind::Operator)
            && self.source_at(file_id, token.span.clone()) == "|>"
        {
            return self.lower_pipe(file_id, items, span);
        }

        // 1. The first item is the function being called
        // We call lower_expr recursively because it might be a
        // variable (f x) or a nested list ((get_fn) x)
        let func = Box::new(self.lower_expr(file_id, &items[0])?);

        // 2. The remaining items are the arguments
        let mut args = Vec::with_capacity(items.len() - 1);
        for arg_sexpr in &items[1..] {
            args.push(self.lower_expr(file_id, arg_sexpr)?);
        }

        Some(Expr::Call { func, args, span })
    }
}
