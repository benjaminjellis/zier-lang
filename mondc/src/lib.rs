pub mod ast;
pub mod codegen;
pub mod hir;
pub mod ir;
pub mod lexer;
pub mod lower;
pub mod pipeline;
pub mod project;
pub mod resolve;
pub mod session;
pub mod sexpr;
pub mod typecheck;

mod compiler;
mod query;
mod typing;
mod warnings;

pub use compiler::{
    CompileWithImportsInput, compile_with_imports, compile_with_imports_in_session,
    compile_with_imports_in_session_with_private_records, compile_with_imports_report,
    compile_with_imports_report_with_private_records,
};
pub use pipeline::{
    CompilePipeline, CompileSession, CompileTarget, ModuleInput, PassContext, ResolvedModuleInput,
};
pub use project::{
    DependencyModuleSource, ProjectAnalysis, ResolvedImports, alias_package_root_module,
    build_project_analysis, build_project_analysis_with_modules,
    build_project_analysis_with_modules_and_package, dependency_erlang_module_name,
    external_modules_from_package_sources, external_modules_from_sources,
    load_dependency_module_sources_from_checkout, load_dependency_modules_from_checkout,
    ordered_module_sources, reachable_module_sources, referenced_modules,
    resolve_imports_for_source,
};
pub use query::{
    exported_extern_types, exported_names, exported_type_decls, has_nullary_main,
    infer_module_bindings, infer_module_exports, infer_module_expr_types, pub_reexports,
    test_declarations, used_modules,
};

#[cfg(test)]
pub(crate) use compiler::compile;

#[cfg(test)]
mod tests;
