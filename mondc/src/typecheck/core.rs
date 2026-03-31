use std::{
    collections::{HashMap, HashSet},
    sync::Arc as Rc,
};

use crate::ast::Expr;

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
    pub preds: Vec<Predicate>,
    pub ty: Rc<Type>,
}

pub type Substitution = HashMap<u64, Rc<Type>>;
pub type TypeEnv = HashMap<String, Scheme>;

/// Type predicates used for constrained polymorphism.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Predicate {
    HasField {
        label: String,
        record_ty: Rc<Type>,
        field_ty: Rc<Type>,
    },
}

// ---------------------------------------------------------------------------
// Error Handling
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct MismatchTypeError {
    pub(super) expected: Rc<Type>,
    pub(super) found: Rc<Type>,
    /// Precise source span of the offending sub-expression, if known.
    pub(super) span: Option<std::ops::Range<usize>>,
    /// Span of an earlier argument that first constrained the expected type, if known.
    pub(super) prior_span: Option<std::ops::Range<usize>>,
    /// Span that introduced a type expectation (e.g. a constraining match pattern).
    pub(super) expected_from_span: Option<std::ops::Range<usize>>,
    /// Human-readable reason for why the expected type was inferred.
    pub(super) expected_from_message: Option<String>,
    /// Actual type of the argument at the offending span (may be richer than `found`,
    /// which is a structural sub-component extracted by unification).
    pub(super) arg_ty: Option<Rc<Type>>,
    /// Full expected argument type at the call site, if known.
    pub(super) expected_arg_ty: Option<Rc<Type>>,
    /// Name of the function being called, for "X expects Y" context in the error.
    pub(super) callee_name: Option<String>,
    /// Source span of the callee expression, for a secondary label.
    pub(super) callee_span: Option<std::ops::Range<usize>>,
    /// Definition site of the callee, if it is a known local/top-level binding.
    pub(super) callee_def: Option<(usize, std::ops::Range<usize>)>,
    /// Inferred type of the callee at the call site, if known.
    pub(super) callee_ty: Option<Rc<Type>>,
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
        from_letq_desugar: bool,
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
    InaccessiblePrivateRecordField {
        field: String,
        record: String,
        modules: Vec<String>,
        field_span: std::ops::Range<usize>,
    },
    AmbiguousFieldAccess {
        field: String,
        record_ty: Rc<Type>,
        field_span: std::ops::Range<usize>,
        candidates: Vec<String>,
    },
    UnsatisfiedFieldConstraint {
        field: String,
        record_ty: Rc<Type>,
        field_ty: Rc<Type>,
        candidates: Vec<String>,
    },
    AmbiguousFieldConstraint {
        field: String,
        record_ty: Rc<Type>,
        field_ty: Rc<Type>,
        candidates: Vec<String>,
    },
    DuplicateRecordField {
        record: String,
        field: String,
        span: std::ops::Range<usize>,
    },
    DuplicateRecordUpdateField {
        field: String,
        span: std::ops::Range<usize>,
    },
    MissingRecordFields {
        record: String,
        missing: Vec<String>,
        span: std::ops::Range<usize>,
    },
    NonExhaustiveMatch {
        missing: Vec<String>,
        span: std::ops::Range<usize>,
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
                } else if !matches!(mismatch.expected.as_ref(), Type::Fun(..))
                    && matches!(mismatch.found.as_ref(), Type::Fun(..))
                {
                    notes.push(
                        "hint: this is a function value; did you mean to call it with arguments?"
                            .into(),
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
                if let Some(ps) = &mismatch.expected_from_span {
                    let msg = mismatch
                        .expected_from_message
                        .clone()
                        .unwrap_or_else(|| format!("`{expected_here}` inferred from this"));
                    labels.push(Label::secondary(file_id, ps.clone()).with_message(msg));
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
                from_letq_desugar,
            } => {
                let mut var_names = std::collections::HashMap::new();
                let expected_s = type_display_inner(expected, &mut var_names);
                let found_s = type_display_inner(found, &mut var_names);
                // arm is 0-indexed; arm 0 sets the expected type, conflict is at arm N
                let conflicting = arm + 1;
                if *from_letq_desugar {
                    return vec![
                        Diagnostic::error()
                            .with_message("`let?` body must return a `Result`")
                            .with_labels(vec![Label::primary(file_id, span).with_message(
                                "the `let?` continuation and propagated `Error` branch must return the same type",
                            )])
                            .with_notes(vec![
                                format!("  `let?` continuation returns: `{expected_s}`"),
                                format!("  propagated `Error` branch returns: `{found_s}`"),
                                "hint: return `(Ok value)` from the `let?` body".into(),
                                "internally, `let?` desugars to a `match` on `Ok`/`Error`".into(),
                            ]),
                    ];
                }
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
            TypeError::InaccessiblePrivateRecordField {
                field,
                record,
                modules,
                field_span,
            } => {
                let origin = if modules.len() == 1 {
                    format!("module `{}`", modules[0])
                } else {
                    format!("modules {}", modules.join(", "))
                };
                vec![
                    Diagnostic::error()
                        .with_message(format!(
                            "cannot access `:{field}` on private record `{record}` from {origin}"
                        ))
                        .with_labels(vec![
                            Label::primary(file_id, field_span.clone()).with_message(format!(
                                "`{record}` is private and its fields are not accessible here"
                            )),
                        ])
                        .with_notes(vec![format!(
                            "export `{record}` as `(pub type {record} [...])` to access `:{field}` outside its module"
                        )]),
                ]
            }
            TypeError::AmbiguousFieldAccess {
                field,
                record_ty,
                field_span,
                candidates,
            } => {
                let ty_s = type_display(record_ty);
                vec![
                    Diagnostic::error()
                        .with_message(format!("ambiguous field access `:{field}` for `{ty_s}`"))
                        .with_labels(vec![
                            Label::primary(file_id, field_span.clone()).with_message(format!(
                                "cannot determine which record accessor for `:{field}` to use"
                            )),
                        ])
                        .with_notes(vec![
                            format!("candidates: {}", candidates.join(", ")),
                            "add a type constraint (for example via pattern matching) before accessing this field".into(),
                        ]),
                ]
            }
            TypeError::UnsatisfiedFieldConstraint {
                field,
                record_ty,
                field_ty,
                candidates,
            } => {
                let mut var_names = std::collections::HashMap::new();
                let constraint_s = predicate_display_inner(
                    &Predicate::HasField {
                        label: field.clone(),
                        record_ty: record_ty.clone(),
                        field_ty: field_ty.clone(),
                    },
                    &mut var_names,
                );
                let mut notes = if candidates.is_empty() {
                    vec![format!("no record type in scope defines `:{field}`")]
                } else {
                    vec![format!(
                        "records with `:{field}`: {}",
                        candidates.join(", ")
                    )]
                };
                notes.push("add a type constraint so the record type is known".into());
                vec![
                    Diagnostic::error()
                        .with_message(format!("unsatisfied field constraint `{constraint_s}`"))
                        .with_labels(vec![Label::primary(file_id, span).with_message(
                            "this expression requires a record field that cannot be resolved",
                        )])
                        .with_notes(notes),
                ]
            }
            TypeError::AmbiguousFieldConstraint {
                field,
                record_ty,
                field_ty,
                candidates,
            } => {
                let mut var_names = std::collections::HashMap::new();
                let constraint_s = predicate_display_inner(
                    &Predicate::HasField {
                        label: field.clone(),
                        record_ty: record_ty.clone(),
                        field_ty: field_ty.clone(),
                    },
                    &mut var_names,
                );
                vec![
                    Diagnostic::error()
                        .with_message(format!("ambiguous field constraint `{constraint_s}`"))
                        .with_labels(vec![
                            Label::primary(file_id, span)
                                .with_message("the record type is still polymorphic here"),
                        ])
                        .with_notes(vec![
                            format!("candidate records: {}", candidates.join(", ")),
                            "add a concrete record type constraint before using this value".into(),
                        ]),
                ]
            }
            TypeError::DuplicateRecordField {
                record,
                field,
                span,
            } => vec![
                Diagnostic::error()
                    .with_message(format!(
                        "duplicate field `:{field}` in record construction of `{record}`"
                    ))
                    .with_labels(vec![
                        Label::primary(file_id, span.clone())
                            .with_message("each field may only be provided once"),
                    ]),
            ],
            TypeError::DuplicateRecordUpdateField { field, span } => vec![
                Diagnostic::error()
                    .with_message(format!("duplicate field `:{field}` in record update"))
                    .with_labels(vec![
                        Label::primary(file_id, span.clone())
                            .with_message("each field may only be updated once"),
                    ]),
            ],
            TypeError::MissingRecordFields {
                record,
                missing,
                span: constructor_span,
            } => vec![
                Diagnostic::error()
                    .with_message(format!(
                        "record construction of `{record}` is missing fields"
                    ))
                    .with_labels(vec![
                        Label::primary(file_id, constructor_span.clone())
                            .with_message("provide values for all declared record fields"),
                    ])
                    .with_notes(vec![format!("missing: {}", missing.join(", "))]),
            ],
            TypeError::NonExhaustiveMatch { missing, span } => vec![
                Diagnostic::error()
                    .with_message("non-exhaustive match")
                    .with_labels(vec![
                        Label::primary(file_id, span.clone())
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

pub fn predicate_display(pred: &Predicate) -> String {
    let mut var_names: std::collections::HashMap<u64, String> = std::collections::HashMap::new();
    predicate_display_inner(pred, &mut var_names)
}

pub fn scheme_display(scheme: &Scheme) -> String {
    let mut var_names: std::collections::HashMap<u64, String> = std::collections::HashMap::new();
    let ty = type_display_inner(&scheme.ty, &mut var_names);
    if scheme.preds.is_empty() {
        ty
    } else {
        let preds = scheme
            .preds
            .iter()
            .map(|pred| predicate_display_inner(pred, &mut var_names))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{preds} => {ty}")
    }
}

fn predicate_display_inner(
    pred: &Predicate,
    var_names: &mut std::collections::HashMap<u64, String>,
) -> String {
    match pred {
        Predicate::HasField {
            label,
            record_ty,
            field_ty,
        } => format!(
            "HasField :{label} {} {}",
            type_display_inner(record_ty, var_names),
            type_display_inner(field_ty, var_names)
        ),
    }
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

pub(super) fn apply_subst_predicate(subst: &Substitution, pred: &Predicate) -> Predicate {
    match pred {
        Predicate::HasField {
            label,
            record_ty,
            field_ty,
        } => Predicate::HasField {
            label: label.clone(),
            record_ty: apply_subst(subst, record_ty),
            field_ty: apply_subst(subst, field_ty),
        },
    }
}

pub(super) fn apply_subst_preds(subst: &Substitution, preds: &[Predicate]) -> Vec<Predicate> {
    preds
        .iter()
        .map(|pred| apply_subst_predicate(subst, pred))
        .collect()
}

fn apply_subst_scheme(subst: &Substitution, scheme: &Scheme) -> Scheme {
    let reduced: Substitution = subst
        .iter()
        .filter(|(k, _)| !scheme.vars.contains(k))
        .map(|(k, v)| (*k, v.clone()))
        .collect();
    Scheme {
        vars: scheme.vars.clone(),
        preds: scheme
            .preds
            .iter()
            .map(|pred| apply_subst_predicate(&reduced, pred))
            .collect(),
        ty: apply_subst(&reduced, &scheme.ty),
    }
}

pub fn apply_subst_env(subst: &Substitution, env: &TypeEnv) -> TypeEnv {
    env.iter()
        .map(|(k, v)| (k.clone(), apply_subst_scheme(subst, v)))
        .collect()
}

fn resolve_type_name(name: &str, aliases: &HashMap<String, String>) -> String {
    aliases
        .get(name)
        .cloned()
        .unwrap_or_else(|| name.to_string())
}

fn apply_type_aliases(ty: &Rc<Type>, aliases: &HashMap<String, String>) -> Rc<Type> {
    match ty.as_ref() {
        Type::Var(_) => ty.clone(),
        Type::Fun(arg, ret) => Type::fun(
            apply_type_aliases(arg, aliases),
            apply_type_aliases(ret, aliases),
        ),
        Type::Con(name, args) => Type::con(
            resolve_type_name(name, aliases),
            args.iter()
                .map(|arg| apply_type_aliases(arg, aliases))
                .collect(),
        ),
    }
}

fn apply_predicate_type_aliases(pred: &Predicate, aliases: &HashMap<String, String>) -> Predicate {
    match pred {
        Predicate::HasField {
            label,
            record_ty,
            field_ty,
        } => Predicate::HasField {
            label: label.clone(),
            record_ty: apply_type_aliases(record_ty, aliases),
            field_ty: apply_type_aliases(field_ty, aliases),
        },
    }
}

pub fn normalize_scheme_type_aliases(scheme: &Scheme, aliases: &HashMap<String, String>) -> Scheme {
    Scheme {
        vars: scheme.vars.clone(),
        preds: scheme
            .preds
            .iter()
            .map(|pred| apply_predicate_type_aliases(pred, aliases))
            .collect(),
        ty: apply_type_aliases(&scheme.ty, aliases),
    }
}

pub fn normalize_env_type_aliases(env: &TypeEnv, aliases: &HashMap<String, String>) -> TypeEnv {
    env.iter()
        .map(|(name, scheme)| (name.clone(), normalize_scheme_type_aliases(scheme, aliases)))
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

fn free_vars_predicate(pred: &Predicate) -> HashSet<u64> {
    match pred {
        Predicate::HasField {
            record_ty,
            field_ty,
            ..
        } => {
            let mut fv = free_vars(record_ty);
            fv.extend(free_vars(field_ty));
            fv
        }
    }
}

fn free_vars_preds(preds: &[Predicate]) -> HashSet<u64> {
    preds
        .iter()
        .flat_map(free_vars_predicate)
        .collect::<HashSet<_>>()
}

pub(super) fn generalize(env: &TypeEnv, ty: &Rc<Type>, preds: &[Predicate]) -> Scheme {
    const GENERALIZED_VAR_BASE: u64 = u64::MAX - 4096;

    // 1. Get all variables currently free in the environment
    let env_fv: HashSet<u64> = env
        .values()
        .flat_map(|s| {
            let mut fv = free_vars(&s.ty);
            fv.extend(free_vars_preds(&s.preds));
            let bound: HashSet<u64> = s.vars.iter().cloned().collect();
            // We need to collect the difference immediately to avoid reference errors
            fv.into_iter()
                .filter(|id| !bound.contains(id))
                .collect::<Vec<u64>>()
        })
        .collect();

    // 2. Get variables in the current type
    let mut ty_fv = free_vars(ty);
    ty_fv.extend(free_vars_preds(preds));

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
    let preds: Vec<Predicate> = preds
        .iter()
        .map(|pred| apply_subst_predicate(&renumbering, pred))
        .collect();
    let vars = (0..vars.len())
        .map(|i| GENERALIZED_VAR_BASE - i as u64)
        .collect();

    Scheme { vars, preds, ty }
}

pub(super) fn is_non_expansive(expr: &Expr) -> bool {
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
        | Expr::RecordUpdate { .. }
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
                expected_from_span: None,
                expected_from_message: None,
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
