use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
};

use crate::ast::{Expr, Literal, Pattern, TypeDecl, TypeUsage};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    /// Curried function: arg -> ret
    Fun(Rc<Type>, Rc<Type>),
    /// Named type constructor: Int, Bool, Option<a>, Array<Int>, user types
    Con(String, Vec<Rc<Type>>),
    /// Unification variable
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
// Type errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum TypeError {
    OccursCheck { var: u64, ty: Rc<Type> },
    Mismatch { expected: Rc<Type>, found: Rc<Type> },
    UnboundVariable(String),
}

impl std::fmt::Display for TypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TypeError::OccursCheck { var, ty } => {
                write!(f, "occurs check failed: t{var} occurs in {ty:?}")
            }
            TypeError::Mismatch { expected, found } => {
                write!(f, "type mismatch: expected {expected:?}, found {found:?}")
            }
            TypeError::UnboundVariable(name) => write!(f, "unbound variable: {name}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Substitution helpers
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
    // Don't substitute over bound variables
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

/// compose_subst(s_later, s_earlier): apply s_earlier first, then s_later
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
// Free type variables
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

fn free_vars_scheme(scheme: &Scheme) -> HashSet<u64> {
    let bound: HashSet<u64> = scheme.vars.iter().cloned().collect();
    free_vars(&scheme.ty).difference(&bound).cloned().collect()
}

fn free_vars_env(env: &TypeEnv) -> HashSet<u64> {
    env.values().flat_map(free_vars_scheme).collect()
}

// ---------------------------------------------------------------------------
// Generalization and instantiation
// ---------------------------------------------------------------------------

fn generalize(env: &TypeEnv, ty: &Rc<Type>) -> Scheme {
    let env_fv = free_vars_env(env);
    let mut vars: Vec<u64> = free_vars(ty).difference(&env_fv).cloned().collect();
    vars.sort(); // deterministic ordering
    Scheme {
        vars,
        ty: ty.clone(),
    }
}

// ---------------------------------------------------------------------------
// Unification
// ---------------------------------------------------------------------------

fn occurs(id: u64, ty: &Type) -> bool {
    free_vars(ty).contains(&id)
}

pub fn unify(t1: &Rc<Type>, t2: &Rc<Type>) -> Result<Substitution, TypeError> {
    match (t1.as_ref(), t2.as_ref()) {
        _ if t1 == t2 => Ok(HashMap::new()),

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

        (Type::Var(id), _) => {
            if occurs(*id, t2) {
                return Err(TypeError::OccursCheck {
                    var: *id,
                    ty: t2.clone(),
                });
            }
            Ok(HashMap::from([(*id, t2.clone())]))
        }

        (_, Type::Var(id)) => {
            if occurs(*id, t1) {
                return Err(TypeError::OccursCheck {
                    var: *id,
                    ty: t1.clone(),
                });
            }
            Ok(HashMap::from([(*id, t1.clone())]))
        }

        _ => Err(TypeError::Mismatch {
            expected: t1.clone(),
            found: t2.clone(),
        }),
    }
}

// ---------------------------------------------------------------------------
// Type checker (Algorithm W)
// ---------------------------------------------------------------------------

pub struct TypeChecker {
    counter: u64,
    pub errors: Vec<TypeError>,
}

impl TypeChecker {
    pub fn new() -> Self {
        Self {
            counter: 0,
            errors: Vec::new(),
        }
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

    /// Infer the type of `expr` in environment `env`.
    /// Returns `(substitution, type)` on success.
    pub fn infer(
        &mut self,
        env: &TypeEnv,
        expr: &Expr,
    ) -> Result<(Substitution, Rc<Type>), TypeError> {
        match expr {
            // ---------------------------------------------------------------
            // Literals
            // ---------------------------------------------------------------
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

            // ---------------------------------------------------------------
            // Variables — instantiate the polymorphic scheme
            // ---------------------------------------------------------------
            Expr::Variable(name, _) => {
                let scheme = env
                    .get(name)
                    .ok_or_else(|| TypeError::UnboundVariable(name.clone()))?;
                Ok((HashMap::new(), self.instantiate(scheme)))
            }

            // ---------------------------------------------------------------
            // Array literals — all elements must have the same type
            // ---------------------------------------------------------------
            Expr::Array(items, _) => {
                let elem_ty = self.fresh();
                let mut subst: Substitution = HashMap::new();
                for item in items {
                    let (s, t) = self.infer(&apply_subst_env(&subst, env), item)?;
                    let s_unify = unify(&apply_subst(&s, &elem_ty), &t)?;
                    subst = compose_subst(&compose_subst(&s_unify, &s), &subst);
                }
                let array_ty = Type::array(apply_subst(&subst, &elem_ty));
                Ok((subst, array_ty))
            }

            // ---------------------------------------------------------------
            // If — cond : Bool, branches must agree
            // ---------------------------------------------------------------
            Expr::If {
                cond, then, els, ..
            } => {
                let (s1, t_cond) = self.infer(env, cond)?;
                let s_bool = unify(&t_cond, &Type::bool())?;
                let s1 = compose_subst(&s_bool, &s1);

                let (s2, t_then) = self.infer(&apply_subst_env(&s1, env), then)?;
                let s12 = compose_subst(&s2, &s1);

                let (s3, t_els) = self.infer(&apply_subst_env(&s12, env), els)?;
                let s123 = compose_subst(&s3, &s12);

                let s_branches = unify(&apply_subst(&s123, &t_then), &apply_subst(&s123, &t_els))?;
                let s_final = compose_subst(&s_branches, &s123);
                Ok((s_final.clone(), apply_subst(&s_final, &t_then)))
            }

            // ---------------------------------------------------------------
            // Function calls — chain curried application
            // ---------------------------------------------------------------
            Expr::Call { func, args, .. } => {
                let (s0, mut t_func) = self.infer(env, func)?;
                let mut subst = s0;

                for arg in args {
                    let (s_arg, t_arg) = self.infer(&apply_subst_env(&subst, env), arg)?;
                    subst = compose_subst(&s_arg, &subst);
                    t_func = apply_subst(&subst, &t_func);

                    let ret = self.fresh();
                    let expected_fun = Type::fun(t_arg, ret.clone());
                    let s_unify = unify(&t_func, &expected_fun)?;
                    subst = compose_subst(&s_unify, &subst);
                    t_func = apply_subst(&subst, &ret);
                }

                Ok((subst, t_func))
            }

            // ---------------------------------------------------------------
            // Let bindings (variable, function, recursive)
            // ---------------------------------------------------------------
            Expr::Let {
                name,
                is_rec,
                args,
                value,
                body,
                ..
            } => {
                // Fresh type vars for every argument
                let arg_tys: Vec<Rc<Type>> = args.iter().map(|_| self.fresh()).collect();
                // Fresh return type (used as the whole type for non-functions too)
                let ret_ty = self.fresh();

                // Build the curried function type: T_a -> T_b -> ... -> T_ret
                let fun_ty = arg_tys
                    .iter()
                    .rev()
                    .fold(ret_ty.clone(), |acc, a| Type::fun(a.clone(), acc));

                // Extend inner env with argument types
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
                // For recursive functions, add the function itself to the inner env
                if *is_rec && !args.is_empty() {
                    inner_env.insert(
                        name.clone(),
                        Scheme {
                            vars: vec![],
                            ty: fun_ty.clone(),
                        },
                    );
                }

                // Infer the value/function body
                let (s1, t_val) = self.infer(&inner_env, value)?;

                // Unify the expected return type with the actual body type
                let s2 = unify(&apply_subst(&s1, &ret_ty), &t_val)?;
                let s12 = compose_subst(&s2, &s1);

                // The full binding type (possibly a function type)
                let binding_ty = apply_subst(&s12, &fun_ty);

                // Generalize over variables not free in the outer env
                let env_s12 = apply_subst_env(&s12, env);
                let scheme = generalize(&env_s12, &binding_ty);

                // Infer body with the new binding in scope
                let mut body_env = env_s12;
                body_env.insert(name.clone(), scheme);
                let (s3, t_body) = self.infer(&body_env, body)?;

                Ok((compose_subst(&s3, &s12), t_body))
            }

            // ---------------------------------------------------------------
            // Match expressions — all arms must produce the same type
            // ---------------------------------------------------------------
            Expr::Match { target, arms, .. } => {
                let (s0, t_target) = self.infer(env, target)?;
                let mut subst = s0;
                let result_ty = self.fresh();

                for (pat, body) in arms {
                    let current_env = apply_subst_env(&subst, env);
                    let t_target_s = apply_subst(&subst, &t_target);

                    let (s_pat, pat_env) = self.infer_pattern(&current_env, pat, &t_target_s)?;
                    subst = compose_subst(&s_pat, &subst);

                    let (s_body, t_body) = self.infer(&apply_subst_env(&subst, &pat_env), body)?;
                    subst = compose_subst(&s_body, &subst);

                    let s_unify = unify(
                        &apply_subst(&subst, &result_ty),
                        &apply_subst(&subst, &t_body),
                    )?;
                    subst = compose_subst(&s_unify, &subst);
                }

                Ok((subst.clone(), apply_subst(&subst, &result_ty)))
            }

            // ---------------------------------------------------------------
            // Field access — look up ":field_name" accessor in env
            // ---------------------------------------------------------------
            Expr::FieldAccess { field, record, .. } => {
                // Look up the field accessor function, stored as ":field_name"
                let accessor_name = format!(":{field}");
                let scheme = env
                    .get(&accessor_name)
                    .ok_or_else(|| TypeError::UnboundVariable(accessor_name.clone()))?;
                let accessor_ty = self.instantiate(scheme);

                let (s1, t_record) = self.infer(env, record)?;
                let ret_ty = self.fresh();
                let expected = Type::fun(t_record, ret_ty.clone());
                let s2 = unify(&apply_subst(&s1, &accessor_ty), &expected)?;
                let s12 = compose_subst(&s2, &s1);

                Ok((s12.clone(), apply_subst(&s12, &ret_ty)))
            }
        }
    }

    /// Infer bindings introduced by a pattern, unifying against `expected`.
    /// Returns a substitution and the extended env with pattern-bound names.
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
                let ty: Rc<Type> = match lit {
                    Literal::Int(_) => Type::int(),
                    Literal::Float(_) => Type::float(),
                    Literal::Bool(_) => Type::bool(),
                    Literal::String(_) => Type::string(),
                    Literal::Unit => Type::unit(),
                };
                let subst = unify(expected, &ty)?;
                Ok((subst, env.clone()))
            }

            Pattern::Constructor(name, arg_pats, _) => {
                // The constructor must be in scope as a function type
                let scheme = env
                    .get(name)
                    .ok_or_else(|| TypeError::UnboundVariable(name.clone()))?;
                let mut con_ty = self.instantiate(scheme);
                let mut subst: Substitution = HashMap::new();
                let mut pat_env = env.clone();

                // Walk through the curried constructor type, matching sub-patterns
                for arg_pat in arg_pats {
                    match con_ty.as_ref() {
                        Type::Fun(arg_ty, ret_ty) => {
                            let arg_ty_s = apply_subst(&subst, arg_ty);
                            let (s_arg, new_env) =
                                self.infer_pattern(&pat_env, arg_pat, &arg_ty_s)?;
                            subst = compose_subst(&s_arg, &subst);
                            con_ty = apply_subst(&subst, ret_ty);
                            pat_env = new_env;
                        }
                        _ => {
                            return Err(TypeError::Mismatch {
                                expected: Type::fun(self.fresh(), self.fresh()),
                                found: con_ty.clone(),
                            });
                        }
                    }
                }

                // Unify the constructor's result type with the expected pattern type
                let s_unify = unify(&apply_subst(&subst, &con_ty), expected)?;
                subst = compose_subst(&s_unify, &subst);
                Ok((subst, pat_env))
            }
        }
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
        let sexprs = crate::sexpr::SExprParser::new(tokens)
            .parse()
            .expect("parse failed");
        let mut lowerer = crate::lower::Lowerer::new();
        let file_id = lowerer.add_file("test.opal".into(), src.into());
        let decls = lowerer.lower_file(file_id, &sexprs);

        let mut checker = TypeChecker::new();
        let mut env = primitive_env();
        let mut last_ty = Type::unit();

        for decl in &decls {
            match decl {
                crate::ast::Declaration::Type(type_decl) => {
                    env.extend(constructor_schemes(type_decl));
                }
                crate::ast::Declaration::Expression(expr) => {
                    let (s, ty) = checker.infer(&env, expr)?;
                    env = apply_subst_env(&s, &env);
                    last_ty = ty;
                }
            }
        }
        Ok(last_ty)
    }

    #[test]
    fn infer_int_literal() {
        let ty = check("42").unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_bool_literal() {
        let ty = check("True").unwrap();
        assert_eq!(ty, Type::bool());
    }

    #[test]
    fn infer_arithmetic() {
        let ty = check("(+ 1 2)").unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_if() {
        let ty = check("(if True 1 2)").unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_let_function() {
        // Apply the function in the body to observe its type
        let ty = check("(let double {x} (* 2 x) (double 5))").unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_let_binding() {
        let ty = check("(let [x 42] x)").unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_identity_is_polymorphic() {
        // Use identity at two different types in the body to verify polymorphism
        // (let id {x} x (let [a (id 42) b (id True)] a))
        let src = "(let id {x} x (let [a (id 42) b (id True)] a))";
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn unbound_variable_error() {
        let tokens = crate::lexer::Lexer::new("x").lex();
        let sexprs = crate::sexpr::SExprParser::new(tokens).parse().unwrap();
        let mut lowerer = crate::lower::Lowerer::new();
        let file_id = lowerer.add_file("test.opal".into(), "x".into());
        let decls = lowerer.lower_file(file_id, &sexprs);

        let mut checker = TypeChecker::new();
        let env = primitive_env();
        if let crate::ast::Declaration::Expression(expr) = &decls[0] {
            assert!(matches!(
                checker.infer(&env, expr),
                Err(TypeError::UnboundVariable(_))
            ));
        }
    }

    #[test]
    fn type_mismatch_error() {
        // (+ True 1) should fail
        let tokens = crate::lexer::Lexer::new("(+ True 1)").lex();
        let sexprs = crate::sexpr::SExprParser::new(tokens).parse().unwrap();
        let mut lowerer = crate::lower::Lowerer::new();
        let file_id = lowerer.add_file("test.opal".into(), "(+ True 1)".into());
        let decls = lowerer.lower_file(file_id, &sexprs);

        let mut checker = TypeChecker::new();
        let env = primitive_env();
        if let crate::ast::Declaration::Expression(expr) = &decls[0] {
            assert!(matches!(
                checker.infer(&env, expr),
                Err(TypeError::Mismatch { .. })
            ));
        }
    }

    #[test]
    fn infer_option_none() {
        let src = r#"
            (type ['a] Option (None (Some ~ 'a)))
            None
        "#;
        let ty = check(src).unwrap();
        // None : Option<'a> — should be Con("Option", [_])
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
            (Some 42)
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
                (Some x) ~> (+ x n))
              (safe_add (Some 5) 3))
        "#;
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_record_construction() {
        let src = r#"
            (type Point ((:x ~ Int) (:y ~ Int)))
            (let [p (Point 0 0)] (+ 1 2))
        "#;
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_field_access() {
        let src = r#"
            (type Point ((:x ~ Int) (:y ~ Int)))
            (let [p (Point 5 10)] (:x p))
        "#;
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_field_access_generic_record() {
        let src = r#"
            (type ['t] Box ((:value ~ 't)))
            (let [b (Box 42)] (:value b))
        "#;
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_result_type() {
        let src = r#"
            (type ['e 'a] Result ((Ok ~ 'a) (Error ~ 'e)))
            (let [r (Ok 42)] r)
        "#;
        let ty = check(src).unwrap();
        // Should be Con("Result", [_, Con("Int", [])])
        match ty.as_ref() {
            Type::Con(name, args) => {
                assert_eq!(name, "Result");
                assert_eq!(args.len(), 2);
                // The second type arg (index 1) should be Int
                assert_eq!(args[1], Type::int());
            }
            _ => panic!("expected Con(Result, _), got {:?}", ty),
        }
    }

    #[test]
    fn infer_recursive_fib() {
        let src = r#"
            (let rec fib {n}
              (if (= n 0)
                0
                (if (= n 1)
                  1
                  (+ (fib (- n 1)) (fib (- n 2)))))
              (fib 10))
        "#;
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn field_access_wrong_type() {
        let src = r#"
            (type Point ((:x ~ Int) (:y ~ Int)))
            (:x 42)
        "#;
        let result = check(src);
        assert!(result.is_err(), "expected type error, got {:?}", result);
    }

    #[test]
    fn infer_nullary_variant_in_match() {
        let src = r#"
            (type ['a] Option (None (Some ~ 'a)))
            (let [x None] x)
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
}
