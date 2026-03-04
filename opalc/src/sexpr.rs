use crate::lexer::{Token, TokenKind};
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
    /// A hash bracket array literal #[ ... ]
    Array(Vec<SExpr>, Range<usize>),
}

impl SExpr {
    pub(crate) fn span(&self) -> Range<usize> {
        match self {
            SExpr::Atom(t) => t.span.clone(),
            SExpr::Round(_, s) => s.clone(),
            SExpr::Square(_, s) => s.clone(),
            SExpr::Array(_, s) => s.clone(),
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
            SExpr::Array(items, _) => {
                let inner = items
                    .iter()
                    .map(|e| e.to_source(source))
                    .collect::<Vec<_>>()
                    .join(" ");
                format!("#[{}]", inner)
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
}

enum SExprType {
    List,
    Bracket,
    Array,
    Curly,
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
            TokenKind::LRound => self.parse_sequence(TokenKind::RRound, SExprType::List),
            TokenKind::LSquare => self.parse_sequence(TokenKind::RSquare, SExprType::Bracket),
            TokenKind::HashLSquare => self.parse_sequence(TokenKind::RSquare, SExprType::Array),
            TokenKind::LCurly => self.parse_sequence(TokenKind::RCurly, SExprType::Curly),
            TokenKind::RRound => Err(format!("Unexpected ')' at {:?}", token.span)),
            TokenKind::RSquare => Err(format!("Unexpected ']' at {:?}", token.span)),
            _ => {
                self.advance();
                Ok(SExpr::Atom(token))
            }
        }
    }

    fn parse_sequence(&mut self, closer: TokenKind, kind: SExprType) -> Result<SExpr, String> {
        let open_token = self.advance();
        let start_byte = open_token.span.start;
        let mut children = Vec::new();

        while let Some(next) = self.peek() {
            if next.kind == closer {
                let close_token = self.advance();
                let span = start_byte..close_token.span.end;
                return Ok(match kind {
                    SExprType::List => SExpr::Round(children, span),
                    SExprType::Bracket => SExpr::Square(children, span),
                    SExprType::Array => SExpr::Array(children, span),
                    SExprType::Curly => SExpr::Curly(children, span),
                });
            }
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
    fn test_top_level_nesting() {
        // 1. A top-level function definition
        let code = "(let f {a} (+ a 10))";
        let exprs = parse_str(code);

        assert_eq!(exprs.len(), 1);
        if let SExpr::Round(inner, _) = &exprs[0] {
            assert_eq!(inner.len(), 4); // 'let', 'f', '{a}', '(+ a 10)'

            // Check the curly brace parameters
            match &inner[2] {
                SExpr::Curly(args, _) => assert_eq!(args.len(), 1),
                _ => panic!("Expected SExpr::Brace for arguments"),
            }

            // Check the function body list
            assert!(matches!(inner[3], SExpr::Round(_, _)));
        }

        // 2. A top-level variable definition
        let code_var = "(let x 100)";
        let exprs_var = parse_str(code_var);
        if let SExpr::Round(inner, _) = &exprs_var[0] {
            assert_eq!(inner.len(), 3); // 'let', 'x', '100'
            assert!(matches!(inner[2], SExpr::Atom(_)));
        }
    }

    #[test]
    fn test_complex_spec() {
        let code = "(type ['a] Option None (Some ~ 'a))";
        let exprs = parse_str(code);

        // Root list: type, ['a], Option, None, (Some ~ 'a)
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
        // We added a new top-level definition 'demo' that uses the #[1 2 3] array syntax
        let original_code = r#"
            (type ['e 'a] Result (Error ~ 'e) (Ok ~ 'a)) 
            (let rec fib {n} (if (or (= n 0) (= n 1)) n (+ (fib (- n 1)) (fib (- n 2)))))
            (let demo {} (let [v #[1 2 3]] v))
        "#;

        // 1. First Pass: String -> SExpr Tree
        let tree_one = parse_helper(original_code);

        // 2. Stringify: SExpr Tree -> New String (normalizes whitespace)
        let printed = tree_one
            .iter()
            .map(|e| e.to_source(original_code))
            .collect::<Vec<_>>()
            .join(" ");

        // 3. Second Pass: New String -> SExpr Tree
        let tree_two = parse_helper(&printed);

        // 4. Verification
        assert_eq!(
            tree_one.len(),
            tree_two.len(),
            "Tree length mismatch after round-trip"
        );

        for (a, b) in tree_one.iter().zip(tree_two.iter()) {
            // This ensures that (SExpr::Array -> "#[...]") survives the trip
            assert_eq!(
                a.to_source(original_code),
                b.to_source(&printed),
                "Structural mismatch in S-Expression"
            );
        }

        // Explicitly check that the array variant was preserved in the second tree
        if let SExpr::Round(items, _) = &tree_two[2] {
            // (let demo [] (let [v #[1 2 3]] v))
            // items[3] is the (let ...) body
            if let SExpr::Round(inner_let, _) = &items[3] {
                // inner_let[1] is the [v #[1 2 3]] bracket
                if let SExpr::Square(binding, _) = &inner_let[1] {
                    assert!(
                        matches!(binding[1], SExpr::Array(_, _)),
                        "Array was not parsed as SExpr::Array in second pass"
                    );
                }
            }
        }
    }

    #[test]
    fn test_array_literal() {
        let code = "#[1 2 3]";
        let exprs = parse_str(code);

        assert_eq!(exprs.len(), 1);
        if let SExpr::Array(items, _) = &exprs[0] {
            assert_eq!(items.len(), 3);
            // Verify items are atoms
            assert!(matches!(items[0], SExpr::Atom(_)));
        } else {
            panic!("Expected Array variant");
        }
    }

    #[test]
    fn test_nested_array_in_let() {
        // This mirrors your 'demo' logic: (let [v #[1 2 3]])
        let code = "(let [v #[1 2 3]])";
        let exprs = parse_str(code);

        if let SExpr::Round(outer, _) = &exprs[0] {
            // outer[1] should be the Bracket [v #[1 2 3]]
            if let SExpr::Square(binding, _) = &outer[1] {
                assert_eq!(binding.len(), 2);
                assert_eq!(binding[0].to_source(code), "v");

                // binding[1] should be the Array
                assert!(matches!(binding[1], SExpr::Array(_, _)));
                assert_eq!(binding[1].to_source(code), "#[1 2 3]");
            } else {
                panic!("Expected structural Bracket for bindings");
            }
        }
    }

    #[test]
    fn test_valid_opal_nested_structure() {
        // A top-level function 'division' with local logic
        let code = "(let division {a b} (if (= b 0) None (Some (/ a b))))";
        let exprs = parse_str(code);

        assert_eq!(exprs.len(), 1);

        if let SExpr::Round(top_level, _) = &exprs[0] {
            // Items: 0:let, 1:division, 2:{a b}, 3:(if ...)
            assert_eq!(top_level.len(), 4, "Top-level let should have 4 parts");

            // 1. Check the Name
            assert_eq!(top_level[1].to_source(code), "division");

            // 2. Check the Parameters (Curly Braces)
            if let SExpr::Curly(args, _) = &top_level[2] {
                assert_eq!(args.len(), 2);
                assert_eq!(args[0].to_source(code), "a");
                assert_eq!(args[1].to_source(code), "b");
            } else {
                panic!("Expected SExpr::Curly for parameters");
            }

            // 3. Check the Implementation (The 'if' list)
            if let SExpr::Round(if_expr, _) = &top_level[3] {
                assert_eq!(if_expr[0].to_source(code), "if");
            } else {
                panic!("Expected SExpr::List for function body");
            }
        }
    }
}
