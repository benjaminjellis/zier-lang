use codespan_reporting::{
    diagnostic::{Diagnostic, Label, LabelStyle, Severity},
    files::SimpleFiles,
};

use crate::{
    ast::*,
    lexer::{Token, TokenKind},
    sexpr::SExpr,
};
use std::ops::Range;

pub struct Lowerer {
    pub files: SimpleFiles<String, String>,
    pub diagnostics: Vec<Diagnostic<usize>>,
}

impl Lowerer {
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
    ) -> Result<TypeDecl, ()> {
        let mut cursor = 1; // Skip "type"

        // 1. Check for Generics: ['e 'a]
        let mut params = Vec::new();
        if let Some(SExpr::Square(gen_items, _)) = items.get(cursor) {
            for s in gen_items {
                if let SExpr::Atom(t) = s {
                    // We expect TokenKind::Generic here (the 'a syntax)
                    params.push(self.source_at(file_id, t.span.clone()).to_string());
                }
            }
            cursor += 1;
        }

        // 2. Get the Type Name (e.g., MyType or Result)
        let name_sexpr = items.get(cursor).ok_or_else(|| {
            self.error(Diagnostic {
                severity: Severity::Error,
                code: Some("E006".to_string()),
                message: "Type Declaration missing a name".to_string(),
                labels: vec![Label {
                    style: LabelStyle::Primary,
                    file_id,
                    range: span.to_owned(),
                    message: "".to_string(),
                }],
                notes: vec![],
            });
        })?;
        let name = match name_sexpr {
            SExpr::Atom(t) => self.source_at(file_id, t.span.clone()).to_string(),
            _ => return Err(()), // Error: Name must be an identifier
        };

        cursor += 1;

        // 3. Parse the Body (Fields or Constructors)
        let mut fields = Vec::new();
        let mut constructors = Vec::new();

        for item in &items[cursor..] {
            if let SExpr::Round(parts, _) = item {
                for part in parts {
                    let SExpr::Round(field_or_constructor, span) = part else {
                        todo!("need to return error here as a type should always have a body");
                    };
                    let item_name = match field_or_constructor.first() {
                        Some(SExpr::Atom(t)) => self.source_at(file_id, t.span.clone()).to_string(),
                        _ => continue,
                    };

                    // Expected shape: (Name ~ Type) or (Name)
                    let item_type = if field_or_constructor.len() >= 3 {
                        match field_or_constructor.get(2) {
                            Some(SExpr::Atom(t)) => {
                                Some(self.source_at(file_id, t.span.clone()).to_string())
                            }
                            _ => None,
                        }
                    } else {
                        None
                    };

                    // Heuristic: If we have params, it's a Variant. If not, it's a Record.
                    if !params.is_empty() {
                        constructors.push((item_name, item_type));
                    } else {
                        // For records, the type is required: (field_one ~ String)
                        if let Some(t_name) = item_type {
                            fields.push((item_name, t_name));
                        }
                    }
                }
            }
        }

        // 4. Return the correct variant
        if !params.is_empty() {
            Ok(TypeDecl::Variant {
                name,
                params,
                constructors,
                span,
            })
        } else {
            Ok(TypeDecl::Record { name, fields, span })
        }
    }

    pub fn lower_file(&mut self, file_id: usize, sexprs: &[SExpr]) -> Vec<Declaration> {
        let mut lowered_declarations = Vec::new();

        for sexpr in sexprs {
            match sexpr {
                SExpr::Round(items, span) => {
                    if let Some(SExpr::Atom(token)) = items.first() {
                        match token.kind {
                            TokenKind::Type => {
                                if let Ok(t) = self.lower_type_decl(file_id, items, span.clone()) {
                                    lowered_declarations.push(Declaration::Type(t));
                                }
                            }
                            TokenKind::Let => {
                                if let Ok(e) = self.lower_let(file_id, items, span.clone()) {
                                    lowered_declarations.push(Declaration::Expression(e));
                                }
                            }
                            _ => {
                                // Handle top-level expressions that aren't 'type' or 'let'
                                if let Ok(e) = self.lower_expr(file_id, sexpr) {
                                    lowered_declarations.push(Declaration::Expression(e));
                                }
                            }
                        }
                    }
                }

                _ => {
                    todo!()
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

    fn lower_atom(&mut self, file_id: usize, token: &Token) -> Result<Expr, ()> {
        match &token.kind {
            // If your lexer already identified it as an Integer
            TokenKind::Int(val) => Ok(Expr::Literal(Literal::Int(*val), token.span.clone())),

            TokenKind::Bool(val) => Ok(Expr::Literal(Literal::Bool(*val), token.span.clone())),

            // If it's a generic word (Identifier)
            TokenKind::Ident | TokenKind::Operator => {
                let name = self.source_at(file_id, token.span.clone());
                Ok(Expr::Variable(name.to_string(), token.span.clone()))
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

                Err(())
            }

            _ => {
                self.error(
                    Diagnostic::error()
                        .with_message("unexpected token")
                        .with_labels(vec![Label::primary(file_id, token.span.clone())]),
                );
                Err(())
            }
        }
    }

    fn lower_array(
        &mut self,
        file_id: usize,
        items: &Vec<SExpr>,
        span: Range<usize>,
    ) -> Result<Expr, ()> {
        let mut lowered_items = Vec::with_capacity(items.len());

        for item in items {
            // Recursively lower each element.
            let expr = self.lower_expr(file_id, item)?;
            lowered_items.push(expr);
        }

        Ok(Expr::Array(lowered_items, span))
    }

    pub fn lower_expr(&mut self, file_id: usize, sexpr: &SExpr) -> Result<Expr, ()> {
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
                Err(())
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

                Err(())
            }
            SExpr::Round(items, span) => {
                if items.is_empty() {
                    return Ok(Expr::Literal(Literal::Unit, span.clone()));
                }

                // Peek at the first item to see if it's a Keyword or a Call
                if let SExpr::Atom(token) = &items[0] {
                    match token.kind {
                        TokenKind::Let => return self.lower_let(file_id, items, span.clone()),
                        TokenKind::If => return self.lower_if(file_id, items, span.clone()),
                        TokenKind::Match => return self.lower_match(file_id, items, span.clone()),
                        _ => {} // Fall through to function call
                    }
                }

                // If it's not a keyword, it's a function call: (func arg1 arg2)
                self.lower_call(file_id, items, span.clone())
            }
        }
    }

    fn lower_match(
        &mut self,
        file_id: usize,
        items: &[SExpr],
        span: Range<usize>,
    ) -> Result<Expr, ()> {
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
            return Err(());
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
                    let err_span = items.get(cursor).map(|s| s.span()).unwrap_or(span.clone());
                    self.error(
                        Diagnostic::error()
                            .with_message("expected arrow '~>' after pattern")
                            .with_labels(vec![Label::primary(file_id, err_span)]),
                    );
                    return Err(());
                }
            }

            // C. Lower the Result expression
            let result_sexpr = items.get(cursor).ok_or_else(|| {
                self.error(
                    Diagnostic::error()
                        .with_message("missing result expression after '~>'")
                        .with_labels(vec![Label::primary(file_id, span.clone())]),
                );
            })?;
            let body = self.lower_expr(file_id, result_sexpr)?;
            cursor += 1;

            arms.push((pattern, body));
        }

        Ok(Expr::Match { target, arms, span })
    }

    fn lower_pattern(&mut self, file_id: usize, sexpr: &SExpr) -> Result<Pattern, ()> {
        match sexpr {
            SExpr::Atom(token) => {
                let text = self.source_at(file_id, token.span.clone());
                let span = token.span.clone();

                match &token.kind {
                    // Handle "_" -> Pattern::Any
                    TokenKind::Ident if text == "_" => Ok(Pattern::Any(span)),

                    // Handle "x" -> Pattern::Variable
                    TokenKind::Ident => Ok(Pattern::Variable(text.to_string(), span)),

                    // Handle literals
                    TokenKind::Int(v) => Ok(Pattern::Literal(Literal::Int(*v), span)),
                    TokenKind::Float(v) => Ok(Pattern::Literal(Literal::Float(*v), span)),
                    TokenKind::Bool(v) => Ok(Pattern::Literal(Literal::Bool(*v), span)),

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
                        Err(())
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

                    return Ok(Pattern::Constructor(name, lowered_args, span.clone()));
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
                Err(())
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
                Err(())
            }
        }
    }

    fn lower_if(
        &mut self,
        file_id: usize,
        items: &[SExpr],
        span: Range<usize>,
    ) -> Result<Expr, ()> {
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
            return Err(());
        }

        // 2. Recursively lower the three parts
        // Using ? ensures that if the condition or branches are broken,
        // we stop building this 'If' node.
        let cond = self.lower_expr(file_id, &items[1])?;
        let then = self.lower_expr(file_id, &items[2])?;
        let els = self.lower_expr(file_id, &items[3])?;

        Ok(Expr::If {
            cond: Box::new(cond),
            then: Box::new(then),
            els: Box::new(els),
            span,
        })
    }

    fn lower_let(
        &mut self,
        file_id: usize,
        items: &[SExpr],
        span: Range<usize>,
    ) -> Result<Expr, ()> {
        let mut cursor = 1; // Skip the 'let' keyword

        // 1. Check for 'rec' keyword
        let mut is_rec = false;
        if let Some(SExpr::Atom(token)) = items.get(cursor)
            && matches!(token.kind, TokenKind::Rec)
        {
            is_rec = true;
            cursor += 1;
        }

        match items.get(cursor) {
            // CASE A: Sequential Variable Bindings -> (let [a 10 b 20] body)
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
                    return Err(());
                }

                // In Opal, if there's no expression after the bracket, it's a top-level binding
                // We use Unit as the "body" placeholder for top-level definitions
                let mut current_expr = if let Some(next_sexpr) = items.get(cursor + 1) {
                    self.lower_expr(file_id, next_sexpr)?
                } else {
                    Expr::Literal(Literal::Unit, span.clone())
                };

                for chunk in bindings.chunks(2).rev() {
                    let name = match &chunk[0] {
                        SExpr::Atom(t) => self.source_at(file_id, t.span.clone()).to_string(),
                        _ => {
                            self.error(
                                Diagnostic::error()
                                    .with_message("expected identifier in let-binding")
                                    .with_labels(vec![Label::primary(file_id, chunk[0].span())]),
                            );
                            return Err(());
                        }
                    };

                    let value = self.lower_expr(file_id, &chunk[1])?;

                    current_expr = Expr::Let {
                        name,
                        is_rec,
                        args: vec![],
                        value: Box::new(value),
                        body: Box::new(current_expr),
                        span: span.clone(),
                    };
                }
                Ok(current_expr)
            }

            Some(SExpr::Atom(name_token)) => {
                let name = self.source_at(file_id, name_token.span.clone()).to_string();
                cursor += 1;

                // 1. Lower args {a b}
                let mut args = Vec::new();
                if let Some(SExpr::Curly(params, _)) = items.get(cursor) {
                    for p in params {
                        if let SExpr::Atom(t) = p {
                            args.push(self.source_at(file_id, t.span.clone()).to_string());
                        }
                    }
                    cursor += 1;
                }

                // 2. Lower the function body (the "value")
                let value_sexpr = items.get(cursor).ok_or_else(|| {
                    self.error(Diagnostic::error().with_message("missing function body"));
                })?;
                let value = self.lower_expr(file_id, value_sexpr)?;
                cursor += 1;

                let body = if let Some(next_item) = items.get(cursor) {
                    self.lower_expr(file_id, next_item)?
                } else {
                    Expr::Literal(Literal::Unit, span.clone())
                };

                Ok(Expr::Let {
                    name,
                    is_rec,
                    args,
                    value: Box::new(value),
                    body: Box::new(body),
                    span,
                })
            }
            _ => {
                self.error(
                    Diagnostic::error()
                        .with_message("invalid let syntax")
                        .with_labels(vec![Label::primary(file_id, span.clone())]),
                );
                Err(())
            }
        }
    }

    fn lower_call(
        &mut self,
        file_id: usize,
        items: &[SExpr],
        span: Range<usize>,
    ) -> Result<Expr, ()> {
        // 1. The first item is the function being called
        // We call lower_expr recursively because it might be a
        // variable (f x) or a nested list ((get_fn) x)
        let func = Box::new(self.lower_expr(file_id, &items[0])?);

        // 2. The remaining items are the arguments
        let mut args = Vec::with_capacity(items.len() - 1);
        for arg_sexpr in &items[1..] {
            args.push(self.lower_expr(file_id, arg_sexpr)?);
        }

        Ok(Expr::Call { func, args, span })
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    // Helper to setup the lowerer with a string
    fn setup(source: &str) -> (Lowerer, usize, Vec<SExpr>) {
        let mut lowerer = Lowerer {
            files: SimpleFiles::new(),
            diagnostics: Vec::new(),
        };

        let tokens = crate::lexer::Lexer::new(source).lex();
        let file_id = lowerer.add_file("test.opal".to_string(), source.to_string());

        // This assumes your Parser returns a Vec<SExpr>
        let sexprs = crate::sexpr::SExprParser::new(tokens)
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
        let exprs = lowerer.lower_file(file_id, &sexprs);

        dbg!(exprs);
        panic!();
    }

    #[test]
    fn test_record_type() {
        let (mut lowerer, file_id, sexprs) = setup(
            "(type MyType (
                        (field_one ~ String)
                        (field_two ~ Int)
                        (field_three ~ Bool)
                        ))",
        );
        let exprs = lowerer.lower_file(file_id, &sexprs);
    }

    #[test]
    fn test_lower_function() {
        let (mut lowerer, file_id, sexprs) = setup("(let f {a} (+ a 10))");
        let exprs = lowerer.lower_file(file_id, &sexprs);

        if let Declaration::Expression(Expr::Let {
            name,
            args,
            value,
            body,
            ..
        }) = &exprs[0]
        {
            // 1. Check metadata
            assert_eq!(name, "f");
            assert_eq!(args, &vec!["a".to_string()]);

            // 2. Check the body (the 'value' of the let)
            if let Expr::Call {
                func,
                args: call_args,
                ..
            } = &**value
            {
                // Check the function being called (+)
                if let Expr::Variable(op_name, _) = &**func {
                    assert_eq!(op_name, "+");
                } else {
                    panic!("Expected function call to be an operator variable '+'");
                }

                // Check call arguments (a, 10)
                assert_eq!(call_args.len(), 2);
                assert!(matches!(call_args[0], Expr::Variable(ref n, _) if n == "a"));
                assert!(matches!(call_args[1], Expr::Literal(Literal::Int(10), _)));
            } else {
                panic!("Expected Let value to be a function call (+ ...)");
            }

            // 3. Check terminal body (Opal top-level style)
            assert!(matches!(**body, Expr::Literal(Literal::Unit, _)));
        } else {
            panic!("Expected a Let expression at the top level");
        }
    }

    #[test]
    fn test_let_sequential_desugaring() {
        // (let [a 10 b 20] a)
        let (mut lowerer, file_id, sexprs) = setup("(let [a 10 b 20] (+ a b ))");
        let exprs = lowerer.lower_file(file_id, &sexprs);

        // This should produce: Let(a, 10, Let(b, 20, Var(a)))
        if let Declaration::Expression(Expr::Let {
            name,
            value: _,
            body,
            ..
        }) = &exprs[0]
        {
            assert_eq!(name, "a");
            if let Expr::Let { name: name2, .. } = &**body {
                assert_eq!(name2, "b");
            } else {
                panic!("Expected nested let for 'b'");
            }
        } else {
            panic!("Expected top-level let for 'a'");
        }
    }

    #[test]
    fn test_valid_if() {
        let (mut lowerer, file_id, sexprs) = setup("(if True 1 2)");
        let exprs = lowerer.lower_file(file_id, &sexprs);
        assert_eq!(exprs.len(), 1, "Should have one lowered expression");

        if let Declaration::Expression(Expr::If {
            cond,
            then,
            els,
            span,
        }) = &exprs[0]
        {
            assert!(
                matches!(**cond, Expr::Literal(Literal::Bool(true), _)),
                "Condition should be True"
            );

            assert!(
                matches!(**then, Expr::Literal(Literal::Int(1), _)),
                "Then-branch should be 1"
            );

            assert!(
                matches!(**els, Expr::Literal(Literal::Int(2), _)),
                "Else-branch should be 2"
            );

            // 4. Verify the Span covers the whole (if ...)
            assert_eq!(span.start, 0);
            assert_eq!(span.end, 13);
        } else {
            panic!("Expected Expr::If, but got: {:?}", exprs[0]);
        }
    }

    #[test]
    fn test_error_reporting_on_invalid_if() {
        // 'if' with missing else branch
        let (mut lowerer, file_id, sexprs) = setup("(if True 1)");
        let exprs = lowerer.lower_file(file_id, &sexprs);

        assert!(exprs.is_empty()); // Should fail to lower
        assert!(!lowerer.diagnostics.is_empty());
        assert_eq!(
            lowerer.diagnostics[0].message,
            "wrong number of arguments for 'if'"
        );
    }

    #[test]
    fn test_match_constructor_patterns() {
        let (mut lowerer, file_id, sexprs) = setup("(match x (Some y) ~> y None ~> 0)");
        let exprs = lowerer.lower_file(file_id, &sexprs);

        if let Declaration::Expression(Expr::Match { arms, .. }) = &exprs[0] {
            let (pattern, _body) = &arms[0];
            if let Pattern::Constructor(name, args, _) = pattern {
                assert_eq!(name, "Some");
                assert_eq!(args.len(), 1);
            } else {
                panic!("Expected Constructor pattern");
            }
        }
    }
}
