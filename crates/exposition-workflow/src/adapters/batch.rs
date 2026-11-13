//! `restrictedPython`/`nativePython`, and (M9) `restrictedR`/`nativeR`,
//! `restrictedJulia`/`nativeJulia`: workflow-owned batches through the
//! research service's [`BatchRunner`] (R and Julia adapters are installed by
//! [`super::rj`]).
//!
//! Declared inputs are exactly the values bound to `data` (and `script`, which
//! becomes the batch code); an inline payload is the code otherwise. Each
//! output port the contract declares is offered with the extensions its type
//! accepts; the batch realizes port `p` with the file `p.<ext>` in its output
//! area. The code is the step's language: a script input must have the
//! language's extension and an inline payload must be of the language
//! (checked before anything is staged). The step's declared parameters (a
//! forEach value already applied) and its instance index reach the code as
//! `EXPOSITION_PARAM_<NAME>` and `EXPOSITION_ITERATION`; its seed, derived
//! from the run, step, and instance, as `EXPOSITION_SEED`. Restricted
//! batches never fall back to native execution: when the restricted backend
//! is unavailable the step fails. A restricted R or Julia step runs only in
//! its language's own image, pinned to the identity its run's plan approved
//! for it (the research runner checks again).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use exposition_core::artifact::RunStatus;
use exposition_graph::OperationRef;
use exposition_graph::workflow::registry::{self, CodeLanguage, PortType};
use exposition_graph::workflow::{ParamValue, StepPayload};
use exposition_research::batch::{
    BatchCode, BatchControl, BatchInput, BatchLanguage, BatchMode, BatchOwner, BatchPort, BatchRequest, BatchRunner,
};

use super::{StepAdapter, StepControl, StepFailure, StepRequest, StepResult, bare_hash, bounded_tail};
use crate::environment::{self, WorkflowEnvironment};
use crate::model::{MAX_ATTEMPT_LOG_CHARS, WorkflowOutputValue, WorkflowPlanStep, WorkflowRejectedOutput};

/// Why batch steps cannot run when no batch runner is installed.
pub const NO_BATCH_RUNNER: &str =
    "The research batch adapter is not installed in this build; Python workflow steps cannot run.";

pub struct BatchAdapter {
    pub runner: Option<Arc<dyn BatchRunner>>,
    pub mode: BatchMode,
    pub language: BatchLanguage,
}

impl BatchAdapter {
    /// Python batches.
    pub fn new(runner: Option<Arc<dyn BatchRunner>>, mode: BatchMode) -> Self {
        Self::for_language(runner, BatchLanguage::Python, mode)
    }

    /// Batches of `language`.
    pub fn for_language(runner: Option<Arc<dyn BatchRunner>>, language: BatchLanguage, mode: BatchMode) -> Self {
        Self { runner, mode, language }
    }

    /// The registered operation this adapter runs.
    pub fn operation_id(&self) -> &'static str {
        match (self.language, self.mode) {
            (BatchLanguage::Python, BatchMode::Restricted) => registry::RESEARCH_PYTHON_BATCH,
            (BatchLanguage::Python, BatchMode::TrustedNative) => registry::RESEARCH_NATIVE_PYTHON,
            (BatchLanguage::R, BatchMode::Restricted) => registry::RESEARCH_R_BATCH,
            (BatchLanguage::R, BatchMode::TrustedNative) => registry::RESEARCH_NATIVE_R,
            (BatchLanguage::Julia, BatchMode::Restricted) => registry::RESEARCH_JULIA_BATCH,
            (BatchLanguage::Julia, BatchMode::TrustedNative) => registry::RESEARCH_NATIVE_JULIA,
        }
    }

    /// The language of the code the operation runs.
    pub fn code_language(&self) -> CodeLanguage {
        match self.language {
            BatchLanguage::Python => CodeLanguage::Python,
            BatchLanguage::R => CodeLanguage::R,
            BatchLanguage::Julia => CodeLanguage::Julia,
        }
    }

    fn no_runner(&self) -> String {
        match self.language {
            BatchLanguage::Python => NO_BATCH_RUNNER.to_owned(),
            other => format!(
                "The research batch adapter is not installed in this build; {} workflow steps cannot run.",
                other.title()
            ),
        }
    }

    /// The image this adapter's restricted steps run in when it is not the
    /// default restricted image (R and Julia): reference and identity now,
    /// or why it is unavailable. `None` for Python (the default image) and
    /// native adapters. Never pulls.
    pub fn restricted_image(&self) -> Option<Result<(String, Option<String>), String>> {
        if self.mode != BatchMode::Restricted || self.language == BatchLanguage::Python {
            return None;
        }
        Some(match &self.runner {
            Some(runner) => runner.restricted_image(self.language),
            None => Err(self.no_runner()),
        })
    }

    /// The environment of one attempt from what the batch runner reported
    /// (`pairs`; empty when the batch failed before it ran).
    fn environment(
        &self,
        step: &WorkflowPlanStep,
        pairs: &[(String, String)],
        approved_image: Option<&str>,
        seed: Option<u64>,
    ) -> WorkflowEnvironment {
        let mut env = WorkflowEnvironment::of_step(step);
        if self.mode == BatchMode::Restricted {
            // The image's platform, not the host's (unknown until listed).
            env.os = "unknown".into();
            env.arch = "unknown".into();
        }
        environment::apply_batch_pairs(&mut env, pairs);
        if self.mode == BatchMode::Restricted {
            match (&mut env.image, approved_image) {
                (Some(image), Some(id)) if image.id.is_none() => image.id = Some(id.to_owned()),
                (None, Some(id)) => env.note(format!("The approved image identity was {id}.")),
                _ => {}
            }
        }
        if pairs.is_empty() {
            env.note("The batch did not report its environment (it stopped before it ran).");
        }
        env.seed = seed.or(env.seed);
        env
    }

    /// See [`StepAdapter::current_environment`].
    pub fn current_environment(&self, root: &Path, step: &WorkflowPlanStep) -> Option<WorkflowEnvironment> {
        let runner = self.runner.as_ref()?;
        let pairs = runner.current_language_environment(root, self.language, self.mode);
        if pairs.is_empty() {
            return None;
        }
        let mut env = self.environment(step, &pairs, None, None);
        env.notes.retain(|n| !n.starts_with("The batch did not report"));
        Some(env)
    }

    /// The approved identity a restricted step of this adapter runs in.
    /// Python: the plan's default-image identity, as approved (the research
    /// runner checks it). R/Julia: required, and it must be the language
    /// image's identity now (refused before anything is staged otherwise).
    fn approved_image(
        &self,
        runner: &dyn BatchRunner,
        request: &StepRequest<'_>,
    ) -> Result<Option<String>, StepFailure> {
        if self.mode != BatchMode::Restricted {
            return Ok(None);
        }
        if self.language == BatchLanguage::Python {
            return Ok(request.restricted_image_id.clone());
        }
        let language = self.language.title();
        let Some(approved) = &request.restricted_image_id else {
            return Err(StepFailure::new(
                "workflow.restrictedUnavailable",
                format!(
                    "Not started: restricted {language} steps run only in an approved image identity, and this run has none. Nothing runs natively instead."
                ),
            ));
        };
        match runner.restricted_image(self.language) {
            Err(reason) => {
                let native = if reason.contains("natively") { "" } else { " Nothing runs natively instead." };
                Err(StepFailure::new("workflow.restrictedUnavailable", format!("Not started: {reason}{native}")))
            }
            Ok((image, now)) if now.as_deref() != Some(approved.as_str()) => Err(StepFailure::new(
                "workflow.restrictedUnavailable",
                format!(
                    "Not started: restricted {language} steps run in the {language} image {image} ({}), but the run's plan approved the image {}. Restricted steps run only in the image identity the plan approved; review a new plan. Nothing runs natively instead.",
                    short_id(now.as_deref()),
                    short_id(Some(approved))
                ),
            )),
            Ok(_) => Ok(Some(approved.clone())),
        }
    }
}

/// `sha256:0123456789ab` (or `of unknown identity`) for messages.
fn short_id(id: Option<&str>) -> String {
    match id {
        Some(id) => id.chars().take("sha256:".len() + 12).collect(),
        None => "of unknown identity".to_owned(),
    }
}

/// The step's declared parameters as text (the forEach value is already
/// applied to them): what the batch receives as `EXPOSITION_PARAM_<NAME>`.
pub fn parameter_texts(parameters: &BTreeMap<String, ParamValue>) -> BTreeMap<String, String> {
    parameters
        .iter()
        .map(|(name, value)| {
            let text = match value {
                ParamValue::Text(t) => t.clone(),
                ParamValue::Integer(i) => i.to_string(),
                ParamValue::Boolean(b) => b.to_string(),
            };
            (name.clone(), text)
        })
        .collect()
}

fn file_input(value: &WorkflowOutputValue) -> Option<BatchInput> {
    match value {
        WorkflowOutputValue::File { path, sha256, .. } => {
            Some(BatchInput { path: path.clone(), sha256: bare_hash(sha256) })
        }
        _ => None,
    }
}

pub(crate) fn extensions(ty: PortType) -> Vec<String> {
    let exts: &[&str] = match ty {
        // What a batch can realize for each output type (subset of the
        // input extensions that are plain files the runner can inspect).
        PortType::Dataset => &["csv", "tsv", "json", "jsonl", "parquet", "txt", "npz", "npy"],
        PortType::Figure => &["svg", "png", "jpg", "jpeg", "pdf"],
        PortType::Table => &["csv", "tsv", "html", "md", "tex", "json"],
        other => other.extensions(),
    };
    // Only what the batch runner actually exports (validated, inert-rendered
    // types); a port is never offered an extension it would refuse.
    exts.iter()
        .filter(|e| exposition_research::sandbox::outputs::EXPORTED_EXTENSIONS.contains(e))
        .map(|e| (*e).to_owned())
        .collect()
}

impl StepAdapter for BatchAdapter {
    fn availability(&self, root: &Path) -> Result<(), String> {
        match &self.runner {
            Some(runner) => runner.language_availability(root, self.language, self.mode),
            None => Err(self.no_runner()),
        }
    }

    fn run(&self, request: &StepRequest<'_>, control: &StepControl<'_>) -> Result<StepResult, StepFailure> {
        let Some(runner) = &self.runner else {
            return Err(StepFailure::new("workflow.runnerUnavailable", self.no_runner()));
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
        let approved_image = self.approved_image(runner.as_ref(), request)?;
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
        // Every instance of a repeated step gets its own seed.
        let iteration = request.iteration.as_ref().map(|i| i.index);
        let seed = environment::step_seed(request.seed_basis, &request.step.step, iteration.unwrap_or(0));
        let batch = BatchRequest {
            mode: self.mode,
            owner: BatchOwner { run: request.run.to_string(), step: request.step.step.clone() },
            code,
            inputs,
            outputs,
            time_limit: request.time_limit,
            max_output_bytes: request.max_output_bytes,
            restricted_image_id: approved_image.clone(),
            seed: Some(seed),
            iteration,
            parameters: parameter_texts(&request.step.parameters),
        };
        let dispatched =
            |id: &str| (control.on_dispatched)(&OperationRef { kind: "researchRun".into(), id: id.into() });
        let batch_control = BatchControl { canceled: control.canceled, on_dispatched: &dispatched };
        let result = runner.run_language(request.root, self.language, &batch, &batch_control).map_err(|e| {
            let code = e.code();
            StepFailure {
                environment: Some(self.environment(request.step, &[], approved_image.as_deref(), Some(seed))),
                ..StepFailure::new(if code.is_empty() { "research.batchFailed" } else { &code }, e.to_string())
            }
        })?;
        let env = self.environment(request.step, &result.environment, approved_image.as_deref(), Some(seed));
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
            environment: Some(env.clone()),
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
        Ok(StepResult {
            operation: Some(operation),
            outputs,
            rejected,
            log,
            model_requests: 0,
            extra_output_bytes,
            environment: Some(env),
        })
    }

    fn current_environment(&self, root: &Path, step: &WorkflowPlanStep) -> Option<WorkflowEnvironment> {
        BatchAdapter::current_environment(self, root, step)
    }

    fn restricted_image(&self) -> Option<Result<(String, Option<String>), String>> {
        BatchAdapter::restricted_image(self)
    }
}

fn file_value(research_run: &str, artifact: &exposition_core::artifact::ArtifactRecord) -> WorkflowOutputValue {
    WorkflowOutputValue::File {
        // A scratch artifact without a project path is refused by the runner
        // (an empty path never resolves inside the project).
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

#[cfg(test)]
mod port_extension_tests {
    use super::*;

    /// Every output type a batch step declares is offered only extensions
    /// the batch runner exports (it refuses a request naming any other), and
    /// at least one.
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
    fn parameters_reach_the_batch_as_text() {
        let parameters = BTreeMap::from([
            ("value".to_owned(), ParamValue::Text("0.25".into())),
            ("timeoutSeconds".to_owned(), ParamValue::Integer(30)),
            ("flag".to_owned(), ParamValue::Boolean(true)),
        ]);
        let texts = parameter_texts(&parameters);
        assert_eq!(texts["value"], "0.25");
        assert_eq!(texts["timeoutSeconds"], "30");
        assert_eq!(texts["flag"], "true");
    }
}
