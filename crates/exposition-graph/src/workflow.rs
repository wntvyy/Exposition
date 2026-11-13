//! Programmable workflow drafts: the strict declarative contract.
//!
//! Presets, agent proposals, imports, and manual authoring all produce the
//! same versioned [`WorkflowDefinition`]. A step names a registered
//! operation/version ([`registry`]); data flows only along typed `data`
//! connections; `prerequisite` connections order steps explicitly;
//! `annotation` lines (decorative, or labelled with a semantic relation) never
//! satisfy an input or order execution. A semantic graph relation never
//! schedules code.
//!
//! [`validate_workflow`] enforces the contract for every origin:
//! 1. every required input is bound to a compatible upstream output or an
//!    explicit, versioned external input;
//! 2. every step lies on a path to a declared outcome (orphans and
//!    disconnected executable components are rejected; unused optional
//!    outputs are fine);
//! 3. connections deliver compatible typed data or are explicit
//!    prerequisites; annotations and semantic edges cannot satisfy execution;
//! 4. declared effects fit the workflow's ceiling (and the context's) and the
//!    runner's containment; connections grant nothing;
//! 5. actual returned outputs are validated by the runner before consumers
//!    unlock (the workflow runner service, `exposition-workflow`);
//! 6. service-owned types (formal evidence, retained artifacts) come only from
//!    their owning operation.
//!
//! plus: no cycles, loops, conditionals, or nesting (the schema has none),
//! schema versions, and size limits. Drafts may be incomplete and are saved
//! with their diagnostics. Validation alone never makes a draft runnable:
//! [`apply_runnability`] marks it runnable only when it is well-formed AND
//! every step's runner is available in the open project (decided by the
//! workflow runner service). Running still needs a reviewed run plan.

pub mod interchange;
pub mod persist;
pub mod presets;
pub mod registry;
pub mod v2;

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use exposition_core::path::WorkspacePath;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::WorkflowId;
use crate::ops::DiagnosticSeverity;
use crate::vocabulary::RelationKind;
pub use interchange::{
    BUNDLE_SCHEMA, BundledWorkflow, ImportedChild, MAX_BUNDLE_BYTES, WorkflowBundle, WorkflowBundleExport,
    WorkflowBundleImport, bundle_text,
};
pub use persist::{SaveWorkflowRequest, SavedWorkflow, WorkflowRevisionInfo, WorkflowSummary};
pub use registry::{
    CodeLanguage, OperationContract, ParameterKind, PayloadSpec, PortSpec, PortType, WorkflowEffect, operation,
    operations, with_availability,
};
pub use v2::{
    COMPOSITE_RUNNER, ExpandedStep, ExpandedWorkflow, MAX_COMPOSITE_DEPTH, MAX_CONDITION_JSON_BYTES,
    MAX_EXPANDED_STEPS, MAX_FOR_EACH_VALUES, MAX_UNTIL_ITERATIONS, Observed, evaluate_condition, expand_workflow,
    verdict_class,
};

/// Schema identifier of a workflow definition (v1: M7/M8 shape). Still read
/// and written for every definition that uses no v2 field, so its
/// serialization and [`definition_hash`] never change.
pub const WORKFLOW_SCHEMA: &str = "exposition.workflow/1";
/// Schema identifier of a definition that uses any v2 field (execution
/// options, composites, conditions, repeats). Required when one is used.
pub const WORKFLOW_SCHEMA_V2: &str = "exposition.workflow/2";
/// Every schema this version reads.
pub const WORKFLOW_SCHEMAS: [&str; 2] = [WORKFLOW_SCHEMA, WORKFLOW_SCHEMA_V2];
/// The registered operation of a composite step (runs another saved workflow
/// at a pinned revision).
pub const COMPOSITE_OPERATION: &str = "workflow.composite";
/// Bounds of `execution.maxParallel`.
pub const MAX_PARALLEL: u32 = 4;
/// Audit verdict labels a `verdictIn` condition may name: the Lean audit
/// adapter's declaration verdicts and the classes of its overall file
/// verdict (an overall label such as "Errors (2)" or "Incomplete (1 sorry)"
/// belongs to the class it starts with; see [`verdict_class`]).
pub const KNOWN_VERDICTS: [&str; 7] = [
    "Checked",
    "Checked with nonstandard axioms",
    "Incomplete",
    "Relies on compiled code",
    "Partially checked",
    "Unverified",
    "Errors",
];
/// Why a draft is not runnable before the runner checked it in a project.
/// (Validation alone never decides runnability; see [`apply_runnability`].)
pub const NOT_RUNNABLE_REASON: &str =
    "Runner availability has not been checked for this draft in the open project; it cannot run until it is.";

pub const MAX_STEPS: usize = 50;
pub const MAX_CONNECTIONS: usize = 200;
pub const MAX_INPUTS: usize = 50;
pub const MAX_OUTCOMES: usize = 20;
pub const MAX_PARAMETERS: usize = 32;
pub const MAX_CODE_BYTES: usize = 256 * 1024;
pub const MAX_PROMPT_BYTES: usize = 32 * 1024;
pub const MAX_TITLE: usize = 200;
pub const MAX_DESCRIPTION: usize = 4000;
/// Serialized definitions larger than this are refused before parsing.
pub const MAX_DEFINITION_BYTES: usize = 1024 * 1024;

/// A workflow definition (one revision's content). Its identity and revision
/// live in the store; the content hash identifies the revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowDefinition {
    /// Must be [`WORKFLOW_SCHEMA`].
    pub schema: String,
    pub title: String,
    pub description: String,
    /// The workflow preset it started from, if any.
    pub preset: Option<String>,
    /// Explicit external inputs (project files, pinned or resolved at run start).
    pub inputs: Vec<WorkflowInput>,
    pub steps: Vec<WorkflowStep>,
    pub connections: Vec<WorkflowConnection>,
    pub outcomes: Vec<WorkflowOutcome>,
    /// The most this workflow may do; every step's effects must fit.
    pub ceiling: AuthorityCeiling,
    /// Execution options (schema /2). Absent: one step at a time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub execution: Option<WorkflowExecutionOptions>,
}

impl WorkflowDefinition {
    /// How many independent steps may run at once (1 when unset).
    pub fn max_parallel(&self) -> u32 {
        self.execution.map(|e| e.max_parallel).unwrap_or(1)
    }

    /// Whether the definition uses any field that needs [`WORKFLOW_SCHEMA_V2`].
    pub fn uses_v2(&self) -> bool {
        self.execution.is_some()
            || self.steps.iter().any(|s| s.composite.is_some() || s.condition.is_some() || s.repeat.is_some())
    }
}

/// Execution options of a definition (schema /2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowExecutionOptions {
    /// Independent steps (no data or prerequisite path between them) that may
    /// run at once, 1..=4 (1 = one step at a time). Global caps still apply.
    pub max_parallel: u32,
}

/// A composite step's pinned reference to another saved workflow revision.
/// Editing the child creates a new revision; the reference never follows it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompositeRef {
    pub workflow: WorkflowId,
    pub revision: u64,
}

/// A step's fixed run condition over a validated upstream output (no
/// expression evaluator). False: the step is skipped (`conditionNotMet`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StepCondition {
    /// An upstream step's output port (creates an ordering dependency).
    pub source: StepPort,
    pub test: ConditionTest,
    /// Run when the test does NOT hold.
    pub negate: bool,
}

/// A fixed predicate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum ConditionTest {
    /// Formal evidence source: its verdict is one of these exact labels.
    VerdictIn { verdicts: Vec<String> },
    /// Dataset source (a JSON file ≤ 1 MiB): the value at an RFC 6901
    /// pointer compared with a constant.
    JsonCompare { pointer: String, op: CompareOp, value: ConditionValue },
    /// The (optional) source output was produced.
    OutputPresent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum CompareOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum ConditionValue {
    /// A decimal number written as text (exact; no float in the definition).
    Number(String),
    Text(String),
    Boolean(bool),
}

/// A bounded repetition of one step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum StepRepeat {
    /// One instance per value of a declared parameter (1..=20 values);
    /// outputs are collected in order into a `multiple` consumer port.
    ForEach { parameter: String, values: Vec<ParamValue> },
    /// Sequential iterations (1..=10) until a predicate on the step's own
    /// output port holds.
    Until { port: String, test: ConditionTest, negate: bool, max_iterations: u32, on_exhausted: Exhausted },
}

/// What an `until` repetition does when every iteration ran and the
/// predicate never held.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum Exhausted {
    Fail,
    UseLast,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowInput {
    /// `[A-Za-z0-9_-]{1,32}`, unique among inputs.
    pub id: String,
    pub label: String,
    pub port_type: PortType,
    pub source: InputSource,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum InputSource {
    /// Not chosen yet (an incomplete draft).
    Unset,
    /// A project file.
    Resource { path: WorkspacePath, version: InputVersion },
}

/// How an external input is versioned.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum InputVersion {
    /// Exactly this content (SHA-256, hex).
    Pinned { content_hash: String },
    /// The version current when a run starts, recorded by the run.
    AtRunStart,
}

/// A registered operation and version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StepOperation {
    pub id: String,
    pub version: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum ParamValue {
    Text(String),
    Integer(i64),
    Boolean(bool),
}

/// Code or prompt for an operation that declares a payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum StepPayload {
    Code { language: CodeLanguage, source: String },
    Prompt { text: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowStep {
    /// `[A-Za-z0-9_-]{1,32}`, unique among steps.
    pub id: String,
    pub label: String,
    pub operation: StepOperation,
    pub parameters: BTreeMap<String, ParamValue>,
    pub payload: Option<StepPayload>,
    /// Composite step (schema /2): the operation must be
    /// [`COMPOSITE_OPERATION`] v1; ports come from the referenced child.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub composite: Option<CompositeRef>,
    /// Run condition (schema /2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub condition: Option<StepCondition>,
    /// Bounded repetition (schema /2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub repeat: Option<StepRepeat>,
}

/// Where data on a connection comes from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum PortSource {
    /// An output port of a step.
    Step { step: String, port: String },
    /// An external input.
    Input { input: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StepPort {
    pub step: String,
    pub port: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum WorkflowConnection {
    /// Typed data delivery into a step's input port.
    Data { from: PortSource, to: StepPort },
    /// `after` starts only once `before` has finished (no data).
    Prerequisite { before: String, after: String },
    /// A decorative line between steps, optionally labelled with a semantic
    /// relation. Never satisfies an input, never orders execution.
    Annotation { from: String, to: String, label: String, relation: Option<RelationKind> },
}

/// What a workflow produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum OutcomeKind {
    TransformedDataset,
    RetainedFigure,
    RetainedTable,
    InvestigationReport,
    AuditEvidence,
    ReviewableProposal,
    ManuscriptPromotion,
}

impl OutcomeKind {
    /// Port types that can realize this outcome.
    pub fn accepts(self) -> &'static [PortType] {
        match self {
            OutcomeKind::TransformedDataset => &[PortType::Dataset, PortType::RetainedArtifact],
            OutcomeKind::RetainedFigure | OutcomeKind::RetainedTable => &[PortType::RetainedArtifact],
            OutcomeKind::InvestigationReport => &[PortType::ReviewReport],
            OutcomeKind::AuditEvidence => &[PortType::FormalEvidence],
            OutcomeKind::ReviewableProposal => &[PortType::AssociationProposal, PortType::PromotionProposal],
            OutcomeKind::ManuscriptPromotion => &[PortType::PromotionProposal],
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            OutcomeKind::TransformedDataset => "Transformed dataset",
            OutcomeKind::RetainedFigure => "Retained figure",
            OutcomeKind::RetainedTable => "Retained table",
            OutcomeKind::InvestigationReport => "Investigation report",
            OutcomeKind::AuditEvidence => "Audit evidence",
            OutcomeKind::ReviewableProposal => "Reviewable proposal",
            OutcomeKind::ManuscriptPromotion => "Manuscript promotion",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowOutcome {
    pub id: String,
    pub kind: OutcomeKind,
    pub label: String,
    /// The output port that realizes it.
    pub from: StepPort,
    /// An optional destination (e.g. manuscript promotion) that may not happen.
    pub optional: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum ExecutionCeiling {
    /// No code runs.
    None,
    /// Only the restricted container.
    Restricted,
    /// Trusted native execution (your account's privileges) as well.
    TrustedNative,
}

/// The authority ceiling of a workflow. Child steps receive equal or narrower
/// authority; a connection never widens it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorityCeiling {
    pub execution: ExecutionCeiling,
    /// Steps may send their declared inputs to the model provider.
    pub model_requests: bool,
    /// Steps may retain results and record service evidence.
    pub retain_results: bool,
    /// Steps may produce reviewable document/graph proposals.
    pub propose_changes: bool,
}

impl AuthorityCeiling {
    pub const NONE: AuthorityCeiling = AuthorityCeiling {
        execution: ExecutionCeiling::None,
        model_requests: false,
        retain_results: false,
        propose_changes: false,
    };

    pub const ALL: AuthorityCeiling = AuthorityCeiling {
        execution: ExecutionCeiling::TrustedNative,
        model_requests: true,
        retain_results: true,
        propose_changes: true,
    };

    pub fn allows(&self, effect: WorkflowEffect) -> bool {
        use WorkflowEffect as E;
        match effect {
            E::ReadDeclaredInputs | E::WriteRunOutputs => true,
            E::ExecuteRestricted => self.execution >= ExecutionCeiling::Restricted,
            E::ExecuteNative => self.execution >= ExecutionCeiling::TrustedNative,
            E::SendToModel => self.model_requests,
            E::RetainArtifact | E::RegisterFormalEvidence => self.retain_results,
            E::ProposeDocumentChange | E::ProposeGraphChange => self.propose_changes,
        }
    }

    /// `self` is equal to or narrower than `other`.
    pub fn within(&self, other: &AuthorityCeiling) -> bool {
        self.execution <= other.execution
            && (!self.model_requests || other.model_requests)
            && (!self.retain_results || other.retain_results)
            && (!self.propose_changes || other.propose_changes)
    }
}

/// Where a draft stands. A well-formed draft is runnable only when its runners are available.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum DraftState {
    /// Has errors (unbound inputs, orphan steps, incompatible connections, ...).
    Incomplete,
    /// Satisfies the contract (runnable once every step's runner is available).
    WellFormed,
}

impl DraftState {
    pub fn as_str(self) -> &'static str {
        match self {
            DraftState::Incomplete => "incomplete",
            DraftState::WellFormed => "wellFormed",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        [DraftState::Incomplete, DraftState::WellFormed].into_iter().find(|s| s.as_str() == raw)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDiagnostic {
    pub severity: DiagnosticSeverity,
    /// Stable code, e.g. `workflow.unboundInput`.
    pub code: String,
    pub message: String,
    /// Contract rule (1–6) the diagnostic enforces, when it is one of them.
    pub rule: Option<u8>,
    pub step: Option<String>,
    pub port: Option<String>,
    pub input: Option<String>,
    pub outcome: Option<String>,
    /// Index into `connections`.
    pub connection: Option<u32>,
}

/// One step of the reviewable plan (dependency order).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PlannedStep {
    /// Step id; an expanded composite child is `<compositeStep>/<childStep>`
    /// (nested: `a/b/c`).
    pub step: String,
    pub label: String,
    pub operation: String,
    pub runner: String,
    pub effects: Vec<WorkflowEffect>,
    /// The runner's id (absent in records saved before schema /2 existed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub runner_id: Option<String>,
    /// Labels of the enclosing composite steps, outermost first (expanded
    /// children only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub composite_path: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowValidation {
    pub state: DraftState,
    /// Well-formed AND every step's runner available in the open project
    /// (set by [`apply_runnability`]; false from validation alone).
    pub runnable: bool,
    pub runnable_reason: String,
    pub diagnostics: Vec<WorkflowDiagnostic>,
    pub errors: u32,
    pub warnings: u32,
    /// Union of the steps' declared effects.
    pub effects: Vec<WorkflowEffect>,
    /// Steps in dependency order (empty when the connections form a cycle).
    pub plan: Vec<PlannedStep>,
}

/// What validation may know about the surrounding project.
#[derive(Default)]
pub struct ValidationContext<'a> {
    /// Ceiling of the requester (e.g. an agent task's permissions); the
    /// workflow's own ceiling must fit inside it.
    pub ceiling: Option<AuthorityCeiling>,
    /// Whether a project file exists (None: not checked).
    pub file_exists: Option<&'a dyn Fn(&WorkspacePath) -> bool>,
    /// Resolves a saved workflow revision for composite steps (None: not
    /// checked — composite steps get `workflow.compositeUnchecked` and the
    /// draft cannot be runnable).
    pub resolve_workflow: Option<&'a ResolveWorkflow<'a>>,
    /// The saved workflow this definition is a revision of, when known: a
    /// composite step may not include it (at any revision).
    pub workflow_id: Option<&'a WorkflowId>,
}

/// Looks up a saved workflow revision (composite children).
pub type ResolveWorkflow<'a> = dyn Fn(&WorkflowId, u64) -> Option<WorkflowDefinition> + 'a;

fn id_ok(id: &str) -> bool {
    !id.is_empty() && id.len() <= 32 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn text_ok(value: &str, max: usize) -> Result<(), String> {
    crate::validate::check_text(value, max, "the text")
}

fn hash_ok(h: &str) -> bool {
    h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit())
}

struct Diags(Vec<WorkflowDiagnostic>);

impl Diags {
    fn push(
        &mut self,
        severity: DiagnosticSeverity,
        code: &str,
        rule: Option<u8>,
        message: impl Into<String>,
    ) -> &mut WorkflowDiagnostic {
        self.0.push(WorkflowDiagnostic {
            severity,
            code: code.to_owned(),
            message: message.into(),
            rule,
            step: None,
            port: None,
            input: None,
            outcome: None,
            connection: None,
        });
        self.0.last_mut().expect("just pushed")
    }

    fn error(&mut self, code: &str, rule: Option<u8>, message: impl Into<String>) -> &mut WorkflowDiagnostic {
        self.push(DiagnosticSeverity::Error, code, rule, message)
    }

    fn warning(&mut self, code: &str, rule: Option<u8>, message: impl Into<String>) -> &mut WorkflowDiagnostic {
        self.push(DiagnosticSeverity::Warning, code, rule, message)
    }
}

trait At {
    fn step(&mut self, s: &str) -> &mut Self;
    fn port(&mut self, p: &str) -> &mut Self;
    fn input(&mut self, i: &str) -> &mut Self;
    fn outcome(&mut self, o: &str) -> &mut Self;
    fn connection(&mut self, c: usize) -> &mut Self;
}

impl At for WorkflowDiagnostic {
    fn step(&mut self, s: &str) -> &mut Self {
        self.step = Some(s.to_owned());
        self
    }
    fn port(&mut self, p: &str) -> &mut Self {
        self.port = Some(p.to_owned());
        self
    }
    fn input(&mut self, i: &str) -> &mut Self {
        self.input = Some(i.to_owned());
        self
    }
    fn outcome(&mut self, o: &str) -> &mut Self {
        self.outcome = Some(o.to_owned());
        self
    }
    fn connection(&mut self, c: usize) -> &mut Self {
        self.connection = Some(c as u32);
        self
    }
}

/// Parse a definition from untrusted JSON text (imports, agents): bounded
/// size, strict schema (unknown fields — free-form expressions, loops,
/// extra outputs, trust or run hooks — are refused).
pub fn parse_definition(text: &str) -> Result<WorkflowDefinition, String> {
    if text.len() > MAX_DEFINITION_BYTES {
        return Err(format!("the workflow definition is larger than {} KiB", MAX_DEFINITION_BYTES / 1024));
    }
    let raw: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("the workflow definition is not valid: {e}"))?;
    definition_from_value(raw)
}

/// Strictly convert untrusted JSON into a definition: `deny_unknown_fields`
/// plus a structural check that every key of the input is carried by the
/// parsed definition (serde ignores extra keys on unit variants of tagged
/// enums, e.g. `{"kind": "unset", "trust": true}`).
pub(crate) fn definition_from_value(raw: serde_json::Value) -> Result<WorkflowDefinition, String> {
    let definition = serde_json::from_value::<WorkflowDefinition>(raw.clone())
        .map_err(|e| format!("the workflow definition is not valid: {e}"))?;
    let carried =
        serde_json::to_value(&definition).map_err(|e| format!("the workflow definition is not valid: {e}"))?;
    if let Some(at) = dropped_key(&raw, &carried, String::new()) {
        return Err(format!("the workflow definition is not valid: unknown field at {at}"));
    }
    Ok(definition)
}

/// The JSON-pointer path of the first object key in `input` that `parsed`
/// does not carry (keys only; scalar values may be normalized by parsing).
pub(crate) fn dropped_key(input: &serde_json::Value, parsed: &serde_json::Value, at: String) -> Option<String> {
    use serde_json::Value;
    match (input, parsed) {
        (Value::Object(a), Value::Object(b)) => a.iter().find_map(|(k, v)| {
            let here = format!("{at}/{}", k.replace('~', "~0").replace('/', "~1"));
            match b.get(k) {
                None => Some(here),
                Some(w) => dropped_key(v, w, here),
            }
        }),
        (Value::Array(a), Value::Array(b)) => {
            if a.len() != b.len() {
                return Some(at);
            }
            a.iter().zip(b).enumerate().find_map(|(i, (v, w))| dropped_key(v, w, format!("{at}/{i}")))
        }
        (Value::Object(_) | Value::Array(_), _) => Some(at),
        _ => None,
    }
}

/// Content hash of a definition (identifies a revision).
pub fn definition_hash(definition: &WorkflowDefinition) -> String {
    use sha2::{Digest, Sha256};
    let bytes = serde_json::to_vec(definition).unwrap_or_default();
    Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Validate a definition against the strict contract. Never fails: problems
/// are diagnostics. `runnable` is false here; [`apply_runnability`] decides it.
/// Composite steps are resolved through `context.resolve_workflow` and their
/// children validated too; the plan lists the expanded steps.
pub fn validate_workflow(definition: &WorkflowDefinition, context: &ValidationContext<'_>) -> WorkflowValidation {
    let mut nest = v2::Nest::new(context.workflow_id);
    validate_in(definition, context, &mut nest).validation
}

/// A validation with what a parent composite needs from it.
pub(crate) struct Validated {
    pub validation: WorkflowValidation,
    /// Contracts of the steps that have one (composites: their dynamic contract).
    pub contracts: BTreeMap<String, OperationContract>,
    /// Steps after expanding composites.
    pub leaves: usize,
}

fn validate_in(definition: &WorkflowDefinition, context: &ValidationContext<'_>, nest: &mut v2::Nest) -> Validated {
    let mut d = Diags(Vec::new());
    let def = definition;

    // ----- schema version and size limits -----
    if !WORKFLOW_SCHEMAS.contains(&def.schema.as_str()) {
        d.error(
            "workflow.schema",
            None,
            format!(
                "unsupported workflow schema {:?} (this version reads {WORKFLOW_SCHEMA} and {WORKFLOW_SCHEMA_V2})",
                truncate(&def.schema, 60)
            ),
        );
    } else if def.schema == WORKFLOW_SCHEMA && def.uses_v2() {
        d.error(
            "workflow.schema",
            None,
            format!(
                "execution options, composite steps, conditions, and repeats need schema {WORKFLOW_SCHEMA_V2} (this definition declares {WORKFLOW_SCHEMA})"
            ),
        );
    }
    if def.title.trim().is_empty() {
        d.error("workflow.title", None, "the workflow needs a title");
    }
    if let Err(m) = text_ok(&def.title, MAX_TITLE)
        .and_then(|_| if def.title.contains('\n') { Err("the title must be one line".into()) } else { Ok(()) })
    {
        d.error("workflow.title", None, m);
    }
    if let Err(m) = text_ok(&def.description, MAX_DESCRIPTION) {
        d.error("workflow.description", None, m);
    }
    if let Some(p) = &def.preset
        && !(1..=64).contains(&p.len())
    {
        d.error("workflow.preset", None, "invalid preset name");
    }
    if let Some(execution) = &def.execution
        && !(1..=MAX_PARALLEL).contains(&execution.max_parallel)
    {
        d.error(
            "workflow.maxParallel",
            None,
            format!("run 1–{MAX_PARALLEL} independent steps at once (it says {})", execution.max_parallel),
        );
    }
    let limits = [
        (def.steps.len(), MAX_STEPS, "steps"),
        (def.connections.len(), MAX_CONNECTIONS, "connections"),
        (def.inputs.len(), MAX_INPUTS, "inputs"),
        (def.outcomes.len(), MAX_OUTCOMES, "outcomes"),
    ];
    let mut too_large = false;
    for (n, max, what) in limits {
        if n > max {
            d.error("workflow.tooLarge", None, format!("a workflow can have at most {max} {what} (it has {n})"));
            too_large = true;
        }
    }
    if too_large {
        let validation = finish(d, def, &BTreeMap::new(), &BTreeMap::new(), None);
        return Validated { validation, contracts: BTreeMap::new(), leaves: def.steps.len() };
    }

    // ----- ceiling -----
    if let Some(outer) = &context.ceiling
        && !def.ceiling.within(outer)
    {
        d.error(
            "workflow.ceilingExceeded",
            Some(4),
            "the workflow's ceiling allows more than this context permits (execution, model requests, retention, or proposals)",
        );
    }

    // ----- inputs -----
    let mut inputs: HashMap<&str, &WorkflowInput> = HashMap::new();
    for input in &def.inputs {
        if !id_ok(&input.id) {
            d.error("workflow.invalidId", None, "input ids are 1–32 letters, digits, '-' or '_'").input(&input.id);
            continue;
        }
        if inputs.insert(input.id.as_str(), input).is_some() {
            d.error("workflow.duplicateId", None, format!("input id {:?} is used twice", input.id)).input(&input.id);
            continue;
        }
        if let Err(m) = crate::validate::check_label(&input.label) {
            d.error("workflow.label", None, format!("input {:?}: {m}", input.id)).input(&input.id);
        }
        if !input.port_type.is_file_input() {
            d.error(
                "workflow.inputType",
                Some(6),
                format!(
                    "an external input cannot be {} — that type is produced only by an operation",
                    input.port_type.title()
                ),
            )
            .input(&input.id);
            continue;
        }
        match &input.source {
            InputSource::Unset => {
                d.error("workflow.inputUnset", Some(1), format!("choose the project file for input “{}”", input.label))
                    .input(&input.id);
            }
            InputSource::Resource { path, version } => {
                if let crate::admission::Admission::Exclude(reason) = crate::admission::classify(path) {
                    d.error(
                        "workflow.inputNotResource",
                        Some(4),
                        format!("{path} cannot be a workflow input: {reason}"),
                    )
                    .input(&input.id);
                }
                let ext = path.extension().map(|e| e.to_ascii_lowercase()).unwrap_or_default();
                if !input.port_type.extensions().contains(&ext.as_str()) {
                    d.error(
                        "workflow.inputTypeMismatch",
                        Some(3),
                        format!("{path} is not a {} file", input.port_type.title()),
                    )
                    .input(&input.id);
                }
                if let InputVersion::Pinned { content_hash } = version
                    && !hash_ok(content_hash)
                {
                    d.error("workflow.inputVersion", Some(1), "a pinned input needs a SHA-256 content hash")
                        .input(&input.id);
                }
                if let Some(exists) = context.file_exists
                    && !exists(path)
                {
                    d.error("workflow.inputMissing", Some(1), format!("{path} does not exist in the project"))
                        .input(&input.id);
                }
            }
        }
    }

    // ----- steps -----
    let mut contracts: BTreeMap<String, OperationContract> = BTreeMap::new();
    // Composite step -> its child's expanded plan.
    let mut expanded: BTreeMap<String, Vec<PlannedStep>> = BTreeMap::new();
    let mut leaves = 0usize;
    let mut seen_steps: BTreeSet<&str> = BTreeSet::new();
    for step in &def.steps {
        if !id_ok(&step.id) {
            d.error("workflow.invalidId", None, "step ids are 1–32 letters, digits, '-' or '_'").step(&step.id);
            continue;
        }
        if !seen_steps.insert(step.id.as_str()) {
            d.error("workflow.duplicateId", None, format!("step id {:?} is used twice", step.id)).step(&step.id);
            continue;
        }
        if let Err(m) = crate::validate::check_label(&step.label) {
            d.error("workflow.label", None, format!("step {:?}: {m}", step.id)).step(&step.id);
        }
        let composite_operation = step.operation.id == COMPOSITE_OPERATION;
        if composite_operation != step.composite.is_some() {
            let message = if composite_operation {
                format!("composite step “{}” must name the saved workflow revision it runs", step.label)
            } else {
                format!(
                    "step “{}” names a saved workflow but only a {COMPOSITE_OPERATION} step can run one",
                    step.label
                )
            };
            d.error("workflow.compositeOperation", None, message).step(&step.id);
            continue;
        }
        let Some(contract) = registry::operation(&step.operation.id, step.operation.version) else {
            if registry::is_registered(&step.operation.id) {
                d.error(
                    "workflow.unsupportedVersion",
                    None,
                    format!(
                        "{} version {} is not supported by this version",
                        step.operation.id, step.operation.version
                    ),
                )
                .step(&step.id);
            } else {
                d.error(
                    "workflow.unknownOperation",
                    None,
                    format!(
                        "{:?} is not a registered operation; steps must name a registered operation",
                        truncate(&step.operation.id, 60)
                    ),
                )
                .step(&step.id);
            }
            continue;
        };
        let repeated_parameter = match &step.repeat {
            Some(StepRepeat::ForEach { parameter, .. }) => Some(parameter.as_str()),
            _ => None,
        };
        check_parameters(&mut d, step, &contract, repeated_parameter);
        check_payload(&mut d, step, &contract);
        if let Some(reference) = &step.composite {
            if let Some(child) = v2::resolve_composite(&mut d, step, reference, &def.ceiling, contract, context, nest) {
                leaves += child.leaves;
                expanded.insert(step.id.clone(), child.plan);
                contracts.insert(step.id.clone(), child.contract);
            }
            continue;
        }
        leaves += 1;
        contracts.insert(step.id.clone(), contract);
    }
    let step_known = |id: &str| seen_steps.contains(id);
    // Composite steps whose child could not be checked: their connections
    // still order and use, nothing else is known about their ports.
    let opaque: BTreeSet<&str> = def
        .steps
        .iter()
        .filter(|s| s.composite.is_some() && seen_steps.contains(s.id.as_str()) && !contracts.contains_key(&s.id))
        .map(|s| s.id.as_str())
        .collect();
    // Steps that run once per value (their outputs are collected, in order).
    let repeated: BTreeSet<&str> = def
        .steps
        .iter()
        .filter(|s| matches!(s.repeat, Some(StepRepeat::ForEach { .. })))
        .map(|s| s.id.as_str())
        .collect();

    // ----- connections -----
    // Data edges into (step, port); execution edges for ordering/reachability.
    let mut into: HashMap<(String, String), Vec<(usize, PortType)>> = HashMap::new();
    let mut exec_edges: BTreeSet<(String, String)> = BTreeSet::new();
    // Step-to-step data deliveries (a prerequisite orders steps but carries nothing).
    let mut data_edges: BTreeSet<(String, String)> = BTreeSet::new();
    let mut annotated: BTreeSet<(String, String)> = BTreeSet::new();
    let mut used_inputs: BTreeSet<String> = BTreeSet::new();
    for (i, conn) in def.connections.iter().enumerate() {
        match conn {
            WorkflowConnection::Data { from, to } => {
                let Some(target) = contracts.get(&to.step) else {
                    if !step_known(&to.step) {
                        d.error(
                            "workflow.unknownStep",
                            Some(3),
                            format!("connection {i} targets unknown step {:?}", to.step),
                        )
                        .connection(i);
                    } else if opaque.contains(to.step.as_str()) {
                        match from {
                            PortSource::Step { step, .. } if step_known(step) && step != &to.step => {
                                exec_edges.insert((step.clone(), to.step.clone()));
                                data_edges.insert((step.clone(), to.step.clone()));
                            }
                            PortSource::Input { input } if inputs.contains_key(input.as_str()) => {
                                used_inputs.insert(input.clone());
                            }
                            _ => {}
                        }
                    }
                    continue;
                };
                let Some(target_port) = target.input(&to.port) else {
                    d.error(
                        "workflow.unknownPort",
                        Some(3),
                        format!("{} has no input port {:?}", target.title, truncate(&to.port, 40)),
                    )
                    .connection(i)
                    .step(&to.step);
                    continue;
                };
                let source_type = match from {
                    PortSource::Step { step, port } => {
                        if step == &to.step {
                            d.error("workflow.cycle", None, "a step cannot feed itself").connection(i).step(step);
                            continue;
                        }
                        let Some(source) = contracts.get(step) else {
                            if !step_known(step) {
                                d.error(
                                    "workflow.unknownStep",
                                    Some(3),
                                    format!("connection {i} starts at unknown step {:?}", truncate(step, 40)),
                                )
                                .connection(i);
                            } else if opaque.contains(step.as_str()) {
                                exec_edges.insert((step.clone(), to.step.clone()));
                                data_edges.insert((step.clone(), to.step.clone()));
                                into.entry((to.step.clone(), to.port.clone()))
                                    .or_default()
                                    .push((i, target_port.types[0]));
                            }
                            continue;
                        };
                        let Some(out) = source.output(port) else {
                            d.error(
                                "workflow.unknownPort",
                                Some(3),
                                format!("{} has no output port {:?}", source.title, truncate(port, 40)),
                            )
                            .connection(i)
                            .step(step);
                            continue;
                        };
                        exec_edges.insert((step.clone(), to.step.clone()));
                        data_edges.insert((step.clone(), to.step.clone()));
                        out.types[0]
                    }
                    PortSource::Input { input } => {
                        let Some(inp) = inputs.get(input.as_str()) else {
                            d.error(
                                "workflow.unknownInput",
                                Some(3),
                                format!("connection {i} starts at unknown input {:?}", truncate(input, 40)),
                            )
                            .connection(i);
                            continue;
                        };
                        used_inputs.insert(input.clone());
                        inp.port_type
                    }
                };
                if !target_port.types.contains(&source_type) {
                    let owned = target_port.types.iter().find(|t| t.is_service_owned());
                    let (code, rule, message) = match owned {
                        Some(t) => (
                            "workflow.serviceOwnedType",
                            6,
                            format!(
                                "{}’s “{}” accepts only {} produced by {}; {} cannot stand in for it",
                                target.title,
                                target_port.label,
                                t.title(),
                                t.owner().unwrap_or("its service"),
                                source_type.title()
                            ),
                        ),
                        None => (
                            "workflow.incompatibleTypes",
                            3,
                            format!(
                                "{}’s “{}” accepts {}, not {}",
                                target.title,
                                target_port.label,
                                target_port.types.iter().map(|t| t.title()).collect::<Vec<_>>().join(" or "),
                                source_type.title()
                            ),
                        ),
                    };
                    d.error(code, Some(rule), message).connection(i).step(&to.step).port(&to.port);
                    continue;
                }
                // A code step runs only scripts of its own language (rule 3):
                // an R batch never runs a `.py` file, Python never an `.R` one.
                if let PortSource::Input { input } = from
                    && source_type == PortType::CodeSource
                    && let Some(WorkflowInput { source: InputSource::Resource { path, .. }, .. }) =
                        inputs.get(input.as_str()).copied()
                    && let Err(message) = registry::check_script_language(target, path.as_str())
                {
                    d.error("workflow.inputTypeMismatch", Some(3), message)
                        .connection(i)
                        .step(&to.step)
                        .port(&to.port)
                        .input(input);
                    continue;
                }
                if let PortSource::Step { step, .. } = from
                    && repeated.contains(step.as_str())
                    && !target_port.multiple
                {
                    d.error(
                        "workflow.iterationFanIn",
                        Some(3),
                        format!(
                            "{}’s “{}” takes one value, but step {:?} repeats; connect it to an input that collects several",
                            target.title,
                            target_port.label,
                            truncate(step, 40)
                        ),
                    )
                    .connection(i)
                    .step(&to.step)
                    .port(&to.port);
                }
                into.entry((to.step.clone(), to.port.clone())).or_default().push((i, source_type));
            }
            WorkflowConnection::Prerequisite { before, after } => {
                if before == after {
                    d.error("workflow.cycle", None, "a step cannot be its own prerequisite").connection(i).step(before);
                    continue;
                }
                let mut ok = true;
                for s in [before, after] {
                    if !step_known(s) {
                        d.error(
                            "workflow.unknownStep",
                            Some(3),
                            format!("prerequisite {i} names unknown step {:?}", truncate(s, 40)),
                        )
                        .connection(i);
                        ok = false;
                    }
                }
                if ok {
                    exec_edges.insert((before.clone(), after.clone()));
                }
            }
            WorkflowConnection::Annotation { from, to, label, .. } => {
                for s in [from, to] {
                    if !step_known(s) && !inputs.contains_key(s.as_str()) {
                        d.error(
                            "workflow.unknownStep",
                            None,
                            format!("annotation {i} names unknown step {:?}", truncate(s, 40)),
                        )
                        .connection(i);
                    }
                }
                if let Err(m) = text_ok(label, 200) {
                    d.error("workflow.label", None, m).connection(i);
                }
                annotated.insert((from.clone(), to.clone()));
            }
        }
    }

    // ----- conditions and repeats (schema /2) -----
    v2::check_conditions_and_repeats(&mut d, def, &contracts, &seen_steps, &repeated, &mut exec_edges, &mut data_edges);

    // ----- rule 1: required inputs bound -----
    for step in &def.steps {
        let Some(contract) = contracts.get(&step.id) else { continue };
        for port in &contract.inputs {
            let bound = into.get(&(step.id.clone(), port.name.clone()));
            let count = bound.map(|v| v.len()).unwrap_or(0);
            if count > 1 && !port.multiple {
                d.error(
                    "workflow.multipleBindings",
                    Some(3),
                    format!("“{}” of step “{}” accepts one connection, not {count}", port.label, step.label),
                )
                .step(&step.id)
                .port(&port.name);
            }
            let payload_alternative = matches!(
                &contract.payload,
                Some(PayloadSpec::Code { unless_input: Some(alt), .. }) if alt == &port.name
            );
            if port.required && count == 0 && !payload_alternative {
                let annotation_only = annotated.iter().any(|(_, to)| to == &step.id);
                let mut message =
                    format!("“{}” of step “{}” is not bound to a compatible output or input", port.label, step.label);
                if annotation_only {
                    message.push_str("; an annotation line does not deliver data");
                }
                d.error("workflow.unboundInput", Some(1), message).step(&step.id).port(&port.name);
            }
        }
        // Code payload or its alternative input.
        if let Some(PayloadSpec::Code { unless_input: Some(alt), .. }) = &contract.payload {
            let has_code = matches!(step.payload, Some(StepPayload::Code { .. }));
            let has_input = into.contains_key(&(step.id.clone(), alt.clone()));
            if !has_code && !has_input {
                d.error(
                    "workflow.missingCode",
                    Some(1),
                    format!("step “{}” needs code: write it here or bind a script input", step.label),
                )
                .step(&step.id);
            } else if has_code && has_input {
                d.error(
                    "workflow.ambiguousCode",
                    None,
                    format!("step “{}” has both inline code and a script input; keep one", step.label),
                )
                .step(&step.id);
            }
        }
    }
    for input in &def.inputs {
        if !used_inputs.contains(&input.id) && inputs.contains_key(input.id.as_str()) {
            d.warning("workflow.unusedInput", None, format!("input “{}” is not connected to any step", input.label))
                .input(&input.id);
        }
    }

    // ----- outcomes -----
    let mut outcome_steps: BTreeSet<String> = BTreeSet::new();
    let mut outcome_ids: BTreeSet<&str> = BTreeSet::new();
    let mut required_outcome = false;
    for outcome in &def.outcomes {
        if !id_ok(&outcome.id) || !outcome_ids.insert(outcome.id.as_str()) {
            d.error(
                "workflow.invalidId",
                None,
                format!("outcome id {:?} is invalid or used twice", truncate(&outcome.id, 40)),
            )
            .outcome(&outcome.id);
            continue;
        }
        if let Err(m) = crate::validate::check_label(&outcome.label) {
            d.error("workflow.label", None, format!("outcome {:?}: {m}", outcome.id)).outcome(&outcome.id);
        }
        let Some(contract) = contracts.get(&outcome.from.step) else {
            if !step_known(&outcome.from.step) {
                d.error("workflow.unknownStep", Some(2), format!("outcome “{}” names an unknown step", outcome.label))
                    .outcome(&outcome.id);
            } else if opaque.contains(outcome.from.step.as_str()) {
                outcome_steps.insert(outcome.from.step.clone());
                required_outcome |= !outcome.optional;
            }
            continue;
        };
        let Some(port) = contract.output(&outcome.from.port) else {
            d.error(
                "workflow.unknownPort",
                Some(2),
                format!("{} has no output {:?}", contract.title, truncate(&outcome.from.port, 40)),
            )
            .outcome(&outcome.id);
            continue;
        };
        if repeated.contains(outcome.from.step.as_str()) {
            d.error(
                "workflow.iterationFanIn",
                Some(3),
                format!(
                    "outcome “{}” comes from a repeating step; collect its outputs in a step that takes several first",
                    outcome.label
                ),
            )
            .outcome(&outcome.id)
            .step(&outcome.from.step);
            continue;
        }
        let produced = port.types[0];
        if !outcome.kind.accepts().contains(&produced) {
            let rule = if outcome.kind == OutcomeKind::AuditEvidence { 6 } else { 3 };
            let code = if rule == 6 { "workflow.serviceOwnedType" } else { "workflow.outcomeType" };
            d.error(
                code,
                Some(rule),
                format!(
                    "{} cannot come from {}’s {} output ({}); it needs {}",
                    outcome.kind.title(),
                    contract.title,
                    port.label,
                    produced.title(),
                    outcome.kind.accepts().iter().map(|t| t.title()).collect::<Vec<_>>().join(" or ")
                ),
            )
            .outcome(&outcome.id)
            .step(&outcome.from.step);
            continue;
        }
        outcome_steps.insert(outcome.from.step.clone());
        required_outcome |= !outcome.optional;
    }
    if !def.steps.is_empty() && def.outcomes.is_empty() {
        d.error("workflow.noOutcome", Some(2), "declare at least one outcome the workflow produces");
    } else if !def.outcomes.is_empty() && !required_outcome {
        d.error(
            "workflow.noOutcome",
            Some(2),
            "at least one outcome must be required (not only optional destinations)",
        );
    }
    if def.steps.is_empty() {
        d.error("workflow.empty", None, "add at least one step");
    }

    // ----- cycles (no loops) -----
    let order = topological_order(&def.steps, &exec_edges);
    if order.is_none() {
        d.error("workflow.cycle", None, "the connections form a cycle; workflows cannot loop");
    }

    // ----- rule 2: every step contributes to an outcome -----
    // Contribution follows data: a step whose outputs reach no outcome adds
    // nothing to the result, even if a "runs after" link orders it first.
    let reach = |edges: &BTreeSet<(String, String)>| {
        let mut reached: BTreeSet<String> = outcome_steps.clone();
        let mut queue: VecDeque<String> = outcome_steps.iter().cloned().collect();
        while let Some(s) = queue.pop_front() {
            for (from, to) in edges {
                if to == &s && reached.insert(from.clone()) {
                    queue.push_back(from.clone());
                }
            }
        }
        reached
    };
    let contributing = reach(&data_edges);
    let ordered_only = reach(&exec_edges);
    for step in &def.steps {
        if contracts.contains_key(&step.id) && !contributing.contains(&step.id) {
            let message = if ordered_only.contains(&step.id) {
                format!(
                    "step “{}” does not lead to any declared outcome: its outputs are not used (a “runs after” link carries no data)",
                    step.label
                )
            } else {
                format!("step “{}” does not lead to any declared outcome", step.label)
            };
            d.error("workflow.orphanStep", Some(2), message).step(&step.id);
        }
    }

    // ----- rule 4: effects within the ceiling -----
    for step in &def.steps {
        let Some(contract) = contracts.get(&step.id) else { continue };
        for effect in &contract.effects {
            if !def.ceiling.allows(*effect) {
                d.error(
                    "workflow.effectNotAllowed",
                    Some(4),
                    format!("step “{}” {} — the workflow's ceiling does not allow it", step.label, effect.title()),
                )
                .step(&step.id);
            }
            if !contract.runner.allows.contains(effect) {
                d.error(
                    "workflow.containment",
                    Some(4),
                    format!("{} cannot contain this effect: {}", contract.runner.title, effect.title()),
                )
                .step(&step.id);
            }
        }
    }

    if leaves > MAX_EXPANDED_STEPS {
        d.error(
            "workflow.tooManySteps",
            None,
            format!(
                "a workflow can have at most {MAX_EXPANDED_STEPS} steps after expanding composites (it has {leaves})"
            ),
        );
    }

    let validation = finish(d, def, &contracts, &expanded, order);
    Validated { validation, contracts, leaves }
}

/// Decide runnability: the draft is well-formed and every step's runner is
/// available now (`runner` maps a runner id to `Ok` or the reason it cannot
/// run). Sets `runnable` and a precise `runnable_reason`.
pub fn apply_runnability(
    validation: &mut WorkflowValidation,
    definition: &WorkflowDefinition,
    runner: &dyn Fn(&str) -> Result<(), String>,
) {
    validation.runnable = false;
    if validation.state != DraftState::WellFormed {
        validation.runnable_reason = format!(
            "The draft has {} error{}; fix {} before it can run.",
            validation.errors,
            if validation.errors == 1 { "" } else { "s" },
            if validation.errors == 1 { "it" } else { "them" }
        );
        return;
    }
    if let Some(unchecked) = validation.diagnostics.iter().find(|x| x.code == "workflow.compositeUnchecked") {
        validation.runnable_reason = format!("Not runnable: {}.", unchecked.message);
        return;
    }
    let mut problems = Vec::new();
    for step in &definition.steps {
        if step.composite.is_some() {
            // Its expanded children run through their own runners.
            let prefix = format!("{}/", step.id);
            let children: Vec<&PlannedStep> = validation.plan.iter().filter(|p| p.step.starts_with(&prefix)).collect();
            if children.is_empty() {
                problems.push(format!("composite step “{}” has no checked steps", step.label));
            }
            for child in children {
                match &child.runner_id {
                    Some(id) => {
                        if let Err(reason) = runner(id) {
                            problems.push(format!("step “{}” ({}): {reason}", child.label, child.runner));
                        }
                    }
                    None => problems.push(format!("step “{}” was not checked", child.label)),
                }
            }
            continue;
        }
        let Some(contract) = registry::operation(&step.operation.id, step.operation.version) else {
            problems.push(format!("step “{}” names an unknown operation", step.label));
            continue;
        };
        if let Err(reason) = runner(&contract.runner.id) {
            problems.push(format!("step “{}” ({}): {reason}", step.label, contract.runner.title));
        }
    }
    if problems.is_empty() {
        validation.runnable = true;
        validation.runnable_reason =
            "Ready to run: every step's runner is available. Review the run plan to start it.".to_owned();
    } else {
        validation.runnable_reason = format!("Not runnable now: {}", problems.join("; "));
    }
}

/// What is wrong with a parameter value, if anything.
pub(crate) fn parameter_problem(kind: &ParameterKind, value: &ParamValue) -> Option<String> {
    match (kind, value) {
        (ParameterKind::Text { max_len }, ParamValue::Text(t)) => text_ok(t, *max_len as usize).err(),
        (ParameterKind::Integer { min, max }, ParamValue::Integer(v)) => {
            (!(min..=max).contains(&v)).then(|| format!("must be between {min} and {max}"))
        }
        (ParameterKind::Boolean, ParamValue::Boolean(_)) => None,
        (ParameterKind::Path { extensions }, ParamValue::Text(t)) => match WorkspacePath::parse(t) {
            Err(e) => Some(e.to_string()),
            Ok(p) if p.as_str() != t => Some("write the path in normalized form".into()),
            Ok(p) => {
                let ext = p.extension().map(|e| e.to_ascii_lowercase()).unwrap_or_default();
                (!extensions.iter().any(|e| e == &ext)).then(|| format!("must be one of: {}", extensions.join(", ")))
            }
        },
        _ => Some("has the wrong type".into()),
    }
}

/// `repeated`: the parameter a `forEach` repeat supplies (not missing).
fn check_parameters(d: &mut Diags, step: &WorkflowStep, contract: &OperationContract, repeated: Option<&str>) {
    if step.parameters.len() > MAX_PARAMETERS {
        d.error("workflow.tooLarge", None, format!("at most {MAX_PARAMETERS} parameters per step")).step(&step.id);
        return;
    }
    for (name, value) in &step.parameters {
        let Some(spec) = contract.parameter(name) else {
            d.error(
                "workflow.unknownParameter",
                None,
                format!("{} has no parameter {:?}", contract.title, truncate(name, 40)),
            )
            .step(&step.id)
            .port(name);
            continue;
        };
        if let Some(problem) = parameter_problem(&spec.kind, value) {
            d.error("workflow.invalidParameter", None, format!("“{}” {problem}", spec.label)).step(&step.id).port(name);
        }
    }
    for spec in &contract.parameters {
        if spec.required && !step.parameters.contains_key(&spec.name) && repeated != Some(spec.name.as_str()) {
            d.error("workflow.missingParameter", Some(1), format!("step “{}” needs “{}”", step.label, spec.label))
                .step(&step.id)
                .port(&spec.name);
        }
    }
}

fn check_payload(d: &mut Diags, step: &WorkflowStep, contract: &OperationContract) {
    match (&contract.payload, &step.payload) {
        (_, None) => {
            if matches!(contract.payload, Some(PayloadSpec::Prompt)) {
                d.error("workflow.missingPrompt", Some(1), format!("step “{}” needs instructions", step.label))
                    .step(&step.id);
            }
        }
        (None, Some(_)) => {
            d.error("workflow.unexpectedPayload", None, format!("{} takes no code or prompt", contract.title))
                .step(&step.id);
        }
        (Some(PayloadSpec::Code { language, .. }), Some(StepPayload::Code { language: got, source })) => {
            if language != got {
                d.error("workflow.payloadLanguage", None, "this operation runs a different language").step(&step.id);
            }
            if source.len() > MAX_CODE_BYTES {
                d.error("workflow.tooLarge", None, format!("code is limited to {} KiB", MAX_CODE_BYTES / 1024))
                    .step(&step.id);
            } else if source.contains('\0') {
                d.error("workflow.payload", None, "code contains NUL characters").step(&step.id);
            }
        }
        (Some(PayloadSpec::Prompt), Some(StepPayload::Prompt { text })) => {
            if text.trim().is_empty() {
                d.error("workflow.missingPrompt", Some(1), format!("step “{}” needs instructions", step.label))
                    .step(&step.id);
            } else if let Err(m) = text_ok(text, MAX_PROMPT_BYTES) {
                d.error("workflow.payload", None, m).step(&step.id);
            }
        }
        _ => {
            d.error("workflow.payloadKind", None, format!("{} takes a different kind of payload", contract.title))
                .step(&step.id);
        }
    }
}

/// Kahn's algorithm over steps (in declaration order for stability).
fn topological_order(steps: &[WorkflowStep], edges: &BTreeSet<(String, String)>) -> Option<Vec<String>> {
    let ids: Vec<&str> = steps.iter().map(|s| s.id.as_str()).collect();
    let mut indegree: HashMap<&str, usize> = ids.iter().map(|id| (*id, 0)).collect();
    for (from, to) in edges {
        if indegree.contains_key(from.as_str())
            && let Some(n) = indegree.get_mut(to.as_str())
        {
            *n += 1;
        }
    }
    let mut ready: VecDeque<&str> = ids.iter().copied().filter(|id| indegree[id] == 0).collect();
    let mut order = Vec::new();
    while let Some(id) = ready.pop_front() {
        order.push(id.to_owned());
        for (from, to) in edges {
            if from == id
                && let Some(n) = indegree.get_mut(to.as_str())
            {
                *n -= 1;
                if *n == 0 {
                    ready.push_back(to.as_str());
                }
            }
        }
    }
    (order.len() == indegree.len()).then_some(order)
}

fn finish(
    d: Diags,
    def: &WorkflowDefinition,
    contracts: &BTreeMap<String, OperationContract>,
    expanded: &BTreeMap<String, Vec<PlannedStep>>,
    order: Option<Vec<String>>,
) -> WorkflowValidation {
    let diagnostics = d.0;
    let errors = diagnostics.iter().filter(|x| x.severity == DiagnosticSeverity::Error).count() as u32;
    let warnings = diagnostics.iter().filter(|x| x.severity == DiagnosticSeverity::Warning).count() as u32;
    let mut effects: Vec<WorkflowEffect> = contracts.values().flat_map(|c| c.effects.iter().copied()).collect();
    effects.sort();
    effects.dedup();
    let mut plan = Vec::new();
    for id in order.unwrap_or_default() {
        let Some(step) = def.steps.iter().find(|s| s.id == id) else { continue };
        if let Some(children) = expanded.get(&id) {
            // The child's own plan (already in its dependency order), in place.
            for child in children {
                let mut c = child.clone();
                c.step = format!("{id}/{}", c.step);
                let mut path = vec![step.label.clone()];
                path.extend(c.composite_path.take().unwrap_or_default());
                c.composite_path = Some(path);
                plan.push(c);
            }
            continue;
        }
        let Some(c) = contracts.get(&id) else { continue };
        plan.push(PlannedStep {
            step: id,
            label: step.label.clone(),
            operation: format!("{} (v{})", c.title, c.version),
            runner: c.runner.title.clone(),
            effects: c.effects.clone(),
            runner_id: Some(c.runner.id.clone()),
            composite_path: None,
        });
    }
    WorkflowValidation {
        state: if errors == 0 { DraftState::WellFormed } else { DraftState::Incomplete },
        runnable: false,
        runnable_reason: NOT_RUNNABLE_REASON.to_owned(),
        diagnostics,
        errors,
        warnings,
        effects,
        plan,
    }
}

fn truncate(s: &str, max: usize) -> String {
    crate::store::truncate(s, max)
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_interchange;
#[cfg(test)]
mod tests_v2;
