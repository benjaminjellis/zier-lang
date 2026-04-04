mod checker;
mod core;
mod env;

#[cfg(test)]
mod tests;

pub use checker::TypeChecker;
pub use core::{
    MismatchTypeError, OccursCheckTypeError, Predicate, Scheme, Substitution, Type, TypeEnv,
    TypeError, apply_subst, apply_subst_env, compose_subst, normalize_env_type_aliases,
    normalize_scheme_type_aliases, predicate_display, scheme_display, type_display, unify,
};
pub use env::{constructor_schemes, constructor_schemes_with_aliases, import_env, primitive_env};
