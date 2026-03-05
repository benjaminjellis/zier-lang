use logos::Logos;
use std::ops::Range;

pub struct Lexer<'a> {
    source: &'a str,
}

impl<'a> Lexer<'a> {
    pub fn new(source: &'a str) -> Self {
        Self { source }
    }

    pub fn lex(self) -> Vec<Token> {
        let mut tokens = vec![];
        let mut lex = TokenKind::lexer(self.source);
        while let Some(kind_result) = lex.next() {
            let kind = kind_result.unwrap_or(TokenKind::Error);
            tokens.push(Token {
                kind,
                span: lex.span(), // Logos tracks the span of the last found token
            });
        }
        tokens
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Range<usize>,
}

#[derive(Logos, Clone, Debug, PartialEq)]
#[logos(skip r"[ \t\r\n]+")]
#[logos(skip(r";[^\n]*", allow_greedy = true))]
pub enum TokenKind {
    // Structural symbols
    #[token("(")]
    LRound,
    #[token(")")]
    RRound,
    #[token("[")]
    LSquare,
    #[token("]")]
    RSquare,
    #[token("~")]
    Tilde,
    #[token("#[")]
    HashLSquare,
    #[token("{")]
    LCurly,
    #[token("}")]
    RCurly,

    // Keywords
    #[token("pub")]
    Pub,
    #[token("type")]
    Type,
    #[token("let")]
    Let,
    #[token("if")]
    If,
    #[token("match")]
    Match,
    #[token("or")]
    Or,
    #[token("and")]
    And,
    #[token("~>")]
    Arrow,
    #[regex(":[a-zA-Z_][a-zA-Z0-9_]*", |lex| lex.slice()[1..].to_string())]
    NamedField(String),

    #[regex(r"[0-9]+\.[0-9]+", |lex| lex.slice().parse::<f64>().ok())] // Matches 3.14, 0.5, etc.
    Float(f64),

    #[regex(r"[0-9]+", |lex| lex.slice().parse::<i64>().ok())]
    Int(i64),

    // Generics (e.g., 'a, 'e)
    #[regex(r"'[a-z][a-zA-Z0-9_]*")]
    Generic,

    // Identifiers (Variables, Type names, Variants)
    // This regex allows for standard camelCase or snake_case
    #[regex(r"[a-zA-Z_][a-zA-Z0-9_]*")]
    Ident,

    // Literals
    #[regex(r#""([^"\\]|\\.)*""#, allow_greedy = true)]
    String,

    #[token("True", |_| true)]
    #[token("False", |_| false)]
    Bool(bool),

    // Operators
    // Includes '.' so that float ops like +. -. *. /. lex as a single token
    #[regex(r"[\+\-\*/=<>!&|\.]+")]
    Operator,

    #[token("error")]
    Error,
}

impl TokenKind {
    pub(crate) fn name(&self) -> &str {
        match self {
            TokenKind::LRound => "opening bracket '('",
            TokenKind::RRound => "closing bracket ')'",
            TokenKind::LSquare => "opening bracket '['",
            TokenKind::RSquare => "closing bracket ']'",
            TokenKind::Tilde => "tilde '~'",
            TokenKind::HashLSquare => "array prefix '#['",
            TokenKind::LCurly => "opening bracket '{'",
            TokenKind::RCurly => "closing bracket '}'",
            TokenKind::Pub => "keyword 'pub'",
            TokenKind::Type => "keyword 'type'",
            TokenKind::Let => "keyword 'let'",
            TokenKind::If => "keyword 'if'",
            TokenKind::Match => "keyword 'match'",
            TokenKind::Or => "operator 'or'",
            TokenKind::And => "operator 'and'",
            TokenKind::Arrow => "arrow '~>'",
            TokenKind::NamedField(_) => "field name (e.g. :name)",
            TokenKind::Float(_) => "float literal",
            TokenKind::Int(_) => "integer literal",
            TokenKind::Generic => "generic type variable (e.g. 'a)",
            TokenKind::Ident => "identifier",
            TokenKind::String => "string literal",
            TokenKind::Bool(_) => "boolean literal",
            TokenKind::Operator => "operator",
            TokenKind::Error => "invalid token",
        }
    }
}

#[cfg(test)]
mod tests {
    use logos::Logos;

    use crate::lexer::{TokenKind, TokenKind::*};

    #[test]
    fn vector_literal() {
        let expected_tokens = [
            LRound,
            Let,
            Ident,
            LCurly,
            RCurly,
            HashLSquare,
            Int(1),
            Int(2),
            Int(3),
            Int(4),
            RSquare,
            RRound,
        ];
        let source = r#"
            (let vec_literal {}
                #[1 2 3 4] )
            "#;
        let lexer = TokenKind::lexer(source);
        let tokens = lexer.into_iter().map(|t| t.unwrap()).collect::<Vec<_>>();
        assert_eq!(tokens, expected_tokens);
    }

    #[test]
    fn bool() {
        let expected_tokens = [LRound, Let, Ident, LCurly, RCurly, Bool(false), RRound];

        let source = r#"
            (let falsey {}
                False )
        "#;
        let lexer = TokenKind::lexer(source);
        let tokens = lexer.into_iter().map(|t| t.unwrap()).collect::<Vec<_>>();
        assert_eq!(tokens, expected_tokens);
    }

    #[test]
    fn string() {
        let expected_tokens = [
            LRound, Let, Ident, LCurly, Ident, RCurly, LRound, Ident, String, Ident, RRound, RRound,
        ];

        let source = r#"
            (let say_hello {name}
                (str "Hello" name))
        "#;
        let lexer = TokenKind::lexer(source);
        let tokens = lexer.into_iter().map(|t| t.unwrap()).collect::<Vec<_>>();

        assert_eq!(tokens, expected_tokens);
    }

    #[test]
    fn int() {
        let expected_tokens = [
            LRound,
            Let,
            Ident,
            LCurly,
            Ident,
            RCurly,
            LRound,
            Operator,
            Int(2),
            Ident,
            RRound,
            RRound,
        ];

        let source = r#"
            (let double {input}
                (* 2 input))
        "#;
        let lexer = TokenKind::lexer(source);
        let tokens = lexer.into_iter().map(|t| t.unwrap()).collect::<Vec<_>>();

        assert_eq!(tokens, expected_tokens);
    }

    #[test]
    fn float() {
        let expected_tokens = [
            LRound,
            Let,
            Ident,
            LCurly,
            Ident,
            RCurly,
            LRound,
            Operator,
            Float(0.5),
            Ident,
            RRound,
            RRound,
        ];

        let source = r#"
            (let half {input}
                (* 0.5 input))
        "#;
        let lexer = TokenKind::lexer(source);
        let tokens = lexer.into_iter().map(|t| t.unwrap()).collect::<Vec<_>>();

        assert_eq!(tokens, expected_tokens);
    }

    #[test]
    fn test_match() {
        let expected_tokens = vec![
            LRound,
            Let,
            Ident,
            LCurly,
            Ident,
            RCurly,
            LRound,
            Match,
            Ident,
            Ident,
            Ident,
            Arrow,
            Ident,
            Ident,
            Arrow,
            Int(0),
            RRound,
            RRound,
        ];
        let source = r#"
            (let match_example {input} 
                (match input 
                    Some x ~> x
                    None ~> 0))
        "#;
        let lexer = TokenKind::lexer(source);
        let tokens = lexer.into_iter().map(|t| t.unwrap()).collect::<Vec<_>>();

        assert_eq!(tokens, expected_tokens);
    }

    #[test]
    fn record_access() {
        let expected_tokens = vec![
            LRound,
            Let,
            Ident,
            LCurly,
            Ident,
            RCurly,
            LRound,
            NamedField("field_one".to_string()),
            Ident,
            RRound,
            RRound,
        ];
        let source = r#"
            (let get_field_one {input}
                (:field_one input))
        "#;
        let lexer = TokenKind::lexer(source);
        let tokens = lexer.into_iter().map(|t| t.unwrap()).collect::<Vec<_>>();

        assert_eq!(tokens, expected_tokens);
    }

    #[test]
    fn product_type() {
        let expected_tokens = vec![
            LRound,
            Type,
            Ident,
            LRound,
            LRound,
            NamedField("field_one".to_string()),
            Tilde,
            Ident,
            RRound,
            LRound,
            NamedField("field_two".to_string()),
            Tilde,
            Ident,
            RRound,
            LRound,
            NamedField("field_three".to_string()),
            Tilde,
            Ident,
            RRound,
            RRound,
            RRound,
        ];
        let source = r#"
            (type MyType (
                (:field_one ~ String)
                (:field_two ~ Int)
                (:field_three ~ Bool)
            ))
        "#;
        let lexer = TokenKind::lexer(source);
        let tokens = lexer.into_iter().map(|t| t.unwrap()).collect::<Vec<_>>();

        assert_eq!(tokens, expected_tokens);
    }

    #[test]
    fn sum_type() {
        let expected_tokens = vec![
            LRound, Type, LSquare, Generic, RSquare, Ident, LRound, Ident, LRound, Ident, Tilde,
            Generic, RRound, RRound, RRound,
        ];
        let source = r#"
            (type ['a] Option (
                None
                (Some ~ 'a)))
        "#;
        let lexer = TokenKind::lexer(source);
        let tokens = lexer.into_iter().map(|t| t.unwrap()).collect::<Vec<_>>();

        assert_eq!(tokens, expected_tokens);
    }

    #[test]
    fn sum_type_multiple_generic() {
        let expected_tokens = vec![
            LRound, Type, LSquare, Generic, Generic, RSquare, Ident, LRound, LRound, Ident, Tilde,
            Generic, RRound, LRound, Ident, Tilde, Generic, RRound, RRound, RRound,
        ];
        let source = r#"
            (type ['a 'b] Result(
                (Ok ~ 'a)
                (Error ~ 'b)))
        "#;
        let lexer = TokenKind::lexer(source);
        let tokens = lexer.into_iter().map(|t| t.unwrap()).collect::<Vec<_>>();

        assert_eq!(tokens, expected_tokens);
    }

    #[test]
    fn empty_input() {
        let lexer = TokenKind::lexer("");
        let tokens: Vec<_> = lexer.into_iter().collect();
        assert!(tokens.is_empty());
    }

    #[test]
    fn comments_are_stripped() {
        let source = r#"
            ;; this is a comment
            42
            ;; another comment
        "#;
        let lexer = TokenKind::lexer(source);
        let tokens: Vec<_> = lexer.into_iter().map(|t| t.unwrap()).collect();
        assert_eq!(tokens, vec![Int(42)]);
    }

    #[test]
    fn inline_comment_after_token() {
        let source = "True ;; rest of line ignored\nFalse";
        let lexer = TokenKind::lexer(source);
        let tokens: Vec<_> = lexer.into_iter().map(|t| t.unwrap()).collect();
        assert_eq!(tokens, vec![Bool(true), Bool(false)]);
    }

    #[test]
    fn generic_type_vars() {
        let source = "'a 'b 'my_type 'z";
        let lexer = TokenKind::lexer(source);
        let tokens: Vec<_> = lexer.into_iter().map(|t| t.unwrap()).collect();
        assert_eq!(tokens, vec![Generic, Generic, Generic, Generic]);
    }

    #[test]
    fn keywords_or_and_rec() {
        let source = "or and";
        let lexer = TokenKind::lexer(source);
        let tokens: Vec<_> = lexer.into_iter().map(|t| t.unwrap()).collect();
        assert_eq!(tokens, vec![Or, And]);
    }

    #[test]
    fn arrow_and_tilde_tokens() {
        let source = "~> ~";
        let lexer = TokenKind::lexer(source);
        let tokens: Vec<_> = lexer.into_iter().map(|t| t.unwrap()).collect();
        assert_eq!(tokens, vec![Arrow, Tilde]);
    }

    #[test]
    fn comparison_operators() {
        // All operator tokens — they're all TokenKind::Operator
        let source = "= != < > <= >=";
        let lexer = TokenKind::lexer(source);
        let tokens: Vec<_> = lexer.into_iter().map(|t| t.unwrap()).collect();
        assert_eq!(
            tokens,
            vec![Operator, Operator, Operator, Operator, Operator, Operator]
        );
    }

    #[test]
    fn named_field_strips_colon() {
        // :field_name should lex as NamedField("field_name") — colon stripped
        let source = ":foo :bar_baz";
        let lexer = TokenKind::lexer(source);
        let tokens: Vec<_> = lexer.into_iter().map(|t| t.unwrap()).collect();
        assert_eq!(
            tokens,
            vec![
                NamedField("foo".to_string()),
                NamedField("bar_baz".to_string()),
            ]
        );
    }

    #[test]
    fn hash_square_bracket() {
        let source = "#[";
        let lexer = TokenKind::lexer(source);
        let tokens: Vec<_> = lexer.into_iter().map(|t| t.unwrap()).collect();
        assert_eq!(tokens, vec![HashLSquare]);
    }

    #[test]
    fn simple_test() {
        let source = r#"
            ;; a custom type
            (type ['a] Option (
                None
                (Some ~ 'a)))

            ;; custom record type
            (type MyType (
                (:field_one ~ String)
                (:field_two ~ Int)
                (:field_three ~ Bool)
            ))

            ;; custom variant types
            (type ['e 'a] Result (
                (Error ~ 'e)
                (Ok ~ 'a)))

            (type MyOtherType (
                VariantOne
                (VariantTwo ~ String)))

            ;; a function
            (let add_three {a b c}
                ;; this is a comment
                (let [intermediate (+ a b)
                    final (+ intermediate c)]
                final))

            (let match_example {input} 
                (match input 
                    Some x ~> x
                    None ~> 0))

            (let rec fib {n}
                (if (or (= n 0) (= n 1))
                    n
                (+ (fib (- n 1)) (fib (- n 2)))))
        "#;

        let mut lexer = TokenKind::lexer(source);
        while let Some(result) = lexer.next() {
            match result {
                Ok(token) => println!("{:?}: {:?}", token, lexer.slice()),
                Err(err) => panic!("failed to parse: {err:?}"),
            }
        }
    }
}
