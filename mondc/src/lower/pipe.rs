use super::*;

impl Lowerer {
    fn count_pipe_holes(expr: &Expr) -> usize {
        match expr {
            Expr::Variable(name, _) if name == "_" => 1,
            Expr::Variable(_, _) | Expr::Literal(_, _) => 0,
            Expr::List(items, _) => items.iter().map(Self::count_pipe_holes).sum(),
            Expr::LetFunc { value, .. } => Self::count_pipe_holes(value),
            Expr::LetLocal { value, body, .. } => {
                Self::count_pipe_holes(value) + Self::count_pipe_holes(body)
            }
            Expr::If {
                cond, then, els, ..
            } => {
                Self::count_pipe_holes(cond)
                    + Self::count_pipe_holes(then)
                    + Self::count_pipe_holes(els)
            }
            Expr::Call { func, args, .. } => {
                Self::count_pipe_holes(func)
                    + args.iter().map(Self::count_pipe_holes).sum::<usize>()
            }
            Expr::Match { targets, arms, .. } => {
                targets.iter().map(Self::count_pipe_holes).sum::<usize>()
                    + arms
                        .iter()
                        .map(|arm| {
                            arm.guard.as_ref().map(Self::count_pipe_holes).unwrap_or(0)
                                + Self::count_pipe_holes(&arm.body)
                        })
                        .sum::<usize>()
            }
            Expr::FieldAccess { record, .. } => Self::count_pipe_holes(record),
            Expr::RecordConstruct { fields, .. } => fields
                .iter()
                .map(|(_, value)| Self::count_pipe_holes(value))
                .sum(),
            Expr::RecordUpdate {
                record, updates, ..
            } => {
                Self::count_pipe_holes(record)
                    + updates
                        .iter()
                        .map(|(_, value)| Self::count_pipe_holes(value))
                        .sum::<usize>()
            }
            Expr::Lambda { body, .. } => Self::count_pipe_holes(body),
            Expr::QualifiedCall { args, .. } => args.iter().map(Self::count_pipe_holes).sum(),
        }
    }

    fn substitute_pipe_hole(expr: Expr, replacement: &mut Option<Expr>) -> Expr {
        match expr {
            Expr::Variable(name, _) if name == "_" => replacement
                .take()
                .expect("pipe hole replacement should be present"),
            Expr::Variable(_, _) | Expr::Literal(_, _) => expr,
            Expr::List(items, span) => Expr::List(
                items
                    .into_iter()
                    .map(|item| Self::substitute_pipe_hole(item, replacement))
                    .collect(),
                span,
            ),
            Expr::LetFunc {
                is_pub,
                name,
                args,
                arg_spans,
                name_span,
                value,
                span,
            } => Expr::LetFunc {
                is_pub,
                name,
                args,
                arg_spans,
                name_span,
                value: Box::new(Self::substitute_pipe_hole(*value, replacement)),
                span,
            },
            Expr::LetLocal {
                name,
                name_span,
                value,
                body,
                span,
            } => Expr::LetLocal {
                name,
                name_span,
                value: Box::new(Self::substitute_pipe_hole(*value, replacement)),
                body: Box::new(Self::substitute_pipe_hole(*body, replacement)),
                span,
            },
            Expr::If {
                cond,
                then,
                els,
                span,
            } => Expr::If {
                cond: Box::new(Self::substitute_pipe_hole(*cond, replacement)),
                then: Box::new(Self::substitute_pipe_hole(*then, replacement)),
                els: Box::new(Self::substitute_pipe_hole(*els, replacement)),
                span,
            },
            Expr::Call { func, args, span } => Expr::Call {
                func: Box::new(Self::substitute_pipe_hole(*func, replacement)),
                args: args
                    .into_iter()
                    .map(|arg| Self::substitute_pipe_hole(arg, replacement))
                    .collect(),
                span,
            },
            Expr::Match {
                targets,
                arms,
                span,
            } => Expr::Match {
                targets: targets
                    .into_iter()
                    .map(|target| Self::substitute_pipe_hole(target, replacement))
                    .collect(),
                arms: arms
                    .into_iter()
                    .map(|arm| MatchArm {
                        patterns: arm.patterns,
                        guard: arm
                            .guard
                            .map(|guard| Self::substitute_pipe_hole(guard, replacement)),
                        body: Self::substitute_pipe_hole(arm.body, replacement),
                    })
                    .collect(),
                span,
            },
            Expr::FieldAccess {
                field,
                record,
                span,
            } => Expr::FieldAccess {
                field,
                record: Box::new(Self::substitute_pipe_hole(*record, replacement)),
                span,
            },
            Expr::RecordConstruct { name, fields, span } => Expr::RecordConstruct {
                name,
                fields: fields
                    .into_iter()
                    .map(|(field, value)| (field, Self::substitute_pipe_hole(value, replacement)))
                    .collect(),
                span,
            },
            Expr::RecordUpdate {
                record,
                updates,
                span,
            } => Expr::RecordUpdate {
                record: Box::new(Self::substitute_pipe_hole(*record, replacement)),
                updates: updates
                    .into_iter()
                    .map(|(field, value)| (field, Self::substitute_pipe_hole(value, replacement)))
                    .collect(),
                span,
            },
            Expr::Lambda {
                args,
                arg_spans,
                body,
                span,
            } => Expr::Lambda {
                args,
                arg_spans,
                body: Box::new(Self::substitute_pipe_hole(*body, replacement)),
                span,
            },
            Expr::QualifiedCall {
                module,
                function,
                args,
                span,
                fn_span,
            } => Expr::QualifiedCall {
                module,
                function,
                args: args
                    .into_iter()
                    .map(|arg| Self::substitute_pipe_hole(arg, replacement))
                    .collect(),
                span,
                fn_span,
            },
        }
    }

    pub(super) fn lower_pipe(
        &mut self,
        file_id: usize,
        items: &[SExpr],
        span: Range<usize>,
    ) -> Option<Expr> {
        if items.len() < 3 {
            self.error(
                Diagnostic::error()
                    .with_message("pipeline requires a value and at least one step")
                    .with_labels(vec![
                        Label::primary(file_id, span)
                            .with_message("syntax: (|> value step1 step2 ...)"),
                    ]),
            );
            return None;
        }

        let mut acc = self.lower_expr(file_id, &items[1])?;
        for step in &items[2..] {
            let step_expr = self.lower_expr(file_id, step)?;
            let hole_count = Self::count_pipe_holes(&step_expr);
            match hole_count {
                0 => {
                    let func = Box::new(step_expr);
                    // Keep each desugared call span tight so type errors point at the
                    // offending pipeline step instead of the whole pipe expression.
                    let call_span = acc.span().start..step.span().end;
                    acc = Expr::Call {
                        func,
                        args: vec![acc],
                        span: call_span,
                    };
                }
                1 => {
                    let mut replacement = Some(acc);
                    acc = Self::substitute_pipe_hole(step_expr, &mut replacement);
                }
                _ => {
                    self.error(
                        Diagnostic::error()
                            .with_message("pipeline step can contain at most one `_` placeholder")
                            .with_labels(vec![
                                Label::primary(file_id, step.span())
                                    .with_message("this step has multiple placeholders"),
                            ])
                            .with_notes(vec![
                                "use exactly one `_` to indicate where the piped value goes"
                                    .to_string(),
                            ]),
                    );
                    return None;
                }
            }
        }
        Some(acc)
    }
}
