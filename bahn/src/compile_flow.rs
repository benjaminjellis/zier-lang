use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::Arc,
};

use codespan_reporting::diagnostic::Diagnostic;
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

struct OwnedCompileUnit {
    output_module_name: String,
    source: String,
    source_label: String,
}

impl OwnedCompileUnit {
    fn from_unit(unit: &CompileUnit<'_>) -> Self {
        Self {
            output_module_name: unit.output_module_name.to_string(),
            source: unit.source.to_string(),
            source_label: unit.source_label.clone(),
        }
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

fn compile_owned_unit(
    unit: OwnedCompileUnit,
    analysis: &mondc::ProjectAnalysis,
    compile_target: mondc::CompileTarget,
) -> CompileOutput {
    let pipeline = mondc::CompilePipeline::new(mondc::PassContext {
        visible_exports: &analysis.module_exports,
        analysis,
        compile_target,
    });
    let report = pipeline.compile_module_report(mondc::ModuleInput {
        output_module_name: &unit.output_module_name,
        source: &unit.source,
        source_path: &unit.source_label,
    });

    CompileOutput {
        output_module_name: unit.output_module_name,
        report,
    }
}

fn compile_units_sequential(
    units: &[CompileUnit<'_>],
    analysis: &mondc::ProjectAnalysis,
    emit_warnings: bool,
    compile_target: mondc::CompileTarget,
) -> (Vec<CompileOutput>, bool) {
    let pipeline = mondc::CompilePipeline::new(mondc::PassContext {
        visible_exports: &analysis.module_exports,
        analysis,
        compile_target,
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

fn should_parallelize(units_len: usize) -> bool {
    let workers = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1);
    units_len > workers
}

fn worker_failure_report(output_module_name: &str, err: tokio::task::JoinError) -> CompileReport {
    CompileReport {
        output: None,
        files: codespan_reporting::files::SimpleFiles::new(),
        diagnostics: vec![Diagnostic::error().with_message(format!(
            "internal compiler worker failed while compiling `{output_module_name}`: {err}"
        ))],
    }
}

fn spawn_compile_task(
    set: &mut tokio::task::JoinSet<(usize, CompileOutput)>,
    task_meta: &mut HashMap<tokio::task::Id, (usize, String)>,
    units: &[CompileUnit<'_>],
    idx: usize,
    analysis: &Arc<mondc::ProjectAnalysis>,
    compile_target: mondc::CompileTarget,
) {
    let owned = OwnedCompileUnit::from_unit(&units[idx]);
    let output_module_name = owned.output_module_name.clone();
    let analysis = Arc::clone(analysis);
    let abort =
        set.spawn_blocking(move || (idx, compile_owned_unit(owned, &analysis, compile_target)));
    task_meta.insert(abort.id(), (idx, output_module_name));
}

pub(crate) async fn compile_units(
    units: &[CompileUnit<'_>],
    analysis: Arc<mondc::ProjectAnalysis>,
    emit_warnings: bool,
    compile_target: mondc::CompileTarget,
) -> (Vec<CompileOutput>, bool) {
    if !should_parallelize(units.len()) {
        return compile_units_sequential(units, &analysis, emit_warnings, compile_target);
    }

    let max_in_flight = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1);
    let mut pending = 0..units.len();
    let mut set = tokio::task::JoinSet::new();
    let mut task_meta: HashMap<tokio::task::Id, (usize, String)> = HashMap::new();
    let mut completed: BTreeMap<usize, CompileOutput> = BTreeMap::new();
    let mut next_emit_idx = 0usize;
    let mut dangling_failures: Vec<CompileOutput> = Vec::new();
    let mut had_error = false;
    let mut outputs = Vec::with_capacity(units.len());

    while set.len() < max_in_flight {
        let Some(idx) = pending.next() else {
            break;
        };
        spawn_compile_task(
            &mut set,
            &mut task_meta,
            units,
            idx,
            &analysis,
            compile_target,
        );
    }

    while let Some(result) = set.join_next_with_id().await {
        match result {
            Ok((task_id, (idx, output))) => {
                task_meta.remove(&task_id);
                completed.insert(idx, output);
            }
            Err(err) => {
                let task_id = err.id();
                let (idx, output_module_name) = task_meta
                    .remove(&task_id)
                    .unwrap_or_else(|| (usize::MAX, format!("unknown-module-for-task-{task_id}")));
                let output = CompileOutput {
                    output_module_name: output_module_name.clone(),
                    report: worker_failure_report(&output_module_name, err),
                };
                if idx == usize::MAX {
                    mondc::session::emit_compile_report_with_color(
                        &output.report,
                        emit_warnings,
                        crate::ui::diagnostic_color_choice(),
                    );
                    had_error |= output.had_errors();
                    dangling_failures.push(output);
                } else {
                    completed.insert(idx, output);
                }
            }
        }

        while let Some(output) = completed.remove(&next_emit_idx) {
            mondc::session::emit_compile_report_with_color(
                &output.report,
                emit_warnings,
                crate::ui::diagnostic_color_choice(),
            );
            had_error |= output.had_errors();
            outputs.push(output);
            next_emit_idx += 1;
        }

        while set.len() < max_in_flight {
            let Some(idx) = pending.next() else {
                break;
            };
            spawn_compile_task(
                &mut set,
                &mut task_meta,
                units,
                idx,
                &analysis,
                compile_target,
            );
        }
    }

    for (_, output) in completed {
        mondc::session::emit_compile_report_with_color(
            &output.report,
            emit_warnings,
            crate::ui::diagnostic_color_choice(),
        );
        had_error |= output.had_errors();
        outputs.push(output);
    }
    outputs.extend(dangling_failures);

    (outputs, had_error)
}

pub(crate) fn write_erl_output(
    erl_dir: &Path,
    output_module_name: &str,
    erl_source: &str,
) -> eyre::Result<PathBuf> {
    let erl_path = erl_dir.join(format!("{output_module_name}.erl"));
    if let Ok(existing) = std::fs::read_to_string(&erl_path)
        && existing == erl_source
    {
        return Ok(erl_path);
    }
    std::fs::write(&erl_path, erl_source)
        .with_context(|| format!("could not write {}", erl_path.display()))?;
    Ok(erl_path)
}

pub(crate) fn dependency_module_exports(
    dependency_mods: &[mondc::DependencyModuleSource],
) -> HashMap<String, Vec<String>> {
    dependency_mods
        .iter()
        .map(|module| {
            (
                module.module_name.clone(),
                mondc::exported_names(module.source.as_str()),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_units_returns_erl_for_valid_single_module() {
        let analysis = mondc::build_project_analysis(&[], &[]).expect("analysis");
        let units = vec![CompileUnit {
            output_module_name: "main",
            source: "(let main {} 1)",
            source_label: "main.mond".to_string(),
        }];

        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let (outputs, had_error) = runtime.block_on(compile_units(
            &units,
            Arc::new(analysis),
            true,
            mondc::CompileTarget::Dev,
        ));
        assert_eq!(outputs.len(), 1);
        assert!(!had_error);
        let output = &outputs[0];
        assert!(!output.had_errors());
        assert!(output.erl_source().is_some());
    }

    #[test]
    fn compile_units_reports_errors_for_invalid_single_module() {
        let analysis = mondc::build_project_analysis(&[], &[]).expect("analysis");
        let units = vec![CompileUnit {
            output_module_name: "main",
            source: "(let main {} unknown)",
            source_label: "main.mond".to_string(),
        }];

        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let (outputs, had_error) = runtime.block_on(compile_units(
            &units,
            Arc::new(analysis),
            true,
            mondc::CompileTarget::Dev,
        ));
        assert_eq!(outputs.len(), 1);
        assert!(had_error);
        let output = &outputs[0];
        assert!(output.had_errors());
        assert!(output.erl_source().is_none());
    }

    #[test]
    fn dependency_module_exports_scans_exported_names() {
        let dependency_mods = vec![mondc::DependencyModuleSource {
            package_name: "std".to_string(),
            module_name: "io".to_string(),
            erlang_name: "mond_io".to_string(),
            source: "(pub let println {x} x)".to_string(),
            source_relpath: "src/io.mond".to_string(),
        }];
        let exports = dependency_module_exports(&dependency_mods);
        assert_eq!(exports.get("io"), Some(&vec!["println".to_string()]));
    }

    #[test]
    fn compile_units_preserves_input_order() {
        let analysis = mondc::build_project_analysis(&[], &[]).expect("analysis");
        let units = vec![
            CompileUnit {
                output_module_name: "z_last",
                source: "(let main {} 1)",
                source_label: "z_last.mond".to_string(),
            },
            CompileUnit {
                output_module_name: "a_first",
                source: "(let main {} 2)",
                source_label: "a_first.mond".to_string(),
            },
        ];

        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let (outputs, had_error) = runtime.block_on(compile_units(
            &units,
            Arc::new(analysis),
            true,
            mondc::CompileTarget::Dev,
        ));
        assert!(!had_error);
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0].output_module_name, "z_last");
        assert_eq!(outputs[1].output_module_name, "a_first");
    }

    #[test]
    fn compile_units_continue_after_errors() {
        let analysis = mondc::build_project_analysis(&[], &[]).expect("analysis");
        let units = vec![
            CompileUnit {
                output_module_name: "broken",
                source: "(let main {} unknown)",
                source_label: "broken.mond".to_string(),
            },
            CompileUnit {
                output_module_name: "ok",
                source: "(let main {} 1)",
                source_label: "ok.mond".to_string(),
            },
        ];

        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let (outputs, had_error) = runtime.block_on(compile_units(
            &units,
            Arc::new(analysis),
            true,
            mondc::CompileTarget::Dev,
        ));
        assert!(had_error);
        assert_eq!(outputs.len(), 2);
        assert!(outputs[0].had_errors());
        assert!(outputs[0].erl_source().is_none());
        assert!(!outputs[1].had_errors());
        assert!(outputs[1].erl_source().is_some());
    }

    #[test]
    fn compile_units_parallel_queue_preserves_order_and_error_state() {
        let analysis = mondc::build_project_analysis(&[], &[]).expect("analysis");
        let workers = std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(1);
        let total_units = workers + 1;

        let mut sources: Vec<String> = Vec::with_capacity(total_units);
        let mut labels: Vec<String> = Vec::with_capacity(total_units);
        let mut units: Vec<CompileUnit<'_>> = Vec::with_capacity(total_units);
        for idx in 0..total_units {
            let module_name = format!("m{idx}");
            let source = if idx == workers / 2 {
                "(let main {} unknown)".to_string()
            } else {
                "(let main {} 1)".to_string()
            };
            let label = format!("{module_name}.mond");
            sources.push(source);
            labels.push(label);
        }
        for idx in 0..total_units {
            units.push(CompileUnit {
                output_module_name: &labels[idx][..labels[idx].len() - 5],
                source: &sources[idx],
                source_label: labels[idx].clone(),
            });
        }

        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let (outputs, had_error) = runtime.block_on(compile_units(
            &units,
            Arc::new(analysis),
            true,
            mondc::CompileTarget::Dev,
        ));

        assert_eq!(outputs.len(), total_units);
        assert!(had_error);
        for (idx, output) in outputs.iter().enumerate() {
            assert_eq!(output.output_module_name, format!("m{idx}"));
        }
        assert!(outputs[workers / 2].had_errors());
    }
}
