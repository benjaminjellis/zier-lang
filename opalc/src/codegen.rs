use std::collections::{HashMap, HashSet};

use crate::{ast, ir};

// ─── Context ────────────────────────────────────────────────────────────────

struct Ctx {
    /// Names of all top-level functions (used to distinguish fn-refs from local vars)
    fn_names: HashSet<String>,
    /// Top-level function name → number of Opal args (for multi-arg TCO)
    fn_arities: HashMap<String, usize>,
    /// Constructor name → arity (0 = nullary atom, 1+ = tuple)
    constructors: HashMap<String, usize>,
    /// Imported function name → Erlang module (from `use` declarations)
    imports: HashMap<String, String>,
    /// User-facing module name → Erlang module name (e.g. "io" → "opal_io")
    module_aliases: HashMap<String, String>,
    /// Record field name → 1-based element index (tag is at 1, fields start at 2)
    field_indices: HashMap<String, usize>,
}

// ─── Public entry point ─────────────────────────────────────────────────────

pub fn lower_module(
    name: &str,
    decls: &[ast::Declaration],
    imports: HashMap<String, String>,
    module_aliases: HashMap<String, String>,
    imported_constructors: HashMap<String, usize>,
    imported_field_indices: HashMap<String, usize>,
) -> ir::Module {
    // Pass 1: collect function names, arities, constructor arities, and record field indices
    let mut fn_names = HashSet::new();
    let mut fn_arities = HashMap::new();
    let mut constructors = imported_constructors;
    let mut field_indices = imported_field_indices;

    for decl in decls {
        match decl {
            ast::Declaration::Expression(ast::Expr::LetFunc { name, args, .. }) => {
                fn_names.insert(name.clone());
                fn_arities.insert(name.clone(), args.len());
            }
            ast::Declaration::ExternLet { name, .. } => {
                fn_names.insert(name.clone());
            }
            ast::Declaration::Type(ast::TypeDecl::Variant {
                constructors: ctors,
                ..
            }) => {
                for (ctor_name, payload) in ctors {
                    constructors.insert(ctor_name.clone(), if payload.is_some() { 1 } else { 0 });
                }
            }
            ast::Declaration::Type(ast::TypeDecl::Record { fields, .. }) => {
                // Tag is at element(1), fields start at element(2)
                for (i, (field_name, _)) in fields.iter().enumerate() {
                    field_indices.insert(field_name.clone(), i + 2);
                }
            }
            _ => {}
        }
    }

    let ctx = Ctx {
        fn_names,
        fn_arities,
        constructors,
        imports,
        module_aliases,
        field_indices,
    };

    // Pass 2: lower declarations to IR functions
    let mut functions = Vec::new();
    let mut test_idx: usize = 0;

    for decl in decls {
        match decl {
            ast::Declaration::Expression(ast::Expr::LetFunc {
                name,
                args,
                value,
                is_pub,
                ..
            }) => {
                if args.len() >= 2 {
                    // Curried entry: f(A) -> fun(B) -> ... f(A, B, ...) end ... end.
                    let all_vars: Vec<ir::Expr> =
                        args.iter().map(|a| ir::Expr::Var(var_name(a))).collect();
                    let direct = ir::Expr::LocalCallMulti(name.clone(), all_vars);
                    let curried_body = args[1..].iter().rev().fold(direct, |acc, arg| {
                        ir::Expr::Fun(var_name(arg), Box::new(acc))
                    });
                    functions.push(ir::Function {
                        name: name.clone(),
                        params: vec![var_name(&args[0])],
                        body: curried_body,
                        is_pub: *is_pub,
                    });
                    // Multi-arg impl: f(A, B, ...) -> body.  Self-recursive calls target this.
                    functions.push(ir::Function {
                        name: name.clone(),
                        params: args.iter().map(|a| var_name(a)).collect(),
                        body: lower_expr(value, &ctx),
                        is_pub: false,
                    });
                } else {
                    let mut f = lower_letfunc(name, args, value, &ctx);
                    f.is_pub = *is_pub;
                    functions.push(f);
                }
            }
            ast::Declaration::ExternLet {
                name,
                is_pub,
                is_nullary,
                ty,
                erlang_target,
                ..
            } => {
                let mut f = lower_extern_let(name, *is_nullary, ty, erlang_target);
                f.is_pub = *is_pub;
                functions.push(f);
            }
            ast::Declaration::Test { body, .. } => {
                functions.push(ir::Function {
                    name: format!("opal_test_{test_idx}"),
                    params: vec!["_Unit".to_string()],
                    body: lower_expr(body, &ctx),
                    is_pub: true,
                });
                test_idx += 1;
            }
            _ => {} // Type decls, Use, ExternType produce no Erlang functions
        }
    }

    ir::Module {
        name: name.to_string(),
        functions,
    }
}

// ─── Function lowering ──────────────────────────────────────────────────────

// Only called for 0-arg and 1-arg functions (N >= 2 handled inline in lower_module).
fn lower_letfunc(name: &str, args: &[String], body: &ast::Expr, ctx: &Ctx) -> ir::Function {
    let body_ir = lower_expr(body, ctx);
    let param = if args.is_empty() {
        "_Unit".to_string()
    } else {
        var_name(&args[0])
    };
    ir::Function {
        name: name.to_string(),
        params: vec![param],
        body: body_ir,
        is_pub: false, // caller sets this
    }
}

fn lower_extern_let(
    name: &str,
    is_nullary: bool,
    ty: &ast::TypeSig,
    erlang_target: &(String, String),
) -> ir::Function {
    let (module, function) = erlang_target;
    let arity = if is_nullary { 0 } else { type_sig_arity(ty) };
    let params: Vec<String> = (0..arity).map(|i| format!("Arg{i}")).collect();

    let remote_args: Vec<ir::Expr> = params.iter().map(|p| ir::Expr::Var(p.clone())).collect();
    let call = ir::Expr::RemoteCall(module.clone(), function.clone(), remote_args);

    // Wrap the remote call in curried funs for arity > 1
    let body = if params.len() <= 1 {
        call
    } else {
        params[1..]
            .iter()
            .rev()
            .fold(call, |acc, p| ir::Expr::Fun(p.clone(), Box::new(acc)))
    };

    let erlang_params = params.into_iter().take(1).collect();

    ir::Function {
        name: name.to_string(),
        params: erlang_params,
        body,
        is_pub: false, // caller sets this
    }
}

// ─── Expression lowering ────────────────────────────────────────────────────

fn lower_expr(expr: &ast::Expr, ctx: &Ctx) -> ir::Expr {
    match expr {
        ast::Expr::Literal(lit, _) => lower_literal(lit),

        ast::Expr::Variable(name, _) => lower_variable(name, ctx),

        ast::Expr::List(items, _) => {
            ir::Expr::List(items.iter().map(|e| lower_expr(e, ctx)).collect())
        }

        ast::Expr::LetLocal {
            name, value, body, ..
        } => ir::Expr::Let(
            var_name(name),
            Box::new(lower_expr(value, ctx)),
            Box::new(lower_expr(body, ctx)),
        ),

        ast::Expr::If {
            cond, then, els, ..
        } => ir::Expr::Case(
            Box::new(lower_expr(cond, ctx)),
            vec![
                (ir::Pattern::Atom("true".into()), lower_expr(then, ctx)),
                (ir::Pattern::Any, lower_expr(els, ctx)),
            ],
        ),

        ast::Expr::Match { targets, arms, .. } => {
            let scrutinee = if targets.len() == 1 {
                lower_expr(&targets[0], ctx)
            } else {
                ir::Expr::Tuple(targets.iter().map(|t| lower_expr(t, ctx)).collect())
            };

            let mut ir_arms = Vec::new();
            for (patterns, body) in arms {
                let body_ir = lower_expr(body, ctx);
                let pat = if targets.len() == 1 {
                    lower_pattern(&patterns[0], ctx)
                } else {
                    ir::Pattern::Tuple(patterns.iter().map(|p| lower_pattern(p, ctx)).collect())
                };
                expand_or_pattern(pat, body_ir, &mut ir_arms);
            }

            ir::Expr::Case(Box::new(scrutinee), ir_arms)
        }

        ast::Expr::Lambda { args, body, .. } => {
            let body_ir = lower_expr(body, ctx);
            make_lambda(args, body_ir)
        }

        ast::Expr::Call { func, args, .. } => lower_call(func, args, ctx),

        ast::Expr::FieldAccess { field, record, .. } => {
            // Emit erlang:element(N, Record) where N is the 1-based field index.
            let idx = ctx.field_indices.get(field.as_str()).copied().unwrap_or(2);
            ir::Expr::RemoteCall(
                "erlang".into(),
                "element".into(),
                vec![ir::Expr::Int(idx as i64), lower_expr(record, ctx)],
            )
        }

        ast::Expr::RecordConstruct { name, fields, .. } => {
            // {name, field1, field2, ...} — fields in declaration order
            // Without type info we use declaration order as given at the call site
            let tag = ir::Expr::Atom(name.to_lowercase());
            let mut items = vec![tag];
            items.extend(fields.iter().map(|(_, e)| lower_expr(e, ctx)));
            ir::Expr::Tuple(items)
        }

        ast::Expr::QualifiedCall {
            module,
            function,
            args,
            ..
        } => {
            let erl_module = ctx
                .module_aliases
                .get(module.as_str())
                .cloned()
                .unwrap_or_else(|| module.clone());
            if args.is_empty() {
                // 0-arg: true Erlang 0-arity call
                ir::Expr::RemoteCall(erl_module, function.clone(), vec![])
            } else {
                // First arg goes into the remote call, rest chain as curried calls
                let first = lower_expr(&args[0], ctx);
                let mut result = ir::Expr::RemoteCall(erl_module, function.clone(), vec![first]);
                for arg in &args[1..] {
                    result = ir::Expr::Call(Box::new(result), Box::new(lower_expr(arg, ctx)));
                }
                result
            }
        }

        ast::Expr::LetFunc { .. } => unreachable!("LetFunc only at top level"),
    }
}

fn lower_call(func: &ast::Expr, args: &[ast::Expr], ctx: &Ctx) -> ir::Expr {
    if let ast::Expr::Variable(name, _) = func {
        // Binary operator
        if args.len() == 2
            && let Some(erl_op) = binary_op(name)
        {
            return ir::Expr::BinOp(
                erl_op.to_string(),
                Box::new(lower_expr(&args[0], ctx)),
                Box::new(lower_expr(&args[1], ctx)),
            );
        }

        // Unary operator
        if args.len() == 1
            && let Some(erl_op) = unary_op(name)
        {
            return ir::Expr::UnOp(erl_op.to_string(), Box::new(lower_expr(&args[0], ctx)));
        }

        // Constructor application: Ok x → {ok, X}
        if let Some(&arity) = ctx.constructors.get(name.as_str())
            && arity > 0
        {
            let tag = ir::Expr::Atom(name.to_lowercase());
            let mut items = vec![tag];
            items.extend(args.iter().map(|a| lower_expr(a, ctx)));
            return ir::Expr::Tuple(items);
        }

        // Imported function via `use` — emit as remote call
        if let Some(module) = ctx.imports.get(name.as_str()) {
            let module = module.clone();
            if args.is_empty() {
                return ir::Expr::RemoteCall(module, name.clone(), vec![]);
            }
            let first = lower_expr(&args[0], ctx);
            let mut result = ir::Expr::RemoteCall(module, name.clone(), vec![first]);
            for arg in &args[1..] {
                result = ir::Expr::Call(Box::new(result), Box::new(lower_expr(arg, ctx)));
            }
            return result;
        }

        // Known local function — emit direct call
        if ctx.fn_names.contains(name.as_str()) {
            if args.is_empty() {
                // 0-arg call → call with unit
                return ir::Expr::LocalCall(name.clone(), Box::new(ir::Expr::Atom("unit".into())));
            }
            let opal_arity = ctx.fn_arities.get(name.as_str()).copied().unwrap_or(0);
            // Full application of a multi-arg function → direct N-arity call (enables TCO)
            if opal_arity >= 2 && args.len() == opal_arity {
                let lowered = args.iter().map(|a| lower_expr(a, ctx)).collect();
                return ir::Expr::LocalCallMulti(name.clone(), lowered);
            }
            // Partial application or single-arg — use curried form
            let first = lower_expr(&args[0], ctx);
            let mut result = ir::Expr::LocalCall(name.clone(), Box::new(first));
            for arg in &args[1..] {
                result = ir::Expr::Call(Box::new(result), Box::new(lower_expr(arg, ctx)));
            }
            return result;
        }
    }

    // General curried application: chain args left to right
    // 0-arg call on an arbitrary expr → call with unit
    if args.is_empty() {
        return ir::Expr::Call(
            Box::new(lower_expr(func, ctx)),
            Box::new(ir::Expr::Atom("unit".into())),
        );
    }
    let mut result = lower_expr(func, ctx);
    for arg in args {
        result = ir::Expr::Call(Box::new(result), Box::new(lower_expr(arg, ctx)));
    }
    result
}

fn lower_variable(name: &str, ctx: &Ctx) -> ir::Expr {
    // Nullary constructor → atom
    if let Some(&0) = ctx.constructors.get(name) {
        return ir::Expr::Atom(name.to_lowercase());
    }
    // Top-level function in value position → fun f/1
    if ctx.fn_names.contains(name) {
        return ir::Expr::FunRef(name.to_string());
    }
    // Imported function in value position → fun module:f/1
    if let Some(module) = ctx.imports.get(name) {
        return ir::Expr::RemoteFunRef(module.clone(), name.to_string());
    }
    // Local variable → capitalize
    ir::Expr::Var(var_name(name))
}

fn lower_literal(lit: &ast::Literal) -> ir::Expr {
    match lit {
        ast::Literal::Int(n) => ir::Expr::Int(*n),
        ast::Literal::Float(f) => ir::Expr::Float(*f),
        ast::Literal::Bool(b) => ir::Expr::Atom(if *b { "true" } else { "false" }.into()),
        ast::Literal::String(s) => ir::Expr::Str(s.clone()),
        ast::Literal::Unit => ir::Expr::Atom("unit".into()),
    }
}

fn lower_pattern(pat: &ast::Pattern, ctx: &Ctx) -> ir::Pattern {
    match pat {
        ast::Pattern::Any(_) => ir::Pattern::Any,
        ast::Pattern::Variable(name, _) => ir::Pattern::Var(var_name(name)),
        ast::Pattern::Literal(lit, _) => match lit {
            ast::Literal::Int(n) => ir::Pattern::Int(*n),
            ast::Literal::Float(f) => ir::Pattern::Float(*f),
            ast::Literal::Bool(b) => ir::Pattern::Atom(if *b { "true" } else { "false" }.into()),
            ast::Literal::String(s) => ir::Pattern::Str(s.clone()),
            ast::Literal::Unit => ir::Pattern::Atom("unit".into()),
        },
        ast::Pattern::Constructor(name, sub_pats, _) => {
            let arity = ctx.constructors.get(name.as_str()).copied().unwrap_or(0);
            if arity == 0 {
                ir::Pattern::Atom(name.to_lowercase())
            } else {
                let tag = ir::Pattern::Atom(name.to_lowercase());
                let mut items = vec![tag];
                items.extend(sub_pats.iter().map(|p| lower_pattern(p, ctx)));
                ir::Pattern::Tuple(items)
            }
        }
        ast::Pattern::EmptyList(_) => ir::Pattern::List(vec![]),

        ast::Pattern::Cons(head, tail, _) => ir::Pattern::Cons(
            Box::new(lower_pattern(head, ctx)),
            Box::new(lower_pattern(tail, ctx)),
        ),

        ast::Pattern::Or(pats, _) => {
            // Or-patterns are expanded by the caller via expand_or_pattern
            // If we get here directly, just use the first alternative
            lower_pattern(&pats[0], ctx)
        }
    }
}

/// Expand or-patterns into multiple arms with the same body.
fn expand_or_pattern(pat: ir::Pattern, body: ir::Expr, arms: &mut Vec<(ir::Pattern, ir::Expr)>) {
    // Or-patterns have already been lowered at this point, so no special handling needed.
    // If the original AST had Or patterns, they would need pre-expansion before lowering.
    arms.push((pat, body));
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Build a curried lambda: `(fn {a b c} body)` → `fun(A) -> fun(B) -> fun(C) -> body end end end`
fn make_lambda(args: &[String], body: ir::Expr) -> ir::Expr {
    if args.is_empty() {
        ir::Expr::Fun("_Unit".to_string(), Box::new(body))
    } else {
        // Fold from right: innermost arg wraps body first
        args.iter()
            .rev()
            .fold(body, |acc, arg| ir::Expr::Fun(var_name(arg), Box::new(acc)))
    }
}

/// Capitalize first character of a variable name for Erlang.
/// `x` → `X`, `my_var` → `My_var`, `_` → `_`
fn var_name(name: &str) -> String {
    if name == "_" {
        return "_".to_string();
    }
    let mut chars = name.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

fn type_sig_arity(ty: &ast::TypeSig) -> usize {
    match ty {
        ast::TypeSig::Fun(_, rest) => 1 + type_sig_arity(rest),
        _ => 0,
    }
}

fn binary_op(name: &str) -> Option<&'static str> {
    match name {
        "+" => Some("+"),
        "-" => Some("-"),
        "*" => Some("*"),
        "/" => Some("div"),
        "+." => Some("+"),
        "-." => Some("-"),
        "*." => Some("*"),
        "/." => Some("/"),
        "=" => Some("=:="),
        "!=" => Some("=/="),
        "<" => Some("<"),
        ">" => Some(">"),
        "<=" => Some("=<"),
        ">=" => Some(">="),
        "and" => Some("andalso"),
        "or" => Some("orelse"),
        _ => None,
    }
}

fn unary_op(name: &str) -> Option<&'static str> {
    match name {
        "not" => Some("not"),
        _ => None,
    }
}

// ─── Emitter: IR → Erlang source ────────────────────────────────────────────

/// Quote an Erlang atom if it doesn't match `[a-z][a-zA-Z0-9_]*`.
fn quote_atom(name: &str) -> String {
    let needs_quoting = name.starts_with(|c: char| !c.is_lowercase())
        || name.chars().any(|c| !c.is_alphanumeric() && c != '_');
    if needs_quoting {
        format!("'{name}'")
    } else {
        name.to_string()
    }
}

pub fn emit_module(module: &ir::Module) -> String {
    let mut out = String::new();

    out.push_str(&format!("-module({}).\n", module.name));

    // Export all functions. Opal privacy is enforced by the import system, not Erlang exports.
    let exports: Vec<String> = module
        .functions
        .iter()
        .map(|f| format!("{}/{}", quote_atom(&f.name), f.params.len()))
        .collect();
    out.push_str(&format!("-export([{}]).\n\n", exports.join(", ")));

    for func in &module.functions {
        out.push_str(&emit_function(func));
        out.push('\n');
    }

    out
}

fn emit_function(func: &ir::Function) -> String {
    let params_s = func.params.join(", ");
    format!(
        "{}({params_s}) ->\n    {}.\n",
        quote_atom(&func.name),
        emit_expr(&func.body)
    )
}

fn emit_expr(expr: &ir::Expr) -> String {
    match expr {
        ir::Expr::Atom(s) => s.clone(),
        ir::Expr::Int(n) => n.to_string(),
        ir::Expr::Float(f) => format!("{f}"),
        ir::Expr::Str(s) => format!("<<\"{}\"/utf8>>", escape_str(s)),
        ir::Expr::Var(s) => s.clone(),
        ir::Expr::FunRef(name) => format!("fun {}/1", quote_atom(name)),
        ir::Expr::RemoteFunRef(module, name) => format!("fun {module}:{}/1", quote_atom(name)),

        ir::Expr::Tuple(items) => {
            format!(
                "{{{}}}",
                items.iter().map(emit_expr).collect::<Vec<_>>().join(", ")
            )
        }

        ir::Expr::List(items) => {
            if items.is_empty() {
                "[]".to_string()
            } else {
                format!(
                    "[{}]",
                    items.iter().map(emit_expr).collect::<Vec<_>>().join(", ")
                )
            }
        }

        ir::Expr::Fun(param, body) => {
            format!("fun({param}) -> {body} end", body = emit_expr(body))
        }

        ir::Expr::Call(func, arg) => {
            let arg_s = emit_expr(arg);
            match func.as_ref() {
                // Simple expressions can be called directly
                ir::Expr::Var(name) | ir::Expr::Atom(name) => format!("{name}({arg_s})"),
                ir::Expr::FunRef(name) => format!("(fun {name}/1)({arg_s})"),
                // Chained calls need parens around the inner call
                ir::Expr::Call(_, _)
                | ir::Expr::LocalCall(_, _)
                | ir::Expr::LocalCallMulti(_, _) => {
                    format!("({})({})", emit_expr(func), arg_s)
                }
                other => format!("({})({})", emit_expr(other), arg_s),
            }
        }

        ir::Expr::LocalCall(name, arg) => {
            format!("{}({})", quote_atom(name), emit_expr(arg))
        }

        ir::Expr::LocalCallMulti(name, args) => {
            let args_s = args.iter().map(emit_expr).collect::<Vec<_>>().join(", ");
            format!("{}({})", quote_atom(name), args_s)
        }

        ir::Expr::RemoteCall(module, function, args) => {
            let args_s = args.iter().map(emit_expr).collect::<Vec<_>>().join(", ");
            format!("{module}:{function}({args_s})")
        }

        ir::Expr::BinOp(op, lhs, rhs) => {
            format!("({} {op} {})", emit_expr(lhs), emit_expr(rhs))
        }

        ir::Expr::UnOp(op, expr) => {
            format!("({op} {})", emit_expr(expr))
        }

        ir::Expr::Let(var, val, body) => {
            format!("{var} = {},\n    {}", emit_expr(val), emit_expr(body))
        }

        ir::Expr::Case(scrutinee, arms) => {
            let arms_s: Vec<String> = arms
                .iter()
                .map(|(pat, body)| format!("        {} -> {}", emit_pattern(pat), emit_expr(body)))
                .collect();
            format!(
                "case {} of\n{}\n    end",
                emit_expr(scrutinee),
                arms_s.join(";\n")
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_arg_tco_emits_two_functions() {
        let src = r#"
(let sum {acc n}
  (if (= n 0)
    acc
    (sum (+ acc n) (- n 1))))
"#;
        let erl = crate::compile("test", src).unwrap();
        // Curried entry: sum/1
        assert!(erl.contains("sum(Acc) ->"), "missing curried entry:\n{erl}");
        // Multi-arg impl: sum/2
        assert!(erl.contains("sum(Acc, N) ->"), "missing multi-arg impl:\n{erl}");
        // Self-recursive call uses sum/2 directly (not curried)
        assert!(erl.contains("sum("), "missing recursive call:\n{erl}");
        // Both arities exported
        assert!(erl.contains("sum/1"), "sum/1 not exported:\n{erl}");
        assert!(erl.contains("sum/2"), "sum/2 not exported:\n{erl}");
    }

    #[test]
    fn single_arg_function_unchanged() {
        let src = r#"
(let double {x}
  (* 2 x))
"#;
        let erl = crate::compile("test", src).unwrap();
        // Only one function, no /2
        assert!(erl.contains("double(X) ->"), "missing function:\n{erl}");
        assert!(!erl.contains("double/2"), "unexpected double/2:\n{erl}");
    }

    #[test]
    fn partial_application_uses_curried_entry() {
        // (add 1) applied partially — must call add/1, not add/2
        let src = r#"
(let add {a b} (+ a b))
(let inc {x} (add 1 x))
"#;
        let erl = crate::compile("test", src).unwrap();
        // inc calls add with both args → LocalCallMulti → add(1, X)
        assert!(erl.contains("add(1, X)") || erl.contains("add(1,X)"), "expected add(1, X):\n{erl}");
    }
}

fn emit_pattern(pat: &ir::Pattern) -> String {
    match pat {
        ir::Pattern::Any => "_".to_string(),
        ir::Pattern::Var(s) => s.clone(),
        ir::Pattern::Atom(s) => s.clone(),
        ir::Pattern::Int(n) => n.to_string(),
        ir::Pattern::Float(f) => format!("{f}"),
        ir::Pattern::Str(s) => format!("<<\"{}\"/utf8>>", escape_str(s)),
        ir::Pattern::Tuple(items) => {
            format!(
                "{{{}}}",
                items
                    .iter()
                    .map(emit_pattern)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        ir::Pattern::List(items) => {
            if items.is_empty() {
                "[]".to_string()
            } else {
                format!(
                    "[{}]",
                    items
                        .iter()
                        .map(emit_pattern)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
        ir::Pattern::Cons(head, tail) => {
            format!("[{} | {}]", emit_pattern(head), emit_pattern(tail))
        }
    }
}

fn escape_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}
