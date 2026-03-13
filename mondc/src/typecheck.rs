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
    /// Named type constructor: Int, Bool, Option<'a>, Result<'a, 'e>
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
        Rc::new(Type::Con("List".into(), vec![elem]))
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
pub struct MismatchTypeError {
    expected: Rc<Type>,
    found: Rc<Type>,
    /// Precise source span of the offending sub-expression, if known.
    span: Option<std::ops::Range<usize>>,
    /// Span of an earlier argument that first constrained the expected type, if known.
    prior_span: Option<std::ops::Range<usize>>,
    /// Actual type of the argument at the offending span (may be richer than `found`,
    /// which is a structural sub-component extracted by unification).
    arg_ty: Option<Rc<Type>>,
    /// Full expected argument type at the call site, if known.
    expected_arg_ty: Option<Rc<Type>>,
    /// Name of the function being called, for "X expects Y" context in the error.
    callee_name: Option<String>,
    /// Source span of the callee expression, for a secondary label.
    callee_span: Option<std::ops::Range<usize>>,
    /// Definition site of the callee, if it is a known local/top-level binding.
    callee_def: Option<(usize, std::ops::Range<usize>)>,
    /// Inferred type of the callee at the call site, if known.
    callee_ty: Option<Rc<Type>>,
}

#[derive(Debug, Clone)]
pub enum TypeError {
    OccursCheck {
        var: u64,
        ty: Rc<Type>,
    },
    Mismatch {
        mismatch: Box<MismatchTypeError>,
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
    UnknownField {
        field: String,
        record_ty: Rc<Type>,
        field_span: std::ops::Range<usize>,
        /// The file and span of the type definition, if known
        def: Option<(usize, std::ops::Range<usize>)>,
    },
    NonExhaustiveMatch {
        missing: Vec<String>,
    },
    UnboundVariable(String, std::ops::Range<usize>),
}

impl TypeError {
    /// Render this type error as a codespan `Diagnostic`.
    /// `span` should cover the expression where the error was detected.
    pub fn to_diagnostics(
        &self,
        file_id: usize,
        span: std::ops::Range<usize>,
    ) -> Vec<codespan_reporting::diagnostic::Diagnostic<usize>> {
        use codespan_reporting::diagnostic::{Diagnostic, Label};
        match self {
            TypeError::Mismatch { mismatch } => {
                // Share var_names so the same inference variable prints the same name
                // in both expected and found (e.g. both show `'a` not `?5`/`?6`).
                let mut var_names = std::collections::HashMap::new();
                let expected_s = type_display_inner(&mismatch.expected, &mut var_names);
                let found_s = type_display_inner(&mismatch.found, &mut var_names);

                let is_non_function_call = mismatch.callee_name.is_some()
                    && !matches!(mismatch.expected.as_ref(), Type::Fun(_, _))
                    && matches!(mismatch.found.as_ref(), Type::Fun(_, _));
                if is_non_function_call {
                    let label_span = mismatch.span.clone().unwrap_or(span.clone());
                    let mut labels = vec![
                        Label::primary(file_id, label_span).with_message("argument provided here"),
                    ];
                    if let (Some(name), Some(cs)) = (&mismatch.callee_name, &mismatch.callee_span) {
                        labels.push(
                            Label::secondary(file_id, cs.clone())
                                .with_message(format!("`{name}` has type `{expected_s}`")),
                        );
                    }

                    let mut notes = vec![format!(
                        "`{}` is not a function and cannot be called",
                        mismatch.callee_name.as_deref().unwrap_or("this expression")
                    )];
                    if let Some(name) = &mismatch.callee_name
                        && name
                            .chars()
                            .next()
                            .map(|c| c.is_ascii_uppercase())
                            .unwrap_or(false)
                    {
                        notes.push(format!(
                            "hint: `{name}` is a nullary constructor. Use `{name}` without arguments"
                        ));
                    }

                    return vec![
                        Diagnostic::error()
                            .with_message(format!(
                                "cannot call non-function `{}`",
                                mismatch.callee_name.as_deref().unwrap_or("expression")
                            ))
                            .with_labels(labels)
                            .with_notes(notes),
                    ];
                }

                let mut notes = vec![format!("expected `{expected_s}`, found `{found_s}`")];
                // Helpful hints for common mismatches
                if mismatch.expected == Type::unit()
                    && matches!(mismatch.found.as_ref(), Type::Fun(..))
                {
                    notes.push(
                        "hint: `Unit` is not a function — if you meant to sequence multiple expressions, use `(do expr1 expr2 ...)`".into(),
                    );
                } else if mismatch.expected == Type::float() && mismatch.found == Type::int() {
                    notes.push(
                        "hint: integer literals like `1` have type `Int`; write `1.0` for a `Float`".into(),
                    );
                    notes.push(
                        "hint: float operators use a `.` suffix — `+.` `-.` `*.` `/.` `<.` `>.`"
                            .into(),
                    );
                } else if mismatch.expected == Type::int() && mismatch.found == Type::float() {
                    notes.push(
                        "hint: float literals like `1.0` have type `Float`; write `1` for an `Int`"
                            .into(),
                    );
                    notes.push(
                        "hint: integer operators have no suffix — `+` `-` `*` `/` `<` `>`".into(),
                    );
                }
                let label_span = mismatch.span.clone().unwrap_or(span);
                let primary_msg = if let Some(actual) = &mismatch.arg_ty {
                    format!("this argument has type `{}`", type_display(actual))
                } else {
                    format!("this has type `{found_s}`")
                };
                let expected_here = mismatch
                    .expected_arg_ty
                    .as_ref()
                    .map(|ty| type_display(ty))
                    .unwrap_or_else(|| expected_s.clone());
                let mut labels =
                    vec![Label::primary(file_id, label_span).with_message(primary_msg)];
                if let Some(ps) = &mismatch.prior_span {
                    labels
                        .push(Label::secondary(file_id, ps.clone()).with_message(format!(
                            "`{expected_here}` inferred from this argument"
                        )));
                }
                if let (Some(name), Some(cs)) = (&mismatch.callee_name, &mismatch.callee_span) {
                    labels.push(
                        Label::secondary(file_id, cs.clone())
                            .with_message(format!("`{name}` expects `{expected_here}` here")),
                    );
                }
                if let (Some((def_file_id, def_span)), Some(name), Some(callee_ty)) = (
                    mismatch.callee_def.clone(),
                    mismatch.callee_name.clone(),
                    &mismatch.callee_ty,
                ) {
                    labels.push(
                        Label::secondary(def_file_id, def_span.clone()).with_message(format!(
                            "`{name}` was inferred as `{}`",
                            type_display(callee_ty)
                        )),
                    );
                }
                vec![
                    Diagnostic::error()
                        .with_message(format!(
                            "type mismatch: expected `{expected_s}`, found `{found_s}`"
                        ))
                        .with_labels(labels)
                        .with_notes(notes),
                ]
            }
            TypeError::ConditionNotBool { found } => {
                let found_s = type_display(found);
                vec![
                    Diagnostic::error()
                        .with_message(format!("if condition must be `Bool`, found `{found_s}`"))
                        .with_labels(vec![
                            Label::primary(file_id, span).with_message(format!(
                                "this has type `{found_s}`, expected `Bool`"
                            )),
                        ]),
                ]
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
                vec![
                    Diagnostic::error()
                        .with_message("match arms have incompatible types")
                        .with_labels(vec![
                            Label::primary(file_id, span)
                                .with_message("arms must all return the same type"),
                        ])
                        .with_notes(vec![
                            format!("  arm 1 returns: `{expected_s}`"),
                            format!("  arm {conflicting} returns: `{found_s}`"),
                        ]),
                ]
            }
            TypeError::BranchMismatch { then_ty, else_ty } => {
                let mut var_names = std::collections::HashMap::new();
                let then_s = type_display_inner(then_ty, &mut var_names);
                let else_s = type_display_inner(else_ty, &mut var_names);
                vec![
                    Diagnostic::error()
                        .with_message("if/else branches have incompatible types")
                        .with_labels(vec![
                            Label::primary(file_id, span)
                                .with_message("branches must return the same type"),
                        ])
                        .with_notes(vec![
                            format!("  then branch: `{then_s}`"),
                            format!("  else branch: `{else_s}`"),
                        ]),
                ]
            }
            TypeError::UnknownField {
                field,
                record_ty,
                field_span,
                def,
            } => {
                let ty_s = type_display(record_ty);
                let mut diags = vec![
                    Diagnostic::error()
                        .with_message(format!("`{field}` is not a field of `{ty_s}`"))
                        .with_labels(vec![
                            Label::primary(file_id, field_span.clone())
                                .with_message(format!("`{ty_s}` has no field `{field}`")),
                        ]),
                ];
                if let Some((def_file_id, def_span)) = def {
                    diags.push(
                        Diagnostic::help()
                            .with_message(format!("`{ty_s}` is defined here"))
                            .with_labels(vec![Label::primary(*def_file_id, def_span.clone())]),
                    );
                }
                diags
            }
            TypeError::NonExhaustiveMatch { missing } => vec![
                Diagnostic::error()
                    .with_message("non-exhaustive match")
                    .with_labels(vec![
                        Label::primary(file_id, span)
                            .with_message("match is missing one or more constructors"),
                    ])
                    .with_notes(vec![format!(
                        "missing patterns for: {}",
                        missing.join(", ")
                    )]),
            ],
            TypeError::UnboundVariable(name, precise_span) => vec![
                Diagnostic::error()
                    .with_message(format!("unbound variable `{name}`"))
                    .with_labels(vec![
                        Label::primary(file_id, precise_span.clone())
                            .with_message(format!("`{name}` is not defined in this scope")),
                    ]),
            ],
            TypeError::OccursCheck { ty, .. } => vec![
                Diagnostic::error()
                    .with_message("infinite type")
                    .with_labels(vec![Label::primary(file_id, span).with_message(format!(
                        "this expression would have the infinite type `{}`",
                        type_display(ty)
                    ))])
                    .with_notes(vec![
                        "hint: this usually means a function is being applied to itself".into(),
                    ]),
            ],
        }
    }
}

pub fn type_display(ty: &Type) -> String {
    let mut var_names: std::collections::HashMap<u64, String> = std::collections::HashMap::new();
    type_display_inner(ty, &mut var_names)
}

fn type_display_inner(ty: &Type, var_names: &mut std::collections::HashMap<u64, String>) -> String {
    match ty {
        Type::Con(name, args) if args.is_empty() => name.clone(),
        Type::Con(name, args) => {
            let args_str = args
                .iter()
                .map(|a| {
                    let s = type_display_inner(a, var_names);
                    // Wrap applied types in parens to avoid ambiguity:
                    // `Option Map String Int` would be misread; needs `Option (Map String Int)`
                    let needs_parens = matches!(a.as_ref(), Type::Con(_, inner) if !inner.is_empty())
                        || matches!(a.as_ref(), Type::Fun(..));
                    if needs_parens { format!("({s})") } else { s }
                })
                .collect::<Vec<_>>()
                .join(" ");
            format!("{name} {args_str}")
        }
        Type::Fun(arg, ret) => {
            let arg_s = match arg.as_ref() {
                Type::Fun(..) => format!("({})", type_display_inner(arg, var_names)),
                _ => type_display_inner(arg, var_names),
            };
            let ret_s = type_display_inner(ret, var_names);
            format!("{arg_s} -> {ret_s}")
        }
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
    const GENERALIZED_VAR_BASE: u64 = u64::MAX - 4096;

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
    let renumbering: Substitution = vars
        .iter()
        .enumerate()
        .map(|(i, old)| (*old, Rc::new(Type::Var(GENERALIZED_VAR_BASE - i as u64))))
        .collect();
    let ty = apply_subst(&renumbering, ty);
    let vars = (0..vars.len())
        .map(|i| GENERALIZED_VAR_BASE - i as u64)
        .collect();

    Scheme { vars, ty }
}

fn is_non_expansive(expr: &Expr) -> bool {
    match expr {
        Expr::Literal(_, _) | Expr::Variable(_, _) | Expr::Lambda { .. } => true,
        Expr::List(items, _) => items.iter().all(is_non_expansive),
        Expr::RecordConstruct { fields, .. } => {
            fields.iter().all(|(_, value)| is_non_expansive(value))
        }
        Expr::Call { .. }
        | Expr::QualifiedCall { .. }
        | Expr::If { .. }
        | Expr::Match { .. }
        | Expr::FieldAccess { .. }
        | Expr::LetFunc { .. }
        | Expr::LetLocal { .. } => false,
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
            mismatch: MismatchTypeError {
                expected: t1.clone(),
                found: t2.clone(),
                span: None,
                prior_span: None,
                arg_ty: None,
                expected_arg_ty: None,
                callee_name: None,
                callee_span: None,
                callee_def: None,
                callee_ty: None,
            }
            .into(),
        }),
    }
}

// ---------------------------------------------------------------------------
// TypeChecker Implementation
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct TypeChecker {
    counter: u64,
    /// Maps type name → (file_id, definition span) for error reporting
    type_def_spans: HashMap<String, (usize, std::ops::Range<usize>)>,
    /// Maps variant type name → constructor names for match exhaustiveness checks.
    variant_constructors: HashMap<String, Vec<String>>,
    /// Maps record type name -> field names in declaration order.
    record_fields: HashMap<String, Vec<String>>,
    /// Maps value/function name → (file_id, definition span) for error reporting.
    value_def_spans: HashMap<String, (usize, std::ops::Range<usize>)>,
    /// Best-effort inferred types for expression spans, used by editor tooling.
    expr_types: Vec<(std::ops::Range<usize>, Rc<Type>)>,
}

impl TypeChecker {
    pub fn new() -> Self {
        Self {
            counter: 0,
            type_def_spans: HashMap::new(),
            variant_constructors: HashMap::new(),
            record_fields: HashMap::new(),
            value_def_spans: HashMap::new(),
            expr_types: Vec::new(),
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

    fn record_expr_type(&mut self, span: std::ops::Range<usize>, ty: Rc<Type>) {
        if let Some((_, existing)) = self.expr_types.iter_mut().find(|(seen, _)| *seen == span) {
            *existing = ty;
            return;
        }
        self.expr_types.push((span, ty));
    }

    fn pattern_is_catch_all(pat: &Pattern) -> bool {
        match pat {
            Pattern::Any(_) | Pattern::Variable(_, _) => true,
            Pattern::Or(pats, _) => pats.iter().any(Self::pattern_is_catch_all),
            Pattern::Literal(_, _)
            | Pattern::Constructor(_, _, _)
            | Pattern::EmptyList(_)
            | Pattern::Cons(_, _, _)
            | Pattern::Record { .. } => false,
        }
    }

    fn collect_top_level_constructors<'a>(pat: &'a Pattern, out: &mut HashSet<&'a str>) {
        match pat {
            Pattern::Constructor(name, _, _) => {
                out.insert(name.as_str());
            }
            Pattern::Or(pats, _) => {
                for pat in pats {
                    Self::collect_top_level_constructors(pat, out);
                }
            }
            Pattern::Any(_)
            | Pattern::Variable(_, _)
            | Pattern::Literal(_, _)
            | Pattern::EmptyList(_)
            | Pattern::Cons(_, _, _)
            | Pattern::Record { .. } => {}
        }
    }

    fn pattern_matches_empty_list(pat: &Pattern) -> bool {
        match pat {
            Pattern::EmptyList(_) => true,
            Pattern::Or(pats, _) => pats.iter().any(Self::pattern_matches_empty_list),
            Pattern::Any(_)
            | Pattern::Variable(_, _)
            | Pattern::Literal(_, _)
            | Pattern::Constructor(_, _, _)
            | Pattern::Cons(_, _, _)
            | Pattern::Record { .. } => false,
        }
    }

    fn pattern_matches_non_empty_list(pat: &Pattern) -> bool {
        match pat {
            Pattern::Cons(_, _, _) => true,
            Pattern::Or(pats, _) => pats.iter().any(Self::pattern_matches_non_empty_list),
            Pattern::Any(_)
            | Pattern::Variable(_, _)
            | Pattern::Literal(_, _)
            | Pattern::Constructor(_, _, _)
            | Pattern::EmptyList(_)
            | Pattern::Record { .. } => false,
        }
    }

    fn pattern_matches_bool(pat: &Pattern, expected: bool) -> bool {
        match pat {
            Pattern::Literal(Literal::Bool(value), _) => *value == expected,
            Pattern::Or(pats, _) => pats
                .iter()
                .any(|pat| Self::pattern_matches_bool(pat, expected)),
            Pattern::Any(_)
            | Pattern::Variable(_, _)
            | Pattern::Literal(_, _)
            | Pattern::Constructor(_, _, _)
            | Pattern::EmptyList(_)
            | Pattern::Cons(_, _, _)
            | Pattern::Record { .. } => false,
        }
    }

    fn ensure_match_exhaustive(
        &self,
        subst: &Substitution,
        target_types: &[Rc<Type>],
        arms: &[(Vec<Pattern>, Expr)],
    ) -> Result<(), TypeError> {
        if target_types.len() != 1 {
            return Ok(());
        }

        if arms
            .iter()
            .any(|(pats, _)| pats.first().is_some_and(Self::pattern_is_catch_all))
        {
            return Ok(());
        }

        let resolved_target = apply_subst(subst, &target_types[0]);
        match resolved_target.as_ref() {
            Type::Con(type_name, args) if type_name == "List" && args.len() == 1 => {
                let mut has_empty = false;
                let mut has_cons = false;
                for (pats, _) in arms {
                    if let Some(pat) = pats.first() {
                        has_empty |= Self::pattern_matches_empty_list(pat);
                        has_cons |= Self::pattern_matches_non_empty_list(pat);
                    }
                }
                let mut missing = Vec::new();
                if !has_empty {
                    missing.push("[]".into());
                }
                if !has_cons {
                    missing.push("[head | tail]".into());
                }
                if missing.is_empty() {
                    Ok(())
                } else {
                    Err(TypeError::NonExhaustiveMatch { missing })
                }
            }
            Type::Con(type_name, _) if type_name == "Bool" => {
                let mut seen_true = false;
                let mut seen_false = false;
                for (pats, _) in arms {
                    if let Some(pat) = pats.first() {
                        seen_true |= Self::pattern_matches_bool(pat, true);
                        seen_false |= Self::pattern_matches_bool(pat, false);
                    }
                }
                let mut missing = Vec::new();
                if !seen_true {
                    missing.push("True".into());
                }
                if !seen_false {
                    missing.push("False".into());
                }
                if missing.is_empty() {
                    Ok(())
                } else {
                    Err(TypeError::NonExhaustiveMatch { missing })
                }
            }
            Type::Con(type_name, _) => {
                let Some(constructors) = self.variant_constructors.get(type_name) else {
                    return Ok(());
                };

                let mut covered = HashSet::new();
                for (pats, _) in arms {
                    if let Some(pat) = pats.first() {
                        Self::collect_top_level_constructors(pat, &mut covered);
                    }
                }

                let missing: Vec<String> = constructors
                    .iter()
                    .filter(|name| !covered.contains(name.as_str()))
                    .cloned()
                    .collect();
                if missing.is_empty() {
                    Ok(())
                } else {
                    Err(TypeError::NonExhaustiveMatch { missing })
                }
            }
            Type::Fun(_, _) | Type::Var(_) => Ok(()),
        }
    }

    pub fn inferred_expr_types(&self) -> &[(std::ops::Range<usize>, Rc<Type>)] {
        &self.expr_types
    }

    fn apply_expr_type_subst(&mut self, subst: &Substitution) {
        for (_, ty) in &mut self.expr_types {
            *ty = apply_subst(subst, ty);
        }
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
                self.record_expr_type(expr.span(), ty.clone());
                Ok((HashMap::new(), ty))
            }

            Expr::Variable(name, span) => {
                let scheme = env
                    .get(name)
                    .ok_or_else(|| TypeError::UnboundVariable(name.clone(), span.clone()))?;
                let ty = self.instantiate(scheme);
                self.record_expr_type(expr.span(), ty.clone());
                Ok((HashMap::new(), ty))
            }

            Expr::List(items, _) => {
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
                let ty = Type::array(apply_subst(&subst, &elem_ty));
                self.record_expr_type(expr.span(), ty.clone());
                Ok((subst.clone(), ty))
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
                let ty = apply_subst(&s_res, &t_then);
                self.record_expr_type(expr.span(), ty.clone());
                Ok((s_res.clone(), ty))
            }

            Expr::Call {
                func, args, span, ..
            } => {
                let (callee_name, callee_span, callee_def, callee_ty) =
                    if let Expr::Variable(name, s) = func.as_ref() {
                        (
                            Some(name.clone()),
                            Some(s.clone()),
                            self.value_def_spans.get(name).cloned(),
                            env.get(name).map(|scheme| self.instantiate(scheme)),
                        )
                    } else {
                        (None, None, None, None)
                    };
                let (s0, mut t_func) = self.infer(env, func)?;
                let mut subst = s0;
                let mut prev_span: Option<std::ops::Range<usize>> = None;

                // 0-arg call `(f)` is equivalent to `(f unit)` in codegen.
                // Enforce this in the type checker so calling a non-function is caught.
                if args.is_empty() {
                    let ret = self.fresh();
                    let s_unify = unify(&t_func, &Type::fun(Type::unit(), ret.clone())).map_err(
                        |e| match e {
                            TypeError::Mismatch { mismatch } => TypeError::Mismatch {
                                mismatch: MismatchTypeError {
                                    expected: mismatch.expected,
                                    found: mismatch.found,
                                    span: Some(span.clone()),
                                    prior_span: None,
                                    arg_ty: None,
                                    expected_arg_ty: None,
                                    callee_name: callee_name.clone(),
                                    callee_span: callee_span.clone(),
                                    callee_def: callee_def.clone(),
                                    callee_ty: callee_ty.clone(),
                                }
                                .into(),
                            },
                            other => other,
                        },
                    )?;
                    subst = compose_subst(&s_unify, &subst);
                    let result_ty = apply_subst(&subst, &ret);
                    self.record_expr_type(expr.span(), result_ty.clone());
                    return Ok((subst, result_ty));
                }

                for arg in args {
                    let (s_arg, t_arg) = self.infer(&apply_subst_env(&subst, env), arg)?;
                    subst = compose_subst(&s_arg, &subst);
                    t_func = apply_subst(&subst, &t_func);

                    let ret = self.fresh();
                    let prior = prev_span.clone();
                    let t_arg_for_err = apply_subst(&subst, &t_arg);
                    let expected_arg_for_err = if let Type::Fun(arg_ty, _) = t_func.as_ref() {
                        Some(arg_ty.clone())
                    } else {
                        None
                    };
                    let callee = callee_name.clone();
                    let cs = callee_span.clone();
                    let s_unify =
                        unify(&t_func, &Type::fun(t_arg, ret.clone())).map_err(|e| match e {
                            TypeError::Mismatch { mismatch } => TypeError::Mismatch {
                                mismatch: MismatchTypeError {
                                    expected: mismatch.expected,
                                    found: mismatch.found,
                                    span: Some(arg.span()),
                                    prior_span: prior,
                                    arg_ty: Some(t_arg_for_err),
                                    expected_arg_ty: expected_arg_for_err,
                                    callee_name: callee,
                                    callee_span: cs,
                                    callee_def: callee_def.clone(),
                                    callee_ty: callee_ty.clone(),
                                }
                                .into(),
                            },
                            other => other,
                        })?;
                    subst = compose_subst(&s_unify, &subst);
                    t_func = apply_subst(&subst, &ret);
                    prev_span = Some(arg.span());
                }
                self.record_expr_type(expr.span(), t_func.clone());
                Ok((subst, t_func))
            }

            // Named function definition — always self-recursive in Mond.
            // The function name is added to inner_env before inferring the body,
            // so it can call itself without any special keyword.
            Expr::LetFunc {
                name,
                args,
                arg_spans,
                name_span,
                value,
                ..
            } => {
                let arg_tys: Vec<Rc<Type>> = args.iter().map(|_| self.fresh()).collect();
                let ret_ty = self.fresh();
                // 0-arg functions are compiled as `f(_Unit) -> body` on the BEAM.
                // Type them as `Unit -> ReturnType` so `(f)` calls unify correctly.
                let fun_ty = arg_tys
                    .iter()
                    .rev()
                    .fold(ret_ty.clone(), |acc, a| Type::fun(a.clone(), acc));

                let mut inner_env = env.clone();
                for ((arg, span), ty) in args.iter().zip(arg_spans.iter()).zip(&arg_tys) {
                    inner_env.insert(
                        arg.clone(),
                        Scheme {
                            vars: vec![],
                            ty: ty.clone(),
                        },
                    );
                    self.record_expr_type(span.clone(), ty.clone());
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

                for (span, ty) in arg_spans.iter().zip(arg_tys.iter()) {
                    self.record_expr_type(span.clone(), apply_subst(&s12, ty));
                }

                let binding_ty = apply_subst(&s12, &fun_ty);
                self.record_expr_type(name_span.clone(), binding_ty.clone());
                self.record_expr_type(expr.span(), binding_ty.clone());
                Ok((s12, binding_ty))
            }

            // Sequential local binding — (let [x val] body).
            // The name is NOT in scope during its own value expression.
            Expr::LetLocal {
                name,
                name_span,
                value,
                body,
                ..
            } => {
                let (s1, t_val) = self.infer(env, value)?;
                let ty = apply_subst(&s1, &t_val);
                self.record_expr_type(name_span.clone(), ty.clone());
                let scheme = if is_non_expansive(value) {
                    generalize(&apply_subst_env(&s1, env), &ty)
                } else {
                    Scheme { vars: vec![], ty }
                };

                let mut body_env = apply_subst_env(&s1, env);
                body_env.insert(name.clone(), scheme);
                let (s2, t_body) = self.infer(&body_env, body)?;

                let subst = compose_subst(&s2, &s1);
                let ty = apply_subst(&subst, &t_body);
                self.record_expr_type(expr.span(), ty.clone());
                Ok((subst, ty))
            }

            Expr::Match { targets, arms, .. } => {
                let mut subst = HashMap::new();
                let mut target_types = Vec::new();
                for target in targets {
                    let (s, t) = self.infer(env, target)?;
                    subst = compose_subst(&s, &subst);
                    target_types.push(t);
                }

                let result_ty = self.fresh();

                for (arm_index, (pats, body)) in arms.iter().enumerate() {
                    let mut pat_env = apply_subst_env(&subst, env);
                    for (pat, t_target) in pats.iter().zip(target_types.iter()) {
                        let t_target_s = apply_subst(&subst, t_target);
                        let (s_pat, new_env) = self.infer_pattern(&pat_env, pat, &t_target_s)?;
                        subst = compose_subst(&s_pat, &subst);
                        pat_env = new_env;
                    }

                    // Apply accumulated pattern substitution before inferring the body,
                    // so pattern-bound variables have their concrete types visible.
                    // Without this, `val` in `(Some val) ~> body` would be Var(?n)
                    // rather than the type inferred from the match target.
                    let (s_body, t_body) = self.infer(&apply_subst_env(&subst, &pat_env), body)?;
                    subst = compose_subst(&s_body, &subst);

                    let expected = apply_subst(&subst, &result_ty);
                    let found = apply_subst(&subst, &t_body);
                    let s_unify = unify(&expected, &found).map_err(|_| TypeError::ArmMismatch {
                        arm: arm_index,
                        expected: expected.clone(),
                        found: found.clone(),
                    })?;
                    subst = compose_subst(&s_unify, &subst);

                    for pat in pats {
                        self.record_pattern_binding_types(pat, &subst, &pat_env);
                    }
                }
                self.ensure_match_exhaustive(&subst, &target_types, arms)?;
                let ty = apply_subst(&subst, &result_ty);
                self.record_expr_type(expr.span(), ty.clone());
                Ok((subst.clone(), ty))
            }

            Expr::FieldAccess {
                field,
                record,
                span,
            } => {
                // Infer the record type first so we can name it in the error if the field
                // doesn't exist.
                let (s1, t_record) = self.infer(env, record)?;

                let accessor_name = format!(":{field}");
                let scheme = env.get(&accessor_name).ok_or_else(|| {
                    let resolved = apply_subst(&s1, &t_record);
                    let def = if let Type::Con(type_name, _) = resolved.as_ref() {
                        self.type_def_spans.get(type_name).cloned()
                    } else {
                        None
                    };
                    TypeError::UnknownField {
                        field: field.clone(),
                        record_ty: resolved,
                        field_span: span.clone(),
                        def,
                    }
                })?;
                let accessor_ty = self.instantiate(scheme);

                let ret_ty = self.fresh();
                let s2 = unify(
                    &apply_subst(&s1, &accessor_ty),
                    &Type::fun(t_record, ret_ty.clone()),
                )?;
                let s12 = compose_subst(&s2, &s1);

                let ty = apply_subst(&s12, &ret_ty);
                self.record_expr_type(expr.span(), ty.clone());
                Ok((s12.clone(), ty))
            }

            Expr::Lambda {
                args,
                arg_spans,
                body,
                ..
            } => {
                let mut inner_env = env.clone();
                let mut arg_tys = Vec::new();

                for (arg, span) in args.iter().zip(arg_spans.iter()) {
                    let tv = self.fresh();
                    inner_env.insert(
                        arg.clone(),
                        Scheme {
                            vars: vec![],
                            ty: tv.clone(),
                        },
                    );
                    self.record_expr_type(span.clone(), tv.clone());
                    arg_tys.push(tv);
                }

                let (s, t_body) = self.infer(&inner_env, body)?;

                // Apply substitution to arg types, then build curried Fun type
                let ty = arg_tys
                    .iter()
                    .rev()
                    .fold(apply_subst(&s, &t_body), |acc, arg_ty| {
                        Type::fun(apply_subst(&s, arg_ty), acc)
                    });

                for (span, ty) in arg_spans.iter().zip(arg_tys.iter()) {
                    self.record_expr_type(span.clone(), apply_subst(&s, ty));
                }

                self.record_expr_type(expr.span(), ty.clone());
                Ok((s, ty))
            }

            Expr::RecordConstruct { fields, span, .. } => {
                // For each :field val pair, look up the field accessor in the env.
                // Accessor type is `RecordType -> FieldType`.
                // Unify all accessor record-sides against a single result_ty so the
                // record type is determined by the fields provided.
                let result_ty = self.fresh();
                let mut subst = HashMap::new();

                for (field_name, value_expr) in fields {
                    let accessor_name = format!(":{field_name}");
                    let scheme = env.get(&accessor_name).ok_or_else(|| {
                        TypeError::UnboundVariable(accessor_name.clone(), span.clone())
                    })?;
                    let accessor_ty = self.instantiate(scheme);

                    // accessor_ty = RecordType -> FieldType; unify record-side with result_ty
                    let field_ty = self.fresh();
                    let s_acc = unify(
                        &apply_subst(&subst, &accessor_ty),
                        &Type::fun(apply_subst(&subst, &result_ty), field_ty.clone()),
                    )?;
                    subst = compose_subst(&s_acc, &subst);

                    let (s_val, t_val) = self.infer(&apply_subst_env(&subst, env), value_expr)?;
                    subst = compose_subst(&s_val, &subst);

                    let s_field = unify(
                        &apply_subst(&subst, &field_ty),
                        &apply_subst(&subst, &t_val),
                    )?;
                    subst = compose_subst(&s_field, &subst);
                }

                let ty = apply_subst(&subst, &result_ty);
                self.record_expr_type(expr.span(), ty.clone());
                Ok((subst.clone(), ty))
            }

            // Cross-module call: look up the function's type and check all arguments.
            Expr::QualifiedCall {
                module,
                function,
                args,
                span,
                fn_span,
            } => {
                let key = format!("{module}/{function}");
                let scheme = env
                    .get(&key)
                    .ok_or_else(|| TypeError::UnboundVariable(key.clone(), span.clone()))?;
                let mut t_func = self.instantiate(scheme);
                let mut subst = HashMap::new();

                let callee_name = format!("{module}/{function}");
                let mut prev_span: Option<std::ops::Range<usize>> = None;
                if args.is_empty() {
                    let ret = self.fresh();
                    let s_unify = unify(&t_func, &Type::fun(Type::unit(), ret.clone())).map_err(
                        |e| match e {
                            TypeError::Mismatch { mismatch } => TypeError::Mismatch {
                                mismatch: MismatchTypeError {
                                    expected: mismatch.expected,
                                    found: mismatch.found,
                                    span: Some(span.clone()),
                                    prior_span: None,
                                    arg_ty: None,
                                    expected_arg_ty: None,
                                    callee_name: Some(callee_name.clone()),
                                    callee_span: Some(fn_span.clone()),
                                    callee_def: None,
                                    callee_ty: None,
                                }
                                .into(),
                            },
                            other => other,
                        },
                    )?;
                    subst = compose_subst(&s_unify, &subst);
                    let result_ty = apply_subst(&subst, &ret);
                    self.record_expr_type(expr.span(), result_ty.clone());
                    return Ok((subst, result_ty));
                }
                for arg in args {
                    let (s_arg, t_arg) = self.infer(&apply_subst_env(&subst, env), arg)?;
                    subst = compose_subst(&s_arg, &subst);
                    t_func = apply_subst(&subst, &t_func);

                    let ret = self.fresh();
                    let prior = prev_span.clone();
                    let t_arg_for_err = apply_subst(&subst, &t_arg);
                    let expected_arg_for_err = if let Type::Fun(arg_ty, _) = t_func.as_ref() {
                        Some(arg_ty.clone())
                    } else {
                        None
                    };
                    let s_unify =
                        unify(&t_func, &Type::fun(t_arg, ret.clone())).map_err(|e| match e {
                            TypeError::Mismatch { mismatch } => TypeError::Mismatch {
                                mismatch: MismatchTypeError {
                                    expected: mismatch.expected,
                                    found: mismatch.found,
                                    span: Some(arg.span()),
                                    prior_span: prior,
                                    arg_ty: Some(t_arg_for_err),
                                    expected_arg_ty: expected_arg_for_err,
                                    callee_name: Some(callee_name.clone()),
                                    callee_span: Some(fn_span.clone()),
                                    callee_def: None,
                                    callee_ty: None,
                                }
                                .into(),
                            },
                            other => other,
                        })?;
                    subst = compose_subst(&s_unify, &subst);
                    prev_span = Some(arg.span());
                    t_func = apply_subst(&subst, &ret);
                }

                self.record_expr_type(expr.span(), t_func.clone());
                Ok((subst, t_func))
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
            Pattern::Variable(name, span) => {
                let mut new_env = env.clone();
                new_env.insert(
                    name.clone(),
                    Scheme {
                        vars: vec![],
                        ty: expected.clone(),
                    },
                );
                self.record_expr_type(span.clone(), expected.clone());
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
            Pattern::Constructor(name, arg_pats, span) => {
                let scheme = env
                    .get(name)
                    .ok_or_else(|| TypeError::UnboundVariable(name.clone(), span.clone()))?;
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
                            mismatch: MismatchTypeError {
                                expected: Type::fun(self.fresh(), self.fresh()),
                                found: con_ty.clone(),
                                span: None,
                                prior_span: None,
                                arg_ty: None,
                                expected_arg_ty: None,
                                callee_name: None,
                                callee_span: None,
                                callee_def: None,
                                callee_ty: None,
                            }
                            .into(),
                        });
                    }
                }
                let s_unify = unify(&apply_subst(&subst, &con_ty), expected)?;
                Ok((compose_subst(&s_unify, &subst), pat_env))
            }
            Pattern::EmptyList(_) => {
                let elem_ty = self.fresh();
                let list_ty = Type::array(elem_ty);
                Ok((unify(expected, &list_ty)?, env.clone()))
            }

            Pattern::Cons(head_pat, tail_pat, _) => {
                let elem_ty = self.fresh();
                let list_ty = Type::array(elem_ty.clone());
                let s_list = unify(expected, &list_ty)?;

                let (s_head, head_env) =
                    self.infer_pattern(env, head_pat, &apply_subst(&s_list, &elem_ty))?;
                let s = compose_subst(&s_head, &s_list);

                let (s_tail, tail_env) =
                    self.infer_pattern(&head_env, tail_pat, &apply_subst(&s, &list_ty))?;
                let s = compose_subst(&s_tail, &s);
                Ok((s, tail_env))
            }

            Pattern::Record { name, fields, span } => {
                let declared_fields = self
                    .record_fields
                    .get(name)
                    .cloned()
                    .ok_or_else(|| TypeError::UnboundVariable(name.clone(), span.clone()))?;
                let scheme = env
                    .get(name)
                    .ok_or_else(|| TypeError::UnboundVariable(name.clone(), span.clone()))?;
                let mut record_ty = self.instantiate(scheme);
                while let Type::Fun(_, ret) = record_ty.as_ref() {
                    record_ty = ret.clone();
                }

                let mut subst = unify(expected, &record_ty)?;
                let mut pat_env = env.clone();

                for (field_name, field_pat, field_span) in fields {
                    if !declared_fields
                        .iter()
                        .any(|declared| declared == field_name)
                    {
                        return Err(TypeError::UnknownField {
                            field: field_name.clone(),
                            record_ty: apply_subst(&subst, &record_ty),
                            field_span: field_span.clone(),
                            def: self.type_def_spans.get(name).cloned(),
                        });
                    }

                    let accessor_name = format!(":{field_name}");
                    let accessor_scheme = env.get(&accessor_name).ok_or_else(|| {
                        TypeError::UnboundVariable(accessor_name, field_span.clone())
                    })?;
                    let accessor_ty = self.instantiate(accessor_scheme);
                    let field_ty = self.fresh();
                    let s_acc = unify(
                        &apply_subst(&subst, &accessor_ty),
                        &Type::fun(apply_subst(&subst, &record_ty), field_ty.clone()),
                    )?;
                    subst = compose_subst(&s_acc, &subst);

                    let expected_field_ty = apply_subst(&subst, &field_ty);
                    let (s_field, new_env) =
                        self.infer_pattern(&pat_env, field_pat, &expected_field_ty)?;
                    subst = compose_subst(&s_field, &subst);
                    pat_env = new_env;
                }

                Ok((subst, pat_env))
            }

            Pattern::Or(pats, _) => {
                // Each alternative must type-check against the expected type.
                // Apply accumulated substitution before each check so alternatives
                // are constrained to the same concrete type.
                // Or-patterns don't introduce variable bindings.
                let mut combined_subst = HashMap::new();
                for pat in pats {
                    let refined = apply_subst(&combined_subst, expected);
                    let (s, _) = self.infer_pattern(env, pat, &refined)?;
                    combined_subst = compose_subst(&s, &combined_subst);
                }
                Ok((combined_subst, env.clone()))
            }
        }
    }

    fn record_pattern_binding_types(&mut self, pat: &Pattern, subst: &Substitution, env: &TypeEnv) {
        match pat {
            Pattern::Variable(name, span) => {
                if let Some(scheme) = env.get(name) {
                    self.record_expr_type(span.clone(), apply_subst(subst, &scheme.ty));
                }
            }
            Pattern::Constructor(_, args, _) | Pattern::Or(args, _) => {
                for arg in args {
                    self.record_pattern_binding_types(arg, subst, env);
                }
            }
            Pattern::Cons(head, tail, _) => {
                self.record_pattern_binding_types(head, subst, env);
                self.record_pattern_binding_types(tail, subst, env);
            }
            Pattern::Record { fields, .. } => {
                for (_, pat, _) in fields {
                    self.record_pattern_binding_types(pat, subst, env);
                }
            }
            Pattern::Any(_) | Pattern::Literal(_, _) | Pattern::EmptyList(_) => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Program-level type checking
// ---------------------------------------------------------------------------

impl TypeChecker {
    /// Type-check a sequence of declarations in order, threading the environment.
    ///
    /// Returns `Ok(last_type)` if all declarations pass, or `Err((error, expr))` on
    /// the first failure, where `expr` is the expression that caused the error.
    pub fn check_program(
        &mut self,
        env: &mut TypeEnv,
        decls: &[crate::ast::Declaration],
        file_id: usize,
    ) -> Result<Rc<Type>, Box<(TypeError, crate::ast::Expr)>> {
        use crate::ast::{Declaration, Expr};

        let mut last_ty = Type::unit();

        for decl in decls {
            match decl {
                Declaration::Type(type_decl) => {
                    let (type_name, type_span) = match type_decl {
                        crate::ast::TypeDecl::Record { name, span, .. } => (name, span),
                        crate::ast::TypeDecl::Variant { name, span, .. } => (name, span),
                    };
                    self.type_def_spans
                        .insert(type_name.clone(), (file_id, type_span.clone()));
                    if let crate::ast::TypeDecl::Variant {
                        name, constructors, ..
                    } = type_decl
                    {
                        self.variant_constructors.insert(
                            name.clone(),
                            constructors.iter().map(|(name, _)| name.clone()).collect(),
                        );
                    } else if let crate::ast::TypeDecl::Record { name, fields, .. } = type_decl {
                        self.record_fields.insert(
                            name.clone(),
                            fields.iter().map(|(field, _)| field.clone()).collect(),
                        );
                    }
                    env.extend(constructor_schemes(type_decl));
                }
                Declaration::Expression(expr) => {
                    match self.infer(env, expr) {
                        Ok((s, ty)) => {
                            self.apply_expr_type_subst(&s);
                            *env = apply_subst_env(&s, env);
                            // Named top-level functions must be available to subsequent declarations.
                            // 0-arg functions (let f {} body) are compiled as f(_Unit) -> body on
                            // the BEAM, so store them as Unit -> ReturnType in the env so that
                            // 0-arg call sites `(f)` unify correctly.
                            if let Expr::LetFunc { name, args, .. } = expr {
                                self.value_def_spans
                                    .insert(name.clone(), (file_id, expr.span()));
                                let env_ty = if args.is_empty() {
                                    Type::fun(Type::unit(), ty.clone())
                                } else {
                                    ty.clone()
                                };
                                let scheme = generalize(env, &env_ty);
                                env.insert(name.clone(), scheme);
                            }
                            last_ty = ty;
                        }
                        Err(error) => return Err(Box::new((error, expr.clone()))),
                    }
                }
                Declaration::ExternLet {
                    name,
                    name_span,
                    is_nullary,
                    ty,
                    ..
                } => {
                    let mut scheme = type_sig_to_scheme(ty);
                    if *is_nullary {
                        // {} means 0-arity Erlang function: wrap as Unit -> ReturnType
                        scheme.ty = Rc::new(Type::Fun(Type::unit(), scheme.ty));
                    }
                    self.record_expr_type(name_span.clone(), scheme.ty.clone());
                    env.insert(name.clone(), scheme);
                }
                Declaration::ExternType { .. } | Declaration::Use { .. } => {
                    // Not yet handled by the typechecker.
                }
                Declaration::Test { name, body, span } => {
                    // Result is ['a 'e] — ok first, error second
                    // test bodies must return Result Unit String (Ok=Unit, Error=String)
                    let expected = Rc::new(Type::Con(
                        "Result".into(),
                        vec![Type::unit(), Type::string()],
                    ));
                    match self.infer(env, body) {
                        Ok((s, ty)) => {
                            self.apply_expr_type_subst(&s);
                            *env = apply_subst_env(&s, env);
                            let ty = apply_subst(&s, &ty);
                            if let Err(e) = unify(&ty, &expected) {
                                return Err(Box::new((e, *body.clone())));
                            }
                            last_ty = ty;
                        }
                        Err(error) => return Err(Box::new((error, *body.clone()))),
                    }
                    let _ = (name, span); // used for discovery by mond, not the typechecker
                }
            }
        }

        Ok(last_ty)
    }
}

// ---------------------------------------------------------------------------
// Primitive environment
// ---------------------------------------------------------------------------

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

/// Build a `Type` from a `TypeSig`, replacing `Generic` names with `Var` IDs
/// from the provided map.
fn type_sig_with_vars(sig: &crate::ast::TypeSig, vars: &HashMap<String, u64>) -> Rc<Type> {
    use crate::ast::TypeSig;
    match sig {
        TypeSig::Named(name) => Type::con(name, vec![]),
        TypeSig::Generic(name) => vars
            .get(name)
            .copied()
            .map(|id| Rc::new(Type::Var(id)))
            .unwrap_or_else(|| Type::con(name, vec![])),
        TypeSig::App(head, args) => Type::con(
            head,
            args.iter().map(|a| type_sig_with_vars(a, vars)).collect(),
        ),
        TypeSig::Fun(a, b) => Type::fun(type_sig_with_vars(a, vars), type_sig_with_vars(b, vars)),
    }
}

/// Convert a `TypeSig` to a properly quantified `Scheme`.
/// Generic type variables (`'k`, `'v`, `'a` etc.) become universally quantified.
fn type_sig_to_scheme(sig: &crate::ast::TypeSig) -> Scheme {
    // Use high var IDs to avoid colliding with the TypeChecker's inference counter.
    const EXTERN_VAR_BASE: u64 = u64::MAX - 2048;
    let mut generics: Vec<String> = Vec::new();
    collect_sig_generics(sig, &mut generics);
    let var_map: HashMap<String, u64> = generics
        .iter()
        .enumerate()
        .map(|(i, name)| (name.clone(), EXTERN_VAR_BASE - i as u64))
        .collect();
    let ty = type_sig_with_vars(sig, &var_map);
    let vars = generics.iter().map(|n| var_map[n]).collect();
    Scheme { vars, ty }
}

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

    fn check_with_env(src: &str, extra_env: TypeEnv) -> Result<Rc<Type>, TypeError> {
        let tokens = crate::lexer::Lexer::new(src).lex();
        let mut lowerer = crate::lower::Lowerer::new();

        let file_id = lowerer.add_file("test.mond".into(), src.into());

        let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
            .parse()
            .expect("parse failed");
        let decls = lowerer.lower_file(file_id, &sexprs);

        let mut checker = TypeChecker::new();
        let mut env = primitive_env();
        env.extend(extra_env);

        checker
            .check_program(&mut env, &decls, file_id)
            .map_err(|err| err.0)
    }

    fn check(src: &str) -> Result<Rc<Type>, TypeError> {
        let tokens = crate::lexer::Lexer::new(src).lex();
        let mut lowerer = crate::lower::Lowerer::new();

        let file_id = lowerer.add_file("test.mond".into(), src.into());

        let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
            .parse()
            .expect("parse failed");
        let decls = lowerer.lower_file(file_id, &sexprs);

        let mut checker = TypeChecker::new();
        let mut env = primitive_env();

        checker
            .check_program(&mut env, &decls, file_id)
            .map_err(|err| err.0)
    }

    fn check_expr(src: &str) -> Result<Rc<Type>, TypeError> {
        let tokens = crate::lexer::Lexer::new(src).lex();
        let mut lowerer = crate::lower::Lowerer::new();
        let file_id = lowerer.add_file("test.mond".into(), src.into());
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
    fn type_display_renders_curried_functions_without_extra_parens() {
        let ty = Type::fun(Type::int(), Type::fun(Type::int(), Type::int()));
        assert_eq!(type_display(&ty), "Int -> Int -> Int");
    }

    #[test]
    fn type_display_keeps_parens_for_function_arguments() {
        let ty = Type::fun(Type::fun(Type::int(), Type::int()), Type::int());
        assert_eq!(type_display(&ty), "(Int -> Int) -> Int");
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
        let ty = check("(let helper {dummy} (let [x 42] x))\n(let main {} (helper 0))").unwrap();
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
    fn infer_lambda_binding_is_polymorphic_under_value_restriction() {
        let src = "(let get_id {dummy} (let [id (f {x} -> x) a (id 42) b (id True)] a))\n(let main {} (get_id 0))";
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_constructor_binding_is_polymorphic_under_value_restriction() {
        let src = "(type ['a] Option [None (Some ~ 'a)])\n(let get_none {dummy} (let [none None a none b none] a))\n(let main {} (get_none 0))";
        let ty = check(src).unwrap();
        match ty.as_ref() {
            Type::Con(name, args) => {
                assert_eq!(name, "Option");
                assert_eq!(args.len(), 1);
            }
            _ => panic!("expected Con(Option, _), got {:?}", ty),
        }
    }

    #[test]
    fn infer_expansive_process_subject_binding_is_monomorphic() {
        let mut env = TypeEnv::new();
        let subject_var = 10_000;
        env.insert(
            "process/new_subject".into(),
            Scheme {
                vars: vec![subject_var],
                ty: Type::con("Subject", vec![Rc::new(Type::Var(subject_var))]),
            },
        );
        env.insert(
            "process/send".into(),
            Scheme {
                vars: vec![subject_var],
                ty: Type::fun(
                    Type::con("Subject", vec![Rc::new(Type::Var(subject_var))]),
                    Type::fun(
                        Rc::new(Type::Var(subject_var)),
                        Rc::new(Type::Var(subject_var)),
                    ),
                ),
            },
        );

        let src = "(let main {} (let [subject process/new_subject a (process/send subject \"hello\") b (process/send subject 10)] a))";
        let err = check_with_env(src, env).expect_err("expected monomorphic subject binding");
        assert!(matches!(err, TypeError::Mismatch { .. }));
    }

    #[test]
    fn unbound_variable_error() {
        let tokens = crate::lexer::Lexer::new("x").lex();

        let mut lowerer = crate::lower::Lowerer::new();
        let file_id = lowerer.add_file("test.mond".into(), "x".into());
        let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
            .parse()
            .unwrap();
        let expr = lowerer.lower_expr(file_id, &sexprs[0]).unwrap();

        let mut checker = TypeChecker::new();
        let env = primitive_env();
        assert!(matches!(
            checker.infer(&env, &expr),
            Err(TypeError::UnboundVariable(_, _))
        ));
    }

    #[test]
    fn type_mismatch_error() {
        // (+ True 1) should fail
        let tokens = crate::lexer::Lexer::new("(+ True 1)").lex();

        let mut lowerer = crate::lower::Lowerer::new();
        let file_id = lowerer.add_file("test.mond".into(), "(+ True 1)".into());
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
    fn nullary_constructor_call_has_helpful_diagnostic() {
        let src = "(type ['a] Option [None (Some ~ 'a)])\n(let always_none {x} (None x))";
        let tokens = crate::lexer::Lexer::new(src).lex();
        let mut lowerer = crate::lower::Lowerer::new();
        let file_id = lowerer.add_file("test.mond".into(), src.into());
        let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
            .parse()
            .expect("parse failed");
        let decls = lowerer.lower_file(file_id, &sexprs);

        let mut checker = TypeChecker::new();
        let mut env = primitive_env();
        let err = checker
            .check_program(&mut env, &decls, file_id)
            .expect_err("expected type error");
        let (type_err, expr) = *err;
        let diags = type_err.to_diagnostics(file_id, expr.span());

        assert!(
            diags[0].message.contains("cannot call non-function `None`"),
            "unexpected message: {}",
            diags[0].message
        );
        assert!(
            diags[0]
                .notes
                .iter()
                .any(|n| n.contains("nullary constructor")),
            "expected nullary constructor hint in notes: {:?}",
            diags[0].notes
        );
    }

    #[test]
    fn call_mismatch_mentions_inferred_callee_type() {
        let src = r#"
            (type ['a 'e] Result [(Ok ~ 'a) (Error ~ 'e)])
            (extern let println ~ (String -> Unit) io/format)
            (let match_and_print {val}
              (match val
                (Ok x) ~> (println x)
                (Error err) ~> (println err)))
            (let main {}
              (let [good (Ok "hello")
                    bad  (Error ())]
                (do (match_and_print good)
                    (match_and_print bad))))
        "#;
        let tokens = crate::lexer::Lexer::new(src).lex();
        let mut lowerer = crate::lower::Lowerer::new();
        let file_id = lowerer.add_file("test.mond".into(), src.into());
        let sexprs = crate::sexpr::SExprParser::new(tokens, file_id)
            .parse()
            .expect("parse failed");
        let decls = lowerer.lower_file(file_id, &sexprs);

        let mut checker = TypeChecker::new();
        let mut env = primitive_env();
        let err = checker
            .check_program(&mut env, &decls, file_id)
            .expect_err("expected type error");
        let (type_err, expr) = *err;
        let diags = type_err.to_diagnostics(file_id, expr.span());

        assert!(
            diags[0]
                .labels
                .iter()
                .any(|label| label.message.contains("`match_and_print` was inferred as")),
            "expected inferred callee type label, got labels: {:?}",
            diags[0].labels
        );
        assert!(
            diags[0].labels.iter().any(|label| label
                .message
                .contains("`match_and_print` expects `Result String String` here")),
            "expected full argument type label, got labels: {:?}",
            diags[0].labels
        );
    }

    #[test]
    fn infer_option_none() {
        let src = r#"
            (type ['a] Option [None (Some ~ 'a)])
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
            (type ['a] Option [None (Some ~ 'a)])
            (let get_some {} (Some 42))
        "#;
        let ty = check(src).unwrap();
        // Some 42 : Option<Int>
        assert_eq!(ty, Type::con("Option", vec![Type::int()]));
    }

    #[test]
    fn infer_match_option() {
        let src = r#"
            (type ['a] Option [None (Some ~ 'a)])
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
            (type Point [(:x ~ Int) (:y ~ Int)])
            (let main {} (Point :x 0 :y 0))
        "#;
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::con("Point", vec![]));
    }

    #[test]
    fn infer_generic_record_construction() {
        let src = r#"
            (type ['t] Box [(:value ~ 't)])
            (let main {} (Box :value 42))
        "#;
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::con("Box", vec![Type::int()]));
    }

    #[test]
    fn infer_record_construction_field_type_error() {
        let src = r#"
            (type Point [(:x ~ Int) (:y ~ Int)])
            (let main {} (Point :x True :y 0))
        "#;
        assert!(
            check(src).is_err(),
            "expected type error: Bool used for Int field"
        );
    }

    #[test]
    fn infer_lambda_identity() {
        let ty = check_expr("(f {x} -> x)").unwrap();
        // Should be 'a -> 'a
        match ty.as_ref() {
            Type::Fun(a, b) => assert_eq!(a, b),
            _ => panic!("expected Fun type, got {ty:?}"),
        }
    }

    #[test]
    fn infer_lambda_applied() {
        // Immediately apply a lambda
        let ty = check_expr("((f {x} -> (+ x 1)) 5)").unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_lambda_as_arg() {
        let src = r#"
            (let apply {func x} (func x))
            (let main {} (apply (f {n} -> (* n 2)) 3))
        "#;
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_let_bind() {
        // let? desugars to bind; define a Result bind and use it
        let src = r#"
            (type ['a 'e] Result [
                (Ok ~ 'a)
                (Error ~ 'e)])
            (let bind {m func}
                (match m
                    (Ok x) ~> (func x)
                    (Error e) ~> (Error e)))
            (let safe_inc {r}
                (let? [x r]
                    (Ok (+ x 1))))
            (let main {} (safe_inc (Ok 41)))
        "#;
        let ty = check(src).unwrap();
        // Error type is polymorphic (never constrained); only check the success type
        match ty.as_ref() {
            Type::Con(name, args) if name == "Result" => assert_eq!(args[0], Type::int()),
            _ => panic!("expected Result type, got {ty:?}"),
        }
    }

    #[test]
    fn infer_field_access() {
        let src = r#"
            (type Point [(:x ~ Int) (:y ~ Int)])
            (let get_x {p} (:x p))
            (let main {} (get_x (Point :x 5 :y 10)))
        "#;
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_field_access_generic_record() {
        let src = r#"
            (type ['t] Box [(:value ~ 't)])
            (let get_val {b} (:value b))
            (let main {} (get_val (Box :value 42)))
        "#;
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_result_type() {
        let src = r#"
            (type ['a 'e] Result [(Ok ~ 'a) (Error ~ 'e)])
            (let identity {r} r)
            (let main {} (identity (Ok 42)))
        "#;
        let ty = check(src).unwrap();
        // Should be Con("Result", [Con("Int", []), _])
        match ty.as_ref() {
            Type::Con(name, args) => {
                assert_eq!(name, "Result");
                assert_eq!(args.len(), 2);
                assert_eq!(args[0], Type::int());
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
            (type Point [(:x ~ Int) (:y ~ Int)])
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
    fn infer_list_of_ints() {
        let ty = check_expr("[1 2 3]").unwrap();
        assert_eq!(ty, Type::array(Type::int()));
    }

    #[test]
    fn infer_empty_list() {
        let ty = check_expr("[]").unwrap();
        // empty list has a polymorphic element type var
        match ty.as_ref() {
            Type::Con(name, args) => {
                assert_eq!(name, "List");
                assert_eq!(args.len(), 1);
                assert!(matches!(args[0].as_ref(), Type::Var(_)));
            }
            _ => panic!("expected List type, got {:?}", ty),
        }
    }

    #[test]
    fn infer_list_type_mismatch() {
        // Int and Bool in the same list must fail
        let result = check_expr("[1 True]");
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
            (type ['a] Option [None (Some ~ 'a)])
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
        // (let recur {x} (recur recur)) — calling recur with itself causes a -> b = a, infinite type
        let result = check("(let recur {x} (recur recur))");
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
            (let apply {func x} (func x))
            (let double {n} (* 2 n))
            (let main {} (apply double 5))
        "#;
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_chained_field_access() {
        let src = r#"
            (type Point [(:x ~ Int) (:y ~ Int)])
            (let sum_coords {p} (+ (:x p) (:y p)))
            (let main {} (sum_coords (Point 3 4)))
        "#;
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_nullary_variant_in_match() {
        let src = r#"
            (type ['a] Option [None (Some ~ 'a)])
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
        let src = "(let helper {dummy} (let [x 5 y (+ x 3)] y))\n(let main {} (helper 0))";
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_let_binding_shadows_outer() {
        // Inner x shadows the outer x — wrapped in a function since local bindings
        // are not valid at the top level
        let src = "(let helper {dummy} (let [x 1] (let [x True] x)))\n(let main {} (helper 0))";
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
        let src = "(let apply {func x} (func x))\n(let main {} (apply not True))";
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::bool());
    }

    #[test]
    fn infer_list_of_bools() {
        let ty = check_expr("[True False True]").unwrap();
        assert_eq!(ty, Type::array(Type::bool()));
    }

    #[test]
    fn infer_list_of_floats() {
        let ty = check_expr("[1.0 2.5 3.14]").unwrap();
        assert_eq!(ty, Type::array(Type::float()));
    }

    #[test]
    fn infer_variant_in_match_with_binding() {
        let src = r#"
            (type ['a] Option [None (Some ~ 'a)])
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
    fn infer_record_pattern_with_partial_destructuring() {
        let src = r#"
            (type Person [(:name ~ String) (:age ~ Int)])
            (let age_of {person}
              (match person
                (Person :age age) ~> age))
            (let main {} (age_of (Person "Ada" 37)))
        "#;
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_nested_record_pattern() {
        let src = r#"
            (type Address [(:city ~ String)])
            (type Person [(:name ~ String) (:address ~ Address)])
            (let city_of {person}
              (match person
                (Person :address (Address :city city)) ~> city))
            (let main {} (city_of (Person "Ada" (Address "London"))))
        "#;
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::string());
    }

    #[test]
    fn reject_unknown_field_in_record_pattern() {
        let src = r#"
            (type Person [(:name ~ String) (:age ~ Int)])
            (let age_of {person}
              (match person
                (Person :height h) ~> h))
        "#;
        let err = check(src).expect_err("expected unknown field error");
        match err {
            TypeError::UnknownField { field, .. } => assert_eq!(field, "height"),
            other => panic!("expected UnknownField, got {other:?}"),
        }
    }

    #[test]
    fn reject_non_exhaustive_variant_match() {
        let src = r#"
            (type LotsOVariants [One Two (Three ~ Int) Four Five (Six ~ String)])
            (let main {}
              (let [x One]
                (match x
                  One ~> ()
                  Two ~> ())))
        "#;
        let err = check(src).expect_err("expected non-exhaustive match");
        match err {
            TypeError::NonExhaustiveMatch { missing } => {
                assert_eq!(missing, vec!["Three", "Four", "Five", "Six"]);
            }
            other => panic!("expected NonExhaustiveMatch, got {other:?}"),
        }
    }

    #[test]
    fn accept_exhaustive_variant_match_with_or_pattern() {
        let src = r#"
            (type TrafficLight [Red Yellow Green])
            (let to_int {light}
              (match light
                Red or Yellow ~> 0
                Green ~> 1))
            (let main {} (to_int Red))
        "#;
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn reject_non_exhaustive_list_match_missing_empty() {
        let src = r#"
            (let classify {xs}
              (match xs
                [h | t] ~> 1))
            (let main {} (classify [1]))
        "#;
        let err = check(src).expect_err("expected non-exhaustive match");
        match err {
            TypeError::NonExhaustiveMatch { missing } => {
                assert_eq!(missing, vec!["[]"]);
            }
            other => panic!("expected NonExhaustiveMatch, got {other:?}"),
        }
    }

    #[test]
    fn reject_non_exhaustive_list_match_missing_cons() {
        let src = r#"
            (let classify {xs}
              (match xs
                [] ~> 0))
            (let main {} (classify []))
        "#;
        let err = check(src).expect_err("expected non-exhaustive match");
        match err {
            TypeError::NonExhaustiveMatch { missing } => {
                assert_eq!(missing, vec!["[head | tail]"]);
            }
            other => panic!("expected NonExhaustiveMatch, got {other:?}"),
        }
    }

    #[test]
    fn accept_exhaustive_list_match() {
        let src = r#"
            (let classify {xs}
              (match xs
                [] ~> 0
                [h | t] ~> 1))
            (let main {} (classify []))
        "#;
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn reject_non_exhaustive_bool_match() {
        let err = check_expr("(match True True ~> 1)").expect_err("expected non-exhaustive match");
        match err {
            TypeError::NonExhaustiveMatch { missing } => {
                assert_eq!(missing, vec!["False"]);
            }
            other => panic!("expected NonExhaustiveMatch, got {other:?}"),
        }
    }

    #[test]
    fn accept_exhaustive_bool_match_with_or_pattern() {
        let ty = check_expr("(match True True or False ~> 1)").unwrap();
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
            matches!(result, Err(TypeError::UnboundVariable(ref s, _)) if s == "odd"),
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
    fn extern_declaration_adds_to_env() {
        // extern makes the name available with the declared type
        let src = r#"
            (extern let my_print ~ (String -> Unit) io/format)
            (let main {} (my_print "hello"))
        "#;
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::unit());
    }

    #[test]
    fn extern_wrong_arg_type_is_rejected() {
        let src = r#"
            (extern let my_print ~ (String -> Unit) io/format)
            (let main {} (my_print 42))
        "#;
        assert!(check(src).is_err());
    }

    #[test]
    fn infer_string_literal_pattern() {
        let src = r#"(let greet {s} (match s "hello" ~> "hi!" _ ~> "?"))
(let main {} (greet "hello"))"#;
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::string());
    }

    #[test]
    fn infer_or_pattern_match() {
        // All alternatives are Int literals; result is Bool
        let src = "(let pred {x} (match x 1 or 2 or 3 ~> True _ ~> False))\n(let main {} (pred 1))";
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::bool());
    }

    #[test]
    fn reject_or_pattern_type_mismatch() {
        // or-pattern alternatives must agree with the target type; 1 is Int, True is Bool
        let result =
            check("(let pred {x} (match x 1 or True ~> 0 _ ~> 1))\n(let main {} (pred 1))");
        assert!(
            result.is_err(),
            "expected type error: Int vs Bool in or-pattern"
        );
    }

    #[test]
    fn infer_multi_target_match() {
        // Two targets, two patterns per arm; both arms return Bool
        let src = "(let both {x y} (match x y 1 1 ~> True _ _ ~> False))\n(let main {} (both 1 1))";
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::bool());
    }

    #[test]
    fn infer_empty_list_pattern() {
        let src =
            "(let classify {lst} (match lst [] ~> 0 [h | _] ~> 1))\n(let main {} (classify []))";
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_cons_pattern_binds_head() {
        // h is bound to the head element — must be Int since the list is Int list
        let src = "(let head_or_zero {lst} (match lst [] ~> 0 [h | _] ~> h))\n(let main {} (head_or_zero [1 2 3]))";
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_cons_pattern_binds_tail() {
        // t is bound to the tail — must be List Int
        let src = "(let tail_or_self {lst} (match lst [] ~> lst [_ | t] ~> t))\n(let main {} (tail_or_self [1 2]))";
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::array(Type::int()));
    }

    #[test]
    fn infer_recursive_list_function() {
        // count elements using [] and [h | t] — classic recursive list function
        let src = r#"
            (let count {lst acc}
              (match lst
                [] ~> acc
                [_ | t] ~> (count t (+ acc 1))))
            (let main {} (count [1 2 3] 0))
        "#;
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn infer_list_pattern_wildcard_tail() {
        // `_` is valid as the tail pattern
        let src = "(let first {lst} (match lst [] ~> 0 [h | _] ~> h))\n(let main {} (first [5]))";
        let ty = check(src).unwrap();
        assert_eq!(ty, Type::int());
    }

    #[test]
    fn reject_cons_pattern_wrong_element_type() {
        // list is List Int but arm body uses h as Bool — should fail
        let src = r#"
            (let only_bool_list {lst}
              (match lst
                [] ~> False
                [h | _] ~> (= h True)))
            (let main {} (only_bool_list [1 2]))
        "#;
        assert!(
            check(src).is_err(),
            "expected type error: Bool vs Int in cons pattern"
        );
    }

    #[test]
    fn reject_let_binding_type_error_in_sequence() {
        // x is Bool, but (+ x 1) requires Int
        let result = check("(let helper {dummy} (let [x True y (+ x 1)] y))");
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
        // The function `helper` uses `unknown` which is not in scope
        let result = check("(let helper {x} unknown)");
        assert!(
            matches!(result, Err(TypeError::UnboundVariable(_, _))),
            "expected UnboundVariable, got {:?}",
            result
        );
    }

    #[test]
    fn reject_field_access_on_non_record() {
        // Accessing :x on an Int literal must fail
        let src = r#"
            (type Point [(:x ~ Int) (:y ~ Int)])
            (let test {} (:x 42))
        "#;
        let result = check(src);
        assert!(result.is_err(), "expected type error: :x applied to Int");
    }

    #[test]
    fn reject_unknown_field_on_record() {
        // :z is not a field of Point — fails during the function's type-check
        let src = r#"
            (type Point [(:x ~ Int) (:y ~ Int)])
            (let get_z {p} (:z p))
        "#;
        let result = check(src);
        assert!(
            matches!(result, Err(TypeError::UnknownField { .. })),
            "expected UnknownField for unknown field :z"
        );
    }

    #[test]
    fn reject_wrong_constructor_arg_type() {
        // (+ s True) — s is Int (from Some 1), True is Bool — type error
        let src = r#"
            (type ['a] Option [None (Some ~ 'a)])
            (let bad_match {x} (match x (Some s) ~> (+ s True) None ~> 0))
        "#;
        let result = check(src);
        assert!(result.is_err(), "expected type error: Bool used with +");
    }
}
