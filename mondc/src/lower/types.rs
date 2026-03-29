use super::*;

impl Lowerer {
    pub(super) fn lower_type_usage_atoms(
        &mut self,
        file_id: usize,
        items: &[SExpr],
        err_span: std::ops::Range<usize>,
    ) -> Option<TypeUsage> {
        if items.is_empty() {
            self.error(
                Diagnostic::error()
                    .with_message("expected a type after `~`")
                    .with_labels(vec![
                        Label::primary(file_id, err_span).with_message("expected a type name here"),
                    ]),
            );
            return None;
        }
        if items.len() == 1 {
            return self.lower_type_usage_sexpr(file_id, &items[0]);
        }
        self.lower_type_usage_application(file_id, items, err_span)
    }

    fn lower_type_usage_sexpr(&mut self, file_id: usize, sexpr: &SExpr) -> Option<TypeUsage> {
        match sexpr {
            SExpr::Atom(token) => {
                let text = self.source_at(file_id, token.span.clone()).to_string();
                Some(match token.kind {
                    TokenKind::Generic => TypeUsage::Generic(text, token.span.clone()),
                    _ => TypeUsage::Named(text, token.span.clone()),
                })
            }
            SExpr::Round(items, span) => self.lower_type_usage_round(file_id, items, span.clone()),
            other => {
                self.error(
                    Diagnostic::error()
                        .with_message("expected a type")
                        .with_labels(vec![Label::primary(file_id, other.span()).with_message(
                        "expected a type variable, type name, or parenthesised type application",
                    )]),
                );
                None
            }
        }
    }

    fn lower_type_usage_round(
        &mut self,
        file_id: usize,
        items: &[SExpr],
        span: std::ops::Range<usize>,
    ) -> Option<TypeUsage> {
        // Parenthesised type: (A -> B -> C), parsed right-associatively.
        // Each segment between `->` tokens may itself be a type application.
        if items.is_empty() {
            self.error(
                Diagnostic::error()
                    .with_message("empty type is not allowed")
                    .with_labels(vec![Label::primary(file_id, span)])
                    .with_notes(vec!["use `Unit` for the unit type".into()]),
            );
            return None;
        }

        let mut groups: Vec<Vec<&SExpr>> = Vec::new();
        let mut current: Vec<&SExpr> = Vec::new();
        let mut saw_arrow = false;
        for item in items {
            if let SExpr::Atom(t) = item
                && matches!(t.kind, TokenKind::ThinArrow)
            {
                saw_arrow = true;
                if !current.is_empty() {
                    groups.push(std::mem::take(&mut current));
                }
            } else {
                current.push(item);
            }
        }
        if !current.is_empty() {
            groups.push(current);
        }

        if !saw_arrow {
            return self.lower_type_usage_application(file_id, items, span);
        }
        if groups.is_empty() {
            self.error(
                Diagnostic::error()
                    .with_message("empty type signature")
                    .with_labels(vec![Label::primary(file_id, span)]),
            );
            return None;
        }

        let lower_group = |this: &mut Self, group: Vec<&SExpr>| -> Option<TypeUsage> {
            if group.len() == 1 {
                this.lower_type_usage_sexpr(file_id, group[0])
            } else {
                let group_span = group
                    .first()
                    .map(|s| s.span().start)
                    .zip(group.last().map(|s| s.span().end))
                    .map(|(start, end)| start..end)
                    .unwrap_or(0..0);
                this.lower_type_usage_application(
                    file_id,
                    &group.into_iter().cloned().collect::<Vec<_>>(),
                    group_span,
                )
            }
        };

        let mut lowered: Vec<TypeUsage> = groups
            .into_iter()
            .map(|g| lower_group(self, g))
            .collect::<Option<Vec<_>>>()?;

        let last = lowered.pop().unwrap();
        Some(lowered.into_iter().rev().fold(last, |acc, seg| {
            let span = seg.span().start..acc.span().end;
            TypeUsage::Fun(Box::new(seg), Box::new(acc), span)
        }))
    }

    fn lower_type_usage_application(
        &mut self,
        file_id: usize,
        items: &[SExpr],
        span: std::ops::Range<usize>,
    ) -> Option<TypeUsage> {
        if items.is_empty() {
            self.error(
                Diagnostic::error()
                    .with_message("expected a type constructor name")
                    .with_labels(vec![
                        Label::primary(file_id, span)
                            .with_message("empty `()` is not a valid type"),
                    ]),
            );
            return None;
        }
        let head_sexpr = &items[0];
        let SExpr::Atom(head_tok) = head_sexpr else {
            self.error(
                Diagnostic::error()
                    .with_message("expected a type constructor name")
                    .with_labels(vec![Label::primary(file_id, head_sexpr.span())]),
            );
            return None;
        };
        let head_text = self.source_at(file_id, head_tok.span.clone()).to_string();
        if items.len() == 1 {
            return Some(match head_tok.kind {
                TokenKind::Generic => TypeUsage::Generic(head_text, head_tok.span.clone()),
                _ => TypeUsage::Named(head_text, head_tok.span.clone()),
            });
        }
        // Multiple items: head is the constructor, rest are type arguments.
        let mut args = Vec::new();
        for arg_sexpr in &items[1..] {
            let arg = self.lower_type_usage_sexpr(file_id, arg_sexpr)?;
            args.push(arg);
        }
        Some(TypeUsage::App(head_text, args, span))
    }

    /// Parse a type signature used inside `extern` declarations.
    /// Syntax: a single atom (Int, String, 'a, ...) or a parenthesised (A -> B -> C).
    fn lower_type_sig(&mut self, file_id: usize, sexpr: &SExpr) -> Option<TypeSig> {
        match sexpr {
            SExpr::Atom(token) => {
                let text = self.source_at(file_id, token.span.clone()).to_string();
                match &token.kind {
                    TokenKind::Generic => Some(TypeSig::Generic(text)),
                    TokenKind::Ident | TokenKind::QualifiedIdent(_) => Some(TypeSig::Named(text)),
                    _ => {
                        self.error(
                            Diagnostic::error()
                                .with_message("expected a type name")
                                .with_labels(vec![
                                    Label::primary(file_id, token.span.clone())
                                        .with_message("expected a type like Int, String, or 'a"),
                                ]),
                        );
                        None
                    }
                }
            }
            SExpr::Round(items, span) => {
                // Parenthesised type: (A -> B -> C) parsed right-associatively,
                // where each segment between `->` tokens may be a type application
                // (e.g. `Map 'k 'v` is three consecutive atoms treated as App("Map", ['k,'v])).
                if items.is_empty() {
                    self.error(
                        Diagnostic::error()
                            .with_message("empty type is not allowed")
                            .with_labels(vec![Label::primary(file_id, span.clone())])
                            .with_notes(vec!["use `Unit` for the unit type".into()]),
                    );
                    return None;
                }

                // Group consecutive non-arrow items; each group becomes one TypeSig.
                let mut groups: Vec<Vec<&SExpr>> = Vec::new();
                let mut current: Vec<&SExpr> = Vec::new();
                for item in items {
                    if let SExpr::Atom(t) = item
                        && matches!(t.kind, TokenKind::ThinArrow)
                    {
                        if !current.is_empty() {
                            groups.push(std::mem::take(&mut current));
                        }
                    } else {
                        current.push(item);
                    }
                }
                if !current.is_empty() {
                    groups.push(current);
                }

                if groups.is_empty() {
                    self.error(
                        Diagnostic::error()
                            .with_message("empty type signature")
                            .with_labels(vec![Label::primary(file_id, span.clone())]),
                    );
                    return None;
                }

                // Lower each group: single item → recurse; multiple items → type application.
                let lower_group = |this: &mut Self, group: Vec<&SExpr>| -> Option<TypeSig> {
                    if group.len() == 1 {
                        this.lower_type_sig(file_id, group[0])
                    } else {
                        // First atom is the type constructor, rest are type arguments.
                        let head = match group[0] {
                            SExpr::Atom(t)
                                if matches!(
                                    t.kind,
                                    TokenKind::Ident | TokenKind::QualifiedIdent(_)
                                ) =>
                            {
                                this.source_at(file_id, t.span.clone()).to_string()
                            }
                            other => {
                                this.error(
                                    Diagnostic::error()
                                        .with_message("expected a type constructor name")
                                        .with_labels(vec![Label::primary(file_id, other.span())]),
                                );
                                return None;
                            }
                        };
                        let args: Vec<TypeSig> = group[1..]
                            .iter()
                            .map(|a| this.lower_type_sig(file_id, a))
                            .collect::<Option<Vec<_>>>()?;
                        Some(TypeSig::App(head, args))
                    }
                };

                let mut lowered: Vec<TypeSig> = groups
                    .into_iter()
                    .map(|g| lower_group(self, g))
                    .collect::<Option<Vec<_>>>()?;

                let last = lowered.pop().unwrap();
                Some(
                    lowered
                        .into_iter()
                        .rev()
                        .fold(last, |acc, seg| TypeSig::Fun(Box::new(seg), Box::new(acc))),
                )
            }
            other => {
                self.error(
                    Diagnostic::error()
                        .with_message("invalid type in extern signature")
                        .with_labels(vec![Label::primary(file_id, other.span())]),
                );
                None
            }
        }
    }

    /// Dispatch between `extern let` and `extern type`.
    pub(super) fn lower_extern_dispatch(
        &mut self,
        file_id: usize,
        items: &[SExpr],
        span: Range<usize>,
        is_pub: bool,
    ) -> Option<Declaration> {
        match items.get(1) {
            Some(SExpr::Atom(t)) if matches!(t.kind, TokenKind::Let) => {
                self.lower_extern_let(file_id, items, span, is_pub)
            }
            Some(SExpr::Atom(t)) if matches!(t.kind, TokenKind::Type) => {
                self.lower_extern_type(file_id, items, span, is_pub)
            }
            other => {
                self.error(
                    Diagnostic::error()
                        .with_message("expected 'let' or 'type' after 'extern'")
                        .with_labels(vec![Label::primary(
                            file_id,
                            other.map(|s| s.span()).unwrap_or(span),
                        )])
                        .with_notes(vec![
                            "syntax: (extern let name ~ (Type -> Type) module/function)".into(),
                            "syntax: (extern type ['k 'v] Name [module/type])".into(),
                        ]),
                );
                None
            }
        }
    }

    /// Lower an `extern let` declaration.
    /// Syntax: (extern let name ~ (TypeSig) module/function)
    ///      or (extern let name ~ (Unit -> ReturnType) module/function)  -- nullary
    fn lower_extern_let(
        &mut self,
        file_id: usize,
        items: &[SExpr],
        span: Range<usize>,
        is_pub: bool,
    ) -> Option<Declaration> {
        // items[0]=extern, items[1]=let, items[2]=name, items[3]=~, items[4]=TypeSig, items[5]=target

        if items.len() != 6 {
            self.error(
                Diagnostic::error()
                    .with_message("invalid extern let declaration")
                    .with_labels(vec![Label::primary(file_id, span.clone())])
                    .with_notes(vec![
                        "syntax: (extern let name ~ (Type -> Type) module/function)".into(),
                        "syntax: (extern let name ~ (Unit -> ReturnType) module/function)".into(),
                    ]),
            );
            return None;
        }

        let name = match &items[2] {
            SExpr::Atom(t) if matches!(t.kind, TokenKind::Ident) => {
                self.source_at(file_id, t.span.clone()).to_string()
            }
            other => {
                self.error(
                    Diagnostic::error()
                        .with_message("expected a function name in extern let")
                        .with_labels(vec![Label::primary(file_id, other.span())]),
                );
                return None;
            }
        };

        match &items[3] {
            SExpr::Atom(t) if matches!(t.kind, TokenKind::Tilde) => {}
            other => {
                self.error(
                    Diagnostic::error()
                        .with_message("expected '~' after extern let name")
                        .with_labels(vec![Label::primary(file_id, other.span())])
                        .with_notes(vec![
                            "syntax: (extern let name ~ (Type -> Type) module/function)".into(),
                            "syntax: (extern let name ~ (Unit -> ReturnType) module/function)"
                                .into(),
                        ]),
                );
                return None;
            }
        }

        let raw_ty = self.lower_type_sig(file_id, &items[4])?;
        let (is_nullary, ty) = match raw_ty {
            TypeSig::Fun(arg, ret) if matches!(arg.as_ref(), TypeSig::Named(name) if name == "Unit") => {
                (true, *ret)
            }
            other => (false, other),
        };

        let erlang_target = match &items[5] {
            SExpr::Atom(t) => match &t.kind {
                TokenKind::QualifiedIdent((module, func)) => (module.clone(), func.clone()),
                _ => {
                    self.error(
                        Diagnostic::error()
                            .with_message("expected an Erlang target as module/function")
                            .with_labels(vec![
                                Label::primary(file_id, t.span.clone())
                                    .with_message("expected e.g. io/format"),
                            ])
                            .with_notes(vec![
                                "syntax: (extern let name ~ (Type -> Type) module/function)".into(),
                            ]),
                    );
                    return None;
                }
            },
            other => {
                self.error(
                    Diagnostic::error()
                        .with_message("expected an Erlang target as module/function")
                        .with_labels(vec![Label::primary(file_id, other.span())]),
                );
                return None;
            }
        };

        Some(Declaration::ExternLet {
            name,
            name_span: items[2].span(),
            is_pub,
            is_nullary,
            ty,
            erlang_target,
            span,
        })
    }

    /// Lower an `extern type` declaration.
    /// Syntax: (extern type ['k 'v] Name [module/type])
    fn lower_extern_type(
        &mut self,
        file_id: usize,
        items: &[SExpr],
        span: Range<usize>,
        is_pub: bool,
    ) -> Option<Declaration> {
        // items[0]=extern, items[1]=type, then optional ['k 'v], then Name, then optional module/type
        let mut cursor = 2;

        let mut params = Vec::new();
        if let Some(SExpr::Square(gen_items, _)) = items.get(cursor) {
            params = self.lower_type_params(file_id, gen_items)?;
            cursor += 1;
        }

        let name = match items.get(cursor) {
            Some(SExpr::Atom(t)) => self.source_at(file_id, t.span.clone()).to_string(),
            other => {
                self.error(
                    Diagnostic::error()
                        .with_message("expected a type name in extern type")
                        .with_labels(vec![Label::primary(
                            file_id,
                            other.map(|s| s.span()).unwrap_or(span.clone()),
                        )]),
                );
                return None;
            }
        };
        cursor += 1;

        let erlang_target = match items.get(cursor) {
            None => None,
            Some(SExpr::Atom(t)) => match &t.kind {
                TokenKind::QualifiedIdent((module, ty)) => Some((module.clone(), ty.clone())),
                _ => {
                    self.error(
                        Diagnostic::error()
                            .with_message("expected an optional extern type target as module/type")
                            .with_labels(vec![
                                Label::primary(file_id, t.span.clone())
                                    .with_message("expected e.g. erlang/map"),
                            ])
                            .with_notes(vec![
                                "syntax: (extern type ['k 'v] Name)".into(),
                                "syntax: (extern type ['k 'v] Name module/type)".into(),
                            ]),
                    );
                    return None;
                }
            },
            Some(other) => {
                self.error(
                    Diagnostic::error()
                        .with_message("expected an optional extern type target as module/type")
                        .with_labels(vec![Label::primary(file_id, other.span())])
                        .with_notes(vec![
                            "syntax: (extern type ['k 'v] Name)".into(),
                            "syntax: (extern type ['k 'v] Name module/type)".into(),
                        ]),
                );
                return None;
            }
        };

        if items.len() > cursor + 1 {
            self.error(
                Diagnostic::error()
                    .with_message("invalid extern type declaration")
                    .with_labels(vec![Label::primary(file_id, span.clone())])
                    .with_notes(vec![
                        "syntax: (extern type ['k 'v] Name)".into(),
                        "syntax: (extern type ['k 'v] Name module/type)".into(),
                    ]),
            );
            return None;
        }

        Some(Declaration::ExternType {
            is_pub,
            name,
            params,
            erlang_target,
            span,
        })
    }

    pub(super) fn lower_type_params(
        &mut self,
        file_id: usize,
        gen_items: &[SExpr],
    ) -> Option<Vec<String>> {
        let mut params = Vec::new();
        for item in gen_items {
            let SExpr::Atom(token) = item else {
                self.error(
                    Diagnostic::error()
                        .with_message("type parameters must be generic variables")
                        .with_labels(vec![Label::primary(file_id, item.span())])
                        .with_notes(vec![
                            "type parameters must look like `'a` or `'state`".into(),
                        ]),
                );
                return None;
            };
            if !matches!(token.kind, TokenKind::Generic) {
                self.error(
                    Diagnostic::error()
                        .with_message("type parameters must start with `'`")
                        .with_labels(vec![
                            Label::primary(file_id, token.span.clone())
                                .with_message("expected a generic type variable like `'a`"),
                        ]),
                );
                return None;
            }
            params.push(self.source_at(file_id, token.span.clone()).to_string());
        }
        Some(params)
    }

    /// Lower a `use` declaration.
    /// Syntax:
    ///   (use std/io)              — qualified only
    ///   (use std/io [println])    — bring specific names into unqualified scope
    ///   (use std/io [*])          — bring all exports into unqualified scope
    pub(super) fn lower_use(
        &mut self,
        file_id: usize,
        items: &[SExpr],
        span: Range<usize>,
        is_pub: bool,
    ) -> Option<Declaration> {
        if items.len() < 2 || items.len() > 3 {
            self.error(
                Diagnostic::error()
                    .with_message("invalid use declaration")
                    .with_labels(vec![Label::primary(file_id, span.clone())])
                    .with_notes(vec![
                        "syntax: (use std/io) or (use std/io [println read])".into(),
                    ]),
            );
            return None;
        }

        let path = match &items[1] {
            SExpr::Atom(t) => match &t.kind {
                // (use std/io) — namespaced module
                TokenKind::QualifiedIdent((namespace, module)) => {
                    (namespace.clone(), module.clone())
                }
                // (use math) — local project module
                TokenKind::Ident => {
                    let name = self.source_at(file_id, t.span.clone()).to_string();
                    (String::new(), name)
                }
                _ => {
                    self.error(
                        Diagnostic::error()
                            .with_message("expected a module path in use")
                            .with_labels(vec![
                                Label::primary(file_id, t.span.clone())
                                    .with_message("expected e.g. std/io or math"),
                            ])
                            .with_notes(vec!["syntax: (use std/io) or (use math)".into()]),
                    );
                    return None;
                }
            },
            other => {
                self.error(
                    Diagnostic::error()
                        .with_message("expected a module path in use")
                        .with_labels(vec![Label::primary(file_id, other.span())]),
                );
                return None;
            }
        };

        let unqualified = if items.len() == 3 {
            match &items[2] {
                SExpr::Square(contents, sq_span) => {
                    if contents.is_empty() {
                        self.error(
                            Diagnostic::error()
                                .with_message("empty import list")
                                .with_labels(vec![Label::primary(file_id, sq_span.clone())])
                                .with_notes(vec![
                                    "use [*] to import everything, or list names: [println read]"
                                        .into(),
                                ]),
                        );
                        return None;
                    }
                    // Check for wildcard [*]
                    if contents.len() == 1 {
                        if let SExpr::Atom(t) = &contents[0] {
                            if matches!(t.kind, TokenKind::Operator)
                                && self.source_at(file_id, t.span.clone()) == "*"
                            {
                                UnqualifiedImports::Wildcard
                            } else {
                                // Single non-wildcard name
                                let name = match &t.kind {
                                    TokenKind::Ident => {
                                        self.source_at(file_id, t.span.clone()).to_string()
                                    }
                                    _ => {
                                        self.error(
                                            Diagnostic::error()
                                                .with_message(
                                                    "expected an identifier in import list",
                                                )
                                                .with_labels(vec![Label::primary(
                                                    file_id,
                                                    t.span.clone(),
                                                )]),
                                        );
                                        return None;
                                    }
                                };
                                UnqualifiedImports::Specific(vec![name])
                            }
                        } else {
                            self.error(
                                Diagnostic::error()
                                    .with_message("expected an identifier in import list")
                                    .with_labels(vec![Label::primary(file_id, contents[0].span())]),
                            );
                            return None;
                        }
                    } else {
                        let mut names = Vec::with_capacity(contents.len());
                        let mut seen = std::collections::HashSet::with_capacity(contents.len());
                        for item in contents {
                            match item {
                                SExpr::Atom(t) if matches!(t.kind, TokenKind::Ident) => {
                                    let name = self.source_at(file_id, t.span.clone()).to_string();
                                    if !seen.insert(name.clone()) {
                                        self.error(
                                            Diagnostic::error()
                                                .with_message("duplicate import in list")
                                                .with_labels(vec![
                                                    Label::primary(file_id, t.span.clone())
                                                        .with_message(format!(
                                                            "`{name}` appears more than once"
                                                        )),
                                                ]),
                                        );
                                        return None;
                                    }
                                    names.push(name);
                                }
                                other => {
                                    self.error(
                                        Diagnostic::error()
                                            .with_message("expected an identifier in import list")
                                            .with_labels(vec![Label::primary(
                                                file_id,
                                                other.span(),
                                            )]),
                                    );
                                    return None;
                                }
                            }
                        }
                        UnqualifiedImports::Specific(names)
                    }
                }
                other => {
                    self.error(
                        Diagnostic::error()
                            .with_message("expected an import list like [println read] or [*]")
                            .with_labels(vec![Label::primary(file_id, other.span())]),
                    );
                    return None;
                }
            }
        } else {
            UnqualifiedImports::None
        };

        Some(Declaration::Use {
            is_pub,
            path,
            unqualified,
            span,
        })
    }
}
