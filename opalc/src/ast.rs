use std::ops::Range;

#[derive(Debug, Clone, PartialEq)]
pub enum Declaration {
    /// For (type MyType ...)
    Type(TypeDecl),
    /// For (let f {a} ...)
    Expression(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeDecl {
    /// (type MyType ( (:field ~ Type) ... ))
    Record {
        name: String,
        params: Vec<String>,              // ["'e", "'a"]
        fields: Vec<(String, TypeUsage)>, // (field_name, type)
        span: Range<usize>,
    },
    /// (type ['e 'a] Result ( (Error ~ 'e) (Ok ~ 'a) ))
    Variant {
        name: String,
        params: Vec<String>,                            // ["'e", "'a"]
        constructors: Vec<(String, Option<TypeUsage>)>, // (name, payload type)
        span: Range<usize>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Literal(Literal, Range<usize>),
    Variable(String, Range<usize>),
    Array(Vec<Expr>, Range<usize>),
    /// (let [name value ...] body) or (let [rec] name {args} body)
    LetFunc {
        name: String,
        is_rec: bool,
        args: Vec<String>,
        value: Box<Expr>,
        body: Box<Expr>,
        span: Range<usize>,
    },
    LetLocal {
        name: String,
        is_rec: bool,
        args: Vec<String>,
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
    FieldAccess {
        field: String,
        record: Box<Expr>,
        span: Range<usize>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    /// Matches anything, no binding: `_`
    Any(Range<usize>),
    /// Matches anything, binds to name: `x`
    Variable(String, Range<usize>),
    /// Matches a constant: `42`, `true`, `"hello"`
    Literal(Literal, Range<usize>),
    /// Matches a constructor: `(Some x)` or `None`
    Constructor(String, Vec<Pattern>, Range<usize>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Int(i64),
    Bool(bool),
    Float(f64),
    String(String),
    Unit,
}

/// A reference to a type in source code
#[derive(Debug, Clone, PartialEq)]
pub enum TypeUsage {
    Named(String),               // e.g. Int, String, MyType
    Generic(String),             // e.g. 'a, 't
    App(String, Vec<TypeUsage>), // e.g. App("Option", [Generic("'a")])
}
