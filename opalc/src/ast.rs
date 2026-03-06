use std::ops::Range;

#[derive(Debug, Clone, PartialEq)]
pub enum Declaration {
    /// For (type MyType ...)
    Type(TypeDecl),
    /// For (let f {a} ...)
    Expression(Expr),
    /// For (extern let name ~ (A -> B) module/function)
    /// or  (extern let name {} ~ ReturnType module/function)  -- nullary Erlang function
    ExternLet {
        name: String,
        is_pub: bool,
        is_nullary: bool,
        ty: TypeSig,
        erlang_target: (String, String), // (module, function)
        span: Range<usize>,
    },
    /// For (extern type ['k 'v] Dict erlang/map)
    ExternType {
        is_pub: bool,
        name: String,
        params: Vec<String>,
        erlang_target: (String, String), // (module, type)
        span: Range<usize>,
    },
    /// For (use std/dict) or (pub use io)
    Use {
        is_pub: bool,
        path: (String, String), // (namespace, module) e.g. ("std", "dict") or ("", "io")
        span: Range<usize>,
    },
}

/// A type written in source — only valid inside `extern` declarations.
#[derive(Debug, Clone, PartialEq)]
pub enum TypeSig {
    Named(String),                   // Int, String, Bool, Unit, a user type
    Generic(String),                 // 'a, 'b
    App(String, Vec<TypeSig>),       // Option 'a, Result 'e 'a
    Fun(Box<TypeSig>, Box<TypeSig>), // A -> B
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
    List(Vec<Expr>, Range<usize>),
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
        targets: Vec<Expr>,
        arms: Vec<(Vec<Pattern>, Expr)>,
        span: Range<usize>,
    },
    FieldAccess {
        field: String,
        record: Box<Expr>,
        span: Range<usize>,
    },
    /// (MyType :field1 val1 :field2 val2) — named-field record construction
    RecordConstruct {
        name: String,
        fields: Vec<(String, Expr)>,
        span: Range<usize>,
    },
    /// (fn {x y} body) — anonymous function
    Lambda {
        args: Vec<String>,
        body: Box<Expr>,
        span: Range<usize>,
    },
    /// (module/function arg1 arg2) — cross-module call
    QualifiedCall {
        module: String,
        function: String,
        args: Vec<Expr>,
        span: Range<usize>,
        /// Span of just the `module/function` ident token (for diagnostics).
        fn_span: Range<usize>,
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
    /// Matches any of several alternatives: `10 or 11 or 12`
    Or(Vec<Pattern>, Range<usize>),
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
            Expr::List(_, s) => s.clone(),
            Expr::LetFunc { span, .. } => span.clone(),
            Expr::LetLocal { span, .. } => span.clone(),
            Expr::If { span, .. } => span.clone(),
            Expr::Call { span, .. } => span.clone(),
            Expr::Match { span, .. } => span.clone(),
            Expr::FieldAccess { span, .. } => span.clone(),
            Expr::RecordConstruct { span, .. } => span.clone(),
            Expr::Lambda { span, .. } => span.clone(),
            Expr::QualifiedCall { span, .. } => span.clone(),
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
