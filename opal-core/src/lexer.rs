use logos::Logos;
use std::ops::Range;

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
    LParen,
    #[token(")")]
    RParen,
    #[token("[")]
    LBracket,
    #[token("]")]
    RBracket,
    #[token("~")]
    Tilde,

    // Keywords
    #[token("type")]
    Type,
    #[token("let")]
    Let,
    #[token("rec")]
    Rec,
    #[token("if")]
    If,
    #[token("match")]
    Match,
    #[token("or")]
    Or,
    #[token("and")]
    And,

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

    #[regex(r"[0-9]+")]
    Int,

    #[token("true")]
    #[token("false")]
    Bool,

    // Operators
    // Supports multi-character ops like != or <= if you add them later
    #[regex(r"[\+\-\*/=<>!&|]+")]
    Operator,
}

#[cfg(test)]
mod tests {
    use logos::Logos;

    use crate::lexer::TokenKind;

    #[test]
    fn simple_test() {
        let source = r#"
            ;; a custom type
            (type ['a] Option
                None
                (Some ~ 'a))

            ;; custom record type
            (type MyType (
                (field_one ~ String)
                (field_two ~ Int)
                (field_three ~ Bool)
            ))

            ;; custom variant types
            (type ['e 'a] Result
                (Error ~ 'e)
                (Ok ~ 'a))

            (type MyOtherType
                VariantOne
                (VariantTwo ~ String))

            ;; a function
            (let add_three [a b c]
                ;; this is a comment
                (let [intermediate (+ a b)
                    final (+ intermediate c)]
                final))

            (let rec fib [n]
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
