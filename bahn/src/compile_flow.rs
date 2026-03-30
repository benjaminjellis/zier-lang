use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use eyre::Context;
use mondc::session::CompileReport;

pub(crate) struct CompileUnit<'a> {
    pub(crate) output_module_name: &'a str,
    pub(crate) source: &'a str,
    pub(crate) source_label: String,
}

pub(crate) struct CompileOutput {
    pub(crate) output_module_name: String,
    report: CompileReport,
}

impl CompileOutput {
    fn had_errors(&self) -> bool {
        self.report.has_errors() || self.report.output.is_none()
    }

    pub(crate) fn erl_source(&self) -> Option<&String> {
        self.report.output.as_ref()
    }
}

fn compile_unit_with_session(
    unit: &CompileUnit<'_>,
    pipeline_session: &mut mondc::CompileSession<'_>,
    emit_warnings: bool,
) -> CompileOutput {
    let report = pipeline_session.compile_module_report(mondc::ModuleInput {
        output_module_name: unit.output_module_name,
        source: unit.source,
        source_path: &unit.source_label,
    });
    mondc::session::emit_compile_report_with_color(
        &report,
        emit_warnings,
        crate::ui::diagnostic_color_choice(),
    );

    CompileOutput {
        output_module_name: unit.output_module_name.to_string(),
        report,
    }
}

pub(crate) fn compile_units(
    units: &[CompileUnit<'_>],
    module_exports: &HashMap<String, Vec<String>>,
    analysis: &mondc::ProjectAnalysis,
    emit_warnings: bool,
) -> (Vec<CompileOutput>, bool) {
    let pipeline = mondc::CompilePipeline::new(mondc::PassContext {
        visible_exports: module_exports,
        analysis,
    });
    let mut pipeline_session = pipeline.session();
    let mut had_error = false;
    let mut outputs = Vec::with_capacity(units.len());

    for unit in units {
        let output = compile_unit_with_session(unit, &mut pipeline_session, emit_warnings);
        had_error |= output.had_errors();
        outputs.push(output);
    }

    (outputs, had_error)
}

pub(crate) fn write_erl_output(
    erl_dir: &Path,
    output_module_name: &str,
    erl_source: &str,
) -> eyre::Result<PathBuf> {
    let erl_path = erl_dir.join(format!("{output_module_name}.erl"));
    std::fs::write(&erl_path, erl_source)
        .with_context(|| format!("could not write {}", erl_path.display()))?;
    Ok(erl_path)
}

pub(crate) fn dependency_module_exports(
    dependency_mods: &[(String, String, String)],
) -> HashMap<String, Vec<String>> {
    dependency_mods
        .iter()
        .map(|(user_name, _, source)| (user_name.clone(), mondc::exported_names(source)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_units_returns_erl_for_valid_single_module() {
        let module_exports = HashMap::new();
        let analysis = mondc::build_project_analysis(&[], &[]).expect("analysis");
        let units = vec![CompileUnit {
            output_module_name: "main",
            source: "(let main {} 1)",
            source_label: "main.mond".to_string(),
        }];

        let (outputs, had_error) = compile_units(&units, &module_exports, &analysis, true);
        assert_eq!(outputs.len(), 1);
        assert!(!had_error);
        let output = &outputs[0];
        assert!(!output.had_errors());
        assert!(output.erl_source().is_some());
    }

    #[test]
    fn compile_units_reports_errors_for_invalid_single_module() {
        let module_exports = HashMap::new();
        let analysis = mondc::build_project_analysis(&[], &[]).expect("analysis");
        let units = vec![CompileUnit {
            output_module_name: "main",
            source: "(let main {} unknown)",
            source_label: "main.mond".to_string(),
        }];

        let (outputs, had_error) = compile_units(&units, &module_exports, &analysis, true);
        assert_eq!(outputs.len(), 1);
        assert!(had_error);
        let output = &outputs[0];
        assert!(output.had_errors());
        assert!(output.erl_source().is_none());
    }

    #[test]
    fn dependency_module_exports_scans_exported_names() {
        let dependency_mods = vec![(
            "io".to_string(),
            "mond_io".to_string(),
            "(pub let println {x} x)".to_string(),
        )];
        let exports = dependency_module_exports(&dependency_mods);
        assert_eq!(exports.get("io"), Some(&vec!["println".to_string()]));
    }
}
