use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
};

use crate::ast::{Expr, Literal, Pattern, TypeDecl, TypeUsage};

// ---------------------------------------------------------------------------
// Internal Type Representation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    /// Curried function: arg -> ret
    Fun(Rc<Type>, Rc<Type>),
    /// Named type constructor: Int, Bool, Option<'a>, Result<'e, 'a>
    Con(String, Vec<Rc<Type>>),
    /// Unification variable (T0, T1, ...)
    Var(u64),
}

impl Type {
    pub fn int() -> Rc<Self> {
        Rc::new(Type::Con("Int".into(), vec![]))
    }
    pub fn float() -> Rc<Self> {
        Rc::new(Type::Con("Float".into(), vec![]))
    }
    pub fn bool() -> Rc<Self> {
        Rc::new(Type::Con("Bool".into(), vec![]))
    }
    pub fn string() -> Rc<Self> {
        Rc::new(Type::Con("String".into(), vec![]))
    }
    pub fn unit() -> Rc<Self> {
        Rc::new(Type::Con("Unit".into(), vec![]))
    }
    pub fn array(elem: Rc<Self>) -> Rc<Self> {
        Rc::new(Type::Con("Array".into(), vec![elem]))
    }
    pub fn fun(arg: Rc<Self>, ret: Rc<Self>) -> Rc<Self> {
        Rc::new(Type::Fun(arg, ret))
    }
    pub fn con(name: impl Into<String>, args: Vec<Rc<Self>>) -> Rc<Self> {
        Rc::new(Type::Con(name.into(), args))
    }
}

/// A polytype: ∀ vars. ty
#[derive(Debug, Clone)]
pub struct Scheme {
    pub vars: Vec<u64>,
    pub ty: Rc<Type>,
}

pub type Substitution = HashMap<u64, Rc<Type>>;
pub type TypeEnv = HashMap<String, Scheme>;

// ---------------------------------------------------------------------------
// Error Handling
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum TypeError {
    OccursCheck {
        var: u64,
        ty: Rc<Type>,
    },
    Mismatch {
        expected: Rc<Type>,
        found: Rc<Type>,
    },
    BranchMismatch {
        then_ty: Rc<Type>,
        else_ty: Rc<Type>,
    },
    ArmMismatch {
        arm: usize,
        expected: Rc<Type>,
        found: Rc<Type>,
    },
    ConditionNotBool {
        found: Rc<Type>,
    },
    UnboundVariable(String),
}

impl TypeError {
    /// Render this type error as a codespan `Diagnostic`.
    /// `span` should cover the expression where the error was detected.
    pub fn to_diagnostic(
        &self,
        file_id: usize,
        span: std::ops::Range<usize>,
    ) -> codespan_reporting::diagnostic::Diagnostic<usize> {
        use codespan_reporting::diagnostic::{Diagnostic, Label};
        match self {
            TypeError::Mismatch { expected, found } => {
                // Share var_names so the same inference variable prints the same name
                // in both expected and found (e.g. both show `'a` not `?5`/`?6`).
                let mut var_names = std::collections::HashMap::new();
                let expected_s = type_display_inner(expected, &mut var_names);
                let found_s = type_display_inner(found, &mut var_names);
                let mut notes = vec![format!("expected `{expected_s}`, found `{found_s}`")];
                // Helpful hints for common mismatches
                if *expected == Type::float() && *found == Type::int() {
                    notes.push(
                        "hint: integer literals like `1` have type `Int`; write `1.0` for a `Float`".into(),
                    );
                    notes.push(
                        "hint: float operators use a `.` suffix — `+.` `-.` `*.` `/.` `<.` `>.`"
                            .into(),
                    );
                } else if *expected == Type::int() && *found == Type::float() {
                    notes.push(
                        "hint: float literals like `1.0` have type `Float`; write `1` for an `Int`"
                            .into(),
                    );
                    notes.push(
                        "hint: integer operators have no suffix — `+` `-` `*` `/` `<` `>`".into(),
                    );
                }
                Diagnostic::error()
                    .with_message(format!(
                        "type mismatch: expected `{expected_s}`, found `{found_s}`"
                    ))
                    .with_labels(vec![
                        Label::primary(file_id, span).with_message("type error originates here"),
                    ])
                    .with_notes(notes)
            }
            TypeError::ConditionNotBool { found } => {
                let found_s = type_display(found);
                Diagnostic::error()
                    .with_message(format!("if condition must be `Bool`, found `{found_s}`"))
                    .with_labels(vec![
                        Label::primary(file_id, span)
                            .with_message(format!("this has type `{found_s}`, expected `Bool`")),
                    ])
            }
            TypeError::ArmMismatch {
                arm,
                expected,
                found,
            } => {
                let mut var_names = std::collections::HashMap::new();
                let expected_s = type_display_inner(expected, &mut var_names);
                let found_s = type_display_inner(found, &mut var_names);
                // arm is 0-indexed; arm 0 sets the expected type, conflict is at arm N
                let conflicting = arm + 1;
                Diagnostic::error()
                    .with_message("match arms have incompatible types")
                    .with_labels(vec![
                        Label::primary(file_id, span)
                            .with_message("arms must all return the same type"),
                    ])
                    .with_notes(vec![
                        format!("  arm 1 returns: `{expected_s}`"),
                        format!("  arm {conflicting} returns: `{found_s}`"),
                    ])
            }
            TypeError::BranchMismatch { then_ty, else_ty } => {
                let mut var_names = std::collections::HashMap::new();
                let then_s = type_display_inner(then_ty, &mut var_names);
                let else_s = type_display_inner(else_ty, &mut var_names);
                Diagnostic::error()
                    .with_message("if/else branches have incompatible types")
                    .with_labels(vec![
                        Label::primary(file_id, span)
                            .with_message("branches must return the same type"),
                    ])
                    .with_notes(vec![
                        format!("  then branch: `{then_s}`"),
                        format!("  else branch: `{else_s}`"),
                    ])
            }
            TypeError::UnboundVariable(name) => Diagnostic::error()
                .with_message(format!("unbound variable `{name}`"))
                .with_labels(vec![
                    Label::primary(file_id, span)
                        .with_message(format!("`{name}` is not defined in this scope")),
                ]),
            TypeError::OccursCheck { ty, .. } => Diagnostic::error()
                .with_message("infinite type")
                .with_labels(vec![Label::primary(file_id, span).with_message(format!(
                    "this expression would have the infinite type `{}`",
                    type_display(ty)
                ))])
                .with_notes(vec![
                    "hint: this usually means a function is being applied to itself".into(),
                ]),
        }
    }
}

fn type_display(ty: &Type) -> String {
    let mut var_names: std::collections::HashMap<u64, String> = std::collections::HashMap::new();
    type_display_inner(ty, &mut var_names)
}

fn type_display_inner(ty: &Type, var_names: &mut std::collections::HashMap<u64, String>) -> String {
    match ty {
        Type::Con(name, args) if args.is_empty() => name.clone(),
        Type::Con(name, args) => {
            let args_str = args
                .iter()
                .map(|a| type_display_inner(a, var_names))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{name}[{args_str}]")
        }
        Type::Fun(arg, ret) => format!(
            "({} -> {})",
            type_display_inner(arg, var_names),
            type_display_inner(ret, var_names)
        ),
        Type::Var(id) => {
            let next = var_names.len();
            var_names
                .entry(*id)
                .or_insert_with(|| {
                    // 'a .. 'z, then 'a1, 'b1, ...
                    let letter = (b'a' + (next % 26) as u8) as char;
                    if next < 26 {
                        format!("'{letter}")
                    } else {
                        format!("'{letter}{}", next / 26)
                    }
                })
                .clone()
        }
    }
}

// ---------------------------------------------------------------------------
// Substitution Logic (The "Notebook")
// ---------------------------------------------------------------------------

pub fn apply_subst(subst: &Substitution, ty: &Rc<Type>) -> Rc<Type> {
    match ty.as_ref() {
        Type::Var(id) => match subst.get(id) {
            Some(t) => apply_subst(subst, t),
            None => ty.clone(),
        },
        Type::Fun(arg, ret) => Rc::new(Type::Fun(apply_subst(subst, arg), apply_subst(subst, ret))),
        Type::Con(name, args) => Rc::new(Type::Con(
            name.clone(),
            args.iter().map(|a| apply_subst(subst, a)).collect(),
        )),
    }
}

fn apply_subst_scheme(subst: &Substitution, scheme: &Scheme) -> Scheme {
    let reduced: Substitution = subst
        .iter()
        .filter(|(k, _)| !scheme.vars.contains(k))
        .map(|(k, v)| (*k, v.clone()))
        .collect();
    Scheme {
        vars: scheme.vars.clone(),
        ty: apply_subst(&reduced, &scheme.ty),
    }
}

pub fn apply_subst_env(subst: &Substitution, env: &TypeEnv) -> TypeEnv {
    env.iter()
        .map(|(k, v)| (k.clone(), apply_subst_scheme(subst, v)))
        .collect()
}

pub fn compose_subst(s_later: &Substitution, s_earlier: &Substitution) -> Substitution {
    let mut result: Substitution = s_earlier
        .iter()
        .map(|(k, v)| (*k, apply_subst(s_later, v)))
        .collect();
    for (k, v) in s_later {
        result.entry(*k).or_insert_with(|| v.clone());
    }
    result
}

// ---------------------------------------------------------------------------
// Generalization & Instantiation
// ---------------------------------------------------------------------------

fn free_vars(ty: &Type) -> HashSet<u64> {
    match ty {
        Type::Var(id) => HashSet::from([*id]),
        Type::Fun(arg, ret) => {
            let mut fv = free_vars(arg);
            fv.extend(free_vars(ret));
            fv
        }
        Type::Con(_, args) => args.iter().flat_map(|a| free_vars(a)).collect(),
    }
}

fn generalize(env: &TypeEnv, ty: &Rc<Type>) -> Scheme {
    // 1. Get all variables currently free in the environment
    let env_fv: HashSet<u64> = env
        .values()
        .flat_map(|s| {
            let fv = free_vars(&s.ty);
            let bound: HashSet<u64> = s.vars.iter().cloned().collect();
            // We need to collect the difference immediately to avoid reference errors
            fv.into_iter()
                .filter(|id| !bound.contains(id))
                .collect::<Vec<u64>>()
        })
        .collect();

    // 2. Get variables in the current type
    let ty_fv = free_vars(ty);

    // 3. Any variable in the type that is NOT in the environment can be generalized
    let mut vars: Vec<u64> = ty_fv
        .into_iter()
        .filter(|id| !env_fv.contains(id))
        .collect();

    vars.sort(); // Keep ordering deterministic
    Scheme {
        vars,
        ty: ty.clone(),
    }
}

// ---------------------------------------------------------------------------
// Unification
// ---------------------------------------------------------------------------

pub fn unify(t1: &Rc<Type>, t2: &Rc<Type>) -> Result<Substitution, TypeError> {
    match (t1.as_ref(), t2.as_ref()) {
        _ if t1 == t2 => Ok(HashMap::new()),
        (Type::Var(id), _) => {
            if free_vars(t2).contains(id) {
                return Err(TypeError::OccursCheck {
                    var: *id,
                    ty: t2.clone(),
                });
            }
            Ok(HashMap::from([(*id, t2.clone())]))
        }
        (_, Type::Var(_)) => unify(t2, t1),
        (Type::Fun(a1, r1), Type::Fun(a2, r2)) => {
            let s1 = unify(a1, a2)?;
            let s2 = unify(&apply_subst(&s1, r1), &apply_subst(&s1, r2))?;
            Ok(compose_subst(&s2, &s1))
        }
        (Type::Con(n1, args1), Type::Con(n2, args2)) if n1 == n2 && args1.len() == args2.len() => {
            args1
                .iter()
                .zip(args2)
                .try_fold(HashMap::new(), |acc, (a, b)| {
                    let s = unify(&apply_subst(&acc, a), &apply_subst(&acc, b))?;
                    Ok(compose_subst(&s, &acc))
                })
        }
        _ => Err(TypeError::Mismatch {
            expected: t1.clone(),
            found: t2.clone(),
        }),
    }
}

// ---------------------------------------------------------------------------
// TypeChecker Implementation
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct TypeChecker {
    counter: u64,
}

impl TypeChecker {
    pub fn new() -> Self {
        Self { counter: 0 }
    }

    fn fresh(&mut self) -> Rc<Type> {
        let id = self.counter;
        self.counter += 1;
        Rc::new(Type::Var(id))
    }

    fn instantiate(&mut self, scheme: &Scheme) -> Rc<Type> {
        let subst: Substitution = scheme.vars.iter().map(|&v| (v, self.fresh())).collect();
        apply_subst(&subst, &scheme.ty)
    }

    pub fn infer(
        &mut self,
        env: &TypeEnv,
        expr: &Expr,
    ) -> Result<(Substitution, Rc<Type>), TypeError> {
        match expr {
            Expr::Literal(lit, _) => {
                let ty = match lit {
                    Literal::Int(_) => Type::int(),
                    Literal::Float(_) => Type::float(),
                    Literal::Bool(_) => Type::bool(),
                    Literal::String(_) => Type::string(),
                    Literal::Unit => Type::unit(),
                };
                Ok((HashMap::new(), ty))
            }

            Expr::Variable(name, _) => {
                let scheme = env
                    .get(name)
                    .ok_or_else(|| TypeError::UnboundVariable(name.clone()))?;
                Ok((HashMap::new(), self.instantiate(scheme)))
            }

            Expr::Array(items, _) => {
                let elem_ty = self.fresh();
                let mut subst = HashMap::new();
                for item in items {
                    let (s, t) = self.infer(&apply_subst_env(&subst, env), item)?;
                    // Apply both accumulated subst and item subst so the
                    // constrained elem_ty is visible when unifying the next item.
                    let known_elem = apply_subst(&compose_subst(&s, &subst), &elem_ty);
                    let s_unify = unify(&known_elem, &t)?;
                    subst = compose_subst(&compose_subst(&s_unify, &s), &subst);
                }
                Ok((subst.clone(), Type::array(apply_subst(&subst, &elem_ty))))
            }

            Expr::If {
                cond, then, els, ..
            } => {
                let (s1, t_cond) = self.infer(env, cond)?;
                let s_bool =
                    unify(&t_cond, &Type::bool()).map_err(|_| TypeError::ConditionNotBool {
                        found: t_cond.clone(),
                    })?;
                let s1 = compose_subst(&s_bool, &s1);

                let (s2, t_then) = self.infer(&apply_subst_env(&s1, env), then)?;
                let s12 = compose_subst(&s2, &s1);

                let (s3, t_els) = self.infer(&apply_subst_env(&s12, env), els)?;
                let s123 = compose_subst(&s3, &s12);

                let then_resolved = apply_subst(&s123, &t_then);
                let else_resolved = apply_subst(&s123, &t_els);
                let s_final = unify(&then_resolved, &else_resolved).map_err(|_| {
                    TypeError::BranchMismatch {
                        then_ty: then_resolved.clone(),
                        else_ty: else_resolved.clone(),
                    }
                })?;
                let s_res = compose_subst(&s_final, &s123);
                Ok((s_res.clone(), apply_subst(&s_res, &t_then)))
            }

            Expr::Call { func, args, .. } => {
                let (s0, mut t_func) = self.infer(env, func)?;
                let mut subst = s0;

                for arg in args {
                    let (s_arg, t_arg) = self.infer(&apply_subst_env(&subst, env), arg)?;
                    subst = compose_subst(&s_arg, &subst);
                    t_func = apply_subst(&subst, &t_func);

                    let ret = self.fresh();
                    let s_unify = unify(&t_func, &Type::fun(t_arg, ret.clone()))?;
                    subst = compose_subst(&s_unify, &subst);
                    t_func = apply_subst(&subst, &ret);
                }
                Ok((subst, t_func))
            }

            // Named function definition — always self-recursive in Opal.
            // The function name is added to inner_env before inferring the body,
            // so it can call itself without any special keyword.
            Expr::LetFunc {
                name, args, value, ..
            } => {
                let arg_tys: Vec<Rc<Type>> = args.iter().map(|_| self.fresh()).collect();
                let ret_ty = self.fresh();
                let fun_ty = arg_tys
                    .iter()
                    .rev()
                    .fold(ret_ty.clone(), |acc, a| Type::fun(a.clone(), acc));

                let mut inner_env = env.clone();
                for (arg, ty) in args.iter().zip(&arg_tys) {
                    inner_env.insert(
                        arg.clone(),
                        Scheme {
                            vars: vec![],
                            ty: ty.clone(),
                        },
                    );
                }
                // Self-reference: name is in scope during its own body
                inner_env.insert(
                    name.clone(),
                    Scheme {
                        vars: vec![],
                        ty: fun_ty.clone(),
                    },
                );

                let (s1, t_val) = self.infer(&inner_env, value)?;
                let s2 = unify(&apply_subst(&s1, &ret_ty), &t_val)?;
                let s12 = compose_subst(&s2, &s1);

                let binding_ty = apply_subst(&s12, &fun_ty);
                Ok((s12, binding_ty))
            }

            // Sequential local binding — (let [x val] body).
            // The name is NOT in scope during its own value expression.
            Expr::LetLocal {
                name, value, body, ..
            } => {
                let (s1, t_val) = self.infer(env, value)?;
                let scheme = generalize(&apply_subst_env(&s1, env), &t_val);

                let mut body_env = apply_subst_env(&s1, env);
                body_env.insert(name.clone(), scheme);
                let (s2, t_body) = self.infer(&body_env, body)?;

                Ok((compose_subst(&s2, &s1), t_body))
            }

            Expr::Match { target, arms, .. } => {
                let (s0, t_target) = self.infer(env, target)?;
                let mut subst = s0;
                let result_ty = self.fresh();

                for (arm_index, (pat, body)) in arms.iter().enumerate() {
                    let t_target_s = apply_subst(&subst, &t_target);
                    let (s_pat, pat_env) =
                        self.infer_pattern(&apply_subst_env(&subst, env), pat, &t_target_s)?;
                    subst = compose_subst(&s_pat, &subst);

                    let (s_body, t_body) = self.infer(&pat_env, body)?;
                    subst = compose_subst(&s_body, &subst);

                    let expected = apply_subst(&subst, &result_ty);
                    let found = apply_subst(&subst, &t_body);
                    let s_unify = unify(&expected, &found).map_err(|_| TypeError::ArmMismatch {
                        arm: arm_index,
                        expected: expected.clone(),
                        found: found.clone(),
                    })?;
                    subst = compose_subst(&s_unify, &subst);
                }
                Ok((subst.clone(), apply_subst(&subst, &result_ty)))
            }

            Expr::FieldAccess { field, record, .. } => {
                let accessor_name = format!(":{field}");
                let scheme = env
                    .get(&accessor_name)
                    .ok_or_else(|| TypeError::UnboundVariable(accessor_name.clone()))?;
                let accessor_ty = self.instantiate(scheme);

                let (s1, t_record) = self.infer(env, record)?;
                let ret_ty = self.fresh();
                let s2 = unify(
                    &apply_subst(&s1, &accessor_ty),
                    &Type::fun(t_record, ret_ty.clone()),
                )?;
                let s12 = compose_subst(&s2, &s1);

                Ok((s12.clone(), apply_subst(&s12, &ret_ty)))
            }
        }
    }

    fn infer_pattern(
        &mut self,
        env: &TypeEnv,
        pat: &Pattern,
        expected: &Rc<Type>,
    ) -> Result<(Substitution, TypeEnv), TypeError> {
        match pat {
            Pattern::Any(_) => Ok((HashMap::new(), env.clone())),
            Pattern::Variable(name, _) => {
                let mut new_env = env.clone();
                new_env.insert(
                    name.clone(),
                    Scheme {
                        vars: vec![],
                        ty: expected.clone(),
                    },
                );
                Ok((HashMap::new(), new_env))
            }
            Pattern::Literal(lit, _) => {
                let ty = match lit {
                    Literal::Int(_) => Type::int(),
                    Literal::Float(_) => Type::float(),
                    Literal::Bool(_) => Type::bool(),
                    Literal::String(_) => Type::string(),
                    Literal::Unit => Type::unit(),
                };
                Ok((unify(expected, &ty)?, env.clone()))
            }
            Pattern::Constructor(name, arg_pats, _) => {
                let scheme = env
                    .get(name)
                    .ok_or_else(|| TypeError::UnboundVariable(name.clone()))?;
                let mut con_ty = self.instantiate(scheme);
                let mut subst = HashMap::new();
                let mut pat_env = env.clone();

                for arg_pat in arg_pats {
                    if let Type::Fun(arg_ty, ret_ty) = con_ty.as_ref() {
                        let (s_arg, new_env) =
                            self.infer_pattern(&pat_env, arg_pat, &apply_subst(&subst, arg_ty))?;
                        subst = compose_subst(&s_arg, &subst);
                        con_ty = apply_subst(&subst, ret_ty);
                        pat_env = new_env;
                    } else {
                        return Err(TypeError::Mismatch {
                            expected: Type::fun(self.fresh(), self.fresh()),
                            found: con_ty.clone(),
                        });
                    }
                }
                let s_unify = unify(&apply_subst(&subst, &con_ty), expected)?;
                Ok((compose_subst(&s_unify, &subst), pat_env))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Program-level type checking
// ---------------------------------------------------------------------------

/// The result of type-checking a single declaration.
pub enum DeclResult {
    /// A type or term declaration was accepted; env is updated.
    Ok(Rc<Type>),
    /// A declaration failed to type-check.
    Err {
        error: TypeError,
        expr: crate::ast::Expr,
    },
}

impl TypeChecker {
    /// Type-check a sequence of declarations in order, threading the environment.
    ///
    /// Returns `Ok(last_type)` if all declarations pass, or `Err((error, expr))` on
    /// the first failure, where `expr` is the expression that caused the error.
    pub fn check_program(
        &mut self,
        env: &mut TypeEnv,
        decls: &[crate::ast::Declaration],
    ) -> Result<Rc<Type>, (TypeError, crate::ast::Expr)> {
        use crate::ast::{Declaration, Expr};

        let mut last_ty = Type::unit();

        for decl in decls {
            match decl {
                Declaration::Type(type_decl) => {
                    env.extend(constructor_schemes(type_decl));
                }
                Declaration::Expression(expr) => {
                    match self.infer(env, expr) {
                        Ok((s, ty)) => {
                            *env = apply_subst_env(&s, env);
                            // Named top-level functions must be available to subsequent declarations
                            if let Expr::LetFunc { name, .. } = expr {
                                let scheme = generalize(env, &ty);
                                env.insert(name.clone(), scheme);
                            }
                            last_ty = ty;
                        }
                        Err(error) => return Err((error, expr.clone())),
                    }
                }
            }
        }

        Ok(last_ty)
    }
}

// ---------------------------------------------------------------------------
// Primitive environment
// ---------------------------------------------------------------------------

/// Build a `TypeEnv` with built-in operator and function types.
/// Constructors for user-defined types must be added separately.
pub fn primitive_env() -> TypeEnv {
    let mut env = TypeEnv::new();

    // Helper to build a curried function type: t1 -> t2 -> ret
    let fun2 = |a: Rc<Type>, b: Rc<Type>, ret: Rc<Type>| Scheme {
        vars: vec![],
        ty: Type::fun(a, Type::fun(b, ret)),
    };

    // Arithmetic — Int
    for op in ["+", "-", "*", "/"] {
        env.insert(op.to_string(), fun2(Type::int(), Type::int(), Type::int()));
    }
    // Arithmetic — Float
    for op in ["+.", "-.", "*.", "/."] {
        env.insert(
            op.to_string(),
            fun2(Type::float(), Type::float(), Type::float()),
        );
    }

    // Int comparisons
    for op in ["<", ">", "<=", ">="] {
        env.insert(op.to_string(), fun2(Type::int(), Type::int(), Type::bool()));
    }

    // Float comparisons
    for op in ["<.", ">.", "<=.", ">=."] {
        env.insert(
            op.to_string(),
            fun2(Type::float(), Type::float(), Type::bool()),
        );
    }

    // Polymorphic equality: ∀a. a -> a -> Bool
    let eq_var = u64::MAX;
    let eq_scheme = Scheme {
        vars: vec![eq_var],
        ty: Type::fun(
            Rc::new(Type::Var(eq_var)),
            Type::fun(Rc::new(Type::Var(eq_var)), Type::bool()),
        ),
    };
    env.insert("=".to_string(), eq_scheme.clone());
    env.insert("!=".to_string(), eq_scheme);

    // Boolean operators
    env.insert(
        "or".to_string(),
        fun2(Type::bool(), Type::bool(), Type::bool()),
    );
    env.insert(
        "and".to_string(),
        fun2(Type::bool(), Type::bool(), Type::bool()),
    );
    env.insert(
        "not".to_string(),
        Scheme {
            vars: vec![],
            ty: Type::fun(Type::bool(), Type::bool()),
        },
    );

    // String concatenation
    env.insert(
        "str".to_string(),
        fun2(Type::string(), Type::string(), Type::string()),
    );

    env
}

// ---------------------------------------------------------------------------
// TypeUsage → Type conversion
// ---------------------------------------------------------------------------

fn type_usage_to_type(usage: &TypeUsage, params: &HashMap<String, Rc<Type>>) -> Rc<Type> {
    match usage {
        TypeUsage::Named(name) => Type::con(name, vec![]),
        TypeUsage::Generic(name) => params
            .get(name)
            .cloned()
            .unwrap_or_else(|| Type::con(name, vec![])),
        TypeUsage::App(head, args) => {
            let arg_tys = args.iter().map(|a| type_usage_to_type(a, params)).collect();
            Type::con(head, arg_tys)
        }
    }
}

// ---------------------------------------------------------------------------
// Constructor scheme generation from type declarations
// ---------------------------------------------------------------------------

/// Given a type declaration, generate all the scheme entries for the TypeEnv.
/// This includes constructor functions and field accessor functions for records.
pub fn constructor_schemes(decl: &TypeDecl) -> TypeEnv {
    let mut env = TypeEnv::new();

    // Use high var IDs for scheme-bound params to avoid colliding with
    // the TypeChecker's counter (which starts at 0 and counts up).
    // We use u64::MAX - index so these IDs are never generated by fresh().
    const SCHEME_VAR_BASE: u64 = u64::MAX - 1024;

    match decl {
        TypeDecl::Variant {
            name,
            params,
            constructors,
            ..
        } => {
            // Map each param "'a" → Var(SCHEME_VAR_BASE - index)
            let scheme_vars: Vec<u64> = (0..params.len())
                .map(|i| SCHEME_VAR_BASE - i as u64)
                .collect();
            let param_map: HashMap<String, Rc<Type>> = params
                .iter()
                .zip(&scheme_vars)
                .map(|(p, &v)| (p.clone(), Rc::new(Type::Var(v))))
                .collect();

            // result_ty = Con(name, [Var(SCHEME_VAR_BASE), Var(SCHEME_VAR_BASE-1), ...])
            let result_ty_args: Vec<Rc<Type>> =
                scheme_vars.iter().map(|&v| Rc::new(Type::Var(v))).collect();
            let result_ty = Type::con(name, result_ty_args);

            for (con_name, payload) in constructors {
                let ty = match payload {
                    // Nullary constructor: None -> Option<'a>
                    None => result_ty.clone(),
                    // Constructor with payload: Some ~ 'a  ->  'a -> Option<'a>
                    Some(usage) => {
                        let payload_ty = type_usage_to_type(usage, &param_map);
                        Type::fun(payload_ty, result_ty.clone())
                    }
                };
                env.insert(
                    con_name.clone(),
                    Scheme {
                        vars: scheme_vars.clone(),
                        ty,
                    },
                );
            }
        }

        TypeDecl::Record {
            name,
            params,
            fields,
            ..
        } => {
            // Map each param "'a" → Var(SCHEME_VAR_BASE - index)
            let scheme_vars: Vec<u64> = (0..params.len())
                .map(|i| SCHEME_VAR_BASE - i as u64)
                .collect();
            let param_map: HashMap<String, Rc<Type>> = params
                .iter()
                .zip(&scheme_vars)
                .map(|(p, &v)| (p.clone(), Rc::new(Type::Var(v))))
                .collect();

            // result_ty = Con(name, [Var(SCHEME_VAR_BASE), ...])
            let result_ty_args: Vec<Rc<Type>> =
                scheme_vars.iter().map(|&v| Rc::new(Type::Var(v))).collect();
            let result_ty = Type::con(name, result_ty_args);

            // Constructor function: name -> T_f1 -> T_f2 -> ... -> result_ty
            // Built by folding fields in reverse
            let ctor_ty = fields
                .iter()
                .rev()
                .fold(result_ty.clone(), |acc, (_, field_ty)| {
                    let ft = type_usage_to_type(field_ty, &param_map);
                    Type::fun(ft, acc)
                });
            env.insert(
                name.clone(),
                Scheme {
                    vars: scheme_vars.clone(),
                    ty: ctor_ty,
                },
            );

            // Field accessors: ":field_name" -> result_ty -> field_type
            for (field_name, field_ty) in fields {
                let ft = type_usage_to_type(field_ty, &param_map);
                let accessor_ty = Type::fun(result_ty.clone(), ft);
                env.insert(
                    format!(":{field_name}"),
                    Scheme {
                        vars: scheme_vars.clone(),
                        ty: accessor_ty,
                    },
                );
            }
        }
    }

    env
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn check(src: &str) -> Result<Rc<Type>, TypeError> {
        let tokens = crate::lexer::Lexer::new(src).lex();
        let mut lowerer = crate::lower::Lowerer::new();

        let file_id = lowerer.add_file("test.opal".into(), src.into());

        let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
            .parse()
            .expect("parse failed");
        let decls = lowerer.lower_file(file_id, &sexprs);

        let mut checker = TypeChecker::new();
        let mut env = primitive_env();

        checker.check_program(&mut env, &decls).map_err(|(e, _)| e)
    }

    fn check_expr(src: &str) -> Result<Rc<Type>, TypeError> {
        let tokens = crate::lexer::Lexer::new(src).lex();
        let mut lowerer = crate::lower::Lowerer::new();
        let file_id = lowerer.add_file("test.opal".into(), src.into());
        let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
            .parse()
            .expect("parse failed");
        let expr = lowerer
            .lower_expr(file_id, &sexprs[0])
            .expect("lowering failed");
        let mut checker = TypeChecker::new();
        let env = primitive_env();
        checker.infer(&env, &expr).map(|(_, ty)| ty)
    }

    #[test]
    fn infer_int_literal() {
        let ty = check_expr("42").unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_bool_literal() {
        let ty = check_expr("True").unwrap();
        assert_eq!(ty, Type::bool());
    }

    #[test]
    fn infer_arithmetic() {
        let ty = check_expr("(+ 1 2)").unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_if() {
        let ty = check_expr("(if True 1 2)").unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_let_function() {
        let ty = check("(let double {x} (* 2 x))\n(let main {} (double 5))").unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_let_binding() {
        // Local bindings live inside function bodies; use a 1-arg wrapper
        let ty = check("(let f {dummy} (let [x 42] x))\n(let main {} (f 0))").unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_identity_is_polymorphic() {
        // Use identity at two different types to verify polymorphism.
        let src = "(let id {x} x)\n(let get_a {dummy} (let [a (id 42) b (id True)] a))\n(let main {} (get_a 0))";
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn unbound_variable_error() {
        let tokens = crate::lexer::Lexer::new("x").lex();

        let mut lowerer = crate::lower::Lowerer::new();
        let file_id = lowerer.add_file("test.opal".into(), "x".into());
        let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
            .parse()
            .unwrap();
        let expr = lowerer.lower_expr(file_id, &sexprs[0]).unwrap();

        let mut checker = TypeChecker::new();
        let env = primitive_env();
        assert!(matches!(
            checker.infer(&env, &expr),
            Err(TypeError::UnboundVariable(_))
        ));
    }

    #[test]
    fn type_mismatch_error() {
        // (+ True 1) should fail
        let tokens = crate::lexer::Lexer::new("(+ True 1)").lex();

        let mut lowerer = crate::lower::Lowerer::new();
        let file_id = lowerer.add_file("test.opal".into(), "(+ True 1)".into());
        let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
            .parse()
            .unwrap();
        let expr = lowerer.lower_expr(file_id, &sexprs[0]).unwrap();

        let mut checker = TypeChecker::new();
        let env = primitive_env();
        assert!(matches!(
            checker.infer(&env, &expr),
            Err(TypeError::Mismatch { .. })
        ));
    }

    #[test]
    fn infer_option_none() {
        let src = r#"
            (type ['a] Option (None (Some ~ 'a)))
            (let get_none {} None)
        "#;
        let ty = check(src).unwrap();
        // get_none : Option<'a> — should be Con("Option", [_])
        match ty.as_ref() {
            Type::Con(name, args) => {
                assert_eq!(name, "Option");
                assert_eq!(args.len(), 1);
            }
            _ => panic!("expected Con(Option, _), got {:?}", ty),
        }
    }

    #[test]
    fn infer_option_some() {
        let src = r#"
            (type ['a] Option (None (Some ~ 'a)))
            (let get_some {} (Some 42))
        "#;
        let ty = check(src).unwrap();
        // Some 42 : Option<Int>
        assert_eq!(ty, Type::con("Option", vec![Type::int()]));
    }

    #[test]
    fn infer_match_option() {
        let src = r#"
            (type ['a] Option (None (Some ~ 'a)))
            (let safe_add {opt n}
              (match opt
                None ~> 0
                (Some x) ~> (+ x n)))
            (let main {} (safe_add (Some 5) 3))
        "#;
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_record_construction() {
        let src = r#"
            (type Point ((:x ~ Int) (:y ~ Int)))
            (let test {p} (+ 1 2))
            (let main {} (test (Point 0 0)))
        "#;
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_field_access() {
        let src = r#"
            (type Point ((:x ~ Int) (:y ~ Int)))
            (let get_x {p} (:x p))
            (let main {} (get_x (Point 5 10)))
        "#;
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_field_access_generic_record() {
        let src = r#"
            (type ['t] Box ((:value ~ 't)))
            (let get_val {b} (:value b))
            (let main {} (get_val (Box 42)))
        "#;
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_result_type() {
        let src = r#"
            (type ['e 'a] Result ((Ok ~ 'a) (Error ~ 'e)))
            (let identity {r} r)
            (let main {} (identity (Ok 42)))
        "#;
        let ty = check(src).unwrap();
        // Should be Con("Result", [_, Con("Int", [])])
        match ty.as_ref() {
            Type::Con(name, args) => {
                assert_eq!(name, "Result");
                assert_eq!(args.len(), 2);
                assert_eq!(args[1], Type::int());
            }
            _ => panic!("expected Con(Result, _), got {:?}", ty),
        }
    }

    #[test]
    fn infer_recursive_fib() {
        // Named functions are self-recursive by default — no `rec` needed
        let src = r#"
            (let fib {n}
              (if (= n 0)
                0
                (if (= n 1)
                  1
                  (+ (fib (- n 1)) (fib (- n 2))))))
            (let main {} (fib 10))
        "#;
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn field_access_wrong_type() {
        let src = r#"
            (type Point ((:x ~ Int) (:y ~ Int)))
            (let test {} (:x 42))
        "#;
        let result = check(src);
        assert!(result.is_err(), "expected type error, got {:?}", result);
    }

    #[test]
    fn infer_float_literal() {
        let ty = check_expr("3.14").unwrap();
        assert_eq!(ty, Type::float());
    }

    #[test]
    fn infer_string_literal() {
        let ty = check_expr(r#""hello world""#).unwrap();
        assert_eq!(ty, Type::string());
    }

    #[test]
    fn infer_float_arithmetic() {
        let ty = check_expr("(+. 1.5 2.5)").unwrap();
        assert_eq!(ty, Type::float());
    }

    #[test]
    fn infer_array_of_ints() {
        let ty = check_expr("#[1 2 3]").unwrap();
        assert_eq!(ty, Type::array(Type::int()));
    }

    #[test]
    fn infer_empty_array() {
        let ty = check_expr("#[]").unwrap();
        // empty array has a polymorphic element type var
        match ty.as_ref() {
            Type::Con(name, args) => {
                assert_eq!(name, "Array");
                assert_eq!(args.len(), 1);
                assert!(matches!(args[0].as_ref(), Type::Var(_)));
            }
            _ => panic!("expected Array type, got {:?}", ty),
        }
    }

    #[test]
    fn infer_array_type_mismatch() {
        // Int and Bool in the same array must fail
        let result = check_expr("#[1 True]");
        assert!(result.is_err(), "expected type error, got {:?}", result);
    }

    #[test]
    fn infer_boolean_and() {
        let ty = check_expr("(and True False)").unwrap();
        assert_eq!(ty, Type::bool());
    }

    #[test]
    fn infer_not() {
        let ty = check_expr("(not True)").unwrap();
        assert_eq!(ty, Type::bool());
    }

    #[test]
    fn infer_comparison_lt() {
        let ty = check_expr("(< 1 2)").unwrap();
        assert_eq!(ty, Type::bool());
    }

    #[test]
    fn infer_equality_polymorphic() {
        // = works on Int
        let ty = check_expr("(= 1 1)").unwrap();
        assert_eq!(ty, Type::bool());
        // = also works on Bool
        let ty2 = check_expr("(= True False)").unwrap();
        assert_eq!(ty2, Type::bool());
    }

    #[test]
    fn infer_string_concat() {
        let ty = check_expr(r#"(str "hello" " world")"#).unwrap();
        assert_eq!(ty, Type::string());
    }

    #[test]
    fn infer_if_non_bool_condition() {
        // Condition must be Bool; Int should fail
        let result = check_expr("(if 1 2 3)");
        assert!(result.is_err(), "expected type error, got {:?}", result);
        assert!(
            matches!(result, Err(TypeError::ConditionNotBool { .. })),
            "expected ConditionNotBool"
        );
    }

    #[test]
    fn infer_if_branch_type_mismatch() {
        // then=Int, else=Bool → branches must agree
        let result = check_expr("(if True 1 True)");
        assert!(result.is_err(), "expected type error, got {:?}", result);
    }

    #[test]
    fn infer_multi_arg_function() {
        let ty = check("(let add {a b} (+ a b))\n(let main {} (add 1 2))").unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_wildcard_pattern() {
        let src = r#"
            (type ['a] Option (None (Some ~ 'a)))
            (let is_some {opt}
              (match opt
                None ~> False
                _ ~> True))
            (let main {} (is_some (Some 1)))
        "#;
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::bool());
    }

    #[test]
    fn infer_literal_pattern_in_match() {
        // Match on integer literals
        let src = "(let classify {n} (match n 0 ~> True _ ~> False))\n(let main {} (classify 0))";
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::bool());
    }

    #[test]
    fn infer_occurs_check() {
        // (let f {x} (f f)) — calling f with itself causes a -> b = a, infinite type
        let result = check("(let f {x} (f f))");
        assert!(
            result.is_err(),
            "expected type error for infinite type, got {:?}",
            result
        );
    }

    #[test]
    fn infer_higher_order_function() {
        // apply : (a -> b) -> a -> b applied to double
        let src = r#"
            (let apply {f x} (f x))
            (let double {n} (* 2 n))
            (let main {} (apply double 5))
        "#;
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_chained_field_access() {
        let src = r#"
            (type Point ((:x ~ Int) (:y ~ Int)))
            (let sum_coords {p} (+ (:x p) (:y p)))
            (let main {} (sum_coords (Point 3 4)))
        "#;
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_nullary_variant_in_match() {
        let src = r#"
            (type ['a] Option (None (Some ~ 'a)))
            (let identity {x} x)
            (let main {} (identity None))
        "#;
        let ty = check(src).unwrap();
        match ty.as_ref() {
            Type::Con(name, args) => {
                assert_eq!(name, "Option");
                assert_eq!(args.len(), 1);
                // The type arg should be a type variable
                assert!(matches!(args[0].as_ref(), Type::Var(_)));
            }
            _ => panic!("expected Con(Option, [Var(_)]), got {:?}", ty),
        }
    }

    // -------------------------------------------------------------------------
    // Typecheck acceptance tests — valid programs that must type-check
    // -------------------------------------------------------------------------

    #[test]
    fn infer_self_recursive_without_rec() {
        // Named functions are self-recursive — no `rec` needed
        let src = "(let sum {n} (if (= n 0) 0 (+ n (sum (- n 1)))))\n(let main {} (sum 5))";
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_all_float_operators() {
        assert_eq!(check_expr("(-. 3.0 1.5)").unwrap(), Type::float());
        assert_eq!(check_expr("(*. 2.0 3.0)").unwrap(), Type::float());
        assert_eq!(check_expr("(/. 6.0 2.0)").unwrap(), Type::float());
    }

    #[test]
    fn infer_boolean_or() {
        let ty = check_expr("(or False True)").unwrap();
        assert_eq!(ty, Type::bool());
    }

    #[test]
    fn infer_all_int_comparisons() {
        for src in ["(< 1 2)", "(> 2 1)", "(<= 1 1)", "(>= 2 1)"] {
            let ty = check_expr(src).unwrap();
            assert_eq!(ty, Type::bool(), "failed for: {src}");
        }
    }

    #[test]
    fn infer_inequality_operator() {
        let ty = check_expr("(!= 1 2)").unwrap();
        assert_eq!(ty, Type::bool());
    }

    #[test]
    fn infer_nested_sequential_let_with_deps() {
        // y depends on x from the same binding block
        let src = "(let f {dummy} (let [x 5 y (+ x 3)] y))\n(let main {} (f 0))";
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_let_binding_shadows_outer() {
        // Inner x shadows the outer x — wrapped in a function since local bindings
        // are not valid at the top level
        let src = "(let f {dummy} (let [x 1] (let [x True] x)))\n(let main {} (f 0))";
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::bool());
    }

    #[test]
    fn infer_match_on_bool_literal() {
        let src = "(match True True ~> 1 False ~> 0)";
        let ty = check_expr(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_match_variable_pattern_binds() {
        // Variable pattern in match binds the matched value
        let src = "(match 42 n ~> (+ n 1))";
        let ty = check_expr(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_partial_application() {
        // (let add {a b} ...) gives add : Int -> Int -> Int
        // (add 1) gives Int -> Int
        let src = "(let add {a b} (+ a b))\n(let main {} (add 1))";
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::fun(Type::int(), Type::int()));
    }

    #[test]
    fn infer_function_as_argument() {
        // Pass `not` as a higher-order argument
        let src = "(let apply {f x} (f x))\n(let main {} (apply not True))";
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::bool());
    }

    #[test]
    fn infer_array_of_bools() {
        let ty = check_expr("#[True False True]").unwrap();
        assert_eq!(ty, Type::array(Type::bool()));
    }

    #[test]
    fn infer_array_of_floats() {
        let ty = check_expr("#[1.0 2.5 3.14]").unwrap();
        assert_eq!(ty, Type::array(Type::float()));
    }

    #[test]
    fn infer_variant_in_match_with_binding() {
        let src = r#"
            (type ['a] Option (None (Some ~ 'a)))
            (let unwrap_or {opt default}
              (match opt
                (Some x) ~> x
                None     ~> default))
            (let main {} (unwrap_or (Some 99) 0))
        "#;
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn reject_mutual_recursion_unbound() {
        // Mutual recursion is not currently supported — `odd` is unbound inside `even`'s body
        let src = r#"
            (let even {n} (if (= n 0) True (odd (- n 1))))
            (let odd  {n} (if (= n 0) False (even (- n 1))))
            (let main {} (even 4))
        "#;
        let result = check(src);
        assert!(
            matches!(result, Err(TypeError::UnboundVariable(ref s)) if s == "odd"),
            "expected UnboundVariable(odd), got {:?}",
            result
        );
    }

    // -------------------------------------------------------------------------
    // Typecheck rejection tests — invalid programs that must fail
    // -------------------------------------------------------------------------

    #[test]
    fn reject_apply_non_function() {
        // (1 2) — applying an integer is a type error
        let result = check_expr("(1 2)");
        assert!(result.is_err(), "expected type error, got {:?}", result);
    }

    #[test]
    fn reject_or_with_non_bool() {
        assert!(
            check_expr("(or 1 2)").is_err(),
            "expected error: or requires Bool"
        );
        assert!(
            check_expr("(or True 1)").is_err(),
            "expected error: second arg must be Bool"
        );
    }

    #[test]
    fn reject_and_with_non_bool() {
        assert!(
            check_expr("(and 1 True)").is_err(),
            "expected error: first arg must be Bool"
        );
    }

    #[test]
    fn reject_not_with_non_bool() {
        let result = check_expr("(not 42)");
        assert!(result.is_err(), "expected error: not requires Bool");
    }

    #[test]
    fn reject_equality_on_mismatched_types() {
        // (= 1 True) — polymorphic equality requires both sides same type
        let result = check_expr("(= 1 True)");
        assert!(result.is_err(), "expected type error for (= 1 True)");
    }

    #[test]
    fn reject_float_op_with_int() {
        // (+. 1 2.5) — float arithmetic requires both operands to be Float
        let result = check_expr("(+. 1 2.5)");
        assert!(
            result.is_err(),
            "expected type error: Int used with float op"
        );
    }

    #[test]
    fn reject_int_op_with_float() {
        // (+ 1.0 2.0) — int arithmetic requires Int
        let result = check_expr("(+ 1.0 2.0)");
        assert!(
            result.is_err(),
            "expected type error: Float used with int op"
        );
    }

    #[test]
    fn reject_string_concat_with_int() {
        let result = check_expr(r#"(str 42 "world")"#);
        assert!(
            result.is_err(),
            "expected type error: str requires String args"
        );
    }

    #[test]
    fn reject_int_comparison_with_float() {
        // Comparison operators are Int-only
        let result = check_expr("(< 1.0 2.0)");
        assert!(result.is_err(), "expected type error: < requires Int");
    }

    #[test]
    fn infer_all_float_comparisons() {
        for src in [
            "(<. 1.0 2.0)",
            "(>. 2.0 1.0)",
            "(<=. 1.0 1.0)",
            "(>=. 2.0 1.0)",
        ] {
            let ty = check_expr(src).unwrap();
            assert_eq!(ty, Type::bool(), "failed for: {src}");
        }
    }

    #[test]
    fn reject_float_comparison_with_int() {
        let result = check_expr("(<. 1 2)");
        assert!(result.is_err(), "expected type error: <. requires Float");
    }

    #[test]
    fn reject_match_arms_type_mismatch() {
        // Arms return different types: Int vs Bool
        let result = check_expr("(match True True ~> 1 False ~> False)");
        assert!(
            matches!(result, Err(TypeError::ArmMismatch { arm: 1, .. })),
            "expected ArmMismatch on arm 2, got {result:?}"
        );
    }

    #[test]
    fn reject_let_binding_type_error_in_sequence() {
        // x is Bool, but (+ x 1) requires Int
        let result = check("(let f {dummy} (let [x True y (+ x 1)] y))");
        assert!(
            result.is_err(),
            "expected type error: Bool used in int addition"
        );
    }

    #[test]
    fn reject_if_condition_not_bool() {
        let result = check_expr(r#"(if "hello" 1 2)"#);
        assert!(result.is_err(), "expected type error: String is not Bool");
    }

    #[test]
    fn reject_unbound_in_let_body() {
        // The function `f` uses `unknown` which is not in scope
        let result = check("(let f {x} unknown)");
        assert!(
            matches!(result, Err(TypeError::UnboundVariable(_))),
            "expected UnboundVariable, got {:?}",
            result
        );
    }

    #[test]
    fn reject_field_access_on_non_record() {
        // Accessing :x on an Int literal must fail
        let src = r#"
            (type Point ((:x ~ Int) (:y ~ Int)))
            (let test {} (:x 42))
        "#;
        let result = check(src);
        assert!(result.is_err(), "expected type error: :x applied to Int");
    }

    #[test]
    fn reject_unknown_field_on_record() {
        // :z is not a field of Point — fails during the function's type-check
        let src = r#"
            (type Point ((:x ~ Int) (:y ~ Int)))
            (let get_z {p} (:z p))
        "#;
        let result = check(src);
        assert!(
            matches!(result, Err(TypeError::UnboundVariable(_))),
            "expected UnboundVariable for unknown field :z"
        );
    }

    #[test]
    fn reject_wrong_constructor_arg_type() {
        // (+ s True) — s is Int (from Some 1), True is Bool — type error
        let src = r#"
            (type ['a] Option (None (Some ~ 'a)))
            (let bad_match {x} (match x (Some s) ~> (+ s True) None ~> 0))
        "#;
        let result = check(src);
        assert!(result.is_err(), "expected type error: Bool used with +");
    }
}
