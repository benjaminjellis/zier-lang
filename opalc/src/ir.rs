/// Erlang IR — a simplified Erlang AST we emit to `.erl` source.
/// All Opal functions are fully curried: every function takes exactly one param.

#[derive(Debug, Clone)]
pub struct Module {
    pub name: String,
    pub functions: Vec<Function>,
}

/// A top-level Erlang function with a single parameter (curried).
#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    pub param: String,
    pub body: Expr,
}

#[derive(Debug, Clone)]
pub enum Expr {
    /// Erlang atom: `none`, `ok`, `unit`, `true`, `false`
    Atom(String),
    Int(i64),
    Float(f64),
    /// Erlang binary string: `<<"hello"/utf8>>`
    Str(String),
    /// Uppercase local variable: `X`, `My_var`
    Var(String),
    /// `fun f/1` — reference to a local top-level function
    FunRef(String),
    /// `fun module:f/1` — reference to a remote function
    RemoteFunRef(String, String),
    /// `{a, b, c}` — tuple, used for variant values and records
    Tuple(Vec<Expr>),
    /// `[1, 2, 3]` — Erlang list
    List(Vec<Expr>),
    /// `fun(Param) -> Body end`
    Fun(String, Box<Expr>),
    /// `F(Arg)` — single-arg call (curried)
    Call(Box<Expr>, Box<Expr>),
    /// `name(Arg)` — known local function call (avoids fun-ref wrapping)
    LocalCall(String, Box<Expr>),
    /// `module:function(arg1, arg2, ...)` — remote (FFI) call
    RemoteCall(String, String, Vec<Expr>),
    /// `Left op Right` — binary operator
    BinOp(String, Box<Expr>, Box<Expr>),
    /// `op Expr` — unary operator (`not`)
    UnOp(String, Box<Expr>),
    /// `Var = Val, Body` — sequential let binding
    Let(String, Box<Expr>, Box<Expr>),
    /// `case Expr of Pat -> Body; ... end`
    Case(Box<Expr>, Vec<(Pattern, Expr)>),
}

#[derive(Debug, Clone)]
pub enum Pattern {
    Any,
    Var(String),
    Atom(String),
    Int(i64),
    Float(f64),
    Str(String),
    /// `{tag, P1, P2}` — tuple pattern for variants/records
    Tuple(Vec<Pattern>),
    /// `[P1, P2, P3]` — list pattern
    List(Vec<Pattern>),
}
