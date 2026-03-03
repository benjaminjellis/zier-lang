use crate::lexer::{Token, TokenKind};
use std::ops::Range;

#[derive(Debug, Clone, PartialEq)]
pub enum SExpr {
    /// A single "word" or "literal" (e.g., 'fib', '10', 'type')
    Atom(Token),
    /// A round bracket list: ( ... )
    List(Vec<SExpr>, Range<usize>),
    /// A square bracket list: [ ... ]
    Bracket(Vec<SExpr>, Range<usize>),
}

impl SExpr {
    fn span(&self) -> Range<usize> {
        match self {
            SExpr::Atom(t) => t.span.clone(),
            SExpr::List(_, s) => s.clone(),
            SExpr::Bracket(_, s) => s.clone(),
        }
    }

    /// Recursively converts the S-Expression back into a string using the original source
    #[cfg(test)]
    pub fn to_source(&self, source: &str) -> String {
        match self {
            SExpr::Atom(token) => source[token.span.clone()].to_string(),
            SExpr::List(items, _) => {
                let inner = items
                    .iter()
                    .map(|e| e.to_source(source))
                    .collect::<Vec<_>>()
                    .join(" ");
                format!("({})", inner)
            }
            SExpr::Bracket(items, _) => {
                let inner = items
                    .iter()
                    .map(|e| e.to_source(source))
                    .collect::<Vec<_>>()
                    .join(" ");
                format!("[{}]", inner)
            }
        }
    }
}

pub struct SExprParser {
    tokens: Vec<Token>,
    pos: usize,
}

impl SExprParser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    /// Entry point: Parse the entire token stream into a list of top-level S-Expressions
    pub fn parse(&mut self) -> Result<Vec<SExpr>, String> {
        let mut results = Vec::new();
        while !self.is_at_end() {
            results.push(self.parse_one()?);
        }
        Ok(results)
    }

    fn parse_one(&mut self) -> Result<SExpr, String> {
        let token = self.peek().ok_or("Unexpected end of input")?;

        match token.kind {
            TokenKind::LParen => self.parse_sequence(TokenKind::RParen, true),
            TokenKind::LBracket => self.parse_sequence(TokenKind::RBracket, false),
            TokenKind::RParen => Err(format!("Unexpected ')' at {:?}", token.span)),
            TokenKind::RBracket => Err(format!("Unexpected ']' at {:?}", token.span)),
            _ => {
                self.advance();
                Ok(SExpr::Atom(token))
            }
        }
    }

    fn parse_sequence(&mut self, closer: TokenKind, is_paren: bool) -> Result<SExpr, String> {
        let open_token = self.advance(); // Consume the ( or [
        let start_byte = open_token.span.start;
        let mut children = Vec::new();

        while let Some(next) = self.peek() {
            if next.kind == closer {
                let close_token = self.advance();
                let span = start_byte..close_token.span.end;
                return Ok(if is_paren {
                    SExpr::List(children, span)
                } else {
                    SExpr::Bracket(children, span)
                });
            }
            // Recurse to find the next element in the list
            children.push(self.parse_one()?);
        }

        Err(format!("Unclosed delimiter starting at {}", start_byte))
    }

    // Helper: Look at current token
    fn peek(&self) -> Option<Token> {
        self.tokens.get(self.pos).cloned()
    }

    // Helper: Move forward and return the consumed token
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
    use logos::Logos;

    // Helper to turn a string into a Vec of SExpr
    fn parse_str(input: &str) -> Vec<SExpr> {
        let lex = crate::lexer::TokenKind::lexer(input);
        let tokens: Vec<Token> = lex
            .spanned()
            .map(|(kind, span)| Token {
                kind: kind.expect("Lex error"),
                span,
            })
            .collect();

        let mut parser = SExprParser::new(tokens);
        parser.parse().expect("Parse error")
    }

    #[test]
    fn test_basic_nesting() {
        let code = "(let x [1 2])";
        let exprs = parse_str(code);

        // Check top-level is one List
        assert_eq!(exprs.len(), 1);
        if let SExpr::List(inner, _) = &exprs[0] {
            assert_eq!(inner.len(), 3); // 'let', 'x', '[1 2]'

            // Check the nested bracket
            match &inner[2] {
                SExpr::Bracket(items, _) => assert_eq!(items.len(), 2),
                _ => panic!("Expected Bracket"),
            }
        } else {
            panic!("Expected List");
        }
    }

    #[test]
    fn test_complex_spec() {
        let code = "(type ['a] Option None (Some ~ 'a))";
        let exprs = parse_str(code);

        // Root list: type, ['a], Option, None, (Some ~ 'a)
        if let SExpr::List(inner, _) = &exprs[0] {
            assert_eq!(inner.len(), 5);
            assert!(matches!(inner[1], SExpr::Bracket(_, _)));
            assert!(matches!(inner[4], SExpr::List(_, _)));
        }
    }

    #[test]
    #[should_panic(expected = "Unclosed delimiter")]
    fn test_unclosed_paren() {
        parse_str("(let x 1");
    }

    #[test]
    #[should_panic(expected = "Unexpected ')'")]
    fn test_extra_closing() {
        parse_str("(let x 1))");
    }

    fn parse_helper(input: &str) -> Vec<SExpr> {
        let tokens: Vec<Token> = TokenKind::lexer(input)
            .spanned()
            .map(|(kind, span)| Token {
                kind: kind.expect("Lex error"),
                span,
            })
            .collect();

        let mut parser = SExprParser::new(tokens);
        parser.parse().expect("Parse error")
    }

    #[test]
    fn test_round_trip() {
        let original_code = r#"(type ['e 'a] Result (Error ~ 'e) (Ok ~ 'a)) (let rec fib [n] (if (or (= n 0) (= n 1)) n (+ (fib (- n 1)) (fib (- n 2)))))"#;

        // 1. First Pass: String -> SExpr Tree
        let tree_one = parse_helper(original_code);

        // 2. Stringify: SExpr Tree -> New String
        let printed = tree_one
            .iter()
            .map(|e| e.to_source(original_code))
            .collect::<Vec<_>>()
            .join(" ");

        // 3. Second Pass: New String -> SExpr Tree
        let tree_two = parse_helper(&printed);

        // 4. Verification: The trees should be structurally identical.
        // Note: Spans will differ because of whitespace normalization,
        // so we compare the "to_source" outputs again or implement a
        // structure-only equality check.
        assert_eq!(tree_one.len(), tree_two.len());

        for (a, b) in tree_one.iter().zip(tree_two.iter()) {
            assert_eq!(a.to_source(original_code), b.to_source(&printed));
        }
    }
}
