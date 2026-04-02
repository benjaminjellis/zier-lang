use std::ops::Range;

/// Which names a `use` declaration brings into unqualified scope.
#[derive(Debug, Clone, PartialEq)]
pub enum UnqualifiedImports {
    /// `(use std/io)` — qualified calls only; nothing unqualified
    None,
    /// `(use std/io [println read])` — only the listed names
    Specific(Vec<String>),
    /// `(use std/io [*])` — every export
    Wildcard,
}

impl UnqualifiedImports {
    /// Returns true if `name` should be brought into unqualified scope.
    pub fn includes(&self, name: &str) -> bool {
        match self {
            UnqualifiedImports::None => false,
            UnqualifiedImports::Specific(names) => names.iter().any(|n| n == name),
            UnqualifiedImports::Wildcard => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Declaration {
    /// For (type MyType ...)
    Type(TypeDecl),
    /// For (let f {a} ...)
    Expression(Expr),
    /// For (extern let name ~ (A -> B) module/function)
    /// or  (extern let name ~ (Unit -> ReturnType) module/function)  -- nullary Erlang function
    ExternLet {
        name: String,
        name_span: Range<usize>,
        is_pub: bool,
        is_nullary: bool,
        ty: TypeSig,
        erlang_target: (String, String), // (module, function)
        span: Range<usize>,
    },
    /// For (extern type ['k 'v] Dict) or (extern type ['k 'v] Dict erlang/map)
    ExternType {
        is_pub: bool,
        name: String,
        params: Vec<String>,
        erlang_target: Option<(String, String)>, // optional (module, type) metadata
        span: Range<usize>,
    },
    /// For (use std/dict) or (pub use io)
    Use {
        is_pub: bool,
        path: (String, String), // (namespace, module) e.g. ("std", "dict") or ("", "io")
        unqualified: UnqualifiedImports,
        span: Range<usize>,
    },
    /// For (test "name" body) — only valid in tests/ directory
    Test {
        name: String,
        body: Box<Expr>,
        span: Range<usize>,
    },
}

/// A type written in source — only valid inside `extern` declarations.
#[derive(Debug, Clone, PartialEq)]
pub enum TypeSig {
    Named(String),                   // Int, String, Bool, Unit, a user type
    Generic(String),                 // 'a, 'b
    App(String, Vec<TypeSig>),       // Option 'a, Result 'a 'e
    Fun(Box<TypeSig>, Box<TypeSig>), // A -> B
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeDecl {
    /// (type MyType [(:field ~ Type) ...])
    Record {
        is_pub: bool,
        name: String,
        params: Vec<String>,              // ["'e", "'a"]
        fields: Vec<(String, TypeUsage)>, // (field_name, type)
        span: Range<usize>,
    },
    /// (type ['a 'e] Result [(Ok ~ 'a) (Error ~ 'e)])
    Variant {
        is_pub: bool,
        name: String,
        params: Vec<String>,                         // ["'a", "'e"]
        constructors: Vec<(String, Vec<TypeUsage>)>, // (name, payload types)
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
        arg_spans: Vec<Range<usize>>,
        name_span: Range<usize>,
        value: Box<Expr>,
        span: Range<usize>,
    },
    /// (let [name value ...] body) — sequential local bindings
    LetLocal {
        name: String,
        name_span: Range<usize>,
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
    Debug {
        value: Box<Expr>,
        span: Range<usize>,
    },
    Call {
        func: Box<Expr>,
        args: Vec<Expr>,
        span: Range<usize>,
    },
    Match {
        targets: Vec<Expr>,
        arms: Vec<MatchArm>,
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
    /// (with record :field1 val1 :field2 val2) — record update
    RecordUpdate {
        record: Box<Expr>,
        updates: Vec<(String, Expr)>,
        span: Range<usize>,
    },
    /// (f {x y} -> body) — anonymous function
    Lambda {
        args: Vec<String>,
        arg_spans: Vec<Range<usize>>,
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
pub struct MatchArm {
    pub patterns: Vec<Pattern>,
    pub guard: Option<Expr>,
    pub body: Expr,
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
    /// Matches any of several alternatives: `10 | 11 | 12`
    Or(Vec<Pattern>, Range<usize>),
    /// Matches an empty list: `[]`
    EmptyList(Range<usize>),
    /// Matches a cons cell: `[head | tail]`
    Cons(Box<Pattern>, Box<Pattern>, Range<usize>),
    /// Matches a record by named fields: `(Person :name n :age age)`
    Record {
        name: String,
        fields: Vec<(String, Pattern, Range<usize>)>,
        span: Range<usize>,
    },
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
            Expr::Debug { span, .. } => span.clone(),
            Expr::Call { span, .. } => span.clone(),
            Expr::Match { span, .. } => span.clone(),
            Expr::FieldAccess { span, .. } => span.clone(),
            Expr::RecordConstruct { span, .. } => span.clone(),
            Expr::RecordUpdate { span, .. } => span.clone(),
            Expr::Lambda { span, .. } => span.clone(),
            Expr::QualifiedCall { span, .. } => span.clone(),
        }
    }
}

/// A reference to a type in source code
#[derive(Debug, Clone, PartialEq)]
pub enum TypeUsage {
    Named(String, Range<usize>),               // e.g. Int, String, MyType
    Generic(String, Range<usize>),             // e.g. 'a, 't
    App(String, Vec<TypeUsage>, Range<usize>), // e.g. App("Option", [Generic("'a")])
    Fun(Box<TypeUsage>, Box<TypeUsage>, Range<usize>), // e.g. Int -> String
}

impl TypeUsage {
    pub fn span(&self) -> Range<usize> {
        match self {
            TypeUsage::Named(_, span)
            | TypeUsage::Generic(_, span)
            | TypeUsage::App(_, _, span)
            | TypeUsage::Fun(_, _, span) => span.clone(),
        }
    }
}
