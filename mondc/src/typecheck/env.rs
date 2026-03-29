use std::{collections::HashMap, rc::Rc};

use crate::ast::{TypeDecl, TypeUsage};

use super::{checker::record_accessor_key, *};

/// Build a `TypeEnv` seeded with imported function names as fully polymorphic types.
/// This allows the typechecker to accept calls to functions from other modules
/// without knowing their concrete types.
pub fn import_env(names: &[String]) -> TypeEnv {
    let mut env = TypeEnv::new();
    for (i, name) in names.iter().enumerate() {
        // Use high IDs that never collide with inference vars (which start at 0)
        let id = u64::MAX - 8192 - i as u64;
        env.insert(
            name.clone(),
            Scheme {
                vars: vec![id],
                preds: vec![],
                ty: Rc::new(Type::Var(id)),
            },
        );
    }
    env
}

/// Build a `TypeEnv` with built-in operator and function types.
/// Constructors for user-defined types must be added separately.
pub fn primitive_env() -> TypeEnv {
    let mut env = TypeEnv::new();

    // Helper to build a curried function type: t1 -> t2 -> ret
    let fun2 = |a: Rc<Type>, b: Rc<Type>, ret: Rc<Type>| Scheme {
        vars: vec![],
        preds: vec![],
        ty: Type::fun(a, Type::fun(b, ret)),
    };

    // Arithmetic — Int
    for op in ["+", "-", "*", "/", "%"] {
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
        preds: vec![],
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
            preds: vec![],
            ty: Type::fun(Type::bool(), Type::bool()),
        },
    );
    env
}

// ---------------------------------------------------------------------------
// TypeUsage → Type conversion
// ---------------------------------------------------------------------------

/// Collect all unique `Generic` names from a `TypeSig` in order of appearance.
fn collect_sig_generics(sig: &crate::ast::TypeSig, out: &mut Vec<String>) {
    use crate::ast::TypeSig;
    match sig {
        TypeSig::Named(_) => {}
        TypeSig::Generic(name) => {
            if !out.contains(name) {
                out.push(name.clone());
            }
        }
        TypeSig::App(_, args) => {
            for a in args {
                collect_sig_generics(a, out);
            }
        }
        TypeSig::Fun(a, b) => {
            collect_sig_generics(a, out);
            collect_sig_generics(b, out);
        }
    }
}

fn resolve_type_name(name: &str, aliases: &HashMap<String, String>) -> String {
    aliases
        .get(name)
        .cloned()
        .unwrap_or_else(|| name.to_string())
}

/// Build a `Type` from a `TypeSig`, replacing `Generic` names with `Var` IDs
/// from the provided map.
fn type_sig_with_vars(
    sig: &crate::ast::TypeSig,
    vars: &HashMap<String, u64>,
    aliases: &HashMap<String, String>,
) -> Rc<Type> {
    use crate::ast::TypeSig;
    match sig {
        TypeSig::Named(name) => Type::con(resolve_type_name(name, aliases), vec![]),
        TypeSig::Generic(name) => vars
            .get(name)
            .copied()
            .map(|id| Rc::new(Type::Var(id)))
            .unwrap_or_else(|| Type::con(name, vec![])),
        TypeSig::App(head, args) => Type::con(
            resolve_type_name(head, aliases),
            args.iter()
                .map(|a| type_sig_with_vars(a, vars, aliases))
                .collect(),
        ),
        TypeSig::Fun(a, b) => Type::fun(
            type_sig_with_vars(a, vars, aliases),
            type_sig_with_vars(b, vars, aliases),
        ),
    }
}

/// Convert a `TypeSig` to a properly quantified `Scheme`.
/// Generic type variables (`'k`, `'v`, `'a` etc.) become universally quantified.
pub(super) fn type_sig_to_scheme(
    sig: &crate::ast::TypeSig,
    aliases: &HashMap<String, String>,
) -> Scheme {
    // Use high var IDs to avoid colliding with the TypeChecker's inference counter.
    const EXTERN_VAR_BASE: u64 = u64::MAX - 2048;
    let mut generics: Vec<String> = Vec::new();
    collect_sig_generics(sig, &mut generics);
    let var_map: HashMap<String, u64> = generics
        .iter()
        .enumerate()
        .map(|(i, name)| (name.clone(), EXTERN_VAR_BASE - i as u64))
        .collect();
    let ty = type_sig_with_vars(sig, &var_map, aliases);
    let vars = generics.iter().map(|n| var_map[n]).collect();
    Scheme {
        vars,
        preds: vec![],
        ty,
    }
}

fn type_usage_to_type(
    usage: &TypeUsage,
    params: &HashMap<String, Rc<Type>>,
    aliases: &HashMap<String, String>,
) -> Rc<Type> {
    match usage {
        TypeUsage::Named(name, _) => Type::con(resolve_type_name(name, aliases), vec![]),
        TypeUsage::Generic(name, _) => params
            .get(name)
            .cloned()
            .unwrap_or_else(|| Type::con(name, vec![])),
        TypeUsage::App(head, args, _) => {
            let arg_tys = args
                .iter()
                .map(|a| type_usage_to_type(a, params, aliases))
                .collect();
            Type::con(resolve_type_name(head, aliases), arg_tys)
        }
        TypeUsage::Fun(arg, ret, _) => Type::fun(
            type_usage_to_type(arg, params, aliases),
            type_usage_to_type(ret, params, aliases),
        ),
    }
}

// ---------------------------------------------------------------------------
// Constructor scheme generation from type declarations
// ---------------------------------------------------------------------------

/// Given a type declaration, generate all the scheme entries for the TypeEnv.
/// This includes constructor functions and field accessor functions for records.
pub fn constructor_schemes(decl: &TypeDecl) -> TypeEnv {
    constructor_schemes_with_aliases(decl, &HashMap::new())
}

pub fn constructor_schemes_with_aliases(
    decl: &TypeDecl,
    aliases: &HashMap<String, String>,
) -> TypeEnv {
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
                        let payload_ty = type_usage_to_type(usage, &param_map, aliases);
                        Type::fun(payload_ty, result_ty.clone())
                    }
                };
                env.insert(
                    con_name.clone(),
                    Scheme {
                        vars: scheme_vars.clone(),
                        preds: vec![],
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
                    let ft = type_usage_to_type(field_ty, &param_map, aliases);
                    Type::fun(ft, acc)
                });
            env.insert(
                name.clone(),
                Scheme {
                    vars: scheme_vars.clone(),
                    preds: vec![],
                    ty: ctor_ty,
                },
            );

            // Field accessors: ":field_name" -> result_ty -> field_type
            for (field_name, field_ty) in fields {
                let ft = type_usage_to_type(field_ty, &param_map, aliases);
                let accessor_ty = Type::fun(result_ty.clone(), ft);
                let scheme = Scheme {
                    vars: scheme_vars.clone(),
                    preds: vec![],
                    ty: accessor_ty,
                };
                // Record-qualified accessor key avoids collisions when multiple
                // records share a field name.
                env.insert(record_accessor_key(name, field_name), scheme.clone());
                // Keep the plain accessor key as a fallback for older call paths.
                env.entry(format!(":{field_name}")).or_insert(scheme);
            }
        }
    }

    env
}
