use crate::ast::{Literal, Pattern};

/// A complete program after name resolution and lambda lifting.
/// All nested functions have been hoisted to the top level.
#[derive(Debug, Clone)]
pub struct Program {
    pub functions: Vec<FlatFunc>,
}

/// A top-level function or lifted closure.
///
/// - `free_vars.is_empty()` → true top-level function (no environment needed)
/// - `!free_vars.is_empty()` → lifted closure (codegen must pass an env struct)
#[derive(Debug, Clone)]
pub struct FlatFunc {
    /// Unique symbol name (e.g. "fib", "inner_0").
    pub name: String,
    /// Explicit parameters in source order.
    pub params: Vec<String>,
    /// Variables captured from enclosing scopes, in deterministic order.
    /// For codegen: these are packed into an `env` pointer passed as an extra first argument.
    pub free_vars: Vec<String>,
    pub body: Expr,
}

/// Resolved, lambda-lifted expression.
#[derive(Debug, Clone)]
pub enum Expr {
    /// A literal value.
    Lit(Literal),

    /// A variable bound locally in the current function (argument or let-binding).
    Local(String),

    /// A variable captured from an enclosing scope.
    /// `index` is the position in this function's `free_vars` list / env struct.
    Capture { index: usize, name: String },

    /// A reference to a top-level symbol (function or constructor).
    /// For codegen: becomes a direct function reference / Cranelift FuncRef.
    Global(String),

    /// Allocate a closure: pair the named lifted function with its captured values.
    /// Produced when a nested function has non-empty free_vars.
    MakeClosure { func: String, captures: Vec<Expr> },

    /// Array literal.
    Array(Vec<Expr>),

    /// Local binding (variable or the closure-ref slot for a nested function).
    Let {
        name: String,
        value: Box<Expr>,
        body: Box<Expr>,
    },

    /// Conditional expression.
    If {
        cond: Box<Expr>,
        then: Box<Expr>,
        els: Box<Expr>,
    },

    /// Function application.
    /// Callee classification for codegen:
    ///   Global(name)  → direct call to a known symbol
    ///   _             → indirect call through a closure (fn_ptr + env_ptr)
    Call { func: Box<Expr>, args: Vec<Expr> },

    /// Pattern match.
    Match {
        target: Box<Expr>,
        arms: Vec<(Pattern, Expr)>,
    },

    /// Record field access. Resolved via field accessor functions.
    FieldAccess { field: String, record: Box<Expr> },
}
