use crate::lexer::Token;
use std::ops::Range;

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Literal(Literal, Range<usize>),
    Variable(String, Range<usize>),
    // (let [name value ...] body) or (let rec name [args] body)
    Let {
        name: String,
        is_rec: bool,
        args: Vec<String>, // empty for non-functions
        value: Box<Expr>,
        body: Box<Expr>,
        span: Range<usize>,
    },
    If {
        cond: Box<Expr>,
        then: Box<Expr>,
        els: Box<Expr>,
        span: Range<usize>,
    },
    Call {
        func: Box<Expr>,
        args: Vec<Expr>,
        span: Range<usize>,
    },
    Match {
        target: Box<Expr>,
        arms: Vec<(Pattern, Expr)>,
        span: Range<usize>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Int(i64),
    Bool(bool),
    String(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    Any(Range<usize>),              // _
    Variable(String, Range<usize>), // x
    Literal(Literal, Range<usize>), // 42
    // (Constructor arg1 arg2)
    Constructor(String, Vec<Pattern>, Range<usize>),
}

#[derive(Debug, Clone)]
pub enum TypeDef {
    // (type ['a] Result (Error ~ 'a) ...)
    Variant {
        name: String,
        params: Vec<String>,
        cases: Vec<VariantCase>,
        span: Range<usize>,
    },
    // (type MyType ((field ~ Type) ...))
    Record {
        name: String,
        fields: Vec<(String, TypeUsage)>,
        span: Range<usize>,
    },
}

#[derive(Debug, Clone)]
pub struct VariantCase {
    pub name: String,
    pub payload: Option<TypeUsage>,
}

#[derive(Debug, Clone)]
pub enum TypeUsage {
    Named(String), // Int
    Generic(String), // 'a
                   // Higher kinded or complex types can be added here later
}
