use std::collections::HashMap;

use crate::{
    ast, compiler,
    project::{ProjectAnalysis, ResolvedImports},
    resolve_imports_for_source, session, typecheck,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompileTarget {
    Dev,
    Release,
}

#[derive(Clone, Copy)]
pub struct PassContext<'a> {
    pub visible_exports: &'a HashMap<String, Vec<String>>,
    pub analysis: &'a ProjectAnalysis,
    pub compile_target: CompileTarget,
}

pub struct ModuleInput<'a> {
    pub output_module_name: &'a str,
    pub source: &'a str,
    pub source_path: &'a str,
}

pub struct ResolvedModuleInput<'a> {
    pub output_module_name: &'a str,
    pub source: &'a str,
    pub source_path: &'a str,
    pub imports: HashMap<String, String>,
    pub module_aliases: HashMap<String, String>,
    pub imported_type_decls: Vec<ast::TypeDecl>,
    pub debug_type_decls: Vec<ast::TypeDecl>,
    pub imported_extern_types: Vec<ast::ExternTypeInfo>,
    pub imported_field_indices: HashMap<(String, String), usize>,
    pub imported_private_records: HashMap<String, Vec<String>>,
    pub imported_schemes: typecheck::TypeEnv,
}

pub struct CompilePipeline<'a> {
    pass_context: PassContext<'a>,
}

pub struct CompileSession<'a> {
    pass_context: PassContext<'a>,
    compiler_session: session::CompilerSession,
}

fn resolve_module_with_context<'a, 'b>(
    pass_context: PassContext<'a>,
    module: ModuleInput<'b>,
) -> ResolvedModuleInput<'b> {
    let ResolvedImports {
        imports,
        imported_schemes,
        imported_type_decls,
        debug_type_decls,
        imported_extern_types,
        imported_field_indices,
        imported_private_records,
        module_aliases,
        ..
    } = resolve_imports_for_source(
        module.source,
        pass_context.visible_exports,
        pass_context.analysis,
    );

    ResolvedModuleInput {
        output_module_name: module.output_module_name,
        source: module.source,
        source_path: module.source_path,
        imports,
        module_aliases,
        imported_type_decls,
        debug_type_decls,
        imported_extern_types,
        imported_field_indices,
        imported_private_records,
        imported_schemes,
    }
}

fn compile_resolved_with_session(
    pass_context: PassContext<'_>,
    compiler_session: &mut session::CompilerSession,
    resolved: ResolvedModuleInput<'_>,
) -> session::CompileReport {
    compiler::compile_with_imports_in_session_with_target_and_private_records(
        compiler_session,
        compiler::CompileWithImportsInput {
            module_name: resolved.output_module_name,
            source: resolved.source,
            source_path: resolved.source_path,
            imports: resolved.imports,
            module_exports: pass_context.visible_exports,
            module_aliases: resolved.module_aliases,
            imported_type_decls: &resolved.imported_type_decls,
            debug_type_decls: &resolved.debug_type_decls,
            imported_extern_types: &resolved.imported_extern_types,
            imported_field_indices: &resolved.imported_field_indices,
            imported_private_records: &resolved.imported_private_records,
            imported_schemes: &resolved.imported_schemes,
            compile_target: pass_context.compile_target,
        },
    )
}

impl<'a> CompilePipeline<'a> {
    pub fn new(pass_context: PassContext<'a>) -> Self {
        Self { pass_context }
    }

    pub fn session(&self) -> CompileSession<'a> {
        CompileSession::new(self.pass_context)
    }

    pub fn session_with_options(&self, options: session::SessionOptions) -> CompileSession<'a> {
        CompileSession::with_options(self.pass_context, options)
    }

    pub fn resolve_module<'b>(&self, module: ModuleInput<'b>) -> ResolvedModuleInput<'b> {
        resolve_module_with_context(self.pass_context, module)
    }

    pub fn compile_resolved_module_report(
        &self,
        resolved: ResolvedModuleInput<'_>,
    ) -> session::CompileReport {
        let mut compiler_session = session::CompilerSession::default();
        compile_resolved_with_session(self.pass_context, &mut compiler_session, resolved)
    }

    pub fn compile_module_report(&self, module: ModuleInput<'_>) -> session::CompileReport {
        let resolved = self.resolve_module(module);
        self.compile_resolved_module_report(resolved)
    }
}

impl<'a> CompileSession<'a> {
    pub fn new(pass_context: PassContext<'a>) -> Self {
        Self::with_options(pass_context, session::SessionOptions::default())
    }

    pub fn with_options(pass_context: PassContext<'a>, options: session::SessionOptions) -> Self {
        Self {
            pass_context,
            compiler_session: session::CompilerSession::new(options),
        }
    }

    pub fn compiler_session(&self) -> &session::CompilerSession {
        &self.compiler_session
    }

    pub fn compiler_session_mut(&mut self) -> &mut session::CompilerSession {
        &mut self.compiler_session
    }

    pub fn resolve_module<'b>(&self, module: ModuleInput<'b>) -> ResolvedModuleInput<'b> {
        resolve_module_with_context(self.pass_context, module)
    }

    pub fn compile_resolved_module_report(
        &mut self,
        resolved: ResolvedModuleInput<'_>,
    ) -> session::CompileReport {
        compile_resolved_with_session(self.pass_context, &mut self.compiler_session, resolved)
    }

    pub fn compile_module_report(&mut self, module: ModuleInput<'_>) -> session::CompileReport {
        let resolved = self.resolve_module(module);
        self.compile_resolved_module_report(resolved)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{CompilePipeline, CompileTarget, ModuleInput, PassContext};

    #[test]
    fn compile_pipeline_compiles_single_module() {
        let analysis = crate::build_project_analysis(&[], &[]).expect("analysis");
        let visible_exports = HashMap::new();
        let pipeline = CompilePipeline::new(PassContext {
            visible_exports: &visible_exports,
            analysis: &analysis,
            compile_target: CompileTarget::Dev,
        });

        let report = pipeline.compile_module_report(ModuleInput {
            output_module_name: "main",
            source: "(let main {} 1)",
            source_path: "main.mond",
        });

        assert!(!report.has_errors());
        assert!(report.output.is_some());
    }

    #[test]
    fn compile_pipeline_reports_unbound_variable() {
        let analysis = crate::build_project_analysis(&[], &[]).expect("analysis");
        let visible_exports = HashMap::new();
        let pipeline = CompilePipeline::new(PassContext {
            visible_exports: &visible_exports,
            analysis: &analysis,
            compile_target: CompileTarget::Dev,
        });

        let report = pipeline.compile_module_report(ModuleInput {
            output_module_name: "main",
            source: "(let main {} unknown)",
            source_path: "main.mond",
        });

        assert!(report.has_errors());
        assert!(report.output.is_none());
    }

    #[test]
    fn compile_session_reuses_symbol_table_cache_across_modules() {
        let analysis = crate::build_project_analysis(&[], &[]).expect("analysis");
        let visible_exports = HashMap::new();
        let pipeline = CompilePipeline::new(PassContext {
            visible_exports: &visible_exports,
            analysis: &analysis,
            compile_target: CompileTarget::Dev,
        });
        let mut session = pipeline.session();

        let first = session.compile_module_report(ModuleInput {
            output_module_name: "one",
            source: "(let main {} 1)",
            source_path: "one.mond",
        });
        let second = session.compile_module_report(ModuleInput {
            output_module_name: "two",
            source: "(let main {} 2)",
            source_path: "two.mond",
        });

        assert!(!first.has_errors());
        assert!(!second.has_errors());
        assert!(session.compiler_session().caches.symbol_table.is_some());
    }
}
