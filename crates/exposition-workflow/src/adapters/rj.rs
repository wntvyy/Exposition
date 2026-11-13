//! `restrictedR`, `nativeR`, `restrictedJulia`, `nativeJulia`: R and Julia
//! workflow batches through the research service's [`BatchRunner`] language
//! methods (M9; the Python batches are [`super::batch`]).
//!
//! The same contract as the Python batches: declared inputs are exactly the
//! values bound to `data` (and `script`, which becomes the batch code); an
//! inline payload is the code otherwise; port `p` is realized by the file
//! `p.<ext>` in the output area and every output is validated by the runner.
//! Language rules on top:
//! - the code is the step's language: a script input must have the
//!   language's extension (`.R`/`.r`, `.jl`) and an inline payload must be
//!   of the language, checked before anything is staged;
//! - a restricted step runs only in its language's configured image, pinned
//!   to the identity the run's plan approved (no approved identity: the step
//!   is refused); the research runner refuses another identity and never
//!   falls back to native execution or to another language's image;
//! - a native step runs the discovered `Rscript`/`julia` with the user's
//!   account privileges (the runner checks folder trust before dispatch).

use std::path::Path;
use std::sync::Arc;

use exposition_core::artifact::{ArtifactRecord, RunStatus};
use exposition_graph::OperationRef;
use exposition_graph::workflow::StepPayload;
use exposition_graph::workflow::registry::{self, CodeLanguage, PortType};
use exposition_research::batch::{
    BatchCode, BatchControl, BatchInput, BatchLanguage, BatchMode, BatchOwner, BatchPort, BatchRequest, BatchRunner,
};

use super::{Adapters, StepAdapter, StepControl, StepFailure, StepRequest, StepResult, bare_hash, bounded_tail};
use crate::model::{MAX_ATTEMPT_LOG_CHARS, WorkflowOutputValue, WorkflowRejectedOutput};

/// Runner ids of the R and Julia operations (see the registry).
pub mod runner_ids {
    pub const RESTRICTED_R: &str = "restrictedR";
    pub const NATIVE_R: &str = "nativeR";
    pub const RESTRICTED_JULIA: &str = "restrictedJulia";
    pub const NATIVE_JULIA: &str = "nativeJulia";
}

/// Why R/Julia steps cannot run when no batch runner is installed.
pub const NO_LANGUAGE_RUNNER: &str =
    "The research batch adapter is not installed in this build; R and Julia workflow steps cannot run.";

/// The (runner id, language, mode) of every R and Julia adapter.
pub const ADAPTERS: [(&str, BatchLanguage, BatchMode); 4] = [
    (runner_ids::RESTRICTED_R, BatchLanguage::R, BatchMode::Restricted),
    (runner_ids::NATIVE_R, BatchLanguage::R, BatchMode::TrustedNative),
    (runner_ids::RESTRICTED_JULIA, BatchLanguage::Julia, BatchMode::Restricted),
    (runner_ids::NATIVE_JULIA, BatchLanguage::Julia, BatchMode::TrustedNative),
];

/// Install the four R and Julia adapters over `runner` (the same research
/// batch runner the Python adapters use; `None`: they report themselves
/// unavailable).
pub fn install(adapters: &mut Adapters, runner: Option<Arc<dyn BatchRunner>>) {
    for (id, language, mode) in ADAPTERS {
        adapters.insert(id.to_owned(), Arc::new(LanguageBatchAdapter::new(runner.clone(), language, mode)));
    }
}

/// Runs one language's batches in one mode.
pub struct LanguageBatchAdapter {
    pub runner: Option<Arc<dyn BatchRunner>>,
    pub language: BatchLanguage,
    pub mode: BatchMode,
}

impl LanguageBatchAdapter {
    /// An adapter for R or Julia (Python steps use [`super::batch::BatchAdapter`]).
    pub fn new(runner: Option<Arc<dyn BatchRunner>>, language: BatchLanguage, mode: BatchMode) -> Self {
        assert_ne!(language, BatchLanguage::Python, "Python batches use the Python batch adapter");
        Self { runner, language, mode }
    }

    /// The registered operation this adapter runs.
    pub fn operation_id(&self) -> &'static str {
        match (self.language, self.mode) {
            (BatchLanguage::R, BatchMode::Restricted) => registry::RESEARCH_R_BATCH,
            (BatchLanguage::R, BatchMode::TrustedNative) => registry::RESEARCH_NATIVE_R,
            (BatchLanguage::Julia, BatchMode::Restricted) => registry::RESEARCH_JULIA_BATCH,
            (BatchLanguage::Julia, BatchMode::TrustedNative) => registry::RESEARCH_NATIVE_JULIA,
            (BatchLanguage::Python, BatchMode::Restricted) => registry::RESEARCH_PYTHON_BATCH,
            (BatchLanguage::Python, BatchMode::TrustedNative) => registry::RESEARCH_NATIVE_PYTHON,
        }
    }

    fn code_language(&self) -> CodeLanguage {
        match self.language {
            BatchLanguage::Python => CodeLanguage::Python,
            BatchLanguage::R => CodeLanguage::R,
            BatchLanguage::Julia => CodeLanguage::Julia,
        }
    }

    /// The image this adapter's restricted steps run in now (reference and
    /// identity), for binding a run plan to it like the default restricted
    /// image. `None` for native adapters; `Some(Err)` when restricted
    /// batches of the language are unavailable (no image configured, image
    /// absent, Docker unreachable). Never pulls.
    pub fn restricted_image(&self) -> Option<Result<(String, Option<String>), String>> {
        if self.mode != BatchMode::Restricted {
            return None;
        }
        Some(match &self.runner {
            Some(runner) => runner.restricted_image(self.language),
            None => Err(NO_LANGUAGE_RUNNER.to_owned()),
        })
    }
}

fn file_input(value: &WorkflowOutputValue) -> Option<BatchInput> {
    match value {
        WorkflowOutputValue::File { path, sha256, .. } => {
            Some(BatchInput { path: path.clone(), sha256: bare_hash(sha256) })
        }
        _ => None,
    }
}

/// What a batch can realize for each output type (the Python adapter offers
/// the same), limited to what the batch runner exports.
fn extensions(ty: PortType) -> Vec<String> {
    let exts: &[&str] = match ty {
        PortType::Dataset => &["csv", "tsv", "json", "jsonl", "parquet", "txt", "npz", "npy"],
        PortType::Figure => &["svg", "png", "jpg", "jpeg", "pdf"],
        PortType::Table => &["csv", "tsv", "html", "md", "tex", "json"],
        other => other.extensions(),
    };
    exts.iter()
        .filter(|e| exposition_research::sandbox::outputs::EXPORTED_EXTENSIONS.contains(e))
        .map(|e| (*e).to_owned())
        .collect()
}

fn file_value(research_run: &str, artifact: &ArtifactRecord) -> WorkflowOutputValue {
    WorkflowOutputValue::File {
        // A scratch artifact without a project path is refused by the runner.
        path: artifact.path.clone().unwrap_or_else(|| {
            exposition_core::path::WorkspacePath::parse(".exposition/missing").expect("static path")
        }),
        sha256: bare_hash(&artifact.content_hash),
        bytes: artifact.size_bytes,
        media_type: artifact.media_type.clone(),
        research_run: Some(research_run.to_owned()),
        artifact: Some(artifact.id.0.clone()),
    }
}

impl StepAdapter for LanguageBatchAdapter {
    fn availability(&self, root: &Path) -> Result<(), String> {
        match &self.runner {
            Some(runner) => runner.language_availability(root, self.language, self.mode),
            None => Err(NO_LANGUAGE_RUNNER.to_owned()),
        }
    }

    fn run(&self, request: &StepRequest<'_>, control: &StepControl<'_>) -> Result<StepResult, StepFailure> {
        let Some(runner) = &self.runner else {
            return Err(StepFailure::new("workflow.runnerUnavailable", NO_LANGUAGE_RUNNER));
        };
        let contract = registry::operation(self.operation_id(), 1)
            .ok_or_else(|| StepFailure::new("workflow.internal", "the batch contract is missing"))?;
        let language = self.language.title();
        let script: Vec<_> = request.inputs_on("script").collect();
        let code = match (&request.step.payload, script.as_slice()) {
            (Some(StepPayload::Code { language: got, source }), []) => {
                if *got != self.code_language() {
                    return Err(StepFailure::new(
                        "workflow.payloadLanguage",
                        format!("“{}” runs {language} code; its code is not {language}.", request.step.label),
                    ));
                }
                BatchCode::Inline { source: source.clone() }
            }
            (None, [one]) => match &one.value {
                WorkflowOutputValue::File { path, sha256, .. } => {
                    registry::check_script_language(&contract, path.as_str())
                        .map_err(|m| StepFailure::new("workflow.invalidInput", format!("Not started: {m}.")))?;
                    BatchCode::Script { path: path.clone(), sha256: bare_hash(sha256) }
                }
                _ => return Err(StepFailure::new("workflow.invalidInput", "the script input is not a file")),
            },
            _ => {
                return Err(StepFailure::new(
                    "workflow.invalidInput",
                    "the step needs exactly one of inline code or a script input",
                ));
            }
        };
        let restricted_image_id = match self.mode {
            BatchMode::Restricted => match &request.restricted_image_id {
                Some(id) => Some(id.clone()),
                None => {
                    return Err(StepFailure::new(
                        "workflow.restrictedUnavailable",
                        format!(
                            "Not started: restricted {language} steps run only in an approved image identity, and this run has none. Nothing runs natively instead."
                        ),
                    ));
                }
            },
            BatchMode::TrustedNative => None,
        };
        let mut inputs = Vec::new();
        for bound in request.inputs_on("data") {
            let Some(input) = file_input(&bound.value) else {
                return Err(StepFailure::new("workflow.invalidInput", "a data input is not a file"));
            };
            if !inputs.iter().any(|i: &BatchInput| i.path == input.path) {
                inputs.push(input);
            }
        }
        let outputs: Vec<BatchPort> = contract
            .outputs
            .iter()
            .map(|p| BatchPort { port: p.name.clone(), extensions: extensions(p.types[0]) })
            .collect();
        let batch = BatchRequest {
            mode: self.mode,
            owner: BatchOwner { run: request.run.to_string(), step: request.step.step.clone() },
            code,
            inputs,
            outputs,
            time_limit: request.time_limit,
            max_output_bytes: request.max_output_bytes,
            restricted_image_id,
        };
        let dispatched =
            |id: &str| (control.on_dispatched)(&OperationRef { kind: "researchRun".into(), id: id.into() });
        let batch_control = BatchControl { canceled: control.canceled, on_dispatched: &dispatched };
        let result = runner.run_language(request.root, self.language, &batch, &batch_control).map_err(|e| {
            let code = e.code();
            StepFailure::new(if code.is_empty() { "research.batchFailed" } else { &code }, e.to_string())
        })?;
        let operation = OperationRef { kind: "researchRun".into(), id: result.research_run.clone() };
        let rejected: Vec<WorkflowRejectedOutput> = result
            .rejected
            .iter()
            .map(|r| WorkflowRejectedOutput { name: r.name.clone(), reason: r.reason.clone() })
            .collect();
        let log = (!result.log.is_empty()).then(|| bounded_tail(&result.log, MAX_ATTEMPT_LOG_CHARS));
        let fail = |code: &str, message: String| StepFailure {
            code: code.to_owned(),
            message,
            operation: Some(operation.clone()),
            rejected: rejected.clone(),
            log: log.clone(),
            model_requests: 0,
        };
        match &result.status {
            RunStatus::Succeeded => {}
            RunStatus::Canceled => return Err(fail("workflow.canceled", "The batch was canceled.".into())),
            RunStatus::Failed { message } => {
                let exit = result.exit_code.map(|c| format!(" (exit code {c})")).unwrap_or_default();
                return Err(fail("research.batchFailed", format!("The {language} batch failed{exit}: {message}")));
            }
            RunStatus::Interrupted | RunStatus::Running => {
                return Err(fail("research.batchFailed", format!("The {language} batch did not finish.")));
            }
        }
        let mut outputs = Vec::new();
        for port in &result.ports {
            // An unknown port is reported as-is; the runner refuses it.
            let ty = contract.output(&port.port).map_or(PortType::Dataset, |spec| spec.types[0]);
            outputs.push(request.output(&port.port, ty, file_value(&result.research_run, &port.artifact), &operation));
        }
        let extra_output_bytes = result.extra.iter().map(|a| a.size_bytes).sum();
        Ok(StepResult { operation: Some(operation), outputs, rejected, log, model_requests: 0, extra_output_bytes })
    }
}

#[cfg(test)]
mod rj_adapter_tests {
    use super::*;

    #[test]
    fn adapters_cover_exactly_the_r_and_julia_operations() {
        let mut adapters = Adapters::new();
        install(&mut adapters, None);
        let ops = registry::operations();
        for (id, language, mode) in ADAPTERS {
            let adapter = LanguageBatchAdapter::new(None, language, mode);
            let op = ops.iter().find(|o| o.id == adapter.operation_id()).expect("registered");
            assert_eq!(op.runner.id, id, "{id} runs {}", op.id);
            assert_eq!(registry::code_language(op), Some(adapter.code_language()));
            assert_eq!(
                op.runner.containment == registry::Containment::RestrictedContainer,
                mode == BatchMode::Restricted
            );
            assert!(adapters.get(id).unwrap().availability(Path::new(".")).is_err(), "no runner: unavailable");
            assert_eq!(adapter.restricted_image().is_some(), mode == BatchMode::Restricted);
        }
        let r_and_julia: Vec<&str> = ops
            .iter()
            .filter(|o| matches!(registry::code_language(o), Some(CodeLanguage::R | CodeLanguage::Julia)))
            .map(|o| o.runner.id.as_str())
            .collect();
        assert_eq!(r_and_julia.len(), ADAPTERS.len());
        assert!(r_and_julia.iter().all(|id| adapters.contains_key(*id)));
    }

    #[test]
    fn output_ports_offer_only_exported_extensions() {
        for ty in [PortType::Dataset, PortType::Figure, PortType::Table] {
            let exts = extensions(ty);
            assert!(!exts.is_empty(), "{ty:?}");
            for e in &exts {
                assert!(
                    exposition_research::sandbox::outputs::EXPORTED_EXTENSIONS.contains(&e.as_str()),
                    "{ty:?} .{e}"
                );
            }
        }
    }

    #[test]
    #[should_panic(expected = "Python batches use the Python batch adapter")]
    fn python_is_not_a_language_adapter() {
        let _ = LanguageBatchAdapter::new(None, BatchLanguage::Python, BatchMode::Restricted);
    }
}
