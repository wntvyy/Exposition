//! Reviewable run plans.
//!
//! A plan resolves one saved revision in the open project into exactly what
//! will happen: every external input (path, SHA-256 now), the steps in the
//! order they run with their complete payloads, parameters, bindings,
//! runners, containment, effects, and limits, the run's budgets, and the
//! execution environment it binds to (restricted image, folder trust). The
//! `approval_hash` covers all of it; starting a run recomputes the plan and
//! refuses if the hash differs.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use exposition_core::path::WorkspacePath;
use exposition_graph::workflow::registry::{self, Containment, OperationContract, WorkflowEffect};
use exposition_graph::workflow::{
    AuthorityCeiling, DraftState, InputSource, InputVersion, ParamValue, ValidationContext, WorkflowConnection,
};
use exposition_graph::{SavedWorkflow, WorkflowPlanId};
use serde::Serialize;

use crate::adapters::Adapters;
use crate::model::{
    PLAN_TTL_MS, WorkflowPlanBinding, WorkflowPlanBlocker, WorkflowPlanExecution, WorkflowPlanInput, WorkflowPlanStep,
    WorkflowRunBudgets, WorkflowRunOutcome, WorkflowRunPlan,
};

/// A run never takes longer than this in total.
pub const MAX_RUN_SECONDS: u32 = 4 * 3600;
/// A run never writes more than this in total.
pub const MAX_RUN_OUTPUT_BYTES: u64 = 1024 * 1024 * 1024;
/// Model requests allowed per model step.
pub const MODEL_REQUESTS_PER_STEP: u32 = 25;

/// What the open project and application settings say right now.
pub struct PlanFacts<'a> {
    /// Canonical project root.
    pub root: &'a Path,
    pub trusted: bool,
    pub restricted_backend: Option<String>,
    pub restricted_image: Option<String>,
    pub adapters: &'a Adapters,
    pub active_run: bool,
}

/// Why a runner cannot run (stable code, message).
pub type Unavailable = (&'static str, String);

fn containment_of(runner_id: &str) -> Option<Containment> {
    registry::operations().into_iter().find(|o| o.runner.id == runner_id).map(|o| o.runner.containment)
}

/// Whether the runner `runner_id` can execute in the project now.
pub fn runner_availability(
    runner_id: &str,
    root: &Path,
    trusted: bool,
    adapters: &Adapters,
) -> Result<(), Unavailable> {
    let containment = containment_of(runner_id);
    let Some(adapter) = adapters.get(runner_id) else {
        return Err(("workflow.runnerUnavailable", "No runner is installed for this operation in this build.".into()));
    };
    if containment == Some(Containment::NativeAccount) && !trusted {
        return Err((
            "workflow.untrustedFolder",
            "The project folder is not trusted. This step runs natively with your account's privileges (not a sandbox); trust the folder first."
                .into(),
        ));
    }
    adapter.availability(root).map_err(|reason| {
        if containment == Some(Containment::RestrictedContainer) {
            (
                "workflow.restrictedUnavailable",
                format!("Restricted execution is unavailable ({reason}); it never falls back to native execution."),
            )
        } else {
            ("workflow.runnerUnavailable", reason)
        }
    })
}

/// SHA-256 (hex) and size of a project file, which must resolve inside `root`
/// (links leading outside are refused).
pub fn hash_project_file(root: &Path, path: &WorkspacePath) -> Result<(String, u64), String> {
    let resolved = resolve(root, path)?;
    let mut file = std::fs::File::open(&resolved).map_err(|e| format!("{path} cannot be read: {e}"))?;
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut bytes = 0u64;
    loop {
        let n = file.read(&mut buffer).map_err(|e| format!("{path} cannot be read: {e}"))?;
        if n == 0 {
            break;
        }
        bytes += n as u64;
        hasher.update(&buffer[..n]);
    }
    Ok((hasher.finalize().iter().map(|b| format!("{b:02x}")).collect(), bytes))
}

/// The canonical location of a project file inside `root`.
pub fn resolve(root: &Path, path: &WorkspacePath) -> Result<PathBuf, String> {
    let mut p = root.to_path_buf();
    for c in path.components() {
        p.push(c);
    }
    let canonical = std::fs::canonicalize(&p).map_err(|_| format!("{path} does not exist in the project"))?;
    if !canonical.starts_with(root) {
        return Err(format!("{path} leads outside the project"));
    }
    if !canonical.is_file() {
        return Err(format!("{path} is not a file"));
    }
    Ok(canonical)
}

pub fn project_name(root: &Path) -> String {
    root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| root.display().to_string())
}

/// The canonical identity of a project (case-insensitive on Windows).
pub fn project_key(root: &Path) -> String {
    let s = root.to_string_lossy().into_owned();
    if cfg!(windows) { s.to_lowercase() } else { s }
}

fn time_limit(step: &exposition_graph::workflow::WorkflowStep, contract: &OperationContract) -> u32 {
    let max = contract.limits.max_seconds.max(1);
    match step.parameters.get("timeoutSeconds") {
        Some(ParamValue::Integer(v)) => (*v).clamp(1, max as i64) as u32,
        _ => max,
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ApprovedStep<'a> {
    step: &'a str,
    label: &'a str,
    operation: &'a exposition_graph::workflow::StepOperation,
    runner: &'a str,
    containment: Containment,
    effects: &'a [WorkflowEffect],
    payload: &'a Option<exposition_graph::workflow::StepPayload>,
    parameters: &'a BTreeMap<String, ParamValue>,
    bindings: &'a [WorkflowPlanBinding],
    prerequisites: &'a [String],
    time_limit_seconds: u32,
    max_output_bytes: u64,
    available: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Approved<'a> {
    schema: &'static str,
    project: &'a str,
    workflow: &'a str,
    revision: u64,
    definition_hash: &'a str,
    inputs: &'a [WorkflowPlanInput],
    steps: Vec<ApprovedStep<'a>>,
    ceiling: &'a AuthorityCeiling,
    budgets: &'a WorkflowRunBudgets,
    execution: &'a WorkflowPlanExecution,
}

/// SHA-256 over a canonical serialization of everything the user approves.
pub fn approval_hash(plan: &WorkflowRunPlan, root: &Path) -> String {
    let key = project_key(root);
    let approved = Approved {
        schema: "exposition.workflowRunApproval/1",
        project: &key,
        workflow: plan.workflow.as_str(),
        revision: plan.revision,
        definition_hash: &plan.definition_hash,
        inputs: &plan.inputs,
        steps: plan
            .steps
            .iter()
            .map(|s| ApprovedStep {
                step: &s.step,
                label: &s.label,
                operation: &s.operation,
                runner: &s.runner,
                containment: s.containment,
                effects: &s.effects,
                payload: &s.payload,
                parameters: &s.parameters,
                bindings: &s.bindings,
                prerequisites: &s.prerequisites,
                time_limit_seconds: s.time_limit_seconds,
                max_output_bytes: s.max_output_bytes,
                available: s.available,
            })
            .collect(),
        ceiling: &plan.ceiling,
        budgets: &plan.budgets,
        execution: &plan.execution,
    };
    crate::adapters::sha256_hex(&serde_json::to_vec(&approved).unwrap_or_default())
}

/// A plan and the declared outcomes a run of it reports.
pub struct BuiltPlan {
    pub plan: WorkflowRunPlan,
    pub outcomes: Vec<WorkflowRunOutcome>,
}

fn blocker(code: &str, message: impl Into<String>, step: Option<&str>, input: Option<&str>) -> WorkflowPlanBlocker {
    WorkflowPlanBlocker {
        code: code.to_owned(),
        message: message.into(),
        step: step.map(str::to_owned),
        input: input.map(str::to_owned),
    }
}

/// Build the reviewable plan of `saved` in the project of `facts`.
pub fn build_plan(saved: &SavedWorkflow, facts: &PlanFacts<'_>, now_ms: u64) -> BuiltPlan {
    let def = &saved.definition;
    // Never trust the stored validation: validate again now.
    let validation = exposition_graph::validate_workflow(def, &ValidationContext::default());
    let mut blockers = Vec::new();
    if validation.state != DraftState::WellFormed {
        let errors: Vec<String> = validation
            .diagnostics
            .iter()
            .filter(|d| d.severity == exposition_graph::DiagnosticSeverity::Error)
            .take(3)
            .map(|d| d.message.clone())
            .collect();
        blockers.push(blocker(
            "workflow.notWellFormed",
            format!(
                "This revision is not well-formed ({} error{}): {}",
                validation.errors,
                if validation.errors == 1 { "" } else { "s" },
                errors.join("; ")
            ),
            None,
            None,
        ));
    }

    // ----- external inputs -----
    let mut inputs = Vec::new();
    for input in &def.inputs {
        let InputSource::Resource { path, version } = &input.source else { continue };
        let pinned = match version {
            InputVersion::Pinned { content_hash } => Some(content_hash.to_ascii_lowercase()),
            InputVersion::AtRunStart => None,
        };
        match hash_project_file(facts.root, path) {
            Ok((sha256, bytes)) => {
                if let Some(pin) = &pinned
                    && pin != &sha256
                {
                    blockers.push(blocker(
                        "workflow.inputChanged",
                        format!(
                            "{path} is not the version the workflow pins (pinned sha256 {}, now {})",
                            &pin[..pin.len().min(12)],
                            &sha256[..12]
                        ),
                        None,
                        Some(&input.id),
                    ));
                }
                inputs.push(WorkflowPlanInput {
                    id: input.id.clone(),
                    label: input.label.clone(),
                    port_type: input.port_type,
                    path: path.clone(),
                    sha256,
                    bytes,
                    pinned: pinned.is_some(),
                });
            }
            Err(message) => blockers.push(blocker("workflow.inputMissing", message, None, Some(&input.id))),
        }
    }

    // ----- steps in dependency order -----
    let mut steps = Vec::new();
    for planned in &validation.plan {
        let Some(step) = def.steps.iter().find(|s| s.id == planned.step) else { continue };
        let Some(contract) = registry::operation(&step.operation.id, step.operation.version) else { continue };
        let bindings: Vec<WorkflowPlanBinding> = def
            .connections
            .iter()
            .filter_map(|c| match c {
                WorkflowConnection::Data { from, to } if to.step == step.id => {
                    Some(WorkflowPlanBinding { port: to.port.clone(), from: from.clone() })
                }
                _ => None,
            })
            .collect();
        let prerequisites: Vec<String> = def
            .connections
            .iter()
            .filter_map(|c| match c {
                WorkflowConnection::Prerequisite { before, after } if after == &step.id => Some(before.clone()),
                _ => None,
            })
            .collect();
        let (available, availability) =
            match runner_availability(&contract.runner.id, facts.root, facts.trusted, facts.adapters) {
                Ok(()) => (true, "Available".to_owned()),
                Err((code, reason)) => {
                    blockers.push(blocker(code, format!("“{}”: {reason}", step.label), Some(&step.id), None));
                    (false, reason)
                }
            };
        steps.push(WorkflowPlanStep {
            step: step.id.clone(),
            label: step.label.clone(),
            operation: step.operation.clone(),
            operation_title: contract.title.clone(),
            runner: contract.runner.id.clone(),
            runner_title: contract.runner.title.clone(),
            containment: contract.runner.containment,
            effects: contract.effects.clone(),
            payload: step.payload.clone(),
            parameters: step.parameters.clone(),
            bindings,
            prerequisites,
            time_limit_seconds: time_limit(step, &contract).min(MAX_RUN_SECONDS),
            max_output_bytes: contract.limits.max_output_bytes,
            available,
            availability,
        });
    }

    // ----- budgets -----
    let model_steps: Vec<String> =
        steps.iter().filter(|s| s.containment == Containment::ReadOnlyModel).map(|s| s.step.clone()).collect();
    let native_steps: Vec<String> =
        steps.iter().filter(|s| s.containment == Containment::NativeAccount).map(|s| s.step.clone()).collect();
    let restricted_steps: Vec<&str> =
        steps.iter().filter(|s| s.containment == Containment::RestrictedContainer).map(|s| s.label.as_str()).collect();
    let budgets = WorkflowRunBudgets {
        max_steps: steps.len() as u32,
        max_total_seconds: steps.iter().map(|s| s.time_limit_seconds).sum::<u32>().clamp(1, MAX_RUN_SECONDS),
        max_output_bytes: steps.iter().map(|s| s.max_output_bytes).sum::<u64>().min(MAX_RUN_OUTPUT_BYTES),
        max_model_requests: model_steps.len() as u32 * MODEL_REQUESTS_PER_STEP,
    };
    let execution = WorkflowPlanExecution {
        restricted_backend: facts.restricted_backend.clone(),
        restricted_image: facts.restricted_image.clone(),
        trusted_folder: facts.trusted,
        native_steps: native_steps.clone(),
        model_steps: model_steps.clone(),
    };

    // ----- notices -----
    let mut notices = vec![
        "Nothing runs until you start the run. Steps run one at a time in the order shown; a failed step stops the workflow, and nothing is retried or repeated."
            .to_owned(),
    ];
    if !restricted_steps.is_empty() {
        notices.push(format!(
            "{} run{} in the restricted container ({}): only the declared inputs and code are staged read-only, with no network and no credentials; outputs are validated before anything uses them.",
            restricted_steps.iter().map(|l| format!("“{l}”")).collect::<Vec<_>>().join(", "),
            if restricted_steps.len() == 1 { "s" } else { "" },
            match (&facts.restricted_backend, &facts.restricted_image) {
                (Some(b), Some(i)) => format!("{b}, image {i}"),
                (Some(b), None) => format!("{b}, no image configured"),
                _ => "not configured".to_owned(),
            }
        ));
    }
    if !native_steps.is_empty() {
        notices.push(format!(
            "{} run{} natively with your account's privileges. This is not a sandbox.",
            label_list(&steps, &native_steps),
            if native_steps.len() == 1 { "s" } else { "" }
        ));
    }
    if !model_steps.is_empty() {
        notices.push(format!(
            "{} send{} the declared material to the model provider.",
            label_list(&steps, &model_steps),
            if model_steps.len() == 1 { "s" } else { "" }
        ));
    }
    if steps.iter().any(|s| s.effects.contains(&WorkflowEffect::ProposeDocumentChange)) {
        notices.push("Manuscript promotions are only prepared; you review and approve each one separately.".to_owned());
    }
    if steps.iter().any(|s| s.effects.contains(&WorkflowEffect::ProposeGraphChange)) {
        notices.push(
            "Evidence associations are only proposed; you accept or reject them after the run. They never assert that a formal declaration is equivalent to an informal statement."
                .to_owned(),
        );
    }
    if facts.active_run {
        blockers.push(blocker(
            "workflow.activeRun",
            "Another workflow run is active; wait for it to finish or cancel it first.",
            None,
            None,
        ));
    }

    let outcomes = def
        .outcomes
        .iter()
        .map(|o| WorkflowRunOutcome {
            id: o.id.clone(),
            label: o.label.clone(),
            optional: o.optional,
            step: o.from.step.clone(),
            port: o.from.port.clone(),
            value: None,
        })
        .collect();
    let mut plan = WorkflowRunPlan {
        plan: WorkflowPlanId::mint(),
        workflow: saved.id.clone(),
        revision: saved.revision,
        definition_hash: saved.hash.clone(),
        title: def.title.clone(),
        project: project_name(facts.root),
        inputs,
        steps,
        ceiling: def.ceiling,
        budgets,
        execution,
        notices,
        blockers,
        approval_hash: String::new(),
        created_at_ms: now_ms,
        expires_at_ms: now_ms + PLAN_TTL_MS,
    };
    plan.approval_hash = approval_hash(&plan, facts.root);
    BuiltPlan { plan, outcomes }
}

fn label_list(steps: &[WorkflowPlanStep], ids: &[String]) -> String {
    ids.iter()
        .map(|id| steps.iter().find(|s| &s.step == id).map(|s| format!("“{}”", s.label)).unwrap_or_default())
        .collect::<Vec<_>>()
        .join(", ")
}

/// What differs between an approved plan and the plan recomputed now.
pub fn describe_changes(approved: &WorkflowRunPlan, now: &WorkflowRunPlan) -> String {
    let mut changes = Vec::new();
    if approved.revision != now.revision || approved.definition_hash != now.definition_hash {
        changes.push(format!("the workflow was saved again (revision {} → {})", approved.revision, now.revision));
    }
    for input in &approved.inputs {
        match now.inputs.iter().find(|i| i.id == input.id) {
            None => changes.push(format!("input “{}” ({}) is no longer available", input.label, input.path)),
            Some(current) if current.path != input.path => {
                changes.push(format!("input “{}” now reads {} instead of {}", input.label, current.path, input.path))
            }
            Some(current) if current.sha256 != input.sha256 => changes.push(format!(
                "input “{}” ({}) changed (sha256 {} → {})",
                input.label,
                input.path,
                &input.sha256[..12.min(input.sha256.len())],
                &current.sha256[..12.min(current.sha256.len())]
            )),
            Some(_) => {}
        }
    }
    if approved.execution.trusted_folder != now.execution.trusted_folder {
        changes.push(if now.execution.trusted_folder {
            "the project folder became trusted".to_owned()
        } else {
            "the project folder is no longer trusted".to_owned()
        });
    }
    if approved.execution.restricted_image != now.execution.restricted_image
        || approved.execution.restricted_backend != now.execution.restricted_backend
    {
        changes.push(format!(
            "the restricted execution image changed ({} → {})",
            approved.execution.restricted_image.as_deref().unwrap_or("none"),
            now.execution.restricted_image.as_deref().unwrap_or("none")
        ));
    }
    for step in &approved.steps {
        if let Some(current) = now.steps.iter().find(|s| s.step == step.step)
            && current.available != step.available
        {
            changes.push(format!(
                "the runner of “{}” is {} available",
                step.label,
                if current.available { "now" } else { "no longer" }
            ));
        }
    }
    if approved.project != now.project {
        changes.push("the open project changed".to_owned());
    }
    if changes.is_empty() {
        changes.push("the plan's steps, budgets, or execution settings changed".to_owned());
    }
    format!("The plan is out of date: {}. Review the new plan before starting.", changes.join("; "))
}
