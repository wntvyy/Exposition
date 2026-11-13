//! The registered workflow operations (allowlist).
//!
//! A programmable step names one of these contracts by id and version; there
//! is no generic "evaluate anything" operation, and a definition cannot add,
//! widen, or redefine a contract. Each contract states its purpose, typed
//! ports, parameters, payload (code or prompt), effects, runner/containment,
//! limits, and failure behaviour. The registry alone never claims a runner
//! is available: availability depends on the open project (trust, restricted
//! backend, toolchains) and is filled in by the workflow runner service
//! ([`with_availability`]).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Typed data that flows along workflow data connections. A closed set: a
/// definition cannot invent new types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum PortType {
    /// Research code (a Python file).
    CodeSource,
    /// A data file (CSV, JSON, Parquet, ...).
    Dataset,
    /// A figure file.
    Figure,
    /// A table file.
    Table,
    /// A manuscript or note (read as material).
    Document,
    /// A Lean source file.
    FormalSource,
    /// Locally checked formal evidence (audited declarations, statements,
    /// axioms, verdict). Service-owned: only the Lean audit produces it.
    FormalEvidence,
    /// A read-only review or summary written by an agent.
    ReviewReport,
    /// A result admitted as a retained artifact. Service-owned: only the
    /// retain operation produces it.
    RetainedArtifact,
    /// A reviewable request to insert/update a retained result in a manuscript.
    PromotionProposal,
    /// A reviewable proposal of evidence associations for audited declarations.
    AssociationProposal,
}

impl PortType {
    pub const ALL: [PortType; 11] = [
        PortType::CodeSource,
        PortType::Dataset,
        PortType::Figure,
        PortType::Table,
        PortType::Document,
        PortType::FormalSource,
        PortType::FormalEvidence,
        PortType::ReviewReport,
        PortType::RetainedArtifact,
        PortType::PromotionProposal,
        PortType::AssociationProposal,
    ];

    pub fn title(self) -> &'static str {
        match self {
            PortType::CodeSource => "code",
            PortType::Dataset => "dataset",
            PortType::Figure => "figure",
            PortType::Table => "table",
            PortType::Document => "document",
            PortType::FormalSource => "Lean source",
            PortType::FormalEvidence => "formal evidence",
            PortType::ReviewReport => "review report",
            PortType::RetainedArtifact => "retained artifact",
            PortType::PromotionProposal => "promotion proposal",
            PortType::AssociationProposal => "association proposal",
        }
    }

    /// The only operation allowed to produce this type, for service-owned
    /// types (privileged evidence and retention records).
    pub fn owner(self) -> Option<&'static str> {
        match self {
            PortType::FormalEvidence => Some(LEAN_AUDIT),
            PortType::RetainedArtifact => Some(ARTIFACT_RETAIN),
            _ => None,
        }
    }

    pub fn is_service_owned(self) -> bool {
        self.owner().is_some()
    }

    /// Types an explicit external input (a project file) can have.
    pub fn is_file_input(self) -> bool {
        matches!(
            self,
            PortType::CodeSource
                | PortType::Dataset
                | PortType::Figure
                | PortType::Table
                | PortType::Document
                | PortType::FormalSource
        )
    }

    /// File extensions an external input of this type may have.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            PortType::CodeSource => &["py"],
            PortType::Dataset => &[
                "csv", "tsv", "json", "jsonl", "parquet", "feather", "xlsx", "xls", "h5", "hdf5", "nc", "npz", "npy",
                "mat", "rds", "rdata", "arrow", "txt",
            ],
            PortType::Figure => &["png", "jpg", "jpeg", "svg", "pdf", "gif", "webp"],
            PortType::Table => &["csv", "tsv", "html", "md", "tex", "json"],
            PortType::Document => &["typ", "tex", "md", "markdown", "txt", "rst", "org"],
            PortType::FormalSource => &["lean"],
            _ => &[],
        }
    }
}

/// What executing a step may do. Effects are fixed by the contract; a
/// definition cannot add or remove them, and the workflow's ceiling must
/// allow every effect of every step.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowEffect {
    /// Read the step's declared inputs (nothing else in the project).
    ReadDeclaredInputs,
    /// Run code inside the restricted container (declared inputs read-only,
    /// writes only to the run's output area, no network, no credentials).
    ExecuteRestricted,
    /// Run natively with the user's account privileges (requires trust and
    /// approval; never a sandbox).
    ExecuteNative,
    /// Send the declared inputs to the model provider.
    SendToModel,
    /// Write new files in the run's own output area.
    WriteRunOutputs,
    /// Admit a result to the project as a retained artifact.
    RetainArtifact,
    /// Record service-owned formal verification evidence.
    RegisterFormalEvidence,
    /// Produce a reviewable document change (applied only after review).
    ProposeDocumentChange,
    /// Produce a reviewable graph change (applied only after review).
    ProposeGraphChange,
}

impl WorkflowEffect {
    pub fn title(self) -> &'static str {
        match self {
            WorkflowEffect::ReadDeclaredInputs => "reads its declared inputs",
            WorkflowEffect::ExecuteRestricted => "runs code in the restricted container",
            WorkflowEffect::ExecuteNative => "runs natively with your account's privileges",
            WorkflowEffect::SendToModel => "sends its inputs to the model provider",
            WorkflowEffect::WriteRunOutputs => "writes files to its run's output area",
            WorkflowEffect::RetainArtifact => "retains a result as a project artifact",
            WorkflowEffect::RegisterFormalEvidence => "records formal verification evidence",
            WorkflowEffect::ProposeDocumentChange => "proposes a reviewable document change",
            WorkflowEffect::ProposeGraphChange => "proposes a reviewable graph change",
        }
    }
}

/// How a runner contains what it executes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum Containment {
    /// The restricted Docker container: declared inputs only, read-only.
    RestrictedContainer,
    /// No containment: the process has the user's account privileges.
    NativeAccount,
    /// A model request with read-only material; no tools that execute or mutate.
    ReadOnlyModel,
    /// An application service operation (no project code runs).
    ApplicationService,
}

/// The runner a contract dispatches to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct RunnerSpec {
    pub id: String,
    pub title: String,
    pub containment: Containment,
    /// Effects this runner's containment can enforce; a contract's effects
    /// must lie within them.
    pub allows: Vec<WorkflowEffect>,
    /// Whether the runner can execute in the open project now (filled in by
    /// the workflow runner service; false in the bare registry).
    pub available: bool,
    pub availability: String,
}

/// What happens when a step fails. No automatic retries, no fallbacks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum FailureBehavior {
    /// The workflow stops; outputs of the failed step are discarded and
    /// downstream steps do not start.
    StopWorkflow,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PortSpec {
    pub name: String,
    pub label: String,
    /// Accepted types (inputs) or the single produced type (outputs).
    pub types: Vec<PortType>,
    /// Inputs: must be bound for a well-formed plan. Outputs: always produced
    /// on success (optional outputs may stay unused).
    pub required: bool,
    /// Inputs only: may receive several connections.
    pub multiple: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ParameterKind {
    Text {
        max_len: u32,
    },
    Integer {
        min: i64,
        max: i64,
    },
    Boolean,
    /// A project file path (validated as a workspace path).
    Path {
        extensions: Vec<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ParameterSpec {
    pub name: String,
    pub label: String,
    pub kind: ParameterKind,
    pub required: bool,
}

/// Language of a code payload. Code is data for an approved adapter, never a
/// way to redefine application behaviour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum CodeLanguage {
    Python,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum PayloadSpec {
    /// Inline code, unless the named input port supplies the code instead.
    Code { language: CodeLanguage, unless_input: Option<String> },
    /// The agent's instructions.
    Prompt,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct OperationLimits {
    pub max_seconds: u32,
    pub max_output_bytes: u64,
}

/// A registered operation contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct OperationContract {
    pub id: String,
    pub version: u32,
    pub title: String,
    pub purpose: String,
    pub inputs: Vec<PortSpec>,
    pub outputs: Vec<PortSpec>,
    pub parameters: Vec<ParameterSpec>,
    pub payload: Option<PayloadSpec>,
    pub effects: Vec<WorkflowEffect>,
    pub runner: RunnerSpec,
    pub limits: OperationLimits,
    pub failure: FailureBehavior,
}

impl OperationContract {
    pub fn input(&self, name: &str) -> Option<&PortSpec> {
        self.inputs.iter().find(|p| p.name == name)
    }

    pub fn output(&self, name: &str) -> Option<&PortSpec> {
        self.outputs.iter().find(|p| p.name == name)
    }

    pub fn parameter(&self, name: &str) -> Option<&ParameterSpec> {
        self.parameters.iter().find(|p| p.name == name)
    }
}

pub const RESEARCH_PYTHON_BATCH: &str = "research.pythonBatch";
pub const RESEARCH_NATIVE_PYTHON: &str = "research.nativePython";
pub const LEAN_AUDIT: &str = "lean.audit";
pub const FORMAL_ASSOCIATE: &str = "formal.associateEvidence";
pub const AGENT_REVIEW: &str = "agent.review";
pub const ARTIFACT_RETAIN: &str = "artifact.retain";
pub const REQUEST_PROMOTION: &str = "manuscript.requestPromotion";

/// Availability text of the bare registry (no project context).
pub const AVAILABILITY_NOT_CHECKED: &str = "Runner availability is checked for the open project.";

fn port(name: &str, label: &str, types: &[PortType], required: bool, multiple: bool) -> PortSpec {
    PortSpec { name: name.into(), label: label.into(), types: types.to_vec(), required, multiple }
}

fn output(name: &str, label: &str, ty: PortType) -> PortSpec {
    PortSpec { name: name.into(), label: label.into(), types: vec![ty], required: false, multiple: false }
}

fn runner(id: &str, title: &str, containment: Containment, allows: &[WorkflowEffect]) -> RunnerSpec {
    RunnerSpec {
        id: id.into(),
        title: title.into(),
        containment,
        allows: allows.to_vec(),
        available: false,
        availability: AVAILABILITY_NOT_CHECKED.into(),
    }
}

fn python_ports() -> (Vec<PortSpec>, Vec<PortSpec>) {
    (
        vec![
            port("script", "Script", &[PortType::CodeSource], false, false),
            port("data", "Input data", &[PortType::Dataset], false, true),
        ],
        vec![
            output("dataset", "Transformed dataset", PortType::Dataset),
            output("figure", "Figure", PortType::Figure),
            output("table", "Table", PortType::Table),
        ],
    )
}

fn timeout_parameter() -> ParameterSpec {
    ParameterSpec {
        name: "timeoutSeconds".into(),
        label: "Time limit (seconds)".into(),
        kind: ParameterKind::Integer { min: 1, max: 3600 },
        required: false,
    }
}

/// Every registered contract.
pub fn operations() -> Vec<OperationContract> {
    use WorkflowEffect as E;
    let (py_in, py_out) = python_ports();
    vec![
        OperationContract {
            id: RESEARCH_PYTHON_BATCH.into(),
            version: 1,
            title: "Research Python batch (restricted)".into(),
            purpose: "Run Python code over declared inputs inside the restricted container and return its declared outputs."
                .into(),
            inputs: py_in.clone(),
            outputs: py_out.clone(),
            parameters: vec![timeout_parameter()],
            payload: Some(PayloadSpec::Code { language: CodeLanguage::Python, unless_input: Some("script".into()) }),
            effects: vec![E::ReadDeclaredInputs, E::ExecuteRestricted, E::WriteRunOutputs],
            runner: runner(
                "restrictedPython",
                "Scoped restricted Python adapter",
                Containment::RestrictedContainer,
                &[E::ReadDeclaredInputs, E::ExecuteRestricted, E::WriteRunOutputs],
            ),
            limits: OperationLimits { max_seconds: 3600, max_output_bytes: 256 * 1024 * 1024 },
            failure: FailureBehavior::StopWorkflow,
        },
        OperationContract {
            id: RESEARCH_NATIVE_PYTHON.into(),
            version: 1,
            title: "Trusted native Python".into(),
            purpose: "Run Python code natively with your account's privileges (a trusted folder and approval are required; this is not a sandbox)."
                .into(),
            inputs: py_in,
            outputs: py_out,
            parameters: vec![timeout_parameter()],
            payload: Some(PayloadSpec::Code { language: CodeLanguage::Python, unless_input: Some("script".into()) }),
            effects: vec![E::ReadDeclaredInputs, E::ExecuteNative, E::WriteRunOutputs],
            runner: runner(
                "nativePython",
                "Native Python kernel (your account's privileges)",
                Containment::NativeAccount,
                &[E::ReadDeclaredInputs, E::ExecuteNative, E::WriteRunOutputs],
            ),
            limits: OperationLimits { max_seconds: 3600, max_output_bytes: 256 * 1024 * 1024 },
            failure: FailureBehavior::StopWorkflow,
        },
        OperationContract {
            id: LEAN_AUDIT.into(),
            version: 1,
            title: "Lean audit".into(),
            purpose: "Run the existing complete audit of one Lean file and return its audited declarations as formal evidence (the only operation that can produce formal evidence)."
                .into(),
            inputs: vec![port("source", "Lean file", &[PortType::FormalSource], true, false)],
            outputs: vec![output("evidence", "Audited declarations", PortType::FormalEvidence)],
            parameters: Vec::new(),
            payload: None,
            effects: vec![E::ReadDeclaredInputs, E::ExecuteNative, E::RegisterFormalEvidence],
            runner: runner(
                "leanAudit",
                "Lean verification service (trusted native toolchain)",
                Containment::NativeAccount,
                &[E::ReadDeclaredInputs, E::ExecuteNative, E::RegisterFormalEvidence],
            ),
            limits: OperationLimits { max_seconds: 1800, max_output_bytes: 16 * 1024 * 1024 },
            failure: FailureBehavior::StopWorkflow,
        },
        OperationContract {
            id: FORMAL_ASSOCIATE.into(),
            version: 1,
            title: "Associate audited declarations".into(),
            purpose: "Select declarations from the returned audited-declaration list and propose their evidence associations for review."
                .into(),
            inputs: vec![port("evidence", "Audited declarations", &[PortType::FormalEvidence], true, false)],
            outputs: vec![output("proposal", "Association proposal", PortType::AssociationProposal)],
            parameters: vec![ParameterSpec {
                name: "declarations".into(),
                label: "Declarations (chosen from the audit; one per line)".into(),
                kind: ParameterKind::Text { max_len: 8192 },
                required: false,
            }],
            payload: None,
            effects: vec![E::ReadDeclaredInputs, E::ProposeGraphChange],
            runner: runner(
                "graphProposal",
                "Project graph (reviewed proposal)",
                Containment::ApplicationService,
                &[E::ReadDeclaredInputs, E::ProposeGraphChange],
            ),
            limits: OperationLimits { max_seconds: 60, max_output_bytes: 1024 * 1024 },
            failure: FailureBehavior::StopWorkflow,
        },
        OperationContract {
            id: AGENT_REVIEW.into(),
            version: 1,
            title: "Read-only agent summary or review".into(),
            purpose: "An agent reads the declared material and writes a review report. It cannot execute code, run Lean, change files or the graph, or promote results."
                .into(),
            inputs: vec![port(
                "material",
                "Material",
                &[
                    PortType::Dataset,
                    PortType::Figure,
                    PortType::Table,
                    PortType::Document,
                    PortType::CodeSource,
                    PortType::FormalSource,
                    PortType::FormalEvidence,
                    PortType::ReviewReport,
                    PortType::RetainedArtifact,
                ],
                true,
                true,
            )],
            outputs: vec![output("report", "Review report", PortType::ReviewReport)],
            parameters: Vec::new(),
            payload: Some(PayloadSpec::Prompt),
            effects: vec![E::ReadDeclaredInputs, E::SendToModel],
            runner: runner(
                "readOnlyAgent",
                "Read-only agent task",
                Containment::ReadOnlyModel,
                &[E::ReadDeclaredInputs, E::SendToModel],
            ),
            limits: OperationLimits { max_seconds: 900, max_output_bytes: 1024 * 1024 },
            failure: FailureBehavior::StopWorkflow,
        },
        OperationContract {
            id: ARTIFACT_RETAIN.into(),
            version: 1,
            title: "Retain artifact".into(),
            purpose: "Keep a validated output as a retained project artifact with its provenance.".into(),
            inputs: vec![port(
                "artifact",
                "Output to keep",
                &[PortType::Figure, PortType::Table, PortType::Dataset],
                true,
                false,
            )],
            outputs: vec![output("retained", "Retained artifact", PortType::RetainedArtifact)],
            parameters: vec![ParameterSpec {
                name: "label".into(),
                label: "Label".into(),
                kind: ParameterKind::Text { max_len: 200 },
                required: false,
            }],
            payload: None,
            effects: vec![E::ReadDeclaredInputs, E::RetainArtifact],
            runner: runner(
                "researchRetention",
                "Research retention service",
                Containment::ApplicationService,
                &[E::ReadDeclaredInputs, E::RetainArtifact],
            ),
            limits: OperationLimits { max_seconds: 60, max_output_bytes: 0 },
            failure: FailureBehavior::StopWorkflow,
        },
        OperationContract {
            id: REQUEST_PROMOTION.into(),
            version: 1,
            title: "Request reviewed promotion".into(),
            purpose: "Prepare a reviewable insertion of a retained artifact into a manuscript (applied only after you review it)."
                .into(),
            inputs: vec![port("artifact", "Retained artifact", &[PortType::RetainedArtifact], true, false)],
            outputs: vec![output("proposal", "Promotion proposal", PortType::PromotionProposal)],
            parameters: vec![
                ParameterSpec {
                    name: "document".into(),
                    label: "Manuscript".into(),
                    kind: ParameterKind::Path {
                        extensions: vec!["typ".into(), "tex".into(), "md".into(), "markdown".into()],
                    },
                    required: true,
                },
                ParameterSpec {
                    name: "caption".into(),
                    label: "Caption".into(),
                    kind: ParameterKind::Text { max_len: 1000 },
                    required: false,
                },
            ],
            payload: None,
            effects: vec![E::ReadDeclaredInputs, E::ProposeDocumentChange],
            runner: runner(
                "reviewedPromotion",
                "Reviewed artifact insertion",
                Containment::ApplicationService,
                &[E::ReadDeclaredInputs, E::ProposeDocumentChange],
            ),
            limits: OperationLimits { max_seconds: 60, max_output_bytes: 0 },
            failure: FailureBehavior::StopWorkflow,
        },
    ]
}

/// Every registered contract with its runner's availability filled in by
/// `check` (runner id → `Ok` or the reason it cannot run now).
pub fn with_availability(check: &dyn Fn(&str) -> Result<(), String>) -> Vec<OperationContract> {
    let mut ops = operations();
    for op in &mut ops {
        match check(&op.runner.id) {
            Ok(()) => {
                op.runner.available = true;
                op.runner.availability = "Available".into();
            }
            Err(reason) => {
                op.runner.available = false;
                op.runner.availability = reason;
            }
        }
    }
    ops
}

/// Look up a contract by id and version.
pub fn operation(id: &str, version: u32) -> Option<OperationContract> {
    operations().into_iter().find(|o| o.id == id && o.version == version)
}

/// Whether any version of `id` is registered.
pub fn is_registered(id: &str) -> bool {
    operations().iter().any(|o| o.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effects_stay_within_their_runners_containment() {
        for op in operations() {
            for e in &op.effects {
                assert!(op.runner.allows.contains(e), "{} {e:?} outside its runner", op.id);
            }
            assert!(!op.runner.available, "the bare registry never claims availability");
        }
    }

    #[test]
    fn service_owned_types_have_exactly_their_owner_as_producer() {
        for op in operations() {
            for out in &op.outputs {
                assert_eq!(out.types.len(), 1, "{}.{} produces one type", op.id, out.name);
                if let Some(owner) = out.types[0].owner() {
                    assert_eq!(op.id, owner, "{} may not produce {:?}", op.id, out.types[0]);
                }
            }
        }
        let producers: Vec<String> = operations()
            .into_iter()
            .filter(|o| o.outputs.iter().any(|p| p.types.contains(&PortType::FormalEvidence)))
            .map(|o| o.id)
            .collect();
        assert_eq!(producers, vec![LEAN_AUDIT.to_owned()]);
    }

    #[test]
    fn agent_review_cannot_execute_or_mutate() {
        let op = operation(AGENT_REVIEW, 1).unwrap();
        for e in [
            WorkflowEffect::ExecuteNative,
            WorkflowEffect::ExecuteRestricted,
            WorkflowEffect::ProposeDocumentChange,
            WorkflowEffect::ProposeGraphChange,
            WorkflowEffect::RetainArtifact,
            WorkflowEffect::RegisterFormalEvidence,
        ] {
            assert!(!op.effects.contains(&e));
            assert!(!op.runner.allows.contains(&e));
        }
    }

    #[test]
    fn ids_and_ports_are_unique() {
        let ops = operations();
        for (i, a) in ops.iter().enumerate() {
            for b in &ops[i + 1..] {
                assert!(!(a.id == b.id && a.version == b.version));
            }
            let mut names: Vec<&str> = a.inputs.iter().chain(&a.outputs).map(|p| p.name.as_str()).collect();
            names.sort();
            let len = names.len();
            names.dedup();
            assert_eq!(names.len(), len, "{} port names unique", a.id);
        }
    }
}
