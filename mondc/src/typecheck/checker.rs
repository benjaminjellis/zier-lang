use std::{
    collections::{HashMap, HashSet},
    sync::Arc as Rc,
};

use crate::ast::{Expr, Literal, MatchArm, Pattern, TypeDecl};

use super::{
    core::{apply_subst_predicate, apply_subst_preds, generalize, is_non_expansive},
    env::type_sig_to_scheme,
    *,
};

fn mismatch_with_span(error: TypeError, span: std::ops::Range<usize>) -> TypeError {
    match error {
        TypeError::Mismatch { mismatch } => {
            let mut mismatch = *mismatch;
            mismatch.span = Some(span);
            TypeError::Mismatch {
                mismatch: Box::new(mismatch),
            }
        }
        other => other,
    }
}

pub(super) fn record_accessor_key(record_name: &str, field_name: &str) -> String {
    format!(":{record_name}:{field_name}")
}

fn lookup_record_accessor<'a>(
    env: &'a TypeEnv,
    record_name: &str,
    field_name: &str,
) -> Option<&'a Scheme> {
    env.get(&record_accessor_key(record_name, field_name))
        .or_else(|| env.get(&format!(":{field_name}")))
}

fn is_desugared_letq_match(arms: &[MatchArm]) -> bool {
    if arms.len() != 2 {
        return false;
    }
    let ok_arm = &arms[0];
    let err_arm = &arms[1];
    if ok_arm.guard.is_some() || err_arm.guard.is_some() {
        return false;
    }
    if ok_arm.patterns.len() != 1 || err_arm.patterns.len() != 1 {
        return false;
    }

    let ok_pat = &ok_arm.patterns[0];
    let err_pat = &err_arm.patterns[0];
    if !matches!(ok_pat, Pattern::Constructor(name, args, _) if name == "Ok" && args.len() == 1) {
        return false;
    }

    let err_name = match err_pat {
        Pattern::Constructor(name, args, _) if name == "Error" && args.len() == 1 => {
            match &args[0] {
                Pattern::Variable(name, _) if name == "__letq_error" => name,
                _ => return false,
            }
        }
        _ => return false,
    };

    match &err_arm.body {
        Expr::Call { func, args, .. } if args.len() == 1 => {
            matches!(func.as_ref(), Expr::Variable(name, _) if name == "Error")
                && matches!(&args[0], Expr::Variable(name, _) if name == err_name)
        }
        _ => false,
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
    /// Field label -> record names that define that field.
    field_instances: HashMap<String, Vec<String>>,
    /// Maps record type name -> number of type parameters.
    record_param_arity: HashMap<String, usize>,
    /// Record type name -> module names where the record exists but is private.
    private_record_origins: HashMap<String, Vec<String>>,
    /// Maps value/function name → (file_id, definition span) for error reporting.
    value_def_spans: HashMap<String, (usize, std::ops::Range<usize>)>,
    /// Best-effort inferred types for expression spans, used by editor tooling.
    expr_types: Vec<(std::ops::Range<usize>, Rc<Type>)>,
    /// Maps qualified type names (`module/Type`) to canonical local names (`Type`).
    qualified_type_aliases: HashMap<String, String>,
}

impl TypeChecker {
    pub fn new() -> Self {
        Self {
            counter: 0,
            type_def_spans: HashMap::new(),
            variant_constructors: HashMap::new(),
            record_fields: HashMap::new(),
            field_instances: HashMap::new(),
            record_param_arity: HashMap::new(),
            private_record_origins: HashMap::new(),
            value_def_spans: HashMap::new(),
            expr_types: Vec::new(),
            qualified_type_aliases: HashMap::new(),
        }
    }

    pub fn seed_qualified_type_aliases(&mut self, aliases: HashMap<String, String>) {
        self.qualified_type_aliases = aliases;
    }

    pub fn seed_imported_type_info(&mut self, imported_type_decls: &[TypeDecl]) {
        for type_decl in imported_type_decls {
            match type_decl {
                TypeDecl::Variant {
                    name, constructors, ..
                } => {
                    self.variant_constructors.insert(
                        name.clone(),
                        constructors.iter().map(|(name, _)| name.clone()).collect(),
                    );
                }
                TypeDecl::Record {
                    name,
                    fields,
                    params,
                    ..
                } => {
                    self.register_record_type_info(
                        name.clone(),
                        fields.iter().map(|(field, _)| field.clone()).collect(),
                        params.len(),
                    );
                }
            }
        }
    }

    pub fn seed_private_record_origins(
        &mut self,
        imported_private_records: HashMap<String, Vec<String>>,
    ) {
        self.private_record_origins = imported_private_records;
    }

    fn register_record_type_info(&mut self, name: String, fields: Vec<String>, arity: usize) {
        self.record_fields.insert(name.clone(), fields.clone());
        self.record_param_arity.insert(name.clone(), arity);
        for field in fields {
            let entry = self.field_instances.entry(field).or_default();
            if !entry.iter().any(|existing| existing == &name) {
                entry.push(name.clone());
                entry.sort();
            }
        }
    }

    fn field_instance_candidates(&self, label: &str) -> Vec<String> {
        self.field_instances.get(label).cloned().unwrap_or_default()
    }

    fn field_instance_candidates_with_env(&self, env: &TypeEnv, label: &str) -> Vec<String> {
        let mut candidates = self.field_instance_candidates(label);
        for key in env.keys() {
            let Some(rest) = key.strip_prefix(':') else {
                continue;
            };
            let Some((record_name, field_name)) = rest.rsplit_once(':') else {
                continue;
            };
            if field_name != label || record_name.is_empty() {
                continue;
            }
            if !candidates.iter().any(|existing| existing == record_name) {
                candidates.push(record_name.to_string());
            }
        }
        candidates.sort();
        candidates
    }

    fn inaccessible_private_record_modules(&self, record_ty: &Rc<Type>) -> Option<Vec<String>> {
        let Type::Con(record_name, _) = record_ty.as_ref() else {
            return None;
        };
        if self.record_fields.contains_key(record_name) {
            return None;
        }
        if self.type_def_spans.contains_key(record_name) {
            return None;
        }
        self.private_record_origins.get(record_name).cloned()
    }

    fn solve_has_field_against_record(
        &mut self,
        env: &TypeEnv,
        label: &str,
        record_name: &str,
        record_ty: &Rc<Type>,
        field_ty: &Rc<Type>,
    ) -> Result<Substitution, TypeError> {
        let accessor_scheme = lookup_record_accessor(env, record_name, label).ok_or_else(|| {
            TypeError::UnsatisfiedFieldConstraint {
                field: label.to_string(),
                record_ty: record_ty.clone(),
                field_ty: field_ty.clone(),
                candidates: self.field_instance_candidates_with_env(env, label),
            }
        })?;
        let accessor_ty = self.instantiate(accessor_scheme);
        match accessor_ty.as_ref() {
            Type::Fun(accessor_record_ty, accessor_field_ty) => {
                let s1 = unify(accessor_record_ty, record_ty)?;
                let s2 = unify(
                    &apply_subst(&s1, accessor_field_ty),
                    &apply_subst(&s1, field_ty),
                )?;
                Ok(compose_subst(&s2, &s1))
            }
            _ => unreachable!("record accessor must be a unary function"),
        }
    }

    fn solve_predicates(
        &mut self,
        env: &TypeEnv,
        preds: &[Predicate],
    ) -> Result<(Substitution, Vec<Predicate>), TypeError> {
        let mut subst = HashMap::new();
        let mut residual = vec![];

        for pred in preds {
            let pred = apply_subst_predicate(&subst, pred);
            match pred {
                Predicate::HasField {
                    label,
                    record_ty,
                    field_ty,
                } => {
                    let candidates = self.field_instance_candidates_with_env(env, &label);
                    match record_ty.as_ref() {
                        Type::Con(record_name, _) => {
                            let s = self
                                .solve_has_field_against_record(
                                    env,
                                    &label,
                                    record_name,
                                    &record_ty,
                                    &field_ty,
                                )
                                .map_err(|_| TypeError::UnsatisfiedFieldConstraint {
                                    field: label,
                                    record_ty: record_ty.clone(),
                                    field_ty: field_ty.clone(),
                                    candidates: candidates.clone(),
                                })?;
                            subst = compose_subst(&s, &subst);
                            residual = apply_subst_preds(&s, &residual);
                        }
                        Type::Fun(_, _) => {
                            return Err(TypeError::UnsatisfiedFieldConstraint {
                                field: label,
                                record_ty,
                                field_ty,
                                candidates,
                            });
                        }
                        Type::Var(_) => match candidates.as_slice() {
                            [] => {
                                let accessor_name = format!(":{label}");
                                let Some(accessor_scheme) = env.get(&accessor_name) else {
                                    return Err(TypeError::UnsatisfiedFieldConstraint {
                                        field: label,
                                        record_ty,
                                        field_ty,
                                        candidates,
                                    });
                                };
                                let accessor_ty = self.instantiate(accessor_scheme);
                                match accessor_ty.as_ref() {
                                    Type::Fun(accessor_record_ty, accessor_field_ty) => {
                                        let s1 = unify(accessor_record_ty, &record_ty)?;
                                        let s2 = unify(
                                            &apply_subst(&s1, accessor_field_ty),
                                            &apply_subst(&s1, &field_ty),
                                        )?;
                                        let s = compose_subst(&s2, &s1);
                                        subst = compose_subst(&s, &subst);
                                        residual = apply_subst_preds(&s, &residual);
                                    }
                                    _ => unreachable!("record accessor must be a unary function"),
                                }
                            }
                            [record_name] => {
                                let s = self.solve_has_field_against_record(
                                    env,
                                    &label,
                                    record_name,
                                    &record_ty,
                                    &field_ty,
                                )?;
                                subst = compose_subst(&s, &subst);
                                residual = apply_subst_preds(&s, &residual);
                            }
                            _ => {
                                residual.push(Predicate::HasField {
                                    label,
                                    record_ty,
                                    field_ty,
                                });
                            }
                        },
                    }
                }
            }
        }

        Ok((subst, residual))
    }

    fn unresolved_predicate_error(&self, pred: &Predicate) -> TypeError {
        match pred {
            Predicate::HasField {
                label,
                record_ty,
                field_ty,
            } => {
                let candidates = self.field_instance_candidates(label);
                if candidates.len() > 1 {
                    TypeError::AmbiguousFieldConstraint {
                        field: label.clone(),
                        record_ty: record_ty.clone(),
                        field_ty: field_ty.clone(),
                        candidates,
                    }
                } else {
                    TypeError::UnsatisfiedFieldConstraint {
                        field: label.clone(),
                        record_ty: record_ty.clone(),
                        field_ty: field_ty.clone(),
                        candidates,
                    }
                }
            }
        }
    }

    fn fresh(&mut self) -> Rc<Type> {
        let id = self.counter;
        self.counter += 1;
        Rc::new(Type::Var(id))
    }

    pub(super) fn instantiate_with_preds(&mut self, scheme: &Scheme) -> (Rc<Type>, Vec<Predicate>) {
        let subst: Substitution = scheme.vars.iter().map(|&v| (v, self.fresh())).collect();
        let ty = apply_subst(&subst, &scheme.ty);
        let preds = scheme
            .preds
            .iter()
            .map(|pred| apply_subst_predicate(&subst, pred))
            .collect();
        (ty, preds)
    }

    fn instantiate(&mut self, scheme: &Scheme) -> Rc<Type> {
        self.instantiate_with_preds(scheme).0
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

    fn constructor_name(name: &str) -> &str {
        name.rsplit_once('/')
            .map_or(name, |(_, constructor)| constructor)
    }

    fn collect_top_level_constructors<'a>(pat: &'a Pattern, out: &mut HashSet<&'a str>) {
        match pat {
            Pattern::Constructor(name, _, _) => {
                out.insert(Self::constructor_name(name));
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
        arms: &[MatchArm],
        match_span: std::ops::Range<usize>,
    ) -> Result<(), TypeError> {
        if target_types.len() != 1 {
            return Ok(());
        }

        if arms.iter().any(|arm| {
            arm.guard.is_none() && arm.patterns.first().is_some_and(Self::pattern_is_catch_all)
        }) {
            return Ok(());
        }

        let resolved_target = apply_subst(subst, &target_types[0]);
        match resolved_target.as_ref() {
            Type::Con(type_name, args) if type_name == "List" && args.len() == 1 => {
                let mut has_empty = false;
                let mut has_cons = false;
                for arm in arms {
                    if arm.guard.is_some() {
                        continue;
                    }
                    if let Some(pat) = arm.patterns.first() {
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
                    Err(TypeError::NonExhaustiveMatch {
                        missing,
                        span: match_span.clone(),
                    })
                }
            }
            Type::Con(type_name, _) if type_name == "Bool" => {
                let mut seen_true = false;
                let mut seen_false = false;
                for arm in arms {
                    if arm.guard.is_some() {
                        continue;
                    }
                    if let Some(pat) = arm.patterns.first() {
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
                    Err(TypeError::NonExhaustiveMatch {
                        missing,
                        span: match_span.clone(),
                    })
                }
            }
            Type::Con(type_name, _) if type_name == "Int" => Err(TypeError::NonExhaustiveMatch {
                missing: vec!["_".into()],
                span: match_span,
            }),
            Type::Con(type_name, _) => {
                let Some(constructors) = self.variant_constructors.get(type_name) else {
                    return Ok(());
                };

                let mut covered = HashSet::new();
                for arm in arms {
                    if arm.guard.is_some() {
                        continue;
                    }
                    if let Some(pat) = arm.patterns.first() {
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
                    Err(TypeError::NonExhaustiveMatch {
                        missing,
                        span: match_span.clone(),
                    })
                }
            }
            Type::Fun(_, _) | Type::Var(_) => Ok(()),
        }
    }

    pub fn inferred_expr_types(&self) -> &[(std::ops::Range<usize>, Rc<Type>)] {
        &self.expr_types
    }

    pub fn inferred_record_expr_types(&self) -> HashMap<(usize, usize), String> {
        let mut out = HashMap::new();
        for (span, ty) in &self.expr_types {
            if let Type::Con(name, _) = ty.as_ref()
                && self.record_fields.contains_key(name)
            {
                out.insert((span.start, span.end), name.clone());
            }
        }
        out
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
    ) -> Result<(Substitution, Rc<Type>, Vec<Predicate>), TypeError> {
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
                Ok((HashMap::new(), ty, vec![]))
            }

            Expr::Variable(name, span) => {
                let scheme = env
                    .get(name)
                    .ok_or_else(|| TypeError::UnboundVariable(name.clone(), span.clone()))?;
                let (ty, preds) = self.instantiate_with_preds(scheme);
                self.record_expr_type(expr.span(), ty.clone());
                Ok((HashMap::new(), ty, preds))
            }

            Expr::List(items, _) => {
                let elem_ty = self.fresh();
                let mut subst = HashMap::new();
                let mut preds = vec![];
                for item in items {
                    let (s, t, item_preds) = self.infer(&apply_subst_env(&subst, env), item)?;
                    // Apply both accumulated subst and item subst so the
                    // constrained elem_ty is visible when unifying the next item.
                    let known_elem = apply_subst(&compose_subst(&s, &subst), &elem_ty);
                    let s_unify =
                        unify(&known_elem, &t).map_err(|e| mismatch_with_span(e, item.span()))?;
                    preds = apply_subst_preds(&s, &preds);
                    preds.extend(item_preds);
                    preds = apply_subst_preds(&s_unify, &preds);
                    subst = compose_subst(&compose_subst(&s_unify, &s), &subst);
                }
                let ty = Type::array(apply_subst(&subst, &elem_ty));
                self.record_expr_type(expr.span(), ty.clone());
                Ok((subst.clone(), ty, apply_subst_preds(&subst, &preds)))
            }

            Expr::If {
                cond, then, els, ..
            } => {
                let (s1, t_cond, cond_preds) = self.infer(env, cond)?;
                let s_bool =
                    unify(&t_cond, &Type::bool()).map_err(|_| TypeError::ConditionNotBool {
                        found: t_cond.clone(),
                    })?;
                let s1 = compose_subst(&s_bool, &s1);
                let mut preds = apply_subst_preds(&s_bool, &cond_preds);

                let (s2, t_then, then_preds) = self.infer(&apply_subst_env(&s1, env), then)?;
                let s12 = compose_subst(&s2, &s1);
                preds = apply_subst_preds(&s2, &preds);
                preds.extend(then_preds);

                let (s3, t_els, else_preds) = self.infer(&apply_subst_env(&s12, env), els)?;
                let s123 = compose_subst(&s3, &s12);
                preds = apply_subst_preds(&s3, &preds);
                preds.extend(else_preds);

                let then_resolved = apply_subst(&s123, &t_then);
                let else_resolved = apply_subst(&s123, &t_els);
                let s_final = unify(&then_resolved, &else_resolved).map_err(|_| {
                    TypeError::BranchMismatch {
                        then_ty: then_resolved.clone(),
                        else_ty: else_resolved.clone(),
                    }
                })?;
                let s_res = compose_subst(&s_final, &s123);
                preds = apply_subst_preds(&s_final, &preds);
                let ty = apply_subst(&s_res, &t_then);
                self.record_expr_type(expr.span(), ty.clone());
                Ok((s_res.clone(), ty, apply_subst_preds(&s_res, &preds)))
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
                let (s0, mut t_func, mut preds) = self.infer(env, func)?;
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
                                    expected_from_span: mismatch.expected_from_span,
                                    expected_from_message: mismatch.expected_from_message,
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
                    preds = apply_subst_preds(&s_unify, &preds);
                    let (s_pred, solved_preds) =
                        self.solve_predicates(&apply_subst_env(&subst, env), &preds)?;
                    subst = compose_subst(&s_pred, &subst);
                    preds = apply_subst_preds(&s_pred, &solved_preds);
                    let result_ty = apply_subst(&subst, &ret);
                    self.record_expr_type(expr.span(), result_ty.clone());
                    return Ok((subst.clone(), result_ty, apply_subst_preds(&subst, &preds)));
                }

                for arg in args {
                    let (s_arg, t_arg, arg_preds) =
                        self.infer(&apply_subst_env(&subst, env), arg)?;
                    subst = compose_subst(&s_arg, &subst);
                    preds = apply_subst_preds(&s_arg, &preds);
                    preds.extend(arg_preds);
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
                                    expected_from_span: mismatch.expected_from_span,
                                    expected_from_message: mismatch.expected_from_message,
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
                    preds = apply_subst_preds(&s_unify, &preds);
                    t_func = apply_subst(&subst, &ret);
                    prev_span = Some(arg.span());
                }
                let (s_pred, solved_preds) =
                    self.solve_predicates(&apply_subst_env(&subst, env), &preds)?;
                subst = compose_subst(&s_pred, &subst);
                preds = apply_subst_preds(&s_pred, &solved_preds);
                t_func = apply_subst(&s_pred, &t_func);
                self.record_expr_type(expr.span(), t_func.clone());
                Ok((subst.clone(), t_func, apply_subst_preds(&subst, &preds)))
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
                let fun_ty = if arg_tys.is_empty() {
                    Type::fun(Type::unit(), ret_ty.clone())
                } else {
                    arg_tys
                        .iter()
                        .rev()
                        .fold(ret_ty.clone(), |acc, a| Type::fun(a.clone(), acc))
                };

                let mut inner_env = env.clone();
                for ((arg, span), ty) in args.iter().zip(arg_spans.iter()).zip(&arg_tys) {
                    inner_env.insert(
                        arg.clone(),
                        Scheme {
                            vars: vec![],
                            preds: vec![],
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
                        preds: vec![],
                        ty: fun_ty.clone(),
                    },
                );

                let (s1, t_val, value_preds) = self.infer(&inner_env, value)?;
                let s2 = unify(&apply_subst(&s1, &ret_ty), &t_val)
                    .map_err(|e| mismatch_with_span(e, value.span()))?;
                let mut s12 = compose_subst(&s2, &s1);
                let preds = apply_subst_preds(&s2, &value_preds);
                let (s_pred, preds) =
                    self.solve_predicates(&apply_subst_env(&s12, &inner_env), &preds)?;
                s12 = compose_subst(&s_pred, &s12);
                let preds = apply_subst_preds(&s_pred, &preds);

                for (span, ty) in arg_spans.iter().zip(arg_tys.iter()) {
                    self.record_expr_type(span.clone(), apply_subst(&s12, ty));
                }

                let binding_ty = apply_subst(&s12, &fun_ty);
                self.record_expr_type(name_span.clone(), binding_ty.clone());
                self.record_expr_type(expr.span(), binding_ty.clone());
                Ok((s12.clone(), binding_ty, apply_subst_preds(&s12, &preds)))
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
                let (s1, t_val, value_preds) = self.infer(env, value)?;
                let mut s1 = s1;
                let mut value_preds = apply_subst_preds(&s1, &value_preds);
                let mut ty = apply_subst(&s1, &t_val);
                let (s_pred, solved_preds) =
                    self.solve_predicates(&apply_subst_env(&s1, env), &value_preds)?;
                s1 = compose_subst(&s_pred, &s1);
                value_preds = apply_subst_preds(&s_pred, &solved_preds);
                ty = apply_subst(&s_pred, &ty);
                self.record_expr_type(name_span.clone(), ty.clone());
                let scheme = if is_non_expansive(value) {
                    generalize(&apply_subst_env(&s1, env), &ty, &value_preds)
                } else {
                    Scheme {
                        vars: vec![],
                        preds: value_preds.clone(),
                        ty,
                    }
                };

                let mut body_env = apply_subst_env(&s1, env);
                body_env.insert(name.clone(), scheme);
                let (s2, t_body, body_preds) = self.infer(&body_env, body)?;

                let subst = compose_subst(&s2, &s1);
                let mut preds = apply_subst_preds(&s2, &value_preds);
                preds.extend(body_preds);
                let ty = apply_subst(&subst, &t_body);
                self.record_expr_type(expr.span(), ty.clone());
                Ok((subst.clone(), ty, apply_subst_preds(&subst, &preds)))
            }

            Expr::Match {
                targets,
                arms,
                span,
            } => {
                let from_letq_desugar = is_desugared_letq_match(arms);
                let mut subst = HashMap::new();
                let mut target_types = Vec::new();
                let mut preds = vec![];
                for target in targets {
                    let (s, t, target_preds) = self.infer(env, target)?;
                    subst = compose_subst(&s, &subst);
                    preds = apply_subst_preds(&s, &preds);
                    preds.extend(target_preds);
                    target_types.push(t);
                }

                let result_ty = self.fresh();

                for (arm_index, arm) in arms.iter().enumerate() {
                    let pats = &arm.patterns;
                    let mut pat_env = apply_subst_env(&subst, env);
                    for ((pat, t_target), target_expr) in
                        pats.iter().zip(target_types.iter()).zip(targets.iter())
                    {
                        let t_target_s = apply_subst(&subst, t_target);
                        let (s_pat, new_env) = self
                            .infer_pattern(&pat_env, pat, &t_target_s)
                            .map_err(|e| mismatch_with_span(e, target_expr.span()))?;
                        subst = compose_subst(&s_pat, &subst);
                        pat_env = new_env;
                    }

                    // Apply accumulated pattern substitution before inferring the body,
                    // so pattern-bound variables have their concrete types visible.
                    // Without this, `val` in `(Some val) ~> body` would be Var(?n)
                    // rather than the type inferred from the match target.
                    let body_env = apply_subst_env(&subst, &pat_env);
                    let guard_preds = if let Some(guard) = &arm.guard {
                        let (s_guard, t_guard, guard_preds) = self.infer(&body_env, guard)?;
                        subst = compose_subst(&s_guard, &subst);
                        preds = apply_subst_preds(&s_guard, &preds);
                        let guard_expected = Rc::new(Type::Con("Bool".into(), vec![]));
                        let guard_found = apply_subst(&subst, &t_guard);
                        let s_guard_bool = unify(&guard_found, &guard_expected)?;
                        subst = compose_subst(&s_guard_bool, &subst);
                        preds = apply_subst_preds(&s_guard_bool, &preds);
                        apply_subst_preds(&s_guard_bool, &guard_preds)
                    } else {
                        vec![]
                    };
                    let (s_body, t_body, body_preds) =
                        self.infer(&apply_subst_env(&subst, &pat_env), &arm.body)?;
                    subst = compose_subst(&s_body, &subst);
                    preds = apply_subst_preds(&s_body, &preds);
                    preds.extend(guard_preds);
                    preds.extend(body_preds);

                    let expected = apply_subst(&subst, &result_ty);
                    let found = apply_subst(&subst, &t_body);
                    let s_unify = unify(&expected, &found).map_err(|_| TypeError::ArmMismatch {
                        arm: arm_index,
                        expected: expected.clone(),
                        found: found.clone(),
                        from_letq_desugar,
                    })?;
                    subst = compose_subst(&s_unify, &subst);
                    preds = apply_subst_preds(&s_unify, &preds);

                    for pat in pats {
                        self.record_pattern_binding_types(pat, &subst, &pat_env);
                    }
                }
                self.ensure_match_exhaustive(&subst, &target_types, arms, span.clone())?;
                let ty = apply_subst(&subst, &result_ty);
                self.record_expr_type(expr.span(), ty.clone());
                Ok((subst.clone(), ty, apply_subst_preds(&subst, &preds)))
            }

            Expr::FieldAccess {
                field,
                record,
                span,
            } => {
                // Infer the record type first so we can name it in the error if the field
                // doesn't exist.
                let (s1, t_record, record_preds) = self.infer(env, record)?;

                let resolved_record = apply_subst(&s1, &t_record);
                let mut candidates = self.field_instance_candidates(field);
                candidates.sort();

                if let Type::Con(record_name, _) = resolved_record.as_ref() {
                    let has_local_layout = self.record_fields.contains_key(record_name);
                    if has_local_layout
                        && !candidates.iter().any(|candidate| candidate == record_name)
                    {
                        return Err(TypeError::UnknownField {
                            field: field.clone(),
                            record_ty: resolved_record.clone(),
                            field_span: span.clone(),
                            def: self.type_def_spans.get(record_name).cloned(),
                        });
                    }

                    let scheme = lookup_record_accessor(env, record_name, field).or_else(|| {
                        if has_local_layout {
                            None
                        } else {
                            env.get(&format!(":{field}"))
                        }
                    });
                    if let Some(scheme) = scheme {
                        let (accessor_ty, accessor_preds) = self.instantiate_with_preds(scheme);

                        let ret_ty = self.fresh();
                        let s2 = unify(
                            &apply_subst(&s1, &accessor_ty),
                            &Type::fun(t_record.clone(), ret_ty.clone()),
                        )
                        .map_err(|e| mismatch_with_span(e, record.span()))?;
                        let s12 = compose_subst(&s2, &s1);
                        let mut preds = apply_subst_preds(&s2, &record_preds);
                        preds.extend(apply_subst_preds(&s2, &accessor_preds));
                        preds.push(Predicate::HasField {
                            label: field.clone(),
                            record_ty: apply_subst(&s12, &t_record),
                            field_ty: apply_subst(&s12, &ret_ty),
                        });

                        let ty = apply_subst(&s12, &ret_ty);
                        self.record_expr_type(expr.span(), ty.clone());
                        return Ok((s12.clone(), ty, apply_subst_preds(&s12, &preds)));
                    }
                }

                if candidates.is_empty() {
                    if let Some(modules) =
                        self.inaccessible_private_record_modules(&resolved_record)
                        && let Type::Con(record_name, _) = resolved_record.as_ref()
                    {
                        return Err(TypeError::InaccessiblePrivateRecordField {
                            field: field.clone(),
                            record: record_name.clone(),
                            modules,
                            field_span: span.clone(),
                        });
                    }
                    return Err(TypeError::UnknownField {
                        field: field.clone(),
                        record_ty: resolved_record.clone(),
                        field_span: span.clone(),
                        def: None,
                    });
                }

                let ret_ty = self.fresh();
                let mut preds = apply_subst_preds(&s1, &record_preds);
                preds.push(Predicate::HasField {
                    label: field.clone(),
                    record_ty: resolved_record,
                    field_ty: ret_ty.clone(),
                });
                self.record_expr_type(expr.span(), ret_ty.clone());
                Ok((s1.clone(), ret_ty, apply_subst_preds(&s1, &preds)))
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
                            preds: vec![],
                            ty: tv.clone(),
                        },
                    );
                    self.record_expr_type(span.clone(), tv.clone());
                    arg_tys.push(tv);
                }

                let (s, t_body, body_preds) = self.infer(&inner_env, body)?;
                let body_ty = apply_subst(&s, &t_body);

                // Apply substitution to arg types, then build curried Fun type
                let ty = if arg_tys.is_empty() {
                    Type::fun(Type::unit(), body_ty)
                } else {
                    arg_tys.iter().rev().fold(body_ty, |acc, arg_ty| {
                        Type::fun(apply_subst(&s, arg_ty), acc)
                    })
                };

                for (span, ty) in arg_spans.iter().zip(arg_tys.iter()) {
                    self.record_expr_type(span.clone(), apply_subst(&s, ty));
                }

                self.record_expr_type(expr.span(), ty.clone());
                Ok((s.clone(), ty, apply_subst_preds(&s, &body_preds)))
            }

            Expr::RecordConstruct { name, fields, span } => {
                // When we know the record declaration, enforce named-field construction rules:
                // - all declared fields must be present
                // - no unknown fields
                // - no duplicate fields
                // - field order at call sites is irrelevant
                if let Some(layout) = self.record_fields.get(name).cloned() {
                    let ctor_scheme = env
                        .get(name)
                        .ok_or_else(|| TypeError::UnboundVariable(name.clone(), span.clone()))?;
                    let (mut ctor_ty, ctor_preds) = self.instantiate_with_preds(ctor_scheme);
                    let mut ctor_arg_tys = Vec::new();
                    while let Type::Fun(arg, ret) = ctor_ty.as_ref() {
                        ctor_arg_tys.push(arg.clone());
                        ctor_ty = ret.clone();
                    }
                    let record_ty = ctor_ty;
                    let mut preds = ctor_preds;

                    let mut by_name: HashMap<String, &Expr> = HashMap::new();
                    for (field_name, value_expr) in fields {
                        if by_name.insert(field_name.clone(), value_expr).is_some() {
                            return Err(TypeError::DuplicateRecordField {
                                record: name.clone(),
                                field: field_name.clone(),
                                span: value_expr.span(),
                            });
                        }
                        if !layout.iter().any(|declared| declared == field_name) {
                            return Err(TypeError::UnknownField {
                                field: field_name.clone(),
                                record_ty: record_ty.clone(),
                                field_span: value_expr.span(),
                                def: self.type_def_spans.get(name).cloned(),
                            });
                        }
                    }

                    let mut missing = Vec::new();
                    for declared in &layout {
                        if !by_name.contains_key(declared) {
                            missing.push(declared.clone());
                        }
                    }
                    if !missing.is_empty() {
                        return Err(TypeError::MissingRecordFields {
                            record: name.clone(),
                            missing,
                            span: span.clone(),
                        });
                    }

                    let mut subst = HashMap::new();
                    for (idx, field_name) in layout.iter().enumerate() {
                        let value_expr = *by_name.get(field_name).expect("field validated above");
                        let value_span = value_expr.span();
                        let expected_ty = ctor_arg_tys
                            .get(idx)
                            .cloned()
                            .unwrap_or_else(|| self.fresh());
                        let (s_val, t_val, value_preds) =
                            self.infer(&apply_subst_env(&subst, env), value_expr)?;
                        subst = compose_subst(&s_val, &subst);
                        preds = apply_subst_preds(&s_val, &preds);
                        preds.extend(value_preds);
                        let s_field = unify(
                            &apply_subst(&subst, &expected_ty),
                            &apply_subst(&subst, &t_val),
                        )
                        .map_err(|e| mismatch_with_span(e, value_span))?;
                        subst = compose_subst(&s_field, &subst);
                        preds = apply_subst_preds(&s_field, &preds);
                    }

                    let ty = apply_subst(&subst, &record_ty);
                    self.record_expr_type(expr.span(), ty.clone());
                    return Ok((subst.clone(), ty, apply_subst_preds(&subst, &preds)));
                }

                // Fallback path (e.g. imported records where local layout metadata is unavailable):
                // infer from accessors only.
                let result_ty = self.fresh();
                let mut subst = HashMap::new();
                let mut preds = vec![];

                for (field_name, value_expr) in fields {
                    let accessor_name = format!(":{field_name}");
                    let scheme = env.get(&accessor_name).ok_or_else(|| {
                        TypeError::UnboundVariable(accessor_name.clone(), span.clone())
                    })?;
                    let (accessor_ty, accessor_preds) = self.instantiate_with_preds(scheme);
                    let value_span = value_expr.span();

                    let field_ty = self.fresh();
                    let s_acc = unify(
                        &apply_subst(&subst, &accessor_ty),
                        &Type::fun(apply_subst(&subst, &result_ty), field_ty.clone()),
                    )?;
                    subst = compose_subst(&s_acc, &subst);
                    preds = apply_subst_preds(&s_acc, &preds);
                    preds.extend(apply_subst_preds(&s_acc, &accessor_preds));

                    let (s_val, t_val, value_preds) =
                        self.infer(&apply_subst_env(&subst, env), value_expr)?;
                    subst = compose_subst(&s_val, &subst);
                    preds = apply_subst_preds(&s_val, &preds);
                    preds.extend(value_preds);

                    let s_field = unify(
                        &apply_subst(&subst, &field_ty),
                        &apply_subst(&subst, &t_val),
                    )
                    .map_err(|e| mismatch_with_span(e, value_span))?;
                    subst = compose_subst(&s_field, &subst);
                    preds = apply_subst_preds(&s_field, &preds);
                }

                let ty = apply_subst(&subst, &result_ty);
                self.record_expr_type(expr.span(), ty.clone());
                Ok((subst.clone(), ty, apply_subst_preds(&subst, &preds)))
            }

            Expr::RecordUpdate {
                record,
                updates,
                span: _,
            } => {
                let (s_record, t_record, record_preds) = self.infer(env, record)?;
                let mut subst = s_record;
                let mut preds = record_preds;
                let resolved_record = apply_subst(&subst, &t_record);

                let mut seen_fields: HashSet<String> = HashSet::new();
                let mut updates_by_name: HashMap<String, &Expr> = HashMap::new();
                for (field_name, value_expr) in updates {
                    let value_span = value_expr.span();
                    if !seen_fields.insert(field_name.clone()) {
                        return Err(TypeError::DuplicateRecordUpdateField {
                            field: field_name.clone(),
                            span: value_span,
                        });
                    }
                    updates_by_name.insert(field_name.clone(), value_expr);
                }

                // Preferred path: infer/resolve a concrete record layout so updates can
                // change type parameters for updated fields while preserving unchanged ones.
                let inferred_layout = if let Type::Con(record_name, args) = resolved_record.as_ref()
                {
                    self.record_fields.get(record_name).cloned().map(|layout| {
                        (
                            record_name.clone(),
                            args.len(),
                            apply_subst(&subst, &t_record),
                            layout,
                        )
                    })
                } else {
                    let updated: HashSet<&str> =
                        updates_by_name.keys().map(String::as_str).collect();
                    let mut candidates: Vec<(String, Vec<String>, usize)> = self
                        .record_fields
                        .iter()
                        .filter_map(|(name, layout)| {
                            let layout_set: HashSet<&str> =
                                layout.iter().map(String::as_str).collect();
                            if updated.iter().all(|field| layout_set.contains(field)) {
                                Some((
                                    name.clone(),
                                    layout.clone(),
                                    *self.record_param_arity.get(name).unwrap_or(&0),
                                ))
                            } else {
                                None
                            }
                        })
                        .collect();
                    if candidates.len() == 1 {
                        let (record_name, layout, arity) = candidates.pop().expect("checked len");
                        let input_args: Vec<Rc<Type>> = (0..arity).map(|_| self.fresh()).collect();
                        let inferred_input = Type::con(record_name.clone(), input_args);
                        let s_infer = unify(&apply_subst(&subst, &t_record), &inferred_input)?;
                        subst = compose_subst(&s_infer, &subst);
                        preds = apply_subst_preds(&s_infer, &preds);
                        Some((
                            record_name,
                            arity,
                            apply_subst(&subst, &inferred_input),
                            layout,
                        ))
                    } else {
                        None
                    }
                };

                if let Some((record_name, param_arity, input_record_ty, layout)) = inferred_layout {
                    let layout_set: HashSet<&str> = layout.iter().map(String::as_str).collect();
                    for (field_name, value_expr) in &updates_by_name {
                        if !layout_set.contains(field_name.as_str()) {
                            return Err(TypeError::UnknownField {
                                field: field_name.clone(),
                                record_ty: input_record_ty.clone(),
                                field_span: value_expr.span(),
                                def: self.type_def_spans.get(&record_name).cloned(),
                            });
                        }
                    }

                    let output_args: Vec<Rc<Type>> =
                        (0..param_arity).map(|_| self.fresh()).collect();
                    let output_record_ty = Type::con(record_name.clone(), output_args);
                    let record_span = record.span();

                    for field_name in &layout {
                        let accessor_scheme = lookup_record_accessor(env, &record_name, field_name)
                            .ok_or_else(|| {
                                TypeError::UnboundVariable(
                                    format!(":{field_name}"),
                                    record_span.clone(),
                                )
                            })?;
                        let (accessor_in_ty, accessor_in_preds) =
                            self.instantiate_with_preds(accessor_scheme);
                        let (accessor_out_ty, accessor_out_preds) =
                            self.instantiate_with_preds(accessor_scheme);

                        let in_field_ty = self.fresh();
                        let out_field_ty = self.fresh();
                        let s_in_acc = unify(
                            &apply_subst(&subst, &accessor_in_ty),
                            &Type::fun(apply_subst(&subst, &input_record_ty), in_field_ty.clone()),
                        )?;
                        subst = compose_subst(&s_in_acc, &subst);
                        preds = apply_subst_preds(&s_in_acc, &preds);
                        preds.extend(apply_subst_preds(&s_in_acc, &accessor_in_preds));
                        let s_out_acc = unify(
                            &apply_subst(&subst, &accessor_out_ty),
                            &Type::fun(
                                apply_subst(&subst, &output_record_ty),
                                out_field_ty.clone(),
                            ),
                        )?;
                        subst = compose_subst(&s_out_acc, &subst);
                        preds = apply_subst_preds(&s_out_acc, &preds);
                        preds.extend(apply_subst_preds(&s_out_acc, &accessor_out_preds));

                        if let Some(value_expr) = updates_by_name.get(field_name) {
                            let value_span = value_expr.span();
                            let (s_val, t_val, value_preds) =
                                self.infer(&apply_subst_env(&subst, env), value_expr)?;
                            subst = compose_subst(&s_val, &subst);
                            preds = apply_subst_preds(&s_val, &preds);
                            preds.extend(value_preds);
                            let s_field = unify(
                                &apply_subst(&subst, &out_field_ty),
                                &apply_subst(&subst, &t_val),
                            )
                            .map_err(|e| mismatch_with_span(e, value_span))?;
                            subst = compose_subst(&s_field, &subst);
                            preds = apply_subst_preds(&s_field, &preds);
                            preds.push(Predicate::HasField {
                                label: field_name.clone(),
                                record_ty: apply_subst(&subst, &output_record_ty),
                                field_ty: apply_subst(&subst, &out_field_ty),
                            });
                        } else {
                            let s_same = unify(
                                &apply_subst(&subst, &in_field_ty),
                                &apply_subst(&subst, &out_field_ty),
                            )
                            .map_err(|e| mismatch_with_span(e, record_span.clone()))?;
                            subst = compose_subst(&s_same, &subst);
                            preds = apply_subst_preds(&s_same, &preds);
                        }
                    }

                    let ty = apply_subst(&subst, &output_record_ty);
                    self.record_expr_type(expr.span(), ty.clone());
                    return Ok((subst.clone(), ty, apply_subst_preds(&subst, &preds)));
                }

                // Fallback for unresolved record layouts: preserve the input record type.
                for (field_name, value_expr) in updates {
                    let value_span = value_expr.span();
                    let candidates = self.field_instance_candidates_with_env(env, field_name);
                    if candidates.is_empty() {
                        let record_ty = apply_subst(&subst, &t_record);
                        if let Some(modules) = self.inaccessible_private_record_modules(&record_ty)
                            && let Type::Con(record_name, _) = record_ty.as_ref()
                        {
                            return Err(TypeError::InaccessiblePrivateRecordField {
                                field: field_name.clone(),
                                record: record_name.clone(),
                                modules,
                                field_span: value_span.clone(),
                            });
                        }
                        return Err(TypeError::UnknownField {
                            field: field_name.clone(),
                            record_ty,
                            field_span: value_span.clone(),
                            def: None,
                        });
                    }

                    let (s_val, t_val, value_preds) =
                        self.infer(&apply_subst_env(&subst, env), value_expr)?;
                    subst = compose_subst(&s_val, &subst);
                    preds = apply_subst_preds(&s_val, &preds);
                    preds.extend(value_preds);

                    let field_ty = self.fresh();
                    let s_field = unify(
                        &apply_subst(&subst, &t_val),
                        &apply_subst(&subst, &field_ty),
                    )
                    .map_err(|e| mismatch_with_span(e, value_span))?;
                    subst = compose_subst(&s_field, &subst);
                    preds = apply_subst_preds(&s_field, &preds);
                    preds.push(Predicate::HasField {
                        label: field_name.clone(),
                        record_ty: apply_subst(&subst, &t_record),
                        field_ty: apply_subst(&subst, &field_ty),
                    });
                }

                let ty = apply_subst(&subst, &t_record);
                self.record_expr_type(expr.span(), ty.clone());
                Ok((subst.clone(), ty, apply_subst_preds(&subst, &preds)))
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
                let (mut t_func, mut preds) = self.instantiate_with_preds(scheme);
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
                                    expected_from_span: mismatch.expected_from_span,
                                    expected_from_message: mismatch.expected_from_message,
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
                    preds = apply_subst_preds(&s_unify, &preds);
                    let (s_pred, solved_preds) =
                        self.solve_predicates(&apply_subst_env(&subst, env), &preds)?;
                    subst = compose_subst(&s_pred, &subst);
                    preds = apply_subst_preds(&s_pred, &solved_preds);
                    let result_ty = apply_subst(&subst, &ret);
                    self.record_expr_type(expr.span(), result_ty.clone());
                    return Ok((subst.clone(), result_ty, apply_subst_preds(&subst, &preds)));
                }
                for arg in args {
                    let (s_arg, t_arg, arg_preds) =
                        self.infer(&apply_subst_env(&subst, env), arg)?;
                    subst = compose_subst(&s_arg, &subst);
                    preds = apply_subst_preds(&s_arg, &preds);
                    preds.extend(arg_preds);
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
                                    expected_from_span: mismatch.expected_from_span,
                                    expected_from_message: mismatch.expected_from_message,
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
                    preds = apply_subst_preds(&s_unify, &preds);
                    prev_span = Some(arg.span());
                    t_func = apply_subst(&subst, &ret);
                }
                let (s_pred, solved_preds) =
                    self.solve_predicates(&apply_subst_env(&subst, env), &preds)?;
                subst = compose_subst(&s_pred, &subst);
                preds = apply_subst_preds(&s_pred, &solved_preds);
                t_func = apply_subst(&s_pred, &t_func);

                self.record_expr_type(expr.span(), t_func.clone());
                Ok((subst.clone(), t_func, apply_subst_preds(&subst, &preds)))
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
                        preds: vec![],
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
                        });
                    }
                }
                let expected_by_pattern = apply_subst(&subst, &con_ty);
                let s_unify = unify(&expected_by_pattern, expected).map_err(|e| match e {
                    TypeError::Mismatch { mismatch } => TypeError::Mismatch {
                        mismatch: MismatchTypeError {
                            expected: mismatch.expected,
                            found: mismatch.found,
                            span: mismatch.span,
                            prior_span: mismatch.prior_span,
                            expected_from_span: Some(span.clone()),
                            expected_from_message: Some(format!(
                                "constructor pattern `{name}` constrains this to `{}`",
                                type_display(&expected_by_pattern)
                            )),
                            arg_ty: mismatch.arg_ty,
                            expected_arg_ty: mismatch.expected_arg_ty,
                            callee_name: mismatch.callee_name,
                            callee_span: mismatch.callee_span,
                            callee_def: mismatch.callee_def,
                            callee_ty: mismatch.callee_ty,
                        }
                        .into(),
                    },
                    other => other,
                })?;
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

                    let accessor_scheme = lookup_record_accessor(env, name, field_name)
                        .ok_or_else(|| {
                            TypeError::UnboundVariable(format!(":{field_name}"), field_span.clone())
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
    /// Type-check a module worth of declarations.
    ///
    /// Top-level value declarations are treated as a single recursive group so
    /// declaration order does not matter for references between top-level
    /// functions.
    pub fn check_program(
        &mut self,
        env: &mut TypeEnv,
        decls: &[crate::ast::Declaration],
        file_id: usize,
    ) -> Result<Rc<Type>, Box<(TypeError, crate::ast::Expr)>> {
        use crate::ast::{Declaration, Expr};

        let mut last_ty = Type::unit();

        let mut top_level_let_decl_indices = Vec::new();
        let mut inferred_top_level_raw_by_decl: HashMap<usize, Rc<Type>> = HashMap::new();

        // Pass 1: seed order-independent top-level declarations.
        for (decl_idx, decl) in decls.iter().enumerate() {
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
                    } else if let crate::ast::TypeDecl::Record {
                        name,
                        fields,
                        params,
                        ..
                    } = type_decl
                    {
                        self.register_record_type_info(
                            name.clone(),
                            fields.iter().map(|(field, _)| field.clone()).collect(),
                            params.len(),
                        );
                    }
                    for (name, scheme) in
                        constructor_schemes_with_aliases(type_decl, &self.qualified_type_aliases)
                    {
                        // Plain field accessor keys (e.g. `:state`) are overloaded
                        // across records. Preserve first declaration order rather than
                        // letting later record declarations overwrite earlier ones.
                        let is_plain_field_accessor =
                            name.starts_with(':') && !name[1..].contains(':');
                        if is_plain_field_accessor && env.contains_key(&name) {
                            continue;
                        }
                        env.insert(name, scheme);
                    }
                }
                Declaration::ExternLet {
                    name,
                    name_span,
                    is_nullary,
                    ty,
                    ..
                } => {
                    let mut scheme = type_sig_to_scheme(ty, &self.qualified_type_aliases);
                    if *is_nullary {
                        // Nullary externs are declared as Unit -> ReturnType in source and
                        // lowered to a 0-arity function internally, so wrap back here.
                        scheme.ty = Rc::new(Type::Fun(Type::unit(), scheme.ty));
                    }
                    self.record_expr_type(name_span.clone(), scheme.ty.clone());
                    env.insert(name.clone(), scheme);
                }
                Declaration::Expression(Expr::LetFunc {
                    name,
                    args,
                    name_span,
                    ..
                }) => {
                    top_level_let_decl_indices.push(decl_idx);
                    self.value_def_spans
                        .insert(name.clone(), (file_id, name_span.clone()));

                    // Predeclare a monomorphic placeholder for recursive-group inference.
                    let ret_ty = self.fresh();
                    let provisional = args
                        .iter()
                        .map(|_| self.fresh())
                        .rev()
                        .fold(ret_ty, |acc, arg| Type::fun(arg, acc));
                    let env_ty = if args.is_empty() {
                        Type::fun(Type::unit(), provisional)
                    } else {
                        provisional
                    };
                    self.record_expr_type(name_span.clone(), env_ty.clone());
                    env.insert(
                        name.clone(),
                        Scheme {
                            vars: vec![],
                            preds: vec![],
                            ty: env_ty,
                        },
                    );
                }
                Declaration::Expression(_)
                | Declaration::ExternType { .. }
                | Declaration::Use { .. } => {}
                Declaration::Test { .. } => {}
            }
        }

        // Pass 2: infer each top-level let body against the shared recursive group.
        for decl_idx in &top_level_let_decl_indices {
            let Some(Declaration::Expression(expr)) = decls.get(*decl_idx) else {
                continue;
            };
            let Expr::LetFunc { name, args, .. } = expr else {
                continue;
            };

            match self.infer(env, expr) {
                Ok((s, mut ty, preds)) => {
                    let mut preds = apply_subst_preds(&s, &preds);
                    self.apply_expr_type_subst(&s);
                    *env = apply_subst_env(&s, env);
                    let (s_pred, solved_preds) = match self.solve_predicates(env, &preds) {
                        Ok(result) => result,
                        Err(error) => return Err(Box::new((error, expr.clone()))),
                    };
                    self.apply_expr_type_subst(&s_pred);
                    *env = apply_subst_env(&s_pred, env);
                    ty = apply_subst(&s_pred, &ty);
                    preds = apply_subst_preds(&s_pred, &solved_preds);

                    let mut env_ty = ty.clone();
                    let declared_ty = env
                        .get(name)
                        .map(|scheme| scheme.ty.clone())
                        .expect("predeclared top-level function should exist in env");
                    let s_decl = unify(&declared_ty, &env_ty).map_err(|error| {
                        Box::new((mismatch_with_span(error, expr.span()), expr.clone()))
                    })?;
                    self.apply_expr_type_subst(&s_decl);
                    *env = apply_subst_env(&s_decl, env);
                    ty = apply_subst(&s_decl, &ty);
                    env_ty = apply_subst(&s_decl, &env_ty);
                    preds = apply_subst_preds(&s_decl, &preds);

                    let mut generalize_env = env.clone();
                    generalize_env.remove(name);
                    let scheme = generalize(&generalize_env, &env_ty, &preds);
                    env.insert(name.clone(), scheme);
                    let raw_decl_ty = if args.is_empty() {
                        match ty.as_ref() {
                            Type::Fun(arg, ret) if arg.as_ref() == Type::unit().as_ref() => {
                                ret.clone()
                            }
                            _ => ty.clone(),
                        }
                    } else {
                        ty.clone()
                    };
                    inferred_top_level_raw_by_decl.insert(*decl_idx, raw_decl_ty);
                }
                Err(error) => return Err(Box::new((error, expr.clone()))),
            }
        }

        // Pass 4: check remaining top-level declarations in source order and compute last_ty.
        for (decl_idx, decl) in decls.iter().enumerate() {
            match decl {
                Declaration::Type(_)
                | Declaration::ExternLet { .. }
                | Declaration::ExternType { .. }
                | Declaration::Use { .. } => {}
                Declaration::Expression(expr) => {
                    if matches!(expr, Expr::LetFunc { .. }) {
                        if let Some(ty) = inferred_top_level_raw_by_decl.get(&decl_idx) {
                            last_ty = ty.clone();
                        }
                        continue;
                    }

                    match self.infer(env, expr) {
                        Ok((s, ty, preds)) => {
                            let mut ty = ty;
                            let mut preds = apply_subst_preds(&s, &preds);
                            self.apply_expr_type_subst(&s);
                            *env = apply_subst_env(&s, env);
                            let (s_pred, solved_preds) = match self.solve_predicates(env, &preds) {
                                Ok(result) => result,
                                Err(error) => return Err(Box::new((error, expr.clone()))),
                            };
                            self.apply_expr_type_subst(&s_pred);
                            *env = apply_subst_env(&s_pred, env);
                            ty = apply_subst(&s_pred, &ty);
                            preds = apply_subst_preds(&s_pred, &solved_preds);
                            if let Some(pred) = preds.first() {
                                return Err(Box::new((
                                    self.unresolved_predicate_error(pred),
                                    expr.clone(),
                                )));
                            }
                            last_ty = ty;
                        }
                        Err(error) => return Err(Box::new((error, expr.clone()))),
                    }
                }
                Declaration::Test { name, body, span } => {
                    // Result is ['a 'e] — ok first, error second
                    // test bodies must return Result Unit String (Ok=Unit, Error=String)
                    let expected = Rc::new(Type::Con(
                        "Result".into(),
                        vec![Type::unit(), Type::string()],
                    ));
                    match self.infer(env, body) {
                        Ok((s, ty, preds)) => {
                            self.apply_expr_type_subst(&s);
                            *env = apply_subst_env(&s, env);
                            let preds = apply_subst_preds(&s, &preds);
                            let (s_pred, solved_preds) = match self.solve_predicates(env, &preds) {
                                Ok(result) => result,
                                Err(error) => return Err(Box::new((error, *body.clone()))),
                            };
                            self.apply_expr_type_subst(&s_pred);
                            *env = apply_subst_env(&s_pred, env);
                            if let Some(pred) = solved_preds.first() {
                                return Err(Box::new((
                                    self.unresolved_predicate_error(pred),
                                    *body.clone(),
                                )));
                            }
                            let ty = apply_subst(&s_pred, &apply_subst(&s, &ty));
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
