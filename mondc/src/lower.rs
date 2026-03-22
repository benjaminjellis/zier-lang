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

    fn lower_type_decl(
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

    fn lower_test(
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

    fn source_at(&self, file_id: usize, span: Range<usize>) -> &str {
        let file = self
            .files
            .get(file_id)
            .expect("Invalid file_id in source_at");

        &file.source()[span]
    }

    fn reject_ambiguous_constructor_sequence(
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
            if token.kind != TokenKind::Ident {
                continue;
            }

            let name = self.source_at(file_id, token.span.clone()).to_string();
            let is_constructor_like = name
                .chars()
                .next()
                .map(|c| c.is_ascii_uppercase())
                .unwrap_or(false);
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

            // Qualified ident in value position — zero-arg cross-module call
            TokenKind::QualifiedIdent((module, function)) => Some(Expr::QualifiedCall {
                module: module.clone(),
                function: function.clone(),
                args: vec![],
                fn_span: token.span.clone(),
                span: token.span.clone(),
            }),

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

    fn lower_lambda(
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

    fn lower_let_bind(
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
        let mut pairs: Vec<(String, Expr)> = Vec::new();
        for chunk in bindings.chunks(2) {
            let name = match &chunk[0] {
                SExpr::Atom(t) => self.source_at(file_id, t.span.clone()).to_string(),
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
            pairs.push((name, val));
        }

        // Fold right: innermost binding wraps the body
        let result = pairs.into_iter().rev().fold(body, |inner, (name, val)| {
            let error_name = "__letq_error".to_string();
            let ok_pat = Pattern::Constructor(
                "Ok".to_string(),
                vec![Pattern::Variable(name, span.clone())],
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

    fn lower_record_construct(
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

    fn lower_record_update(
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

    /// Parse a type usage after `~`.
    /// Supported forms include:
    ///   - `Int`
    ///   - `Option 'a`
    ///   - `Map 'k 'v`
    ///   - `(Selector (Option 'm))`
    fn lower_type_usage_atoms(
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
    fn lower_extern_dispatch(
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

    fn lower_type_params(&mut self, file_id: usize, gen_items: &[SExpr]) -> Option<Vec<String>> {
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
    fn lower_use(
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

    fn lower_match(&mut self, file_id: usize, items: &[SExpr], span: Range<usize>) -> Option<Expr> {
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

    fn lower_pattern(&mut self, file_id: usize, sexpr: &SExpr) -> Option<Pattern> {
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
                if let [SExpr::Atom(token), rest @ ..] = items.as_slice()
                    && let TokenKind::Ident = token.kind
                {
                    let name = self.source_at(file_id, token.span.clone()).to_string();
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

    fn lower_if(&mut self, file_id: usize, items: &[SExpr], span: Range<usize>) -> Option<Expr> {
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

    fn lower_do(&mut self, file_id: usize, items: &[SExpr], span: Range<usize>) -> Option<Expr> {
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

    fn lower_field_access(
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

    fn lower_pipe(&mut self, file_id: usize, items: &[SExpr], span: Range<usize>) -> Option<Expr> {
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
            let func = Box::new(self.lower_expr(file_id, step)?);
            // Keep each desugared call span tight so type errors point at the
            // offending pipeline step instead of the whole pipe expression.
            let call_span = acc.span().start..step.span().end;
            acc = Expr::Call {
                func,
                args: vec![acc],
                span: call_span,
            };
        }
        Some(acc)
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    // Helper to setup the lowerer with a string
    fn setup(source: &str) -> (Lowerer, usize, Vec<SExpr>) {
        let mut lowerer = Lowerer::new();

        let tokens = crate::lexer::Lexer::new(source).lex();
        let file_id = lowerer.add_file("test.mond".to_string(), source.to_string());

        // This assumes your Parser returns a Vec<SExpr>
        let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
            .parse()
            .expect("S-Expr parse failed");

        (lowerer, file_id, sexprs)
    }

    #[test]
    fn test_variant_type() {
        let (mut lowerer, file_id, sexprs) = setup(
            r#"(type ['a] Option [
                        None
                        (Some ~ 'a)])
                    "#,
        );

        let _exprs = lowerer.lower_file(file_id, &sexprs);
    }

    #[test]
    fn test_record_type_with_generics() {
        let (mut lowerer, file_id, sexprs) = setup(
            "
                (type ['t] MyGenericType [
                    (:name ~ String)
                    (:data ~ 't)
                ])",
        );

        let exprs = lowerer.lower_file(file_id, &sexprs);
        if let Declaration::Type(TypeDecl::Record {
            name,
            params,
            fields,
            ..
        }) = &exprs[0]
        {
            assert_eq!(name, "MyGenericType");
            assert_eq!(params, &vec!["'t".to_string()]);
            assert_eq!(fields.len(), 2);
            assert_eq!(fields[0].0, "name");
            assert!(matches!(&fields[0].1, TypeUsage::Named(name, _) if name == "String"));
            assert_eq!(fields[1].0, "data");
            assert!(matches!(&fields[1].1, TypeUsage::Generic(name, _) if name == "'t"));
        } else {
            panic!("expected a generic record type");
        }
    }

    #[test]
    fn test_record_type_with_nested_type_application() {
        let (mut lowerer, file_id, sexprs) = setup(
            "
                (extern type ['p] Selector)
                (type ['m] ContinuePayload [
                    (:select ~ (Selector (Option 'm)))
                ])",
        );

        let exprs = lowerer.lower_file(file_id, &sexprs);
        if let Declaration::Type(TypeDecl::Record { fields, .. }) = &exprs[1] {
            assert_eq!(fields.len(), 1);
            assert_eq!(fields[0].0, "select");
            assert!(matches!(
                &fields[0].1,
                TypeUsage::App(selector, args, _)
                    if selector == "Selector"
                        && args.len() == 1
                        && matches!(
                            &args[0],
                            TypeUsage::App(option, option_args, _)
                                if option == "Option"
                                    && option_args.len() == 1
                                    && matches!(&option_args[0], TypeUsage::Generic(name, _) if name == "'m")
                        )
            ));
        } else {
            panic!("expected nested type application record type");
        }
    }

    #[test]
    fn test_record_type_with_function_field() {
        let (mut lowerer, file_id, sexprs) = setup(
            "
                (type ['m] Builder [
                    (:initialised ~ ((Subject 'm) -> Unit))
                ])",
        );

        let exprs = lowerer.lower_file(file_id, &sexprs);
        if let Declaration::Type(TypeDecl::Record { fields, .. }) = &exprs[0] {
            assert_eq!(fields.len(), 1);
            assert_eq!(fields[0].0, "initialised");
            assert!(matches!(
                &fields[0].1,
                TypeUsage::Fun(arg, ret, _)
                    if matches!(
                        arg.as_ref(),
                        TypeUsage::App(subject, args, _)
                            if subject == "Subject"
                                && args.len() == 1
                                && matches!(&args[0], TypeUsage::Generic(name, _) if name == "'m")
                    )
                    && matches!(ret.as_ref(), TypeUsage::Named(name, _) if name == "Unit")
            ));
        } else {
            panic!("expected record type with function field");
        }
    }

    #[test]
    fn test_record_type_with_empty_parenthesized_type_reports_error() {
        let (mut lowerer, file_id, sexprs) = setup(
            "
                (type Broken [
                    (:value ~ ())
                ])",
        );

        let decls = lowerer.lower_file(file_id, &sexprs);
        assert!(decls.is_empty(), "expected lowering to fail");
        assert!(!lowerer.diagnostics.is_empty());
        assert!(
            lowerer
                .diagnostics
                .iter()
                .any(|d| d.message.contains("empty type is not allowed"))
        );
    }

    #[test]
    fn test_record_type() {
        let (mut lowerer, file_id, sexprs) = setup(
            "(type MyType [
                        (:field_one ~ String)
                        (:field_two ~ Int)
                        (:field_three ~ Bool)
                        ])",
        );

        let exprs = lowerer.lower_file(file_id, &sexprs);
        if let Declaration::Type(TypeDecl::Record {
            name,
            params,
            fields,
            ..
        }) = &exprs[0]
        {
            assert_eq!(name, "MyType");
            assert_eq!(*params, Vec::<String>::new());
            assert_eq!(fields.len(), 3);
            assert_eq!(fields[0].0, "field_one");
            assert!(matches!(&fields[0].1, TypeUsage::Named(name, _) if name == "String"));
            assert_eq!(fields[1].0, "field_two");
            assert!(matches!(&fields[1].1, TypeUsage::Named(name, _) if name == "Int"));
            assert_eq!(fields[2].0, "field_three");
            assert!(matches!(&fields[2].1, TypeUsage::Named(name, _) if name == "Bool"));
        } else {
            panic!("expected a type not an expression")
        }
    }

    #[test]
    fn test_record_type_square_bracket_body() {
        let (mut lowerer, file_id, sexprs) =
            setup("(pub type ExitMessage [(:pid ~ Pid) (:reason ~ ExitReason)])");

        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);
        if let Declaration::Type(TypeDecl::Record {
            is_pub,
            name,
            params,
            fields,
            ..
        }) = &exprs[0]
        {
            assert!(*is_pub);
            assert_eq!(name, "ExitMessage");
            assert!(params.is_empty());
            assert_eq!(fields.len(), 2);
            assert_eq!(fields[0].0, "pid");
            assert!(matches!(&fields[0].1, TypeUsage::Named(name, _) if name == "Pid"));
            assert_eq!(fields[1].0, "reason");
            assert!(matches!(&fields[1].1, TypeUsage::Named(name, _) if name == "ExitReason"));
        } else {
            panic!("expected a record type declaration");
        }
    }

    #[test]
    fn test_lower_function() {
        let (mut lowerer, file_id, sexprs) = setup("(let f {a} (+ a 10))");
        let exprs = lowerer.lower_file(file_id, &sexprs);

        if let Declaration::Expression(Expr::LetFunc {
            name, args, value, ..
        }) = &exprs[0]
        {
            assert_eq!(name, "f");
            assert_eq!(args, &vec!["a".to_string()]);

            if let Expr::Call {
                func,
                args: call_args,
                ..
            } = &**value
            {
                if let Expr::Variable(op_name, _) = &**func {
                    assert_eq!(op_name, "+");
                } else {
                    panic!("Expected function call to be an operator variable '+'");
                }
                assert_eq!(call_args.len(), 2);
                assert!(matches!(call_args[0], Expr::Variable(ref n, _) if n == "a"));
                assert!(matches!(call_args[1], Expr::Literal(Literal::Int(10), _)));
            } else {
                panic!("Expected Let value to be a function call (+ ...)");
            }
        } else {
            panic!("Expected a Let expression at the top level");
        }
    }

    #[test]
    fn test_let_sequential_desugaring() {
        // local let bindings are expressions — test via lower_expr, not lower_file
        let (mut lowerer, file_id, sexprs) = setup("(let [a 10 b 20] (+ a b))");
        let expr = lowerer
            .lower_expr(file_id, &sexprs[0])
            .expect("lowering failed");

        // Should desugar to: LetLocal(a, 10, LetLocal(b, 20, Call(+, [a, b])))
        if let Expr::LetLocal { name, body, .. } = expr {
            assert_eq!(name, "a");
            if let Expr::LetLocal { name: name2, .. } = *body {
                assert_eq!(name2, "b");
            } else {
                panic!("Expected nested LetLocal for 'b'");
            }
        } else {
            panic!("Expected LetLocal for 'a'");
        }
    }

    #[test]
    fn test_valid_if() {
        let (mut lowerer, file_id, sexprs) = setup("(if True 1 2)");
        let expr = lowerer
            .lower_expr(file_id, &sexprs[0])
            .expect("lowering failed");

        if let Expr::If {
            cond,
            then,
            els,
            span,
        } = expr
        {
            assert!(
                matches!(*cond, Expr::Literal(Literal::Bool(true), _)),
                "Condition should be True"
            );

            assert!(
                matches!(*then, Expr::Literal(Literal::Int(1), _)),
                "Then-branch should be 1"
            );

            assert!(
                matches!(*els, Expr::Literal(Literal::Int(2), _)),
                "Else-branch should be 2"
            );

            // 4. Verify the Span covers the whole (if ...)
            assert_eq!(span.start, 0);
            assert_eq!(span.end, 13);
        } else {
            panic!("Expected Expr::If");
        }
    }

    #[test]
    fn test_valid_if_let_desugars_to_match() {
        let (mut lowerer, file_id, sexprs) = setup("(if let [(Some x) maybe] x 0)");
        let expr = lowerer
            .lower_expr(file_id, &sexprs[0])
            .expect("lowering failed");

        if let Expr::Match { targets, arms, .. } = expr {
            assert_eq!(targets.len(), 1);
            assert!(matches!(targets[0], Expr::Variable(ref n, _) if n == "maybe"));
            assert_eq!(arms.len(), 2);

            match &arms[0].patterns[0] {
                Pattern::Constructor(name, args, _) => {
                    assert_eq!(name, "Some");
                    assert!(matches!(
                        args.first(),
                        Some(Pattern::Variable(name, _)) if name == "x"
                    ));
                }
                other => panic!("expected constructor pattern in if-let arm, got {other:?}"),
            }

            assert!(matches!(arms[0].body, Expr::Variable(ref n, _) if n == "x"));
            assert!(matches!(arms[1].patterns[0], Pattern::Any(_)));
            assert!(matches!(arms[1].body, Expr::Literal(Literal::Int(0), _)));
        } else {
            panic!("Expected Expr::Match desugaring for if-let");
        }
    }

    #[test]
    fn test_valid_if_let_legacy_syntax_desugars_to_match() {
        let (mut lowerer, file_id, sexprs) = setup("(if let (Some x) maybe x 0)");
        let expr = lowerer
            .lower_expr(file_id, &sexprs[0])
            .expect("lowering failed");

        assert!(
            matches!(expr, Expr::Match { .. }),
            "Expected Expr::Match desugaring for legacy if-let syntax"
        );
    }

    #[test]
    fn test_error_reporting_on_invalid_if() {
        // 'if' with missing else branch
        let (mut lowerer, file_id, sexprs) = setup("(if True 1)");
        let _ = lowerer.lower_expr(file_id, &sexprs[0]);

        assert!(!lowerer.diagnostics.is_empty());
        assert_eq!(
            lowerer.diagnostics[0].message,
            "wrong number of arguments for 'if'"
        );
    }

    #[test]
    fn test_error_reporting_on_invalid_if_let_arity() {
        let (mut lowerer, file_id, sexprs) = setup("(if let [(Some x) maybe] x)");
        let _ = lowerer.lower_expr(file_id, &sexprs[0]);
        assert!(!lowerer.diagnostics.is_empty());
        assert_eq!(
            lowerer.diagnostics[0].message,
            "wrong number of arguments for 'if let'"
        );
    }

    #[test]
    fn test_error_reporting_on_invalid_if_let_binding_shape() {
        let (mut lowerer, file_id, sexprs) = setup("(if let [(Some x)] x 0)");
        let _ = lowerer.lower_expr(file_id, &sexprs[0]);
        assert!(!lowerer.diagnostics.is_empty());
        assert_eq!(lowerer.diagnostics[0].message, "invalid if let binding");
    }

    #[test]
    fn test_float_literal() {
        let (mut lowerer, file_id, sexprs) = setup("6.14");
        let expr = lowerer
            .lower_expr(file_id, &sexprs[0])
            .expect("lowering failed");
        if let Expr::Literal(Literal::Float(f), _) = expr {
            assert!((f - 6.14).abs() < 1e-10);
        } else {
            panic!("expected Float literal");
        }
    }

    #[test]
    fn test_string_literal() {
        let (mut lowerer, file_id, sexprs) = setup(r#""hello world""#);
        let expr = lowerer
            .lower_expr(file_id, &sexprs[0])
            .expect("lowering failed");
        if let Expr::Literal(Literal::String(s), _) = expr {
            assert_eq!(s, "hello world");
        } else {
            panic!("expected String literal");
        }
    }

    #[test]
    fn test_unit_literal() {
        let (mut lowerer, file_id, sexprs) = setup("()");
        let expr = lowerer
            .lower_expr(file_id, &sexprs[0])
            .expect("lowering failed");
        assert!(matches!(expr, Expr::Literal(Literal::Unit, _)));
    }

    #[test]
    fn test_list_literal_lowering() {
        let (mut lowerer, file_id, sexprs) = setup("[1 2 3]");
        let expr = lowerer
            .lower_expr(file_id, &sexprs[0])
            .expect("lowering failed");
        if let Expr::List(items, _) = expr {
            assert_eq!(items.len(), 3);
            assert!(matches!(items[0], Expr::Literal(Literal::Int(1), _)));
            assert!(matches!(items[1], Expr::Literal(Literal::Int(2), _)));
            assert!(matches!(items[2], Expr::Literal(Literal::Int(3), _)));
        } else {
            panic!("expected List expression");
        }
    }

    #[test]
    fn test_field_access_lowering() {
        let (mut lowerer, file_id, sexprs) = setup("(:my_field some_record)");
        let expr = lowerer
            .lower_expr(file_id, &sexprs[0])
            .expect("lowering failed");
        if let Expr::FieldAccess { field, record, .. } = expr {
            assert_eq!(field, "my_field");
            assert!(matches!(record.as_ref(), Expr::Variable(n, _) if n == "some_record"));
        } else {
            panic!("expected FieldAccess");
        }
    }

    #[test]
    fn test_field_access_too_many_args() {
        let (mut lowerer, file_id, sexprs) = setup("(:x p q)");
        let result = lowerer.lower_expr(file_id, &sexprs[0]);
        assert!(result.is_none());
        assert!(!lowerer.diagnostics.is_empty());
    }

    #[test]
    fn test_field_access_zero_args() {
        let (mut lowerer, file_id, sexprs) = setup("(:x)");
        let result = lowerer.lower_expr(file_id, &sexprs[0]);
        assert!(result.is_none());
        assert!(!lowerer.diagnostics.is_empty());
    }

    #[test]
    fn test_record_update_lowering() {
        let (mut lowerer, file_id, sexprs) = setup("(with point :x 10 :y 20)");
        let expr = lowerer
            .lower_expr(file_id, &sexprs[0])
            .expect("lowering failed");
        if let Expr::RecordUpdate {
            record, updates, ..
        } = expr
        {
            assert!(matches!(record.as_ref(), Expr::Variable(name, _) if name == "point"));
            assert_eq!(updates.len(), 2);
            assert_eq!(updates[0].0, "x");
            assert_eq!(updates[1].0, "y");
            assert!(matches!(updates[0].1, Expr::Literal(Literal::Int(10), _)));
            assert!(matches!(updates[1].1, Expr::Literal(Literal::Int(20), _)));
        } else {
            panic!("expected RecordUpdate");
        }
    }

    #[test]
    fn test_record_update_missing_value_is_error() {
        let (mut lowerer, file_id, sexprs) = setup("(with point :x)");
        let result = lowerer.lower_expr(file_id, &sexprs[0]);
        assert!(result.is_none());
        assert!(!lowerer.diagnostics.is_empty());
    }

    #[test]
    fn test_bare_field_accessor_error() {
        // :field used as a standalone atom (not inside parens) should error
        let (mut lowerer, file_id, sexprs) = setup("(let f {r} :my_field)");
        let exprs = lowerer.lower_file(file_id, &sexprs);
        // The :my_field as a bare value in position should produce a diagnostic
        assert!(exprs.is_empty() || !lowerer.diagnostics.is_empty());
    }

    #[test]
    fn test_non_recursive() {
        let (mut lowerer, file_id, sexprs) = setup(
            "(let fib {n}
  (if (or (= n 0) (= n 1))
    n
    (+ (fib (- n 1)) (fib (- n 2)))))
",
        );

        let _a = lowerer.lower_file(file_id, &sexprs);
        assert!(lowerer.diagnostics.is_empty());
    }

    #[test]
    fn test_let_with_continuation_fails() {
        // (let x 42 x) is an invalid let binding
        let (mut lowerer, file_id, sexprs) = setup("(let x 42 x)");
        let _ = lowerer.lower_file(file_id, &sexprs);
        let diag = &lowerer.diagnostics[0];
        assert_eq!(diag.message, "invalid let syntax")
    }

    #[test]
    fn test_match_wildcard_pattern() {
        let (mut lowerer, file_id, sexprs) = setup("(match x _ ~> 0)");
        let expr = lowerer
            .lower_expr(file_id, &sexprs[0])
            .expect("lowering failed");
        if let Expr::Match { arms, .. } = expr {
            assert_eq!(arms.len(), 1);
            assert!(
                matches!(arms[0].patterns[0], Pattern::Any(_)),
                "expected Any pattern"
            );
        } else {
            panic!("expected Match");
        }
    }

    #[test]
    fn test_match_literal_pattern() {
        let (mut lowerer, file_id, sexprs) = setup("(match x 0 ~> True 1 ~> False _ ~> False)");
        let expr = lowerer
            .lower_expr(file_id, &sexprs[0])
            .expect("lowering failed");
        if let Expr::Match { arms, .. } = expr {
            assert_eq!(arms.len(), 3);
            assert!(matches!(
                arms[0].patterns[0],
                Pattern::Literal(Literal::Int(0), _)
            ));
            assert!(matches!(
                arms[1].patterns[0],
                Pattern::Literal(Literal::Int(1), _)
            ));
            assert!(matches!(arms[2].patterns[0], Pattern::Any(_)));
        } else {
            panic!("expected Match");
        }
    }

    #[test]
    fn test_if_too_many_args() {
        let (mut lowerer, file_id, sexprs) = setup("(if True 1 2 3)");
        let _ = lowerer.lower_expr(file_id, &sexprs[0]);
        assert!(!lowerer.diagnostics.is_empty());
        assert_eq!(
            lowerer.diagnostics[0].message,
            "wrong number of arguments for 'if'"
        );
    }

    #[test]
    fn test_variant_type_multiple_constructors() {
        let (mut lowerer, file_id, sexprs) =
            setup("(type ['a 'e] Result [(Ok ~ 'a) (Error ~ 'e)])");
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert_eq!(exprs.len(), 1);
        if let Declaration::Type(TypeDecl::Variant {
            name,
            params,
            constructors,
            ..
        }) = &exprs[0]
        {
            assert_eq!(name, "Result");
            assert_eq!(params, &vec!["'a".to_string(), "'e".to_string()]);
            assert_eq!(constructors.len(), 2);
            let (ok_name, ok_payload) = &constructors[0];
            assert_eq!(ok_name, "Ok");
            assert!(ok_payload.is_some());
            let (err_name, err_payload) = &constructors[1];
            assert_eq!(err_name, "Error");
            assert!(err_payload.is_some());
        } else {
            panic!("expected Variant type declaration");
        }
    }

    #[test]
    fn test_variant_type_square_bracket_body() {
        let (mut lowerer, file_id, sexprs) =
            setup("(pub type ['a] ExitReason [Normal Killed (Abnormal ~ 'a)])");
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert_eq!(exprs.len(), 1);
        assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);
        if let Declaration::Type(TypeDecl::Variant {
            is_pub,
            name,
            params,
            constructors,
            ..
        }) = &exprs[0]
        {
            assert!(*is_pub);
            assert_eq!(name, "ExitReason");
            assert_eq!(params, &vec!["'a".to_string()]);
            assert_eq!(constructors.len(), 3);
            assert_eq!(constructors[0], ("Normal".into(), None));
            assert_eq!(constructors[1], ("Killed".into(), None));
            assert_eq!(constructors[2].0, "Abnormal");
            assert!(matches!(
                &constructors[2].1,
                Some(TypeUsage::Generic(name, _)) if name == "'a"
            ));
        } else {
            panic!("expected Variant type declaration");
        }
    }

    #[test]
    fn test_match_constructor_patterns() {
        let (mut lowerer, file_id, sexprs) = setup("(match x (Some y) ~> y None ~> 0)");
        let expr = lowerer
            .lower_expr(file_id, &sexprs[0])
            .expect("lowering failed");

        if let Expr::Match { arms, .. } = expr {
            if let Pattern::Constructor(name, args, _) = &arms[0].patterns[0] {
                assert_eq!(name, "Some");
                assert_eq!(args.len(), 1);
            } else {
                panic!("Expected Constructor pattern");
            }
        } else {
            panic!("expected Match");
        }
    }

    // -------------------------------------------------------------------------
    // Lowerer acceptance tests — valid syntax that must lower without errors
    // -------------------------------------------------------------------------

    #[test]
    fn test_float_operators() {
        // All four float ops must lower without errors
        for src in [
            "(+. 1.0 2.0)",
            "(-. 3.0 1.0)",
            "(*. 2.0 3.0)",
            "(/. 6.0 2.0)",
        ] {
            let (mut lowerer, file_id, sexprs) = setup(src);
            let result = lowerer.lower_expr(file_id, &sexprs[0]);
            assert!(result.is_some(), "failed for: {src}");
            assert!(lowerer.diagnostics.is_empty(), "failed for: {src}");
        }
    }

    #[test]
    fn test_or_and_in_call_position() {
        // `or` and `and` are operators callable in function position
        for src in ["(or True False)", "(and True False)"] {
            let (mut lowerer, file_id, sexprs) = setup(src);
            let result = lowerer.lower_expr(file_id, &sexprs[0]);
            assert!(result.is_some(), "failed for: {src}");
            assert!(
                lowerer.diagnostics.is_empty(),
                "diagnostics: {:?}",
                lowerer.diagnostics
            );
        }
    }

    #[test]
    fn test_pipe_desugars_to_nested_unary_calls() {
        let (mut lowerer, file_id, sexprs) = setup("(|> x inc double)");
        let expr = lowerer
            .lower_expr(file_id, &sexprs[0])
            .expect("lowering failed");
        assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);

        if let Expr::Call { func, args, .. } = expr {
            assert!(matches!(*func, Expr::Variable(ref name, _) if name == "double"));
            assert_eq!(args.len(), 1);
            if let Expr::Call {
                func: inner_func,
                args: inner_args,
                ..
            } = &args[0]
            {
                assert!(matches!(inner_func.as_ref(), Expr::Variable(name, _) if name == "inc"));
                assert_eq!(inner_args.len(), 1);
                assert!(matches!(inner_args[0], Expr::Variable(ref name, _) if name == "x"));
            } else {
                panic!("expected nested call for first pipe step");
            }
        } else {
            panic!("expected Call");
        }
    }

    #[test]
    fn test_pipe_accepts_partial_application_steps() {
        let (mut lowerer, file_id, sexprs) = setup("(|> 3 (add 1) (mul 2))");
        let expr = lowerer
            .lower_expr(file_id, &sexprs[0])
            .expect("lowering failed");
        assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);

        if let Expr::Call { func, args, .. } = expr {
            assert_eq!(args.len(), 1);
            assert!(matches!(*func, Expr::Call { .. }));
        } else {
            panic!("expected Call");
        }
    }

    #[test]
    fn test_pipe_requires_a_step() {
        let (mut lowerer, file_id, sexprs) = setup("(|> x)");
        let expr = lowerer.lower_expr(file_id, &sexprs[0]);
        assert!(expr.is_none(), "expected lowering to fail");
        assert!(
            lowerer.diagnostics.iter().any(|d| d
                .message
                .contains("pipeline requires a value and at least one step")),
            "unexpected diagnostics: {:?}",
            lowerer
                .diagnostics
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_multi_arg_function_lowering() {
        let (mut lowerer, file_id, sexprs) = setup("(let add {a b c} (+ a (+ b c)))");
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert!(lowerer.diagnostics.is_empty());
        if let Declaration::Expression(Expr::LetFunc { name, args, .. }) = &exprs[0] {
            assert_eq!(name, "add");
            assert_eq!(args, &["a", "b", "c"]);
        } else {
            panic!("expected LetFunc");
        }
    }

    #[test]
    fn test_function_implicit_sequencing() {
        // Multiple expressions in a function body desugar to nested LetLocal "_"
        let src = "(let f {x} (+ x 1) (+ x 2))";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let decls = lowerer.lower_file(file_id, &sexprs);
        assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);
        if let Declaration::Expression(Expr::LetFunc { value, .. }) = &decls[0] {
            // Body should be LetLocal { name: "_", value: (+ x 1), body: (+ x 2) }
            assert!(matches!(value.as_ref(), Expr::LetLocal { name, .. } if name == "_"));
        } else {
            panic!("expected LetFunc");
        }
    }

    #[test]
    fn test_constructor_followed_by_body_expr_is_error() {
        let src = "(let always_none {x} None x)";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let _ = lowerer.lower_file(file_id, &sexprs);
        assert!(
            !lowerer.diagnostics.is_empty(),
            "expected diagnostic for ambiguous constructor sequencing"
        );
        assert!(
            lowerer.diagnostics[0]
                .message
                .contains("constructor `None` cannot be followed"),
            "unexpected error message: {}",
            lowerer.diagnostics[0].message
        );
    }

    #[test]
    fn test_let_body_implicit_sequencing() {
        // Multiple expressions in a let body desugar to nested LetLocal "_"
        let src = "(let f {} (let [x 1] (+ x 1) (+ x 2)))";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let decls = lowerer.lower_file(file_id, &sexprs);
        assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);
        if let Declaration::Expression(Expr::LetFunc { value, .. }) = &decls[0] {
            // Outer let [x 1] binds x, body is sequenced
            if let Expr::LetLocal { name, body, .. } = value.as_ref() {
                assert_eq!(name, "x");
                // Body of x binding should be LetLocal "_" for the sequence
                assert!(matches!(body.as_ref(), Expr::LetLocal { name, .. } if name == "_"));
            } else {
                panic!("expected LetLocal for x binding");
            }
        } else {
            panic!("expected LetFunc");
        }
    }

    #[test]
    fn test_let_binding_with_function_call_value() {
        // local let with a call as value — test via lower_expr
        let (mut lowerer, file_id, sexprs) = setup("(let [x (+ 1 2)] x)");
        let expr = lowerer
            .lower_expr(file_id, &sexprs[0])
            .expect("lowering failed");
        assert!(lowerer.diagnostics.is_empty());
        assert!(matches!(expr, Expr::LetLocal { ref name, .. } if name == "x"));
    }

    #[test]
    fn test_match_multiple_arms() {
        let src = "(match n 0 ~> False 1 ~> True _ ~> False)";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let expr = lowerer
            .lower_expr(file_id, &sexprs[0])
            .expect("lowering failed");
        assert!(lowerer.diagnostics.is_empty());
        if let Expr::Match { arms, .. } = expr {
            assert_eq!(arms.len(), 3);
        } else {
            panic!("expected Match");
        }
    }

    #[test]
    fn test_match_guard_lowers_on_arm() {
        let src = "(match x (Some y) if (> y 0) ~> y _ ~> 0)";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let expr = lowerer
            .lower_expr(file_id, &sexprs[0])
            .expect("lowering failed");
        assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);
        if let Expr::Match { arms, .. } = expr {
            assert_eq!(arms.len(), 2);
            assert!(arms[0].guard.is_some(), "expected first arm guard");
            assert!(arms[1].guard.is_none(), "expected second arm without guard");
        } else {
            panic!("expected Match");
        }
    }

    #[test]
    fn test_match_guard_requires_expression_after_if() {
        let src = "(match x (Some y) if ~> y _ ~> 0)";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let _ = lowerer.lower_expr(file_id, &sexprs[0]);
        assert!(!lowerer.diagnostics.is_empty());
        assert!(
            lowerer.diagnostics[0]
                .message
                .contains("missing guard expression after `if`"),
            "unexpected diagnostic: {}",
            lowerer.diagnostics[0].message
        );
    }

    #[test]
    fn test_do_sequences_expressions() {
        let src = "(let f {} (do (g 1) (h 2) (i 3)))";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let decls = lowerer.lower_file(file_id, &sexprs);
        assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);
        if let Declaration::Expression(Expr::LetFunc { value, .. }) = &decls[0] {
            // (do e1 e2 e3) ~> LetLocal("_", e1, LetLocal("_", e2, e3))
            assert!(matches!(value.as_ref(), Expr::LetLocal { name, .. } if name == "_"));
        } else {
            panic!("expected LetFunc");
        }
    }

    #[test]
    fn test_do_single_expr_is_identity() {
        let src = "(let f {} (do (g 1)))";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let decls = lowerer.lower_file(file_id, &sexprs);
        assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);
        if let Declaration::Expression(Expr::LetFunc { value, .. }) = &decls[0] {
            // (do e) ~> e — no wrapping
            assert!(!matches!(value.as_ref(), Expr::LetLocal { .. }));
        } else {
            panic!("expected LetFunc");
        }
    }

    #[test]
    fn test_do_empty_is_error() {
        let src = "(let f {} (do))";
        let (mut lowerer, file_id, sexprs) = setup(src);
        lowerer.lower_file(file_id, &sexprs);
        assert!(!lowerer.diagnostics.is_empty());
    }

    #[test]
    fn test_match_arm_multi_expr_suggests_do() {
        // Two call expressions in a match arm without `do` should produce an error
        // with a hint to use `do`.
        let src = "(let f {x} (match x _ ~> (g x) (h x)))";
        let (mut lowerer, file_id, sexprs) = setup(src);
        lowerer.lower_file(file_id, &sexprs);
        assert!(
            !lowerer.diagnostics.is_empty(),
            "expected an error for multi-expr match arm"
        );
        assert!(
            lowerer.diagnostics[0].message.contains("do"),
            "error should mention `do`: {}",
            lowerer.diagnostics[0].message
        );
    }

    #[test]
    fn test_variable_pattern_in_match() {
        let src = "(match x n ~> n)";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let expr = lowerer
            .lower_expr(file_id, &sexprs[0])
            .expect("lowering failed");
        assert!(lowerer.diagnostics.is_empty());
        if let Expr::Match { arms, .. } = expr {
            assert!(matches!(arms[0].patterns[0], Pattern::Variable(ref s, _) if s == "n"));
        } else {
            panic!("expected Match");
        }
    }

    #[test]
    fn test_bool_literal_patterns() {
        let src = "(match b True ~> 0 False ~> 1)";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let expr = lowerer
            .lower_expr(file_id, &sexprs[0])
            .expect("lowering failed");
        assert!(lowerer.diagnostics.is_empty());
        if let Expr::Match { arms, .. } = expr {
            assert!(matches!(
                arms[0].patterns[0],
                Pattern::Literal(Literal::Bool(true), _)
            ));
            assert!(matches!(
                arms[1].patterns[0],
                Pattern::Literal(Literal::Bool(false), _)
            ));
        } else {
            panic!("expected Match");
        }
    }

    #[test]
    fn test_nullary_constructor_pattern() {
        // None as a bare constructor pattern (no payload)
        let src = "(match x None ~> 0 (Some v) ~> v)";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let expr = lowerer
            .lower_expr(file_id, &sexprs[0])
            .expect("lowering failed");
        assert!(lowerer.diagnostics.is_empty());
        if let Expr::Match { arms, .. } = expr {
            assert!(
                matches!(&arms[0].patterns[0], Pattern::Constructor(name, args, _) if name == "None" && args.is_empty())
            );
            assert!(
                matches!(&arms[1].patterns[0], Pattern::Constructor(name, args, _) if name == "Some" && args.len() == 1)
            );
        } else {
            panic!("expected Match");
        }
    }

    #[test]
    fn test_record_pattern() {
        let src = "(match person (Person :name name :age age) ~> age)";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let expr = lowerer
            .lower_expr(file_id, &sexprs[0])
            .expect("lowering failed");
        assert!(lowerer.diagnostics.is_empty());
        if let Expr::Match { arms, .. } = expr {
            match &arms[0].patterns[0] {
                Pattern::Record { name, fields, .. } => {
                    assert_eq!(name, "Person");
                    assert_eq!(fields.len(), 2);
                    assert_eq!(fields[0].0, "name");
                    assert!(matches!(fields[0].1, Pattern::Variable(ref s, _) if s == "name"));
                    assert_eq!(fields[1].0, "age");
                    assert!(matches!(fields[1].1, Pattern::Variable(ref s, _) if s == "age"));
                }
                other => panic!("expected record pattern, got {other:?}"),
            }
        } else {
            panic!("expected Match");
        }
    }

    #[test]
    fn test_singleton_list_pattern_lowers_to_cons_empty() {
        let src = "(match xs [t] ~> t)";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let expr = lowerer
            .lower_expr(file_id, &sexprs[0])
            .expect("lowering failed");
        assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);
        if let Expr::Match { arms, .. } = expr {
            match &arms[0].patterns[0] {
                Pattern::Cons(head, tail, _) => {
                    assert!(matches!(head.as_ref(), Pattern::Variable(name, _) if name == "t"));
                    assert!(matches!(tail.as_ref(), Pattern::EmptyList(_)));
                }
                other => panic!("expected cons pattern, got {other:?}"),
            }
        } else {
            panic!("expected Match");
        }
    }

    #[test]
    fn test_multi_head_list_cons_pattern_lowers_to_nested_cons() {
        let src = "(match xs [h t | rest] ~> rest)";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let expr = lowerer
            .lower_expr(file_id, &sexprs[0])
            .expect("lowering failed");
        assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);
        if let Expr::Match { arms, .. } = expr {
            match &arms[0].patterns[0] {
                Pattern::Cons(first_head, first_tail, _) => {
                    assert!(
                        matches!(first_head.as_ref(), Pattern::Variable(name, _) if name == "h")
                    );
                    match first_tail.as_ref() {
                        Pattern::Cons(second_head, second_tail, _) => {
                            assert!(
                                matches!(second_head.as_ref(), Pattern::Variable(name, _) if name == "t")
                            );
                            assert!(
                                matches!(second_tail.as_ref(), Pattern::Variable(name, _) if name == "rest")
                            );
                        }
                        other => panic!("expected nested cons tail, got {other:?}"),
                    }
                }
                other => panic!("expected cons pattern, got {other:?}"),
            }
        } else {
            panic!("expected Match");
        }
    }

    // -------------------------------------------------------------------------
    // Or-pattern and multi-target match tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_or_pattern_lowers_to_pattern_or() {
        let src = "(match x 1 | 2 | 3 ~> True _ ~> False)";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let expr = lowerer
            .lower_expr(file_id, &sexprs[0])
            .expect("lowering failed");
        assert!(lowerer.diagnostics.is_empty());
        if let Expr::Match { targets, arms, .. } = expr {
            assert_eq!(targets.len(), 1);
            assert_eq!(arms.len(), 2);
            assert!(matches!(&arms[0].patterns[0], Pattern::Or(pats, _) if pats.len() == 3));
            assert!(matches!(&arms[1].patterns[0], Pattern::Any(_)));
        } else {
            panic!("expected Match");
        }
    }

    #[test]
    fn test_match_or_keyword_separator_is_rejected() {
        let src = "(match x 1 or 2 ~> True _ ~> False)";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let expr = lowerer.lower_expr(file_id, &sexprs[0]);
        assert!(expr.is_none(), "expected lowering to fail");
        assert!(
            lowerer
                .diagnostics
                .iter()
                .any(|d| d.message.contains("invalid pattern")),
            "expected invalid pattern diagnostic, got: {:?}",
            lowerer
                .diagnostics
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_multi_target_match_lowers_two_targets() {
        let src = "(match x y 1 1 ~> True _ _ ~> False)";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let expr = lowerer
            .lower_expr(file_id, &sexprs[0])
            .expect("lowering failed");
        assert!(lowerer.diagnostics.is_empty());
        if let Expr::Match { targets, arms, .. } = expr {
            assert_eq!(targets.len(), 2);
            assert_eq!(arms.len(), 2);
            assert_eq!(arms[0].patterns.len(), 2);
            assert!(matches!(
                &arms[0].patterns[0],
                Pattern::Literal(Literal::Int(1), _)
            ));
            assert!(matches!(
                &arms[0].patterns[1],
                Pattern::Literal(Literal::Int(1), _)
            ));
            assert!(matches!(&arms[1].patterns[0], Pattern::Any(_)));
            assert!(matches!(&arms[1].patterns[1], Pattern::Any(_)));
        } else {
            panic!("expected Match");
        }
    }

    // -------------------------------------------------------------------------
    // Visibility (pub) tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_pub_function_is_pub() {
        let (mut lowerer, file_id, sexprs) = setup("(pub let add {a b} (+ a b))");
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert!(lowerer.diagnostics.is_empty());
        if let Declaration::Expression(Expr::LetFunc { is_pub, name, .. }) = &exprs[0] {
            assert!(is_pub, "expected is_pub = true");
            assert_eq!(name, "add");
        } else {
            panic!("expected LetFunc");
        }
    }

    #[test]
    fn test_private_function_is_not_pub() {
        let (mut lowerer, file_id, sexprs) = setup("(let add {a b} (+ a b))");
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert!(lowerer.diagnostics.is_empty());
        if let Declaration::Expression(Expr::LetFunc { is_pub, .. }) = &exprs[0] {
            assert!(!is_pub, "expected is_pub = false");
        } else {
            panic!("expected LetFunc");
        }
    }

    #[test]
    fn test_pub_type_is_pub() {
        let (mut lowerer, file_id, sexprs) = setup("(pub type ['a] Option [None (Some ~ 'a)])");
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert!(lowerer.diagnostics.is_empty());
        if let Declaration::Type(TypeDecl::Variant { is_pub, name, .. }) = &exprs[0] {
            assert!(is_pub, "expected is_pub = true");
            assert_eq!(name, "Option");
        } else {
            panic!("expected Variant TypeDecl");
        }
    }

    #[test]
    fn test_pub_record_type_is_pub() {
        let (mut lowerer, file_id, sexprs) = setup("(pub type Point [(:x ~ Int) (:y ~ Int)])");
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert!(lowerer.diagnostics.is_empty());
        if let Declaration::Type(TypeDecl::Record { is_pub, name, .. }) = &exprs[0] {
            assert!(is_pub, "expected is_pub = true");
            assert_eq!(name, "Point");
        } else {
            panic!("expected Record TypeDecl");
        }
    }

    #[test]
    fn test_duplicate_record_field_rejected() {
        let src = "(type LotsOfFields [(:record ~ String) (:record ~ String)])";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let decls = lowerer.lower_file(file_id, &sexprs);
        assert!(decls.is_empty(), "expected lowering to fail");
        assert_eq!(lowerer.diagnostics.len(), 1);
        assert_eq!(
            lowerer.diagnostics[0].message,
            "duplicate record field `:record`"
        );
    }

    #[test]
    fn test_duplicate_variant_constructor_rejected() {
        let src = "(type LotsOVariants [One One Two])";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let decls = lowerer.lower_file(file_id, &sexprs);
        assert!(decls.is_empty(), "expected lowering to fail");
        assert_eq!(lowerer.diagnostics.len(), 1);
        assert_eq!(
            lowerer.diagnostics[0].message,
            "duplicate variant constructor `One`"
        );
    }

    #[test]
    fn test_extern_type_without_target_is_valid() {
        let (mut lowerer, file_id, sexprs) = setup("(pub extern type Pid)");
        let decls = lowerer.lower_file(file_id, &sexprs);
        assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);
        match &decls[0] {
            Declaration::ExternType {
                is_pub,
                name,
                params,
                erlang_target,
                ..
            } => {
                assert!(*is_pub);
                assert_eq!(name, "Pid");
                assert!(params.is_empty());
                assert!(erlang_target.is_none());
            }
            _ => panic!("expected ExternType"),
        }
    }

    #[test]
    fn test_extern_type_with_target_is_still_valid() {
        let (mut lowerer, file_id, sexprs) = setup("(pub extern type ['k 'v] Map maps/map)");
        let decls = lowerer.lower_file(file_id, &sexprs);
        assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);
        match &decls[0] {
            Declaration::ExternType {
                name,
                params,
                erlang_target,
                ..
            } => {
                assert_eq!(name, "Map");
                assert_eq!(params, &vec!["'k".to_string(), "'v".to_string()]);
                assert_eq!(
                    erlang_target.as_ref(),
                    Some(&("maps".to_string(), "map".to_string()))
                );
            }
            _ => panic!("expected ExternType"),
        }
    }

    #[test]
    fn test_extern_let_missing_fields_is_error_not_panic() {
        let (mut lowerer, file_id, sexprs) = setup("(extern let)");
        let decls = lowerer.lower_file(file_id, &sexprs);
        assert!(decls.is_empty(), "expected lowering to fail");
        assert!(!lowerer.diagnostics.is_empty());
        assert_eq!(
            lowerer.diagnostics[0].message,
            "invalid extern let declaration"
        );
    }

    // -------------------------------------------------------------------------
    // Lowerer rejection tests — invalid syntax that must produce diagnostics
    // -------------------------------------------------------------------------

    #[test]
    fn test_rec_keyword_produces_error() {
        // `rec` was removed from the language
        let (mut lowerer, file_id, sexprs) = setup("(let rec f {x} x)");
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert!(exprs.is_empty());
        dbg!(&lowerer.diagnostics[0].labels);
        assert_eq!(lowerer.diagnostics[0].message, "invalid let syntax");
    }

    #[test]
    fn test_top_level_local_binding_rejected() {
        // (let [x 42] x) is not valid at the top level — only inside a function body
        let (mut lowerer, file_id, sexprs) = setup("(let [x 42] x)");
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert!(exprs.is_empty());
        assert_eq!(
            lowerer.diagnostics[0].message,
            "local let binding is not valid at the top level"
        );
    }

    #[test]
    fn test_empty_let_is_error() {
        // (let) with nothing after it is invalid
        let (mut lowerer, file_id, sexprs) = setup("(let)");
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert!(exprs.is_empty());
        assert!(!lowerer.diagnostics.is_empty());
    }

    #[test]
    fn test_let_value_binding_without_braces_is_error() {
        // (let x 42) — missing {} means this is invalid let syntax
        let (mut lowerer, file_id, sexprs) = setup("(let x 42)");
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert!(exprs.is_empty());
        assert_eq!(lowerer.diagnostics[0].message, "invalid let syntax");
    }

    #[test]
    fn test_let_binding_odd_count_is_error() {
        // (let [x] body) — one name with no value
        let (mut lowerer, file_id, sexprs) = setup("(let [x] 0)");
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert!(exprs.is_empty());
        assert!(!lowerer.diagnostics.is_empty());
    }

    #[test]
    fn test_let_bind_missing_body_is_error() {
        let (mut lowerer, file_id, sexprs) = setup("(let? [_ (Ok ())])");
        let result = lowerer.lower_expr(file_id, &sexprs[0]);
        assert!(result.is_none());
        assert!(!lowerer.diagnostics.is_empty());
        assert_eq!(
            lowerer.diagnostics[0].message,
            "let? requires a body expression"
        );
    }

    #[test]
    fn test_let_bind_desugars_to_result_match() {
        let (mut lowerer, file_id, sexprs) = setup("(let? [x (safe)] (Ok x))");
        let expr = lowerer
            .lower_expr(file_id, &sexprs[0])
            .expect("expected let? to lower");
        assert!(lowerer.diagnostics.is_empty(), "{:?}", lowerer.diagnostics);

        match expr {
            Expr::Match { arms, .. } => {
                assert_eq!(arms.len(), 2);
                match &arms[0].patterns[0] {
                    Pattern::Constructor(name, args, _) => {
                        assert_eq!(name, "Ok");
                        assert!(matches!(
                            args.first(),
                            Some(Pattern::Variable(n, _)) if n == "x"
                        ));
                    }
                    other => panic!("expected Ok constructor pattern, got {other:?}"),
                }

                match &arms[1].patterns[0] {
                    Pattern::Constructor(name, args, _) => {
                        assert_eq!(name, "Error");
                        assert!(matches!(
                            args.first(),
                            Some(Pattern::Variable(n, _)) if n == "__letq_error"
                        ));
                    }
                    other => panic!("expected Error constructor pattern, got {other:?}"),
                }

                match &arms[1].body {
                    Expr::Call { func, args, .. } => {
                        assert!(matches!(
                            func.as_ref(),
                            Expr::Variable(name, _) if name == "Error"
                        ));
                        assert!(matches!(
                            args.first(),
                            Some(Expr::Variable(name, _)) if name == "__letq_error"
                        ));
                    }
                    other => panic!("expected Error constructor call, got {other:?}"),
                }
            }
            other => panic!("expected let? to desugar to match, got {other:?}"),
        }
    }

    #[test]
    fn test_match_with_no_arms_is_error() {
        // (match x) — no patterns at all
        let (mut lowerer, file_id, sexprs) = setup("(match x)");
        let result = lowerer.lower_expr(file_id, &sexprs[0]);
        assert!(result.is_none());
        assert!(!lowerer.diagnostics.is_empty());
    }

    #[test]
    fn test_match_missing_arrow_is_error() {
        // (match x pat body) — missing ~>
        let (mut lowerer, file_id, sexprs) = setup("(match x 0 1)");
        let result = lowerer.lower_expr(file_id, &sexprs[0]);
        assert!(result.is_none());
        assert!(!lowerer.diagnostics.is_empty());
    }

    #[test]
    fn test_match_missing_body_is_error() {
        // (match x 0 ~>) — arrow present but no result expression
        let (mut lowerer, file_id, sexprs) = setup("(match x 0 ~>)");
        let result = lowerer.lower_expr(file_id, &sexprs[0]);
        assert!(result.is_none());
        assert!(!lowerer.diagnostics.is_empty());
    }

    #[test]
    fn test_let_function_missing_body_is_error() {
        // (let f {}) — args present but body missing
        let (mut lowerer, file_id, sexprs) = setup("(let f {})");
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert!(exprs.is_empty());
        assert!(!lowerer.diagnostics.is_empty());
    }

    #[test]
    fn test_let_function_rejects_f_as_argument_name() {
        let (mut lowerer, file_id, sexprs) = setup("(let main {f} f)");
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert!(exprs.is_empty());
        assert!(!lowerer.diagnostics.is_empty());
        assert_eq!(
            lowerer.diagnostics[0].message,
            "invalid argument name, 'f' is a reserved keyword for anonymous functions"
        );
    }

    #[test]
    fn test_lambda_rejects_f_as_argument_name() {
        let (mut lowerer, file_id, sexprs) = setup("(f {f} -> f)");
        let result = lowerer.lower_expr(file_id, &sexprs[0]);
        assert!(result.is_none());
        assert!(!lowerer.diagnostics.is_empty());
        assert_eq!(
            lowerer.diagnostics[0].message,
            "invalid argument name, 'f' is a reserved keyword for anonymous functions"
        );
    }

    #[test]
    fn test_standalone_curly_is_error() {
        // {} as a top-level expression is invalid
        let (mut lowerer, file_id, sexprs) = setup("{x}");
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert!(exprs.is_empty());
        assert!(!lowerer.diagnostics.is_empty());
    }

    #[test]
    fn test_list_literal_at_top_level_is_error() {
        // [1 2 3] at top level is not a valid declaration
        let (mut lowerer, file_id, sexprs) = setup("[1 2 3]");
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert!(exprs.is_empty());
        assert!(!lowerer.diagnostics.is_empty());
    }

    // -------------------------------------------------------------------------
    // Variant type declaration rejection tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_variant_spurious_atom_rejected() {
        // `None x` — `x` is not a valid constructor name (lowercase)
        let src = "(type ['a] Option [None x (Some ~ 'a)])";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert!(exprs.is_empty(), "expected lowering to fail");
        assert!(!lowerer.diagnostics.is_empty());
        assert_eq!(
            lowerer.diagnostics[0].message,
            "invalid variant constructor"
        );
    }

    #[test]
    fn test_variant_lowercase_constructor_rejected() {
        // Constructor names must start with uppercase
        let src = "(type ['a] Option [none (Some ~ 'a)])";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert!(exprs.is_empty(), "expected lowering to fail");
        assert_eq!(
            lowerer.diagnostics[0].message,
            "invalid variant constructor"
        );
    }

    #[test]
    fn test_variant_constructor_missing_tilde_rejected() {
        // (Some 'a) instead of (Some ~ 'a)
        let src = "(type ['a] Option [None (Some 'a)])";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert!(exprs.is_empty(), "expected lowering to fail");
        assert!(!lowerer.diagnostics.is_empty());
    }

    #[test]
    fn test_variant_square_payload_requires_parentheses() {
        // In square-body form, payload constructors still require parens:
        // [Normal Killed (Abnormal ~ 'a)]
        let src = "(type ['a] ExitReason [Normal Killed Abnormal ~ 'a])";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert!(exprs.is_empty(), "expected lowering to fail");
        assert!(!lowerer.diagnostics.is_empty());
        assert_eq!(
            lowerer.diagnostics[0].message,
            "invalid variant constructor"
        );
    }

    #[test]
    fn test_variant_constructor_lowercase_name_in_payload_rejected() {
        // (some ~ 'a) — constructor name is lowercase
        let src = "(type ['a] Option [None (some ~ 'a)])";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert!(exprs.is_empty(), "expected lowering to fail");
        assert_eq!(
            lowerer.diagnostics[0].message,
            "constructor name must start with an uppercase letter"
        );
    }

    #[test]
    fn test_variant_integer_in_body_rejected() {
        // A literal in the variant body is not a constructor
        let src = "(type Foo [Bar 42])";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert!(exprs.is_empty(), "expected lowering to fail");
        assert!(!lowerer.diagnostics.is_empty());
    }

    #[test]
    fn test_type_params_require_quote_prefix() {
        let src = "(type ['a b] Pair [(:left ~ 'a) (:right ~ b)])";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert!(exprs.is_empty(), "expected lowering to fail");
        assert!(!lowerer.diagnostics.is_empty());
        assert_eq!(
            lowerer.diagnostics[0].message,
            "type parameters must start with `'`"
        );
    }

    #[test]
    fn test_extern_type_params_require_quote_prefix() {
        let src = "(extern type ['k v] Dict maps/map)";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert!(exprs.is_empty(), "expected lowering to fail");
        assert!(!lowerer.diagnostics.is_empty());
        assert_eq!(
            lowerer.diagnostics[0].message,
            "type parameters must start with `'`"
        );
    }

    #[test]
    fn test_type_round_body_rejected() {
        let src = "(type Flag (On Off))";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert!(exprs.is_empty(), "expected lowering to fail");
        assert!(!lowerer.diagnostics.is_empty());
        assert_eq!(
            lowerer.diagnostics[0].message,
            "type body must be wrapped in square brackets"
        );
    }

    #[test]
    fn test_bare_expression_at_top_level_rejected() {
        let (mut lowerer, file_id, sexprs) = setup("42");
        let _ = lowerer.lower_file(file_id, &sexprs);
        assert!(!lowerer.diagnostics.is_empty());
        assert_eq!(
            lowerer.diagnostics[0].message,
            "only function and type declarations are valid at the top level"
        );
    }

    #[test]
    fn test_bare_call_at_top_level_rejected() {
        let (mut lowerer, file_id, sexprs) = setup("(foo 1 2)");
        let _ = lowerer.lower_file(file_id, &sexprs);
        assert!(!lowerer.diagnostics.is_empty());
        assert_eq!(
            lowerer.diagnostics[0].message,
            "only function and type declarations are valid at the top level"
        );
    }

    #[test]
    fn test_use_duplicate_import_name_rejected() {
        let src = "(use std/io [println println])";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert!(exprs.is_empty(), "expected lowering to fail");
        assert!(!lowerer.diagnostics.is_empty());
        assert_eq!(lowerer.diagnostics[0].message, "duplicate import in list");
    }
}
