//! `restrictedPython` and `nativePython`: workflow-owned Python batches
//! through the research service's [`BatchRunner`].
//!
//! Declared inputs are exactly the values bound to `data` (and `script`, which
//! becomes the batch code); an inline payload is the code otherwise. Each
//! output port the contract declares is offered with the extensions its type
//! accepts; the batch realizes port `p` with the file `p.<ext>` in its output
//! area. Restricted batches never fall back to native execution: when the
//! restricted backend is unavailable the step fails.

use std::path::Path;
use std::sync::Arc;

use exposition_core::artifact::RunStatus;
use exposition_graph::OperationRef;
use exposition_graph::workflow::StepPayload;
use exposition_graph::workflow::registry::{self, PortType};
use exposition_research::batch::{
    BatchCode, BatchControl, BatchInput, BatchMode, BatchOwner, BatchPort, BatchRequest, BatchRunner,
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
}

impl BatchAdapter {
    pub fn new(runner: Option<Arc<dyn BatchRunner>>, mode: BatchMode) -> Self {
        Self { runner, mode }
    }

    fn operation_id(&self) -> &'static str {
        match self.mode {
            BatchMode::Restricted => registry::RESEARCH_PYTHON_BATCH,
            BatchMode::TrustedNative => registry::RESEARCH_NATIVE_PYTHON,
        }
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
        let pairs = runner.current_environment(root, self.mode);
        if pairs.is_empty() {
            return None;
        }
        let mut env = self.environment(step, &pairs, None, None);
        env.notes.retain(|n| !n.starts_with("The batch did not report"));
        Some(env)
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

fn extensions(ty: PortType) -> Vec<String> {
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
            Some(runner) => runner.availability(root, self.mode),
            None => Err(NO_BATCH_RUNNER.to_owned()),
        }
    }

    fn run(&self, request: &StepRequest<'_>, control: &StepControl<'_>) -> Result<StepResult, StepFailure> {
        let Some(runner) = &self.runner else {
            return Err(StepFailure::new("workflow.runnerUnavailable", NO_BATCH_RUNNER));
        };
        let contract = registry::operation(self.operation_id(), 1)
            .ok_or_else(|| StepFailure::new("workflow.internal", "the batch contract is missing"))?;
        let script: Vec<_> = request.inputs_on("script").collect();
        let code = match (&request.step.payload, script.as_slice()) {
            (Some(StepPayload::Code { source, .. }), []) => BatchCode::Inline { source: source.clone() },
            (None, [one]) => match &one.value {
                WorkflowOutputValue::File { path, sha256, .. } => {
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
        // Each instance of a repeated step has its own seed.
        let instance = request.iteration.as_ref().map_or(0, |i| i.index);
        let seed = environment::step_seed(request.seed_basis, &request.step.step, instance);
        let approved_image = match self.mode {
            BatchMode::Restricted => request.restricted_image_id.clone(),
            BatchMode::TrustedNative => None,
        };
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
        };
        let dispatched =
            |id: &str| (control.on_dispatched)(&OperationRef { kind: "researchRun".into(), id: id.into() });
        let batch_control = BatchControl { canceled: control.canceled, on_dispatched: &dispatched };
        let result = runner.run(request.root, &batch, &batch_control).map_err(|e| {
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
                return Err(fail("research.batchFailed", format!("The Python batch failed{exit}: {message}")));
            }
            RunStatus::Interrupted | RunStatus::Running => {
                return Err(fail("research.batchFailed", "The Python batch did not finish.".into()));
            }
        }
        let mut outputs = Vec::new();
        for port in &result.ports {
            let Some(spec) = contract.output(&port.port) else {
                // Reported as-is; the runner refuses unknown ports.
                outputs.push(request.output(
                    &port.port,
                    PortType::Dataset,
                    file_value(&result.research_run, &port.artifact),
                    &operation,
                ));
                continue;
            };
            outputs.push(request.output(
                &port.port,
                spec.types[0],
                file_value(&result.research_run, &port.artifact),
                &operation,
            ));
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

    /// Every output type a Python step declares is offered only extensions
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
}
