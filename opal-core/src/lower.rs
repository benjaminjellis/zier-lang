use crate::{
    ast::*,
    lexer::{Token, TokenKind},
    sexpr::SExpr,
};
use std::ops::Range;

pub struct Lowerer<'a> {
    source: &'a str,
}

impl<'a> Lowerer<'a> {
    pub fn new(source: &'a str) -> Self {
        Self { source }
    }

    /// Entry point for lowering a stream of top-level S-Expressions
    pub fn lower_program(&self, sexprs: Vec<SExpr>) -> Result<(Vec<TypeDef>, Vec<Expr>), String> {
        let mut types = Vec::new();
        let mut exprs = Vec::new();

        for sexpr in sexprs {
            if self.is_type_def(&sexpr) {
                types.push(self.lower_type(sexpr)?);
            } else {
                exprs.push(self.lower_expr(sexpr)?);
            }
        }
        Ok((types, exprs))
    }

    fn is_type_def(&self, sexpr: &SExpr) -> bool {
        if let SExpr::List(items, _) = sexpr {
            matches!(items.first(), Some(SExpr::Atom(t)) if t.kind == TokenKind::Type)
        } else {
            false
        }
    }

    pub fn lower_expr(&self, sexpr: SExpr) -> Result<Expr, String> {
        match sexpr {
            SExpr::Atom(token) => self.lower_atom(token),
            SExpr::List(items, span) => self.lower_list(items, span),
            SExpr::Bracket(_, span) => Err(format!("Unexpected bracket at {:?}", span)),
        }
    }

    fn lower_atom(&self, token: Token) -> Result<Expr, String> {
        let text = &self.source[token.span.clone()];
        match token.kind {
            TokenKind::Int => Ok(Expr::Literal(
                Literal::Int(text.parse().unwrap()),
                token.span,
            )),
            TokenKind::Bool => Ok(Expr::Literal(Literal::Bool(text == "true"), token.span)),
            TokenKind::String => {
                let s = text[1..text.len() - 1].to_string();
                Ok(Expr::Literal(Literal::String(s), token.span))
            }
            // Treat both Identifiers AND Operators as variables/functions
            TokenKind::Ident | TokenKind::Operator => {
                Ok(Expr::Variable(text.to_string(), token.span))
            }
            _ => Err(format!(
                "Unexpected token {:?} at {:?}",
                token.kind, token.span
            )),
        }
    }

    fn lower_list(&self, items: Vec<SExpr>, span: Range<usize>) -> Result<Expr, String> {
        let first = items
            .first()
            .ok_or_else(|| format!("Empty list at {:?}", span))?;

        if let SExpr::Atom(token) = first {
            match token.kind {
                TokenKind::Let => self.lower_let(items, span),
                TokenKind::If => self.lower_if(items, span),
                TokenKind::Match => self.lower_match(items, span),
                _ => self.lower_call(items, span),
            }
        } else {
            self.lower_call(items, span)
        }
    }

    fn lower_let(&self, items: Vec<SExpr>, span: Range<usize>) -> Result<Expr, String> {
        // Syntax: (let rec name [args] value body) or (let [name value] body)
        let mut idx = 1;
        let is_rec = if let Some(SExpr::Atom(t)) = items.get(idx) {
            if t.kind == TokenKind::Rec {
                idx += 1;
                true
            } else {
                false
            }
        } else {
            false
        };

        if is_rec {
            // (let rec fib [n] body)
            let name = self.extract_ident(items.get(idx), "function name")?;
            let args_sexpr = items.get(idx + 1).ok_or("Missing args in let rec")?;
            let args = self.extract_bracket_idents(args_sexpr)?;
            let body = self.lower_expr(
                items
                    .get(idx + 2)
                    .cloned()
                    .ok_or("Missing body in let rec")?,
            )?;

            // In a rec definition, the "value" is the function itself
            Ok(Expr::Let {
                name,
                is_rec: true,
                args,
                value: Box::new(body.clone()), // Logic handled at type-check/codegen
                body: Box::new(body),
                span,
            })
        } else {
            // (let [x 10] body)
            let binding_pair = items.get(1).ok_or("Missing binding pair in let")?;
            let (name, value) = self.extract_binding(binding_pair)?;
            let body = self.lower_expr(items.get(2).cloned().ok_or("Missing body in let")?)?;

            Ok(Expr::Let {
                name,
                is_rec: false,
                args: vec![],
                value: Box::new(value),
                body: Box::new(body),
                span,
            })
        }
    }

    fn lower_if(&self, items: Vec<SExpr>, span: Range<usize>) -> Result<Expr, String> {
        if items.len() != 4 {
            return Err(format!(
                "'if' requires 3 arguments, found {}",
                items.len() - 1
            ));
        }
        Ok(Expr::If {
            cond: Box::new(self.lower_expr(items[1].clone())?),
            then: Box::new(self.lower_expr(items[2].clone())?),
            els: Box::new(self.lower_expr(items[3].clone())?),
            span,
        })
    }

    fn lower_call(&self, items: Vec<SExpr>, span: Range<usize>) -> Result<Expr, String> {
        let func = self.lower_expr(items[0].clone())?;
        let mut args = Vec::new();
        for item in items.into_iter().skip(1) {
            args.push(self.lower_expr(item)?);
        }
        Ok(Expr::Call {
            func: Box::new(func),
            args,
            span,
        })
    }

    pub fn lower_type(&self, sexpr: SExpr) -> Result<TypeDef, String> {
        let (items, span) = match sexpr {
            SExpr::List(items, span) => (items, span),
            _ => return Err("Type definition must be a list".into()),
        };

        // (type ['a] Name ...) or (type Name ...)
        let mut idx = 1;
        let mut params = Vec::new();
        if let Some(SExpr::Bracket(p_items, _)) = items.get(idx) {
            for p in p_items {
                params.push(self.extract_generic(Some(p), "type parameter")?);
            }
            idx += 1;
        }

        let name = self.extract_ident(items.get(idx), "type name")?;
        idx += 1;

        let body = items.get(idx).ok_or("Missing type body")?;
        match body {
            // (type MyType ((f ~ T) ...)) -> Record
            SExpr::List(fields, _) if matches!(fields.first(), Some(SExpr::List(_, _))) => {
                let mut parsed_fields = Vec::new();
                for f in fields {
                    parsed_fields.push(self.extract_field(f)?);
                }
                Ok(TypeDef::Record {
                    name,
                    fields: parsed_fields,
                    span,
                })
            }
            // (type Option None (Some ~ 'a)) -> Variant
            _ => {
                let mut cases = Vec::new();
                for i in idx..items.len() {
                    cases.push(self.extract_variant_case(&items[i])?);
                }
                Ok(TypeDef::Variant {
                    name,
                    params,
                    cases,
                    span,
                })
            }
        }
    }

    // --- Helpers for extraction ---

    fn extract_ident(&self, sexpr: Option<&SExpr>, context: &str) -> Result<String, String> {
        match sexpr {
            Some(SExpr::Atom(t)) if t.kind == TokenKind::Ident => {
                Ok(self.source[t.span.clone()].to_string())
            }
            _ => Err(format!("Expected identifier for {}", context)),
        }
    }

    fn extract_generic(&self, sexpr: Option<&SExpr>, context: &str) -> Result<String, String> {
        match sexpr {
            Some(SExpr::Atom(t)) if t.kind == TokenKind::Generic => {
                Ok(self.source[t.span.clone()].to_string())
            }
            _ => Err(format!("Expected generic (e.g. 'a) for {}", context)),
        }
    }

    fn extract_bracket_idents(&self, sexpr: &SExpr) -> Result<Vec<String>, String> {
        match sexpr {
            SExpr::Bracket(items, _) => items
                .iter()
                .map(|i| self.extract_ident(Some(i), "argument"))
                .collect(),
            _ => Err("Expected bracketed list of identifiers".into()),
        }
    }

    fn extract_binding(&self, sexpr: &SExpr) -> Result<(String, Expr), String> {
        match sexpr {
            SExpr::Bracket(items, _) if items.len() == 2 => {
                let name = self.extract_ident(items.get(0), "binding name")?;
                let val = self.lower_expr(items[1].clone())?;
                Ok((name, val))
            }
            _ => Err("Invalid binding format, expected [name value]".into()),
        }
    }

    fn extract_field(&self, sexpr: &SExpr) -> Result<(String, TypeUsage), String> {
        // (field ~ Type)
        if let SExpr::List(items, _) = sexpr {
            let name = self.extract_ident(items.get(0), "field name")?;
            let usage = self.extract_type_usage(items.get(2))?;
            Ok((name, usage))
        } else {
            Err("Invalid field format".into())
        }
    }

    fn extract_variant_case(&self, sexpr: &SExpr) -> Result<VariantCase, String> {
        match sexpr {
            SExpr::Atom(t) => Ok(VariantCase {
                name: self.source[t.span.clone()].to_string(),
                payload: None,
            }),
            SExpr::List(items, _) => {
                let name = self.extract_ident(items.get(0), "variant name")?;
                let payload = Some(self.extract_type_usage(items.get(2))?);
                Ok(VariantCase { name, payload })
            }
            _ => Err("Invalid variant case".into()),
        }
    }

    fn extract_type_usage(&self, sexpr: Option<&SExpr>) -> Result<TypeUsage, String> {
        match sexpr {
            Some(SExpr::Atom(t)) if t.kind == TokenKind::Ident => {
                Ok(TypeUsage::Named(self.source[t.span.clone()].to_string()))
            }
            Some(SExpr::Atom(t)) if t.kind == TokenKind::Generic => {
                Ok(TypeUsage::Generic(self.source[t.span.clone()].to_string()))
            }
            _ => Err("Invalid type usage".into()),
        }
    }

    fn lower_match(&self, items: Vec<SExpr>, span: Range<usize>) -> Result<Expr, String> {
        let target = Box::new(self.lower_expr(items[1].clone())?);
        let mut arms = Vec::new();
        let mut i = 2;
        while i < items.len() {
            let pat = self.lower_pattern(&items[i])?;
            let res = self.lower_expr(items[i + 1].clone())?;
            arms.push((pat, res));
            i += 2;
        }
        Ok(Expr::Match { target, arms, span })
    }

    fn lower_pattern(&self, sexpr: &SExpr) -> Result<Pattern, String> {
        match sexpr {
            SExpr::Atom(t) => {
                let text = &self.source[t.span.clone()];
                if text == "_" {
                    Ok(Pattern::Any(t.span.clone()))
                } else if t.kind == TokenKind::Ident {
                    Ok(Pattern::Variable(text.to_string(), t.span.clone()))
                } else {
                    Ok(Pattern::Literal(self.lower_literal(t)?, t.span.clone()))
                }
            }
            SExpr::List(items, span) => {
                let name = self.extract_ident(items.get(0), "pattern constructor")?;
                let mut args = Vec::new();
                for arg in items.iter().skip(1) {
                    args.push(self.lower_pattern(arg)?);
                }
                Ok(Pattern::Constructor(name, args, span.clone()))
            }
            _ => Err("Invalid pattern".into()),
        }
    }

    fn lower_literal(&self, t: &Token) -> Result<Literal, String> {
        let text = &self.source[t.span.clone()];
        match t.kind {
            TokenKind::Int => Ok(Literal::Int(text.parse().unwrap())),
            TokenKind::Bool => Ok(Literal::Bool(text == "true")),
            TokenKind::String => Ok(Literal::String(text[1..text.len() - 1].to_string())),
            _ => Err("Not a literal".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        lexer::{Token, TokenKind},
        sexpr::SExprParser,
    };
    use logos::Logos;

    fn full_lower(source: &str) -> (Vec<TypeDef>, Vec<Expr>) {
        let tokens: Vec<Token> = TokenKind::lexer(source)
            .spanned()
            .map(|(kind, span)| Token {
                kind: kind.expect("Lex error"),
                span,
            })
            .collect();

        let sexprs = SExprParser::new(tokens).parse().expect("SExpr Parse error");
        let lowerer = Lowerer::new(source);
        lowerer.lower_program(sexprs).expect("Lowering error")
    }

    #[test]
    fn test_lower_fib_rec() {
        let code = "(let rec fib [n] (if (= n 0) 0 1))";
        let (_, exprs) = full_lower(code);

        assert_eq!(exprs.len(), 1);
        if let Expr::Let {
            name, is_rec, args, ..
        } = &exprs[0]
        {
            assert_eq!(name, "fib");
            assert!(is_rec);
            assert_eq!(args, &vec!["n".to_string()]);
        } else {
            panic!("Expected Let expression");
        }
    }

    #[test]
    fn test_lower_variant_type() {
        let code = "(type ['a] Option None (Some ~ 'a))";
        let (types, _) = full_lower(code);

        assert_eq!(types.len(), 1);
        if let TypeDef::Variant {
            name,
            params,
            cases,
            ..
        } = &types[0]
        {
            assert_eq!(name, "Option");
            assert_eq!(params, &vec!["'a".to_string()]);
            assert_eq!(cases.len(), 2);
            assert_eq!(cases[0].name, "None");
            assert_eq!(cases[1].name, "Some");
            assert!(matches!(cases[1].payload, Some(TypeUsage::Generic(_))));
        } else {
            panic!("Expected Variant definition");
        }
    }

    #[test]
    fn test_lower_record_type() {
        let code = "(type Point ((x ~ Int) (y ~ Int)))";
        let (types, _) = full_lower(code);

        if let TypeDef::Record { name, fields, .. } = &types[0] {
            assert_eq!(name, "Point");
            assert_eq!(fields.len(), 2);
            assert_eq!(fields[0].0, "x");
            assert_eq!(fields[1].0, "y");
        } else {
            panic!("Expected Record definition");
        }
    }

    #[test]
    fn test_lower_match_expression() {
        let code = "(match t VariantOne 1 (VariantTwo msg) 2)";
        let (_, exprs) = full_lower(code);

        if let Expr::Match {
            target: _, arms, ..
        } = &exprs[0]
        {
            assert_eq!(arms.len(), 2);
            // Check first pattern
            match &arms[0].0 {
                Pattern::Variable(name, _) => assert_eq!(name, "VariantOne"),
                _ => panic!("Expected Variable pattern"),
            }
            // Check second pattern (Constructor)
            match &arms[1].0 {
                Pattern::Constructor(name, args, _) => {
                    assert_eq!(name, "VariantTwo");
                    assert_eq!(args.len(), 1);
                }
                _ => panic!("Expected Constructor pattern"),
            }
        }
    }
}
