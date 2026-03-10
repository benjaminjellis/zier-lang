pub mod ast;
pub mod codegen;
pub mod ir;
pub mod lexer;
pub mod lower;
pub mod project;
pub mod resolve;
pub mod session;
pub mod sexpr;
pub mod typecheck;

mod compiler;
mod query;
mod warnings;

pub use compiler::{
    compile_with_imports, compile_with_imports_in_session, compile_with_imports_report,
};
pub use project::{
    ProjectAnalysis, ResolvedImports, build_project_analysis, ordered_module_sources,
    referenced_modules, resolve_imports_for_source, std_modules_from_sources,
};
pub use query::{
    exported_names, exported_type_decls, has_nullary_main, infer_module_bindings,
    infer_module_exports, infer_module_expr_types, pub_reexports, test_declarations, used_modules,
};

#[cfg(test)]
pub(crate) use compiler::compile;

#[cfg(test)]
mod tests;
