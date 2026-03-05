use codespan_reporting::{
    diagnostic::{Diagnostic, Label, LabelStyle, Severity},
    files::SimpleFiles,
};

use crate::{
    ast::{Declaration, Expr, Literal, Pattern, TypeDecl, TypeUsage},
    lexer::{Token, TokenKind},
    sexpr::SExpr,
};
use std::ops::Range;

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
        let mut cursor = 1; // Skip the "type" atom

        // 1. Parse optional generics: ['t] or ['e 'a]
        let mut params = Vec::new();
        if let Some(SExpr::Square(gen_items, _)) = items.get(cursor) {
            for s in gen_items {
                if let SExpr::Atom(t) = s {
                    params.push(self.source_at(file_id, t.span.clone()).to_string());
                }
            }
            cursor += 1;
        }

        // 2. Get Type Name: MyType, MyGenericType, Result, etc.
        let name = match items.get(cursor) {
            Some(SExpr::Atom(t)) => self.source_at(file_id, t.span.clone()).to_string(),
            _ => return None, // Error: Missing type name
        };
        cursor += 1;

        // 3. Peak at the first item in the body to determine the Kind
        // We expect a list like (:field ~ Type) or (Constructor ~ Type)
        let body_items = &items[cursor..];
        let first_body_item = body_items.first()?;

        // Determine if we are building a Record or a Variant based on the first token
        let is_record = if let SExpr::Round(inner, _) = first_body_item {
            // Look at the very first thing inside the first definition
            match inner.first() {
                Some(SExpr::Round(nested_inner, _)) => {
                    if let Some(SExpr::Atom(t)) = nested_inner.first() {
                        matches!(t.kind, TokenKind::NamedField(_))
                    } else {
                        false
                    }
                }
                _ => false,
            }
        } else {
            false
        };

        if is_record {
            // --- Lowering as a Record (Product Type) ---
            let mut fields = Vec::new();
            for item in body_items {
                if let SExpr::Round(inner, _) = item {
                    for i in inner {
                        let SExpr::Round(field_items, field_span) = i else {
                            self.error(
                                Diagnostic::error()
                                    .with_message(
                                        "each record field must be wrapped in parentheses",
                                    )
                                    .with_labels(vec![
                                        Label::primary(file_id, i.span())
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
                                    .with_notes(vec![
                                        "field names must start with `:`, e.g. `:x`".into(),
                                    ]),
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
                                    .with_labels(vec![Label::primary(
                                        file_id,
                                        tilde_token.span.clone(),
                                    )]),
                            );
                            return None;
                        }
                        let Some(SExpr::Atom(type_token)) = field_items.get(2) else {
                            self.error(
                                Diagnostic::error()
                                    .with_message("expected type name after `~`")
                                    .with_labels(vec![Label::primary(file_id, field_span.clone())])
                                    .with_notes(vec!["example: `(:x ~ Int)`".into()]),
                            );
                            return None;
                        };
                        let type_str = self.source_at(file_id, type_token.span.clone()).to_string();
                        let type_usage = match type_token.kind {
                            TokenKind::Generic => TypeUsage::Generic(type_str),
                            _ => TypeUsage::Named(type_str),
                        };
                        fields.push((field_name.clone(), type_usage));
                    }
                }
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
            let Some(SExpr::Round(body_items, _)) = body_items.first() else {
                self.error(
                    Diagnostic::error()
                        .with_message("variant body must be wrapped in parentheses")
                        .with_labels(vec![Label::primary(file_id, span.clone())])
                        .with_notes(vec![
                            "expected `(Constructor1 (Constructor2 ~ Type) ...)`".into(),
                        ]),
                );
                return None;
            };
            for item in body_items {
                match item {
                    // Case: (Some ~ 'a) or (Error ~ 'e)
                    SExpr::Round(inner, inner_span) => {
                        // Expect exactly: (ConstructorName ~ TypeName)
                        let (name_token, tilde_token, type_token) =
                            match (inner.first(), inner.get(1), inner.get(2)) {
                                (Some(n), Some(t), Some(ty)) => (n, t, ty),
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

                        let SExpr::Atom(type_tok) = type_token else {
                            self.error(
                                Diagnostic::error()
                                    .with_message("expected a type name after '~'")
                                    .with_labels(vec![Label::primary(file_id, type_token.span())]),
                            );
                            return None;
                        };
                        let type_str = self.source_at(file_id, type_tok.span.clone()).to_string();
                        let type_usage = match type_tok.kind {
                            TokenKind::Generic => TypeUsage::Generic(type_str),
                            _ => TypeUsage::Named(type_str),
                        };
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

    fn source_at(&self, file_id: usize, span: Range<usize>) -> &str {
        let file = self
            .files
            .get(file_id)
            .expect("Invalid file_id in source_at");

        &file.source()[span]
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
            TokenKind::Let | TokenKind::If | TokenKind::Match => {
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

    fn lower_array(
        &mut self,
        file_id: usize,
        items: &Vec<SExpr>,
        span: Range<usize>,
    ) -> Option<Expr> {
        let mut lowered_items = Vec::with_capacity(items.len());

        for item in items {
            // Recursively lower each element.
            let expr = self.lower_expr(file_id, item)?;
            lowered_items.push(expr);
        }

        Some(Expr::Array(lowered_items, span))
    }

    pub fn lower_expr(&mut self, file_id: usize, sexpr: &SExpr) -> Option<Expr> {
        match sexpr {
            SExpr::Atom(token) => self.lower_atom(file_id, token),
            SExpr::Array(items, span) => self.lower_array(file_id, items, span.to_owned()),
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
            SExpr::Square(_, span) => {
                self.error(Diagnostic {
                    severity: Severity::Error,
                    code: Some("E002".to_string()),
                    message: "Square brackets are used to in local let bindings".to_string(),
                    labels: vec![Label {
                        style: LabelStyle::Primary,
                        file_id,
                        range: span.to_owned(),
                        message: "".to_string(),
                    }],
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
                        TokenKind::If => return self.lower_if(file_id, items, span.clone()),
                        TokenKind::Match => return self.lower_match(file_id, items, span.clone()),
                        TokenKind::NamedField(_) => {
                            return self.lower_field_access(file_id, items, span.clone());
                        }
                        _ => {} // Fall through to function call
                    }
                }

                // If it's not a keyword, it's a function call: (func arg1 arg2)
                self.lower_call(file_id, items, span.clone())
            }
        }
    }

    fn lower_match(&mut self, file_id: usize, items: &[SExpr], span: Range<usize>) -> Option<Expr> {
        // 1. Initial validation: (match target ...)
        // Minimum valid: (match x pat ~> res) = 5 items
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

        // 2. Lower the target
        let target = Box::new(self.lower_expr(file_id, &items[1])?);

        // 3. Lower the arms (triplets: Pattern, Arrow, Result)
        let mut arms = Vec::new();
        let mut cursor = 2;

        while cursor < items.len() {
            // A. Lower the Pattern
            let pattern_sexpr = &items[cursor];
            let pattern = self.lower_pattern(file_id, pattern_sexpr)?;
            cursor += 1;

            // B. Expect and consume the arrow '~>'
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
                            .with_labels(vec![Label::primary(file_id, span)]),
                    );
                    return None;
                }
            }

            // C. Lower the Result expression
            let result_sexpr = items.get(cursor).or_else(|| {
                self.error(
                    Diagnostic::error()
                        .with_message("missing result expression after '~>'")
                        .with_labels(vec![Label::primary(file_id, span.clone())]),
                );
                None
            })?;
            let body = self.lower_expr(file_id, result_sexpr)?;
            cursor += 1;

            arms.push((pattern, body));
        }

        Some(Expr::Match { target, arms, span })
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
                // Pattern like (Some x)
                if let [SExpr::Atom(token), args @ ..] = items.as_slice()
                    && let TokenKind::Ident = token.kind
                {
                    let name = self.source_at(file_id, token.span.clone()).to_string();

                    // Idiomatic recursive lowering of sub-patterns
                    let mut lowered_args = Vec::new();
                    for arg in args {
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

            // Rejection of syntax variants not valid in patterns (Arrays, Brackets, Curlies)
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

    pub fn lower_let(
        &mut self,
        file_id: usize,
        items: &[SExpr],
        span: Range<usize>,
        is_pub: bool,
    ) -> Option<Expr> {
        let mut cursor = 1; // Skip the 'let' keyword

        // Detect the removed `rec` keyword and emit a helpful error.
        if let Some(SExpr::Atom(token)) = items.get(cursor)
            && matches!(token.kind, TokenKind::Ident)
            && self.source_at(file_id, token.span.clone()) == "rec"
        {
            self.error(
                Diagnostic::error()
                    .with_message("the `rec` keyword has been removed")
                    .with_labels(vec![
                        Label::primary(file_id, token.span.clone())
                            .with_message("remove `rec` here"),
                    ])
                    .with_notes(vec![
                        "named functions in Opal are self-recursive by default".into(),
                    ]),
            );
            return None;
        }

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

                // The body is the expression following the square brackets
                let mut current_expr = if let Some(next_sexpr) = items.get(cursor) {
                    self.lower_expr(file_id, next_sexpr)?
                } else {
                    // If no body, default to Unit (e.g. top-level definitions)
                    Expr::Literal(Literal::Unit, span.clone())
                };

                // We fold the bindings backwards to create nested LetLocal expressions
                for chunk in bindings.chunks(2).rev() {
                    let name = match &chunk[0] {
                        SExpr::Atom(t) => self.source_at(file_id, t.span.clone()).to_string(),
                        _ => {
                            self.error(
                                Diagnostic::error()
                                    .with_message("expected identifier in let-binding")
                                    .with_labels(vec![Label::primary(file_id, chunk[0].span())]),
                            );
                            return None;
                        }
                    };

                    let value = self.lower_expr(file_id, &chunk[1])?;

                    current_expr = Expr::LetLocal {
                        name,
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
                cursor += 1;

                // STRICT ENFORCEMENT: The next item MUST be Curly brackets {}
                let args = if let Some(SExpr::Curly(params, _)) = items.get(cursor) {
                    let mut arg_names = Vec::new();
                    for p in params {
                        if let SExpr::Atom(t) = p {
                            arg_names.push(self.source_at(file_id, t.span.clone()).to_string());
                        }
                    }
                    cursor += 1;
                    arg_names
                } else {
                    // This catches (let x 42 ...) and rejects it.
                    let err_span = items.get(cursor).map(|s| s.span()).unwrap_or(span.clone());
                    self.error(
                    Diagnostic::error()
                        .with_message("invalid let syntax")
                        .with_labels(vec![
                            Label::primary(file_id, err_span)
                                .with_message("expected '{args}' for function definition or '[' for variable bindings")
                        ]),
                );
                    return None;
                };

                // The function value/body (e.g. (+ a b))
                let value_sexpr = items.get(cursor).or_else(|| {
                    self.error(Diagnostic::error().with_message("missing function body"));
                    None
                })?;
                let value = self.lower_expr(file_id, value_sexpr)?;
                cursor += 1;

                // Top-level functions have no continuation — reject trailing expressions
                if let Some(extra) = items.get(cursor) {
                    let extra_span = extra.span();
                    self.error(
                        Diagnostic::error()
                            .with_message(
                                "top-level function definitions cannot have a continuation",
                            )
                            .with_labels(vec![Label::primary(file_id, extra_span)])
                            .with_notes(vec!["use separate top-level declarations instead".into()]),
                    );
                    return None;
                }

                Some(Expr::LetFunc {
                    is_pub,
                    name,
                    args,
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

#[cfg(test)]
mod tests {

    use super::*;

    // Helper to setup the lowerer with a string
    fn setup(source: &str) -> (Lowerer, usize, Vec<SExpr>) {
        let mut lowerer = Lowerer::new();

        let tokens = crate::lexer::Lexer::new(source).lex();
        let file_id = lowerer.add_file("test.opal".to_string(), source.to_string());

        // This assumes your Parser returns a Vec<SExpr>
        let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
            .parse()
            .expect("S-Expr parse failed");

        (lowerer, file_id, sexprs)
    }

    #[test]
    fn test_variant_type() {
        let (mut lowerer, file_id, sexprs) = setup(
            r#"(type ['a] Option (
                        None
                        (Some ~ 'a)))
                    "#,
        );

        let _exprs = lowerer.lower_file(file_id, &sexprs);
    }

    #[test]
    fn test_record_type_with_generics() {
        let (mut lowerer, file_id, sexprs) = setup(
            "
                (type ['t] MyGenericType (
                    (:name ~ String)
                    (:data ~ 't)
                ))",
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
            assert_eq!(
                fields,
                &vec![
                    ("name".into(), TypeUsage::Named("String".into())),
                    ("data".into(), TypeUsage::Generic("'t".into())),
                ]
            );
        } else {
            panic!("expected a generic record type");
        }
    }

    #[test]
    fn test_record_type() {
        let (mut lowerer, file_id, sexprs) = setup(
            "(type MyType (
                        (:field_one ~ String)
                        (:field_two ~ Int)
                        (:field_three ~ Bool)
                        ))",
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
            assert_eq!(
                fields,
                &vec![
                    ("field_one".into(), TypeUsage::Named("String".into())),
                    ("field_two".into(), TypeUsage::Named("Int".into())),
                    ("field_three".into(), TypeUsage::Named("Bool".into())),
                ]
            );
        } else {
            panic!("expected a type not an expression")
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
    fn test_array_literal_lowering() {
        let (mut lowerer, file_id, sexprs) = setup("#[1 2 3]");
        let expr = lowerer
            .lower_expr(file_id, &sexprs[0])
            .expect("lowering failed");
        if let Expr::Array(items, _) = expr {
            assert_eq!(items.len(), 3);
            assert!(matches!(items[0], Expr::Literal(Literal::Int(1), _)));
            assert!(matches!(items[1], Expr::Literal(Literal::Int(2), _)));
            assert!(matches!(items[2], Expr::Literal(Literal::Int(3), _)));
        } else {
            panic!("expected Array expression");
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
    fn test_bare_field_accessor_error() {
        // :field used as a standalone atom (not inside parens) should error
        let (mut lowerer, file_id, sexprs) = setup("(let f {r} :my_field)");
        let exprs = lowerer.lower_file(file_id, &sexprs);
        // The :my_field as a bare value in position should produce a diagnostic
        assert!(exprs.is_empty() || !lowerer.diagnostics.is_empty());
    }

    #[test]
    fn test_rec_keyword_rejected() {
        // `rec` was removed from the language — all named functions are self-recursive
        let (mut lowerer, file_id, sexprs) =
            setup("(let rec countdown {n} (if (= n 0) 0 (countdown (- n 1))))");
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert!(exprs.is_empty(), "expected lowering to fail");
        assert!(!lowerer.diagnostics.is_empty());
        assert_eq!(
            lowerer.diagnostics[0].message,
            "the `rec` keyword has been removed"
        );
    }

    #[test]
    fn test_self_recursive_function() {
        // Without `rec` — named functions are self-recursive by default
        let (mut lowerer, file_id, sexprs) =
            setup("(let countdown {n} (if (= n 0) 0 (countdown (- n 1))))");
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert!(lowerer.diagnostics.is_empty());
        assert_eq!(exprs.len(), 1);
        if let Declaration::Expression(Expr::LetFunc { name, args, .. }) = &exprs[0] {
            assert_eq!(name, "countdown");
            assert_eq!(args, &vec!["n".to_string()]);
        } else {
            panic!("expected LetFunc");
        }
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
            assert!(matches!(arms[0].0, Pattern::Any(_)), "expected Any pattern");
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
            assert!(matches!(arms[0].0, Pattern::Literal(Literal::Int(0), _)));
            assert!(matches!(arms[1].0, Pattern::Literal(Literal::Int(1), _)));
            assert!(matches!(arms[2].0, Pattern::Any(_)));
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
            setup("(type ['e 'a] Result ((Ok ~ 'a) (Error ~ 'e)))");
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
            assert_eq!(params, &vec!["'e".to_string(), "'a".to_string()]);
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
    fn test_match_constructor_patterns() {
        let (mut lowerer, file_id, sexprs) = setup("(match x (Some y) ~> y None ~> 0)");
        let expr = lowerer
            .lower_expr(file_id, &sexprs[0])
            .expect("lowering failed");

        if let Expr::Match { arms, .. } = expr {
            let (pattern, _body) = &arms[0];
            if let Pattern::Constructor(name, args, _) = pattern {
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
    fn test_function_with_continuation_is_error() {
        // Top-level functions cannot have a continuation — must use separate declarations
        let src = "(let f {x} (+ x 1) (f 5))";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let _ = lowerer.lower_file(file_id, &sexprs);
        assert!(!lowerer.diagnostics.is_empty());
        assert_eq!(
            lowerer.diagnostics[0].message,
            "top-level function definitions cannot have a continuation"
        );
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
    fn test_variable_pattern_in_match() {
        let src = "(match x n ~> n)";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let expr = lowerer
            .lower_expr(file_id, &sexprs[0])
            .expect("lowering failed");
        assert!(lowerer.diagnostics.is_empty());
        if let Expr::Match { arms, .. } = expr {
            assert!(matches!(arms[0].0, Pattern::Variable(ref s, _) if s == "n"));
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
                arms[0].0,
                Pattern::Literal(Literal::Bool(true), _)
            ));
            assert!(matches!(
                arms[1].0,
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
                matches!(&arms[0].0, Pattern::Constructor(name, args, _) if name == "None" && args.is_empty())
            );
            assert!(
                matches!(&arms[1].0, Pattern::Constructor(name, args, _) if name == "Some" && args.len() == 1)
            );
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
        let (mut lowerer, file_id, sexprs) = setup("(pub type ['a] Option (None (Some ~ 'a)))");
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
        let (mut lowerer, file_id, sexprs) = setup("(pub type Point ((:x ~ Int) (:y ~ Int)))");
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert!(lowerer.diagnostics.is_empty());
        if let Declaration::Type(TypeDecl::Record { is_pub, name, .. }) = &exprs[0] {
            assert!(is_pub, "expected is_pub = true");
            assert_eq!(name, "Point");
        } else {
            panic!("expected Record TypeDecl");
        }
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
        assert_eq!(
            lowerer.diagnostics[0].message,
            "the `rec` keyword has been removed"
        );
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
    fn test_standalone_curly_is_error() {
        // {} as a top-level expression is invalid
        let (mut lowerer, file_id, sexprs) = setup("{x}");
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert!(exprs.is_empty());
        assert!(!lowerer.diagnostics.is_empty());
    }

    #[test]
    fn test_standalone_square_is_error() {
        // [1 2 3] as an expression is invalid (must use #[...] for arrays)
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
        let src = "(type ['a] Option (None x (Some ~ 'a)))";
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
        let src = "(type ['a] Option (none (Some ~ 'a)))";
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
        let src = "(type ['a] Option (None (Some 'a)))";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert!(exprs.is_empty(), "expected lowering to fail");
        assert!(!lowerer.diagnostics.is_empty());
    }

    #[test]
    fn test_variant_constructor_lowercase_name_in_payload_rejected() {
        // (some ~ 'a) — constructor name is lowercase
        let src = "(type ['a] Option (None (some ~ 'a)))";
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
        let src = "(type Foo (Bar 42))";
        let (mut lowerer, file_id, sexprs) = setup(src);
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert!(exprs.is_empty(), "expected lowering to fail");
        assert!(!lowerer.diagnostics.is_empty());
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
}
