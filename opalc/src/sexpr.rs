use crate::lexer::{Token, TokenKind};
use codespan_reporting::diagnostic::{Diagnostic, Label};
use std::ops::Range;

#[derive(Debug, Clone, PartialEq)]
pub enum SExpr {
    /// A single "word" or "literal" (e.g., 'fib', '10', 'type')
    Atom(Token),
    /// A round bracket list: ( ... )
    Round(Vec<SExpr>, Range<usize>),
    /// A square bracket list: [ ... ]
    Square(Vec<SExpr>, Range<usize>),
    /// A curly bracket arg list: { ... }
    Curly(Vec<SExpr>, Range<usize>),
}

impl SExpr {
    pub(crate) fn span(&self) -> Range<usize> {
        match self {
            SExpr::Atom(t) => t.span.clone(),
            SExpr::Round(_, s) => s.clone(),
            SExpr::Square(_, s) => s.clone(),
            SExpr::Curly(_, s) => s.clone(),
        }
    }

    /// Recursively converts the S-Expression back into a string using the original source
    #[cfg(test)]
    pub fn to_source(&self, source: &str) -> String {
        match self {
            SExpr::Atom(token) => source[token.span.clone()].to_string(),
            SExpr::Round(items, _) => {
                let inner = items
                    .iter()
                    .map(|e| e.to_source(source))
                    .collect::<Vec<_>>()
                    .join(" ");
                format!("({})", inner)
            }
            SExpr::Square(items, _) => {
                let inner = items
                    .iter()
                    .map(|e| e.to_source(source))
                    .collect::<Vec<_>>()
                    .join(" ");
                format!("[{}]", inner)
            }
            SExpr::Curly(items, _) => {
                let inner = items
                    .iter()
                    .map(|e| e.to_source(source))
                    .collect::<Vec<_>>()
                    .join(" ");
                format!("{{ {} }}", inner)
            }
        }
    }
}

pub struct SExprParser {
    tokens: Vec<Token>,
    pos: usize,
    file_id: usize,
}

enum SExprType {
    Round,
    Square,
    Curly,
}

impl SExprParser {
    pub fn new(tokens: Vec<Token>, file_id: usize) -> Self {
        Self {
            tokens,
            pos: 0,
            file_id,
        }
    }

    pub fn parse(&mut self) -> Result<Vec<SExpr>, Diagnostic<usize>> {
        let mut results = Vec::new();
        while !self.is_at_end() {
            if let Some(next) = self.peek()
                && matches!(
                    next.kind,
                    TokenKind::RRound | TokenKind::RSquare | TokenKind::RCurly
                )
            {
                return Err(self.error(
                    format!("Unexpected top-level delimiter {:?}", next.kind),
                    next.span,
                ));
            }
            results.push(self.parse_one()?);
        }
        Ok(results)
    }

    fn parse_one(&mut self) -> Result<SExpr, Diagnostic<usize>> {
        let token = self.peek().ok_or_else(|| {
            let last_span = self.tokens.last().map(|t| t.span.clone()).unwrap_or(0..0);
            self.error("Unexpected end of input".to_string(), last_span)
        })?;

        match token.kind {
            TokenKind::LRound => self.parse_sequence(TokenKind::RRound, SExprType::Round),
            TokenKind::LSquare => self.parse_sequence(TokenKind::RSquare, SExprType::Square),
            TokenKind::LCurly => self.parse_sequence(TokenKind::RCurly, SExprType::Curly),

            TokenKind::RRound | TokenKind::RSquare | TokenKind::RCurly => {
                Err(self.error(format!("Unexpected delimiter {:?}", token.kind), token.span))
            }

            _ => {
                self.advance();
                Ok(SExpr::Atom(token))
            }
        }
    }

    fn parse_sequence(
        &mut self,
        closer_kind: TokenKind,
        sexpr_type: SExprType,
    ) -> Result<SExpr, Diagnostic<usize>> {
        let open_token = self.advance();
        let open_span = open_token.span.clone();
        let mut children = Vec::new();

        while let Some(next) = self.peek() {
            if next.kind == closer_kind {
                let close_token = self.advance();
                let full_span = open_span.start..close_token.span.end;
                return Ok(match sexpr_type {
                    SExprType::Round => SExpr::Round(children, full_span),
                    SExprType::Square => SExpr::Square(children, full_span),
                    SExprType::Curly => SExpr::Curly(children, full_span),
                });
            }

            if matches!(
                next.kind,
                TokenKind::RRound | TokenKind::RSquare | TokenKind::RCurly
            ) {
                return Err(self.mismatch_error(open_span, closer_kind, next));
            }

            children.push(self.parse_one()?);
        }

        Err(self.error(
            format!("Unclosed delimiter: expected {:?}", closer_kind),
            open_span,
        ))
    }

    fn error(&self, message: String, span: Range<usize>) -> Diagnostic<usize> {
        Diagnostic::error()
            .with_message("Syntax Error")
            .with_labels(vec![
                Label::primary(self.file_id, span).with_message(message),
            ])
    }

    fn mismatch_error(
        &self,
        open_span: Range<usize>,
        expected: TokenKind,
        found: Token,
    ) -> Diagnostic<usize> {
        Diagnostic::error()
            .with_message("Mismatched Delimiters")
            .with_labels(vec![
                Label::primary(self.file_id, found.span).with_message(format!(
                    "Found {}, but expected a {}",
                    found.kind.name(),
                    expected.name()
                )),
                Label::secondary(self.file_id, open_span).with_message(format!(
                    "This bracket is opened but the {} is missing",
                    expected.name()
                )),
            ])
    }

    fn peek(&self) -> Option<Token> {
        self.tokens.get(self.pos).cloned()
    }

    fn advance(&mut self) -> Token {
        let t = self.tokens[self.pos].clone();
        self.pos += 1;
        t
    }

    fn is_at_end(&self) -> bool {
        self.pos >= self.tokens.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codespan_reporting::files::SimpleFiles;
    use logos::Logos;

    fn parse_str(input: &str) -> Vec<SExpr> {
        let lex = crate::lexer::TokenKind::lexer(input);
        let tokens: Vec<Token> = lex
            .spanned()
            .map(|(kind, span)| Token {
                kind: kind.expect("Lex error"),
                span,
            })
            .collect();
        let mut file = SimpleFiles::new();
        let file_id = file.add("test.opal", input);

        let mut parser = SExprParser::new(tokens, file_id);
        parser.parse().expect("Parse error")
    }

    #[test]
    fn test_top_level_nesting() {
        let code = "(let f {a} (+ a 10))";
        let exprs = parse_str(code);
        assert_eq!(exprs.len(), 1);
        if let SExpr::Round(inner, _) = &exprs[0] {
            assert_eq!(inner.len(), 4);
            match &inner[2] {
                SExpr::Curly(args, _) => assert_eq!(args.len(), 1),
                _ => panic!("Expected SExpr::Curly for arguments"),
            }
            assert!(matches!(inner[3], SExpr::Round(_, _)));
        }
    }

    #[test]
    fn test_complex_spec() {
        let code = "(type ['a] Option None (Some ~ 'a))";
        let exprs = parse_str(code);
        if let SExpr::Round(inner, _) = &exprs[0] {
            assert_eq!(inner.len(), 5);
            assert!(matches!(inner[1], SExpr::Square(_, _)));
            assert!(matches!(inner[4], SExpr::Round(_, _)));
        }
    }

    #[test]
    #[should_panic(expected = "Unclosed delimiter")]
    fn test_unclosed_paren() {
        parse_str("(let x 1");
    }

    #[test]
    #[should_panic(expected = "Unexpected top-level delimiter")]
    fn test_extra_closing() {
        parse_str("(let x 1))");
    }
}
