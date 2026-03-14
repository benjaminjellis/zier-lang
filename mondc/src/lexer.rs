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
pub enum TokenKind {
    /// A doc comment: `;;;` to end of line. Preserved in the token stream so
    /// that tooling can attach it to the following declaration.
    #[regex(r";;;[^\n]*", allow_greedy = true)]
    DocComment,
    /// A line comment: `;;` to end of line. Preserved in the token stream so
    /// that formatters and tooling can see comments; the parser filters these out.
    #[regex(r";;[^\n]*", allow_greedy = true)]
    Comment,
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
    #[token("{")]
    LCurly,
    #[token("}")]
    RCurly,

    // Keywords
    #[token("pub")]
    Pub,
    #[token("type")]
    Type,
    #[token("let?")]
    LetBind,
    #[token("let")]
    Let,
    #[token("f", priority = 3)]
    Fn,
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
    #[token("->")]
    ThinArrow,
    #[token("extern")]
    Extern,
    #[token("use")]
    Use,
    #[token("test")]
    Test,
    #[token("do")]
    Do,
    #[token("with")]
    With,
    #[regex(":[a-zA-Z_][a-zA-Z0-9_]*", |lex| lex.slice()[1..].to_string())]
    NamedField(String),

    #[regex(r"-?[0-9]+\.[0-9]+", |lex| lex.slice().parse::<f64>().ok())]
    // Matches 3.14, -0.5, etc.
    Float(f64),

    #[regex(r"-?[0-9]+", |lex| lex.slice().parse::<i64>().ok())]
    Int(i64),

    // Generics (e.g., 'a, 'e)
    #[regex(r"'[a-z][a-zA-Z0-9_]*")]
    Generic,

    // Qualified identifier: module/function or module/Type
    // Module is always lowercase; must be matched before plain Ident (longer match wins)
    #[regex(r"[a-z][a-zA-Z0-9_]*/[a-zA-Z_][a-zA-Z0-9_]*", |lex| {
        let s = lex.slice();
        let slash = s.find('/').unwrap();
        (s[..slash].to_string(), s[slash + 1..].to_string())
    })]
    QualifiedIdent((String, String)),

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
            TokenKind::DocComment => "doc comment",
            TokenKind::Comment => "comment",
            TokenKind::LRound => "opening bracket '('",
            TokenKind::RRound => "closing bracket ')'",
            TokenKind::LSquare => "opening bracket '['",
            TokenKind::RSquare => "closing bracket ']'",
            TokenKind::Tilde => "tilde '~'",
            TokenKind::LCurly => "opening bracket '{'",
            TokenKind::RCurly => "closing bracket '}'",
            TokenKind::Pub => "keyword 'pub'",
            TokenKind::Type => "keyword 'type'",
            TokenKind::LetBind => "keyword 'let?'",
            TokenKind::Let => "keyword 'let'",
            TokenKind::Fn => "keyword 'f'",
            TokenKind::If => "keyword 'if'",
            TokenKind::Match => "keyword 'match'",
            TokenKind::Or => "operator 'or'",
            TokenKind::And => "operator 'and'",
            TokenKind::Arrow => "arrow '~>'",
            TokenKind::ThinArrow => "arrow '->'",
            TokenKind::Extern => "keyword 'extern'",
            TokenKind::Use => "keyword 'use'",
            TokenKind::Test => "keyword 'test'",
            TokenKind::Do => "keyword 'do'",
            TokenKind::With => "keyword 'with'",
            TokenKind::NamedField(_) => "field name (e.g. :name)",
            TokenKind::Float(_) => "float literal",
            TokenKind::Int(_) => "integer literal",
            TokenKind::Generic => "generic type variable (e.g. 'a)",
            TokenKind::QualifiedIdent(_) => "qualified identifier (e.g. math/add)",
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
    fn list_literal() {
        let expected_tokens = [
            LRound,
            Let,
            Ident,
            LCurly,
            RCurly,
            LSquare,
            Int(1),
            Int(2),
            Int(3),
            Int(4),
            RSquare,
            RRound,
        ];
        let source = r#"
            (let list_literal {}
                [1 2 3 4] )
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
    fn negative_int_literal() {
        let source = "-42";
        let lexer = TokenKind::lexer(source);
        let tokens = lexer.into_iter().map(|t| t.unwrap()).collect::<Vec<_>>();
        assert_eq!(tokens, vec![Int(-42)]);
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
    fn negative_float_literal() {
        let source = "-0.5";
        let lexer = TokenKind::lexer(source);
        let tokens = lexer.into_iter().map(|t| t.unwrap()).collect::<Vec<_>>();
        assert_eq!(tokens, vec![Float(-0.5)]);
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
    fn comments_are_emitted_as_tokens() {
        let source = r#"
            ;; this is a comment
            ;;; this is docs
            42
            ;; another comment
        "#;
        let lexer = TokenKind::lexer(source);
        let tokens: Vec<_> = lexer.into_iter().map(|t| t.unwrap()).collect();
        assert_eq!(tokens, vec![Comment, DocComment, Int(42), Comment]);
    }

    #[test]
    fn inline_comment_after_token() {
        let source = "True ;; rest of line ignored\nFalse";
        let lexer = TokenKind::lexer(source);
        let tokens: Vec<_> = lexer.into_iter().map(|t| t.unwrap()).collect();
        assert_eq!(tokens, vec![Bool(true), Comment, Bool(false)]);
    }

    #[test]
    fn doc_comments_are_emitted_as_tokens() {
        let source = ";;; hello\n;;; world\n42";
        let lexer = TokenKind::lexer(source);
        let tokens: Vec<_> = lexer.into_iter().map(|t| t.unwrap()).collect();
        assert_eq!(tokens, vec![DocComment, DocComment, Int(42)]);
    }

    #[test]
    fn generic_type_vars() {
        let source = "'a 'b 'my_type 'z";
        let lexer = TokenKind::lexer(source);
        let tokens: Vec<_> = lexer.into_iter().map(|t| t.unwrap()).collect();
        assert_eq!(tokens, vec![Generic, Generic, Generic, Generic]);
    }

    #[test]
    fn keywords_or_and_with() {
        let source = "or and with";
        let lexer = TokenKind::lexer(source);
        let tokens: Vec<_> = lexer.into_iter().map(|t| t.unwrap()).collect();
        assert_eq!(tokens, vec![Or, And, With]);
    }

    #[test]
    fn qualified_ident() {
        let source = "math/add collections/map math/MyType";
        let lexer = TokenKind::lexer(source);
        let tokens: Vec<_> = lexer.into_iter().map(|t| t.unwrap()).collect();
        assert_eq!(
            tokens,
            vec![
                QualifiedIdent(("math".into(), "add".into())),
                QualifiedIdent(("collections".into(), "map".into())),
                QualifiedIdent(("math".into(), "MyType".into())),
            ]
        );
    }

    #[test]
    fn qualified_ident_does_not_consume_division() {
        // (/ a b) — the / here is an operator, not a qualified ident
        let source = "/ a b";
        let lexer = TokenKind::lexer(source);
        let tokens: Vec<_> = lexer.into_iter().map(|t| t.unwrap()).collect();
        assert_eq!(tokens, vec![Operator, Ident, Ident]);
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
    fn subtraction_still_lexes_as_operator_with_whitespace() {
        let source = "(- 1 2)";
        let lexer = TokenKind::lexer(source);
        let tokens: Vec<_> = lexer.into_iter().map(|t| t.unwrap()).collect();
        assert_eq!(tokens, vec![LRound, Operator, Int(1), Int(2), RRound]);
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
    fn square_bracket() {
        let source = "[";
        let lexer = TokenKind::lexer(source);
        let tokens: Vec<_> = lexer.into_iter().map(|t| t.unwrap()).collect();
        assert_eq!(tokens, vec![LSquare]);
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
            (type ['a 'e] Result (
                (Ok ~ 'a)
                (Error ~ 'e)))

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
