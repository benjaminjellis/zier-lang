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
    /// (type MyType ( (field ~ Type) ... ))
    Record {
        name: String,
        fields: Vec<(String, String)>, // (Field Name, Type Name)
        span: Range<usize>,
    },
    /// (type ['e 'a] Result (Error ~ 'e) (Ok ~ 'a))
    Variant {
        name: String,
        params: Vec<String>,                         // ["e", "a"]
        constructors: Vec<(String, Option<String>)>, // (Name, TypePayload)
        span: Range<usize>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Literal(Literal, Range<usize>),
    Variable(String, Range<usize>),
    Array(Vec<Expr>, Range<usize>),
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
pub enum Pattern {
    /// Matches anything and does not bind a name: `_`
    Any(Range<usize>),
    /// Matches anything and binds it to a name: `x`
    Variable(String, Range<usize>),
    /// Matches a specific constant value: `42`, `true`, `"hello"`
    Literal(Literal, Range<usize>),
    /// Matches a constructor/enum variant: `(Some x)` or `(None)`
    /// String is the constructor name, Vec<Pattern> are the nested arguments.
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
