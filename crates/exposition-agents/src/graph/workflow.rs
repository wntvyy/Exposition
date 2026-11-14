//! `workflow_propose`: an agent saves a workflow *draft* (a declarative
//! definition) through the graph service's one validator and store.
//!
//! - Saving is a deliberate admission of a `Workflow` entity, so it goes
//!   through the central gate like a graph change: refused in read-only
//!   tasks, approved by the user (the complete definition, its validation
//!   diagnostics, and that saving runs nothing) in review-edits
//!   tasks, saved directly in auto-apply tasks. Nothing is ever executed.
//! - The agent cannot raise the authority ceiling beyond its own task: the
//!   task's ceiling is derived from its execution mode, permission profile,
//!   and consent; a definition whose ceiling exceeds it is refused.
//! - Service-owned outputs (formal evidence, retained artifacts) cannot be
//!   forged: a definition that wires anything else into them (contract rule
//!   6) is refused, never saved.
//! - Incomplete drafts are saved with their diagnostics. Saving a proposal
//!   never runs it: a run needs the user's review of a run plan and an
//!   explicit start. Workflows are project-level definitions, so
//!   the tool needs a task scoped to the whole project.

use std::collections::BTreeMap;
use std::path::Path;

use exposition_core::path::WorkspacePath;
use exposition_graph::workflow::{
    AuthorityCeiling, CodeLanguage, ExecutionCeiling, InputSource, InputVersion, MAX_CONNECTIONS, MAX_INPUTS,
    MAX_OUTCOMES, MAX_PARAMETERS, MAX_STEPS, OutcomeKind, ParamValue, PortSource, PortType, StepOperation, StepPayload,
    StepPort, WORKFLOW_SCHEMA, WorkflowConnection, WorkflowInput, WorkflowOutcome, WorkflowStep,
};
use exposition_graph::{
    DiagnosticSeverity, Origin, RelationKind, SaveWorkflowRequest, ValidationContext, WorkflowDefinition, WorkflowId,
    validate_workflow,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::{GraphEnv, bound_status, refusal};
use crate::model::{ContextConsent, ExecutionMode, PermissionProfile, TaskId, TaskScope, TaskSpec};
use crate::tools::exec::{ToolOutput, codes, shown};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentInput {
    pub id: String,
    pub label: String,
    pub port_type: String,
    /// Project file, or "" when not chosen yet.
    pub path: String,
    /// SHA-256 to pin the input to, or "" for the version at run start.
    pub pinned_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentParameter {
    pub name: String,
    /// `text`, `integer`, or `boolean`: which of the value fields is used.
    pub kind: String,
    pub text: String,
    pub integer: i64,
    pub boolean: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentPayload {
    None {},
    Code { language: String, source: String },
    Prompt { text: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentStep {
    pub id: String,
    pub label: String,
    pub operation: String,
    pub version: u32,
    pub parameters: Vec<AgentParameter>,
    pub payload: AgentPayload,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentConnection {
    /// Typed data: from a step's output port (`from_step`/`from_port`) or an
    /// external input (`from_input`), into a step's input port.
    Data {
        from_step: String,
        from_port: String,
        from_input: String,
        to_step: String,
        to_port: String,
    },
    Prerequisite {
        before: String,
        after: String,
    },
    /// Decorative only: never satisfies an input, never orders execution.
    Annotation {
        from: String,
        to: String,
        label: String,
        relation: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentOutcome {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub step: String,
    pub port: String,
    pub optional: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCeiling {
    pub execution: String,
    pub model_requests: bool,
    pub retain_results: bool,
    pub propose_changes: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowProposeInput {
    /// "" for a new workflow, or the id (wf-…) of one to save a new revision of.
    pub workflow_id: String,
    /// The revision being edited, or -1.
    pub expected_revision: i64,
    pub title: String,
    pub description: String,
    pub preset: String,
    pub inputs: Vec<AgentInput>,
    pub steps: Vec<AgentStep>,
    pub connections: Vec<AgentConnection>,
    pub outcomes: Vec<AgentOutcome>,
    pub ceiling: AgentCeiling,
}

impl WorkflowProposeInput {
    /// Bounds checked before anything else (the validator applies the rest).
    pub fn check_bounds(&self) -> Result<(), String> {
        let limits = [
            ("inputs", self.inputs.len(), MAX_INPUTS),
            ("steps", self.steps.len(), MAX_STEPS),
            ("connections", self.connections.len(), MAX_CONNECTIONS),
            ("outcomes", self.outcomes.len(), MAX_OUTCOMES),
        ];
        for (what, n, max) in limits {
            if n > max {
                return Err(format!("a workflow can have at most {max} {what}"));
            }
        }
        if self.steps.iter().any(|s| s.parameters.len() > MAX_PARAMETERS) {
            return Err(format!("a step can have at most {MAX_PARAMETERS} parameters"));
        }
        Ok(())
    }

    /// Project files the definition names (scope-checked by the gate).
    pub fn named_paths(&self) -> Vec<&str> {
        self.inputs.iter().map(|i| i.path.trim()).filter(|p| !p.is_empty()).collect()
    }
}

fn enum_value<T: DeserializeOwned>(raw: &str, what: &str) -> Result<T, String> {
    serde_json::from_value(Value::String(raw.to_owned())).map_err(|_| format!("unknown {what} {raw:?}"))
}

/// The definition the agent described, in the contract's own shape.
pub fn definition(input: &WorkflowProposeInput) -> Result<WorkflowDefinition, String> {
    input.check_bounds()?;
    let inputs = input
        .inputs
        .iter()
        .map(|i| {
            let source = match i.path.trim() {
                "" => InputSource::Unset,
                raw => InputSource::Resource {
                    path: WorkspacePath::parse(raw).map_err(|e| format!("input {:?}: invalid path: {e}", i.id))?,
                    version: match i.pinned_hash.trim() {
                        "" => InputVersion::AtRunStart,
                        hash => InputVersion::Pinned { content_hash: hash.to_owned() },
                    },
                },
            };
            Ok(WorkflowInput {
                id: i.id.clone(),
                label: i.label.clone(),
                port_type: enum_value::<PortType>(&i.port_type, "port type")?,
                source,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let steps = input
        .steps
        .iter()
        .map(|s| {
            let mut parameters = BTreeMap::new();
            for p in &s.parameters {
                let value = match p.kind.as_str() {
                    "text" => ParamValue::Text(p.text.clone()),
                    "integer" => ParamValue::Integer(p.integer),
                    "boolean" => ParamValue::Boolean(p.boolean),
                    other => return Err(format!("step {:?}: unknown parameter kind {other:?}", s.id)),
                };
                if parameters.insert(p.name.clone(), value).is_some() {
                    return Err(format!("step {:?}: parameter {:?} is given twice", s.id, p.name));
                }
            }
            let payload = match &s.payload {
                AgentPayload::None {} => None,
                AgentPayload::Code { language, source } => Some(StepPayload::Code {
                    language: enum_value::<CodeLanguage>(language, "code language")?,
                    source: source.clone(),
                }),
                AgentPayload::Prompt { text } => Some(StepPayload::Prompt { text: text.clone() }),
            };
            Ok(WorkflowStep {
                id: s.id.clone(),
                label: s.label.clone(),
                operation: StepOperation { id: s.operation.clone(), version: s.version },
                parameters,
                payload,
                composite: None,
                condition: None,
                repeat: None,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let connections = input
        .connections
        .iter()
        .map(|c| {
            Ok(match c {
                AgentConnection::Data { from_step, from_port, from_input, to_step, to_port } => {
                    let from = if from_input.trim().is_empty() {
                        PortSource::Step { step: from_step.clone(), port: from_port.clone() }
                    } else {
                        PortSource::Input { input: from_input.clone() }
                    };
                    WorkflowConnection::Data { from, to: StepPort { step: to_step.clone(), port: to_port.clone() } }
                }
                AgentConnection::Prerequisite { before, after } => {
                    WorkflowConnection::Prerequisite { before: before.clone(), after: after.clone() }
                }
                AgentConnection::Annotation { from, to, label, relation } => WorkflowConnection::Annotation {
                    from: from.clone(),
                    to: to.clone(),
                    label: label.clone(),
                    relation: match relation.trim() {
                        "" => None,
                        raw => Some(RelationKind::parse(raw).ok_or_else(|| format!("unknown relation {raw:?}"))?),
                    },
                },
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let outcomes = input
        .outcomes
        .iter()
        .map(|o| {
            Ok(WorkflowOutcome {
                id: o.id.clone(),
                kind: enum_value::<OutcomeKind>(&o.kind, "outcome kind")?,
                label: o.label.clone(),
                from: StepPort { step: o.step.clone(), port: o.port.clone() },
                optional: o.optional,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(WorkflowDefinition {
        schema: WORKFLOW_SCHEMA.to_owned(),
        title: input.title.clone(),
        description: input.description.clone(),
        preset: (!input.preset.trim().is_empty()).then(|| input.preset.trim().to_owned()),
        inputs,
        steps,
        connections,
        outcomes,
        ceiling: AuthorityCeiling {
            execution: enum_value::<ExecutionCeiling>(&input.ceiling.execution, "execution ceiling")?,
            model_requests: input.ceiling.model_requests,
            retain_results: input.ceiling.retain_results,
            propose_changes: input.ceiling.propose_changes,
        },
        execution: None,
    })
}

/// The most a workflow saved by this task may do: never more than the task
/// itself is allowed (execution mode, document changes, content consent).
pub fn task_ceiling(spec: &TaskSpec) -> AuthorityCeiling {
    let changes = spec.permission != PermissionProfile::ReadOnly;
    AuthorityCeiling {
        execution: match (spec.permission, spec.execution) {
            (PermissionProfile::ReadOnly, _) | (_, ExecutionMode::Disabled) => ExecutionCeiling::None,
            (_, ExecutionMode::Restricted) => ExecutionCeiling::Restricted,
            (_, ExecutionMode::ApproveEach) => ExecutionCeiling::TrustedNative,
        },
        model_requests: spec.consent == ContextConsent::ScopeContent,
        retain_results: changes,
        propose_changes: changes,
    }
}

fn ceiling_text(c: &AuthorityCeiling) -> String {
    let execution = match c.execution {
        ExecutionCeiling::None => "no code execution",
        ExecutionCeiling::Restricted => "restricted container execution",
        ExecutionCeiling::TrustedNative => "trusted native execution",
    };
    let mut parts = vec![execution.to_owned()];
    parts.push(if c.model_requests { "model requests" } else { "no model requests" }.to_owned());
    parts.push(if c.retain_results { "retaining results" } else { "no retaining" }.to_owned());
    parts.push(if c.propose_changes { "reviewable proposals" } else { "no proposals" }.to_owned());
    parts.join(", ")
}

/// A validated draft, as shown for approval.
/// Why a draft saved by an agent did not run (saving never runs anything).
pub const SAVED_NOT_RUN: &str =
    "Saving a proposal never runs it; the user reviews a run plan and starts a run separately.";

#[derive(Debug, Clone, PartialEq)]
pub struct PreparedWorkflow {
    pub request: SaveWorkflowRequest,
    pub ceiling: AuthorityCeiling,
    pub title: String,
    /// Complete approval text (definition, diagnostics, saving runs nothing).
    pub detail: String,
    pub errors: u32,
}

/// Whether a project file exists inside the task's folder (links leading
/// outside count as missing).
fn exists_in(root: &Path, path: &WorkspacePath) -> bool {
    let mut p = root.to_path_buf();
    for c in path.components() {
        p.push(c);
    }
    std::fs::canonicalize(&p).is_ok_and(|c| c.starts_with(root) && c.is_file())
}

fn diagnostics_json(validation: &exposition_graph::WorkflowValidation) -> Vec<Value> {
    validation
        .diagnostics
        .iter()
        .take(200)
        .map(|d| {
            json!({
                "severity": d.severity,
                "code": d.code,
                "message": d.message,
                "rule": d.rule,
                "step": d.step,
                "port": d.port,
                "input": d.input,
                "outcome": d.outcome,
            })
        })
        .collect()
}

/// Gate preparation: scope, ceiling, the contract's validation, and the
/// complete approval text. Never saves anything.
pub fn prepare(
    graph: GraphEnv<'_>,
    root: &Path,
    spec: &TaskSpec,
    input: &WorkflowProposeInput,
) -> Result<PreparedWorkflow, ToolOutput> {
    if spec.scope != TaskScope::Workspace {
        return Err(refusal(
            codes::WHOLE_PROJECT_ONLY,
            format!(
                "Workflow drafts are project-level definitions, so saving one needs a task scoped to the whole project \
                 (this task: {}). Nothing was saved.",
                spec.scope.describe()
            ),
        ));
    }
    bound_status(graph, root)?;
    let definition = definition(input).map_err(|m| {
        refusal(codes::WORKFLOW_INVALID, format!("The workflow definition is not well formed: {m}. Nothing was saved."))
    })?;
    let id = match input.workflow_id.trim() {
        "" => None,
        raw => Some(WorkflowId::parse(raw).map_err(|_| {
            refusal(codes::WORKFLOW_INVALID, format!("{raw:?} is not a workflow id (wf-…). Nothing was saved."))
        })?),
    };
    if let Some(id) = &id {
        // A new revision of an existing workflow of this project. Agents write
        // schema /1 only: a revision of a workflow that uses /2 features would
        // silently drop them (a safety condition could vanish), so it is
        // refused, never downgraded.
        let current = graph.service.workflow(id, None).map_err(|e| refusal(codes::WORKFLOW_INVALID, e.message))?;
        if current.definition.schema != WORKFLOW_SCHEMA || current.definition.uses_v2() {
            return Err(refusal(
                codes::WORKFLOW_UNSUPPORTED,
                format!(
                    "Workflow {id} (revision {}) uses features agents cannot edit (parallel steps, composite steps, \
                     conditions, or repeats). Saving an agent revision would remove them, so nothing was saved; ask \
                     the user to make this change, or propose a new workflow instead.",
                    current.revision
                ),
            ));
        }
    }
    let ceiling = task_ceiling(spec);
    if !definition.ceiling.within(&ceiling) {
        return Err(refusal(
            codes::WORKFLOW_CEILING,
            format!(
                "The workflow's authority ceiling ({}) is wider than this task allows ({}). An agent cannot give a \
                 workflow more authority than its own task; nothing was saved.",
                ceiling_text(&definition.ceiling),
                ceiling_text(&ceiling)
            ),
        ));
    }
    let exists = |p: &WorkspacePath| exists_in(root, p);
    // The same validation the save performs (composites resolve against the
    // project's saved revisions).
    let resolve = |wf: &WorkflowId, revision: u64| graph.service.workflow_definition(wf, revision);
    let validation = validate_workflow(
        &definition,
        &ValidationContext {
            ceiling: Some(ceiling),
            file_exists: Some(&exists),
            resolve_workflow: Some(&resolve),
            workflow_id: id.as_ref(),
        },
    );
    let forged: Vec<&exposition_graph::workflow::WorkflowDiagnostic> =
        validation.diagnostics.iter().filter(|d| d.rule == Some(6)).collect();
    if !forged.is_empty() {
        return Err(ToolOutput::error_with(
            "The workflow wires something else into a service-owned result (formal evidence or a retained artifact). \
             Only the Lean audit and the retain operation produce those; an agent cannot forge them. Nothing was \
             saved.",
            json!({
                "code": codes::WORKFLOW_SERVICE_OWNED,
                "diagnostics": forged.iter().map(|d| json!({"code": d.code, "message": d.message, "rule": d.rule, "step": d.step, "outcome": d.outcome})).collect::<Vec<_>>(),
            }),
        ));
    }
    let title = format!("Save workflow draft “{}”", definition.title.trim());
    let mut detail = format!(
        "Save the workflow draft “{}”{} in the project graph. Saving runs nothing: the workflow runs only if you \
         later review its run plan and start it yourself.\n\nValidation: {} ({} error(s), {} warning(s)). \
         Authority ceiling: {}.",
        shown(definition.title.trim()),
        match &id {
            Some(id) => format!(" as a new revision of {id}"),
            None => " as a new workflow".to_owned(),
        },
        validation.state.as_str(),
        validation.errors,
        validation.warnings,
        ceiling_text(&definition.ceiling),
    );
    if !validation.diagnostics.is_empty() {
        detail.push_str("\n\nDiagnostics:\n");
        for d in &validation.diagnostics {
            let severity = match d.severity {
                DiagnosticSeverity::Error => "error",
                DiagnosticSeverity::Warning => "warning",
                DiagnosticSeverity::Info => "info",
            };
            detail.push_str(&format!("- {severity} {}: {}\n", d.code, shown(&d.message)));
        }
    }
    let pretty = serde_json::to_string_pretty(&definition).unwrap_or_default();
    detail.push_str("\nDefinition (complete):\n");
    detail.push_str(&shown(&pretty));
    let request =
        SaveWorkflowRequest { id, expected_revision: u64::try_from(input.expected_revision).ok(), definition };
    Ok(PreparedWorkflow { request, ceiling, title, detail, errors: validation.errors })
}

/// Save the prepared draft (after the gate and any approval).
pub fn save(
    graph: GraphEnv<'_>,
    root: &Path,
    task: &TaskId,
    prepared: &PreparedWorkflow,
) -> Result<ToolOutput, ToolOutput> {
    bound_status(graph, root)?;
    let exists = |p: &WorkspacePath| exists_in(root, p);
    let context =
        ValidationContext { ceiling: Some(prepared.ceiling), file_exists: Some(&exists), ..Default::default() };
    let origin = Origin::Agent { task_id: task.0.clone() };
    let saved = graph
        .service
        .save_workflow(&origin, &prepared.request, &context)
        .map_err(|e| refusal(codes::WORKFLOW_INVALID, format!("The workflow draft was not saved: {}", e.message)))?;
    Ok(ToolOutput::ok(json!({
        "saved": !saved.unchanged,
        "unchanged": saved.unchanged,
        "workflow": saved.id,
        "revision": saved.revision,
        "entity": saved.entity,
        "state": saved.validation.state,
        "errors": saved.validation.errors,
        "warnings": saved.validation.warnings,
        "diagnostics": diagnostics_json(&saved.validation),
        "runnable": false,
        "runnableReason": SAVED_NOT_RUN,
        "note": "Saved as a draft in the project graph; nothing was run. A run needs the user's review of its run plan and an explicit start.",
    })))
}
