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
        is_pub: bool,
        name: String,
        params: Vec<String>,              // ["'e", "'a"]
        fields: Vec<(String, TypeUsage)>, // (field_name, type)
        span: Range<usize>,
    },
    /// (type ['e 'a] Result ( (Error ~ 'e) (Ok ~ 'a) ))
    Variant {
        is_pub: bool,
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
    /// (let name {args} body) — top-level named function, always self-recursive, no continuation
    LetFunc {
        is_pub: bool,
        name: String,
        args: Vec<String>,
        value: Box<Expr>,
        span: Range<usize>,
    },
    /// (let [name value ...] body) — sequential local bindings
    LetLocal {
        name: String,
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

impl Expr {
    pub fn span(&self) -> Range<usize> {
        match self {
            Expr::Literal(_, s) => s.clone(),
            Expr::Variable(_, s) => s.clone(),
            Expr::Array(_, s) => s.clone(),
            Expr::LetFunc { span, .. } => span.clone(),
            Expr::LetLocal { span, .. } => span.clone(),
            Expr::If { span, .. } => span.clone(),
            Expr::Call { span, .. } => span.clone(),
            Expr::Match { span, .. } => span.clone(),
            Expr::FieldAccess { span, .. } => span.clone(),
        }
    }
}

/// A reference to a type in source code
#[derive(Debug, Clone, PartialEq)]
pub enum TypeUsage {
    Named(String),               // e.g. Int, String, MyType
    Generic(String),             // e.g. 'a, 't
    App(String, Vec<TypeUsage>), // e.g. App("Option", [Generic("'a")])
}
