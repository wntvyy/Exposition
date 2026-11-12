//! Workflow presets: reusable procedures within a project (distinct from
//! project presets). Each produces an ordinary draft definition that people
//! and agents can clone and customize; it is validated like any other.

use std::collections::BTreeMap;

use exposition_core::path::WorkspacePath;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::registry::{ARTIFACT_RETAIN, FORMAL_ASSOCIATE, LEAN_AUDIT, REQUEST_PROMOTION, RESEARCH_PYTHON_BATCH};
use super::{
    AuthorityCeiling, ExecutionCeiling, InputSource, InputVersion, OutcomeKind, PortSource, PortType, StepOperation,
    StepPort, WORKFLOW_SCHEMA, WorkflowConnection, WorkflowDefinition, WorkflowInput, WorkflowOutcome, WorkflowStep,
};

pub const NUMERICAL_EXPERIMENT: &str = "numericalExperiment";
pub const FORMAL_AUDIT: &str = "formalAudit";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPresetInfo {
    pub id: String,
    pub title: String,
    pub description: String,
    /// What the template asks for (shown before creating a draft).
    pub inputs: Vec<String>,
}

/// Files a preset draft is bound to (unset ones leave an incomplete draft).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowPresetRequest {
    pub preset: String,
    /// Numerical experiment: the Python script.
    pub script: Option<WorkspacePath>,
    /// Numerical experiment: an input dataset.
    pub data: Option<WorkspacePath>,
    /// Numerical experiment: the manuscript a retained figure may be promoted
    /// into (the promotion step is included only when a manuscript exists).
    pub manuscript: Option<WorkspacePath>,
    /// Formal audit: the Lean file.
    pub lean_file: Option<WorkspacePath>,
}

pub fn catalog() -> Vec<WorkflowPresetInfo> {
    vec![
        WorkflowPresetInfo {
            id: NUMERICAL_EXPERIMENT.into(),
            title: "Numerical experiment".into(),
            description: "Run a research Python script over declared inputs in the restricted container, keep the selected figure as a retained artifact, and request a reviewed promotion into the manuscript if the project has one."
                .into(),
            inputs: vec!["Python script".into(), "Input data (optional)".into(), "Manuscript (optional)".into()],
        },
        WorkflowPresetInfo {
            id: FORMAL_AUDIT.into(),
            title: "Formal audit".into(),
            description: "Run the complete Lean audit of one file, then select declarations from the audited list and propose their evidence associations for review."
                .into(),
            inputs: vec!["Lean file".into()],
        },
    ]
}

fn resource(path: &Option<WorkspacePath>) -> InputSource {
    match path {
        Some(p) => InputSource::Resource { path: p.clone(), version: InputVersion::AtRunStart },
        None => InputSource::Unset,
    }
}

fn step(id: &str, label: &str, op: &str) -> WorkflowStep {
    WorkflowStep {
        id: id.into(),
        label: label.into(),
        operation: StepOperation { id: op.into(), version: 1 },
        parameters: BTreeMap::new(),
        payload: None,
    }
}

fn data(from: PortSource, step: &str, port: &str) -> WorkflowConnection {
    WorkflowConnection::Data { from, to: StepPort { step: step.into(), port: port.into() } }
}

fn out(step: &str, port: &str) -> PortSource {
    PortSource::Step { step: step.into(), port: port.into() }
}

/// A draft from a workflow preset. Unknown preset ids return `None`.
pub fn draft(request: &WorkflowPresetRequest) -> Option<WorkflowDefinition> {
    match request.preset.as_str() {
        NUMERICAL_EXPERIMENT => Some(numerical_experiment(request)),
        FORMAL_AUDIT => Some(formal_audit(request)),
        _ => None,
    }
}

fn numerical_experiment(r: &WorkflowPresetRequest) -> WorkflowDefinition {
    let mut inputs = vec![WorkflowInput {
        id: "script".into(),
        label: "Python script".into(),
        port_type: PortType::CodeSource,
        source: resource(&r.script),
    }];
    let mut connections = vec![data(PortSource::Input { input: "script".into() }, "run", "script")];
    if r.data.is_some() {
        inputs.push(WorkflowInput {
            id: "data".into(),
            label: "Input data".into(),
            port_type: PortType::Dataset,
            source: resource(&r.data),
        });
        connections.push(data(PortSource::Input { input: "data".into() }, "run", "data"));
    }
    let mut steps = vec![
        step("run", "Run the experiment", RESEARCH_PYTHON_BATCH),
        step("retain", "Keep the figure", ARTIFACT_RETAIN),
    ];
    connections.push(data(out("run", "figure"), "retain", "artifact"));
    let mut outcomes = vec![WorkflowOutcome {
        id: "figure".into(),
        kind: OutcomeKind::RetainedFigure,
        label: "Retained figure".into(),
        from: StepPort { step: "retain".into(), port: "retained".into() },
        optional: false,
    }];
    if let Some(manuscript) = &r.manuscript {
        let mut promote = step("promote", "Request promotion into the manuscript", REQUEST_PROMOTION);
        promote.parameters.insert("document".into(), super::ParamValue::Text(manuscript.to_string()));
        steps.push(promote);
        connections.push(data(out("retain", "retained"), "promote", "artifact"));
        outcomes.push(WorkflowOutcome {
            id: "promotion".into(),
            kind: OutcomeKind::ManuscriptPromotion,
            label: "Reviewed promotion".into(),
            from: StepPort { step: "promote".into(), port: "proposal".into() },
            optional: true,
        });
    }
    WorkflowDefinition {
        schema: WORKFLOW_SCHEMA.into(),
        title: "Numerical experiment".into(),
        description: "Run the script in the restricted container, inspect its validated outputs and provenance, keep the figure, and request a reviewed promotion."
            .into(),
        preset: Some(NUMERICAL_EXPERIMENT.into()),
        inputs,
        steps,
        connections,
        outcomes,
        ceiling: AuthorityCeiling {
            execution: ExecutionCeiling::Restricted,
            model_requests: false,
            retain_results: true,
            propose_changes: true,
        },
    }
}

fn formal_audit(r: &WorkflowPresetRequest) -> WorkflowDefinition {
    WorkflowDefinition {
        schema: WORKFLOW_SCHEMA.into(),
        title: "Formal audit".into(),
        description: "Audit the Lean file completely, then select audited declarations and propose their evidence associations. A file badge never substitutes for declaration evidence."
            .into(),
        preset: Some(FORMAL_AUDIT.into()),
        inputs: vec![WorkflowInput {
            id: "leanFile".into(),
            label: "Lean file".into(),
            port_type: PortType::FormalSource,
            source: resource(&r.lean_file),
        }],
        steps: vec![
            step("audit", "Audit the Lean file", LEAN_AUDIT),
            step("associate", "Associate audited declarations", FORMAL_ASSOCIATE),
        ],
        connections: vec![
            data(PortSource::Input { input: "leanFile".into() }, "audit", "source"),
            data(out("audit", "evidence"), "associate", "evidence"),
        ],
        outcomes: vec![
            WorkflowOutcome {
                id: "evidence".into(),
                kind: OutcomeKind::AuditEvidence,
                label: "Audit evidence".into(),
                from: StepPort { step: "audit".into(), port: "evidence".into() },
                optional: false,
            },
            WorkflowOutcome {
                id: "associations".into(),
                kind: OutcomeKind::ReviewableProposal,
                label: "Evidence associations for review".into(),
                from: StepPort { step: "associate".into(), port: "proposal".into() },
                optional: true,
            },
        ],
        ceiling: AuthorityCeiling {
            execution: ExecutionCeiling::TrustedNative,
            model_requests: false,
            retain_results: true,
            propose_changes: true,
        },
    }
}
