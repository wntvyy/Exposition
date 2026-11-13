//! Step adapters: one per runner id of the operation registry.
//!
//! An adapter turns one planned step (its approved payload, parameters, and
//! the validated values bound to its input ports) into a call of an existing
//! application service, and reports the service operation it started and the
//! outputs it claims. It never decides whether those outputs are valid: the
//! runner validates every returned output (declared port and type, recomputed
//! digests, provenance, service ownership) before any consumer sees it.

// A failed attempt carries its log and refusals; failures are rare and not on
// a hot path, so the larger error type is deliberate.
#![allow(clippy::result_large_err)]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use exposition_graph::OperationRef;
use exposition_graph::WorkflowRunId;
use exposition_graph::workflow::registry::PortType;

use crate::model::{
    WorkflowOutputProvenance, WorkflowOutputValue, WorkflowPlanStep, WorkflowRejectedOutput, WorkflowStepOutput,
};

pub mod agent;
pub mod associate;
pub mod batch;
pub mod lean;
pub mod research;

/// A validated value bound to one input port of the step.
#[derive(Clone, Debug)]
pub struct BoundInput {
    pub port: String,
    pub port_type: PortType,
    /// External inputs arrive as `File` values (no research run).
    pub value: WorkflowOutputValue,
}

/// Everything an adapter needs to run one attempt of one step.
pub struct StepRequest<'a> {
    pub root: &'a Path,
    pub run: &'a WorkflowRunId,
    pub step: &'a WorkflowPlanStep,
    pub attempt: u32,
    pub inputs: &'a [BoundInput],
    /// The step's time limit, already capped by the run's remaining budget.
    pub time_limit: Duration,
    pub max_output_bytes: u64,
    /// Model requests the run may still make (model steps).
    pub model_requests_remaining: u32,
}

impl StepRequest<'_> {
    pub fn inputs_on<'b>(&'b self, port: &'b str) -> impl Iterator<Item = &'b BoundInput> + 'b {
        self.inputs.iter().filter(move |i| i.port == port)
    }

    /// Provenance an output of this attempt carries.
    pub fn provenance(&self, operation: OperationRef) -> WorkflowOutputProvenance {
        WorkflowOutputProvenance {
            run: self.run.clone(),
            step: self.step.step.clone(),
            attempt: self.attempt,
            operation,
        }
    }

    pub fn output(
        &self,
        port: &str,
        port_type: PortType,
        value: WorkflowOutputValue,
        op: &OperationRef,
    ) -> WorkflowStepOutput {
        WorkflowStepOutput { port: port.to_owned(), port_type, value, provenance: self.provenance(op.clone()) }
    }

    pub fn text_parameter(&self, name: &str) -> Option<&str> {
        match self.step.parameters.get(name) {
            Some(exposition_graph::workflow::ParamValue::Text(t)) => Some(t.as_str()),
            _ => None,
        }
    }
}

/// Cancellation and dispatch callbacks of one attempt.
pub struct StepControl<'a> {
    pub canceled: &'a (dyn Fn() -> bool + Sync),
    /// Called once with the service operation as soon as it is known (the
    /// runner persists it before waiting for the outcome).
    pub on_dispatched: &'a (dyn Fn(&OperationRef) + Sync),
}

/// What an adapter returns on success (claims, validated by the runner).
#[derive(Clone, Debug, Default)]
pub struct StepResult {
    pub operation: Option<OperationRef>,
    pub outputs: Vec<WorkflowStepOutput>,
    pub rejected: Vec<WorkflowRejectedOutput>,
    pub log: Option<String>,
    /// Model requests the step made (counted against the run's budget).
    pub model_requests: u32,
}

/// A failed attempt.
#[derive(Clone, Debug)]
pub struct StepFailure {
    pub code: String,
    pub message: String,
    pub operation: Option<OperationRef>,
    pub rejected: Vec<WorkflowRejectedOutput>,
    pub log: Option<String>,
}

impl StepFailure {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self { code: code.to_owned(), message: message.into(), operation: None, rejected: Vec::new(), log: None }
    }

    pub fn canceled() -> Self {
        Self::new("workflow.canceled", "The step was canceled.")
    }
}

/// Runs the steps of one runner id.
pub trait StepAdapter: Send + Sync {
    /// Whether the runner can execute in the project at `root` now (backend,
    /// image, toolchain, service). Trust of native runners is checked by the
    /// runner itself, not here.
    fn availability(&self, root: &Path) -> Result<(), String>;
    /// Run one attempt to completion, cancellation, or failure.
    fn run(&self, request: &StepRequest<'_>, control: &StepControl<'_>) -> Result<StepResult, StepFailure>;
}

/// A runner that is honestly not available in this build.
pub struct Unavailable(pub String);

impl StepAdapter for Unavailable {
    fn availability(&self, _root: &Path) -> Result<(), String> {
        Err(self.0.clone())
    }

    fn run(&self, _request: &StepRequest<'_>, _control: &StepControl<'_>) -> Result<StepResult, StepFailure> {
        Err(StepFailure::new("workflow.runnerUnavailable", self.0.clone()))
    }
}

/// Runner id → adapter.
pub type Adapters = BTreeMap<String, Arc<dyn StepAdapter>>;

/// Runner ids of the registry.
pub mod runner_ids {
    pub const RESTRICTED_PYTHON: &str = "restrictedPython";
    pub const NATIVE_PYTHON: &str = "nativePython";
    pub const LEAN_AUDIT: &str = "leanAudit";
    pub const GRAPH_PROPOSAL: &str = "graphProposal";
    pub const READ_ONLY_AGENT: &str = "readOnlyAgent";
    pub const RESEARCH_RETENTION: &str = "researchRetention";
    pub const REVIEWED_PROMOTION: &str = "reviewedPromotion";
}

/// Why read-only agent steps cannot run in this version.
pub const AGENT_REVIEW_UNAVAILABLE: &str = "Read-only agent review steps are not available in this version of the workflow runner; remove the step or run the review as an agent task.";

/// SHA-256 (hex) of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// A digest without a `sha256:` prefix, lowercase.
pub fn bare_hash(hash: &str) -> String {
    hash.strip_prefix("sha256:").unwrap_or(hash).to_ascii_lowercase()
}

/// Keep at most `max` characters (the tail, where errors are).
pub fn bounded_tail(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_owned();
    }
    let skip = count - max;
    let mut out = String::from("…");
    out.extend(text.chars().skip(skip));
    out
}
