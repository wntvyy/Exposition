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
//!    unlock (execution arrives in a later version);
//! 6. service-owned types (formal evidence, retained artifacts) come only from
//!    their owning operation.
//!
//! plus: no cycles, loops, conditionals, or nesting (the schema has none),
//! schema versions, and size limits. Drafts may be incomplete and are saved
//! with their diagnostics; in this version nothing is ever runnable.

pub mod persist;
pub mod presets;
pub mod registry;

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use exposition_core::path::WorkspacePath;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ops::DiagnosticSeverity;
use crate::vocabulary::RelationKind;
pub use persist::{SaveWorkflowRequest, SavedWorkflow, WorkflowRevisionInfo, WorkflowSummary};
pub use registry::{
    CodeLanguage, OperationContract, ParameterKind, PayloadSpec, PortSpec, PortType, WorkflowEffect, operation,
    operations,
};

/// Schema identifier of a workflow definition.
pub const WORKFLOW_SCHEMA: &str = "exposition.workflow/1";
/// Why no workflow runs in this version.
pub const NOT_RUNNABLE_REASON: &str =
    "Workflow execution arrives in a later version of Exposition; this draft is saved but cannot run yet.";

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

/// Where a draft stands. A well-formed draft is still not runnable in this version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum DraftState {
    /// Has errors (unbound inputs, orphan steps, incompatible connections, ...).
    Incomplete,
    /// Satisfies the contract; would need a runner (later version) to execute.
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
    pub step: String,
    pub label: String,
    pub operation: String,
    pub runner: String,
    pub effects: Vec<WorkflowEffect>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowValidation {
    pub state: DraftState,
    /// Always false in this version (no runner exists yet).
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
}

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
/// size, strict schema (unknown fields — conditions, loops, nested
/// workflows, extra outputs, trust or run hooks — are refused).
pub fn parse_definition(text: &str) -> Result<WorkflowDefinition, String> {
    if text.len() > MAX_DEFINITION_BYTES {
        return Err(format!("the workflow definition is larger than {} KiB", MAX_DEFINITION_BYTES / 1024));
    }
    serde_json::from_str::<WorkflowDefinition>(text).map_err(|e| format!("the workflow definition is not valid: {e}"))
}

/// Content hash of a definition (identifies a revision).
pub fn definition_hash(definition: &WorkflowDefinition) -> String {
    use sha2::{Digest, Sha256};
    let bytes = serde_json::to_vec(definition).unwrap_or_default();
    Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Validate a definition against the strict contract. Never fails: problems
/// are diagnostics, and `runnable` is always false in this version.
pub fn validate_workflow(definition: &WorkflowDefinition, context: &ValidationContext<'_>) -> WorkflowValidation {
    let mut d = Diags(Vec::new());
    let def = definition;

    // ----- schema version and size limits -----
    if def.schema != WORKFLOW_SCHEMA {
        d.error(
            "workflow.schema",
            None,
            format!(
                "unsupported workflow schema {:?} (this version reads {WORKFLOW_SCHEMA})",
                truncate(&def.schema, 60)
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
        return finish(d, def, &BTreeMap::new(), None);
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
        check_parameters(&mut d, step, &contract);
        check_payload(&mut d, step, &contract);
        contracts.insert(step.id.clone(), contract);
    }
    let step_known = |id: &str| seen_steps.contains(id);

    // ----- connections -----
    // Data edges into (step, port); execution edges for ordering/reachability.
    let mut into: HashMap<(String, String), Vec<(usize, PortType)>> = HashMap::new();
    let mut exec_edges: BTreeSet<(String, String)> = BTreeSet::new();
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
    let mut contributing: BTreeSet<String> = outcome_steps.clone();
    let mut queue: VecDeque<String> = outcome_steps.into_iter().collect();
    while let Some(s) = queue.pop_front() {
        for (from, to) in &exec_edges {
            if to == &s && contributing.insert(from.clone()) {
                queue.push_back(from.clone());
            }
        }
    }
    for step in &def.steps {
        if contracts.contains_key(&step.id) && !contributing.contains(&step.id) {
            d.error(
                "workflow.orphanStep",
                Some(2),
                format!("step “{}” does not lead to any declared outcome", step.label),
            )
            .step(&step.id);
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

    finish(d, def, &contracts, order)
}

fn check_parameters(d: &mut Diags, step: &WorkflowStep, contract: &OperationContract) {
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
        let problem = match (&spec.kind, value) {
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
                    (!extensions.iter().any(|e| e == &ext))
                        .then(|| format!("must be one of: {}", extensions.join(", ")))
                }
            },
            _ => Some("has the wrong type".into()),
        };
        if let Some(problem) = problem {
            d.error("workflow.invalidParameter", None, format!("“{}” {problem}", spec.label)).step(&step.id).port(name);
        }
    }
    for spec in &contract.parameters {
        if spec.required && !step.parameters.contains_key(&spec.name) {
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
    order: Option<Vec<String>>,
) -> WorkflowValidation {
    let diagnostics = d.0;
    let errors = diagnostics.iter().filter(|x| x.severity == DiagnosticSeverity::Error).count() as u32;
    let warnings = diagnostics.iter().filter(|x| x.severity == DiagnosticSeverity::Warning).count() as u32;
    let mut effects: Vec<WorkflowEffect> = contracts.values().flat_map(|c| c.effects.iter().copied()).collect();
    effects.sort();
    effects.dedup();
    let plan = order
        .unwrap_or_default()
        .into_iter()
        .filter_map(|id| {
            let step = def.steps.iter().find(|s| s.id == id)?;
            let c = contracts.get(&id)?;
            Some(PlannedStep {
                step: id,
                label: step.label.clone(),
                operation: format!("{} (v{})", c.title, c.version),
                runner: c.runner.title.clone(),
                effects: c.effects.clone(),
            })
        })
        .collect();
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
