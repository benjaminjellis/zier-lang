use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    Int,
    Bool,
    String,
    /// A function type: (Arg1, Arg2) -> Return
    Func(Vec<Rc<Type>>, Rc<Type>),
    /// A named type (like 'Option' or 'MyType') with potential generic arguments
    Constructor(String, Vec<Rc<Type>>),
    /// A type variable used during inference (e.g., T0, T1)
    Var(u64),
}

/// A "Scheme" is a type that may contain polymorphic variables (forall 'a. 'a -> 'a)
#[derive(Debug, Clone)]
pub struct Scheme {
    pub vars: Vec<u64>,
    pub ty: Rc<Type>,
}
