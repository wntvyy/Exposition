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
    AuthorityCeiling, DraftState, InputSource, InputVersion, ParamValue, StepCondition, StepRepeat, ValidationContext,
    WorkflowConnection, WorkflowStep,
};
use exposition_graph::{SavedWorkflow, WorkflowPlanId};
use serde::Serialize;

use crate::adapters::Adapters;
use crate::model::{
    MAX_PARALLEL, PLAN_TTL_MS, WorkflowConcurrencyLimits, WorkflowPlanBinding, WorkflowPlanBlocker,
    WorkflowPlanExecution, WorkflowPlanInput, WorkflowPlanStep, WorkflowRunBudgets, WorkflowRunOutcome,
    WorkflowRunPlan, WorkflowRunnerImage,
};

/// A run never takes longer than this in total.
pub const MAX_RUN_SECONDS: u32 = 4 * 3600;
/// A run never writes more than this in total.
pub const MAX_RUN_OUTPUT_BYTES: u64 = 1024 * 1024 * 1024;
/// Model requests allowed per model step.
pub const MODEL_REQUESTS_PER_STEP: u32 = 25;
/// A code input is shown in full in the plan; larger ones cannot be reviewed.
pub const MAX_REVIEW_CODE_BYTES: u64 = 1024 * 1024;

/// What the open project and application settings say right now.
pub struct PlanFacts<'a> {
    /// Canonical project root.
    pub root: &'a Path,
    pub trusted: bool,
    pub restricted_backend: Option<String>,
    pub restricted_image: Option<String>,
    /// Resolves the configured image's identity (asked only when a step
    /// runs in the restricted container).
    pub restricted_image_id: &'a dyn Fn() -> Option<String>,
    pub adapters: &'a Adapters,
    /// Why another run keeps this one from starting now (a run of the same
    /// workflow is active, or the active-run limit is reached).
    pub active_run: Option<String>,
    /// Input id → SHA-256 the plan pins instead of the definition's version.
    pub pins: &'a BTreeMap<String, String>,
    /// The application-wide concurrency limits.
    pub limits: &'a WorkflowConcurrencyLimits,
    /// Saved workflow revisions of the project (composite children).
    pub resolve: &'a exposition_graph::workflow::ResolveWorkflow<'a>,
}

/// The definition's `maxParallel` (1 when it declares none; validation
/// refuses values outside 1..=4, the runner never exceeds them either).
fn max_parallel_of(def: &exposition_graph::workflow::WorkflowDefinition) -> u32 {
    def.max_parallel().clamp(1, MAX_PARALLEL)
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
            let detail = reason.strip_prefix("Restricted execution is unavailable: ").unwrap_or(&reason).trim_end();
            (
                "workflow.restrictedUnavailable",
                format!("Restricted execution is unavailable: {detail} It never falls back to native execution."),
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

/// The complete text of a code input whose content hashed to `sha256`.
fn review_text(root: &Path, path: &WorkspacePath, sha256: &str, bytes: u64) -> Result<String, (&'static str, String)> {
    if bytes > MAX_REVIEW_CODE_BYTES {
        return Err((
            "workflow.codeTooLarge",
            format!("{path} is too large to review in full ({bytes} bytes; at most {MAX_REVIEW_CODE_BYTES})"),
        ));
    }
    let content = resolve(root, path)
        .and_then(|p| std::fs::read(p).map_err(|e| format!("{path} cannot be read: {e}")))
        .map_err(|m| ("workflow.inputMissing", m))?;
    if crate::adapters::sha256_hex(&content) != sha256 {
        return Err(("workflow.inputChanged", format!("{path} changed while the plan was made; review a new plan")));
    }
    String::from_utf8(content)
        .map_err(|_| ("workflow.codeNotText", format!("{path} is not UTF-8 text, so its code cannot be reviewed")))
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

/// The canonical identity of a project (case-insensitive on Windows, with
/// or without the verbatim `\\?\` prefix).
pub fn project_key(root: &Path) -> String {
    let s = root.to_string_lossy().into_owned();
    if !cfg!(windows) {
        return s;
    }
    let s = match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => rest.to_owned(),
        _ => s,
    };
    s.to_lowercase()
}

/// The time limit of a step with `parameters` (its `timeoutSeconds`, at most
/// the contract's limit).
pub(crate) fn time_limit_of(parameters: &BTreeMap<String, ParamValue>, contract: &OperationContract) -> u32 {
    let max = contract.limits.max_seconds.max(1);
    match parameters.get("timeoutSeconds") {
        Some(ParamValue::Integer(v)) => (*v).clamp(1, max as i64) as u32,
        _ => max,
    }
    .min(MAX_RUN_SECONDS)
}

/// At most this many forEach instances (validation allows 1..=20).
pub const MAX_FOR_EACH: usize = exposition_graph::workflow::MAX_FOR_EACH_VALUES;
/// At most this many until iterations (validation allows 1..=10).
pub const MAX_UNTIL: u32 = exposition_graph::workflow::MAX_UNTIL_ITERATIONS;

/// How many instances a repeated step may run (1 for other steps).
pub fn instances_of(repeat: Option<&StepRepeat>) -> u32 {
    match repeat {
        None => 1,
        Some(StepRepeat::ForEach { values, .. }) => values.len().clamp(1, MAX_FOR_EACH) as u32,
        Some(StepRepeat::Until { max_iterations, .. }) => (*max_iterations).clamp(1, MAX_UNTIL),
    }
}

/// The parameters of forEach instance `index` (the step's own otherwise).
pub fn instance_parameters(step: &WorkflowPlanStep, index: u32) -> BTreeMap<String, ParamValue> {
    let mut parameters = step.parameters.clone();
    if let Some(StepRepeat::ForEach { parameter, values }) = &step.repeat
        && let Some(value) = values.get(index as usize)
    {
        parameters.insert(parameter.clone(), value.clone());
    }
    parameters
}

/// The longest time limit any instance of the step gets.
fn step_time_limit(step: &WorkflowStep, contract: &OperationContract) -> u32 {
    match &step.repeat {
        Some(StepRepeat::ForEach { parameter, values }) => values
            .iter()
            .map(|v| {
                let mut p = step.parameters.clone();
                p.insert(parameter.clone(), v.clone());
                time_limit_of(&p, contract)
            })
            .max()
            .unwrap_or_else(|| time_limit_of(&step.parameters, contract)),
        _ => time_limit_of(&step.parameters, contract),
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
    max_model_requests: u32,
    available: bool,
    composite_path: &'a [String],
    condition: &'a Option<StepCondition>,
    repeat: &'a Option<StepRepeat>,
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
    max_parallel: u32,
    limits: &'a WorkflowConcurrencyLimits,
    execution: ApprovedExecution<'a>,
}

/// The execution binding; folder trust binds only plans with native steps.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ApprovedExecution<'a> {
    restricted_backend: &'a Option<String>,
    restricted_image: &'a Option<String>,
    restricted_image_id: &'a Option<String>,
    trusted_folder: Option<bool>,
    native_steps: &'a [String],
    model_steps: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    runner_images: &'a Option<Vec<WorkflowRunnerImage>>,
}

/// SHA-256 over a canonical serialization of everything the user approves.
pub fn approval_hash(plan: &WorkflowRunPlan, root: &Path) -> String {
    let key = project_key(root);
    let approved = Approved {
        schema: "exposition.workflowRunApproval/2",
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
                max_model_requests: s.max_model_requests,
                available: s.available,
                composite_path: &s.composite_path,
                condition: &s.condition,
                repeat: &s.repeat,
            })
            .collect(),
        ceiling: &plan.ceiling,
        budgets: &plan.budgets,
        max_parallel: plan.max_parallel,
        limits: &plan.limits,
        execution: ApprovedExecution {
            restricted_backend: &plan.execution.restricted_backend,
            restricted_image: &plan.execution.restricted_image,
            restricted_image_id: &plan.execution.restricted_image_id,
            trusted_folder: (!plan.execution.native_steps.is_empty()).then_some(plan.execution.trusted_folder),
            native_steps: &plan.execution.native_steps,
            model_steps: &plan.execution.model_steps,
            runner_images: &plan.execution.runner_images,
        },
    };
    crate::adapters::sha256_hex(&serde_json::to_vec(&approved).unwrap_or_default())
}

/// A plan and the declared outcomes a run of it reports.
pub struct BuiltPlan {
    pub plan: WorkflowRunPlan,
    pub outcomes: Vec<WorkflowRunOutcome>,
}

/// `sha256:0123456789ab` (or `unknown`) for messages.
fn short_id(id: Option<&str>) -> String {
    match id {
        Some(id) => id.chars().take("sha256:".len() + 12).collect(),
        None => "unknown".to_owned(),
    }
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
    let validation = exposition_graph::validate_workflow(
        def,
        &ValidationContext {
            resolve_workflow: Some(facts.resolve),
            workflow_id: Some(&saved.id),
            ..Default::default()
        },
    );
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
        let pinned = match (facts.pins.get(&input.id), version) {
            (Some(pin), _) => Some(pin.to_ascii_lowercase()),
            (None, InputVersion::Pinned { content_hash }) => Some(content_hash.to_ascii_lowercase()),
            (None, InputVersion::AtRunStart) => None,
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
                            pin.chars().take(12).collect::<String>(),
                            &sha256[..12]
                        ),
                        None,
                        Some(&input.id),
                    ));
                }
                // Code is reviewed in full: scripts, and Lean sources (Lean
                // elaborates them natively, and `#eval` runs code).
                let text =
                    if matches!(input.port_type, registry::PortType::CodeSource | registry::PortType::FormalSource) {
                        match review_text(facts.root, path, &sha256, bytes) {
                            Ok(text) => Some(text),
                            Err((code, message)) => {
                                blockers.push(blocker(code, message, None, Some(&input.id)));
                                None
                            }
                        }
                    } else {
                        None
                    };
                inputs.push(WorkflowPlanInput {
                    id: input.id.clone(),
                    label: input.label.clone(),
                    port_type: input.port_type,
                    path: path.clone(),
                    sha256,
                    bytes,
                    pinned: pinned.is_some(),
                    text,
                });
            }
            Err(message) => blockers.push(blocker("workflow.inputMissing", message, None, Some(&input.id))),
        }
    }

    // ----- steps in dependency order (composites expanded) -----
    // The plan is flat: each composite step is replaced by its pinned
    // child's steps (`<composite>/<child>`, validation's dependency order).
    let expanded = match exposition_graph::workflow::expand_workflow(def, facts.resolve) {
        Ok(expanded) => Some(expanded),
        Err(message) => {
            if validation.state == DraftState::WellFormed {
                blockers.push(blocker("workflow.compositeNotWellFormed", message, None, None));
            }
            None
        }
    };
    let flat_def = expanded.as_ref().map(|e| &e.definition).unwrap_or(def);
    let mut steps: Vec<WorkflowPlanStep> = Vec::new();
    for planned in &validation.plan {
        let Some(step) = flat_def.steps.iter().find(|s| s.id == planned.step) else { continue };
        let Some(contract) = registry::operation(&step.operation.id, step.operation.version) else { continue };
        let composite_path = expanded
            .as_ref()
            .and_then(|e| e.steps.iter().find(|x| x.id == step.id))
            .map(|x| x.composite_path.clone())
            .unwrap_or_default();
        let bindings: Vec<WorkflowPlanBinding> = flat_def
            .connections
            .iter()
            .filter_map(|c| match c {
                WorkflowConnection::Data { from, to } if to.step == step.id => {
                    Some(WorkflowPlanBinding { port: to.port.clone(), from: from.clone() })
                }
                _ => None,
            })
            .collect();
        let mut prerequisites: Vec<String> = Vec::new();
        for c in &flat_def.connections {
            if let WorkflowConnection::Prerequisite { before, after } = c
                && after == &step.id
                && !prerequisites.contains(before)
            {
                prerequisites.push(before.clone());
            }
        }
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
            time_limit_seconds: step_time_limit(step, &contract),
            max_output_bytes: contract.limits.max_output_bytes,
            max_model_requests: if contract.runner.containment == Containment::ReadOnlyModel {
                MODEL_REQUESTS_PER_STEP
            } else {
                0
            },
            available,
            availability,
            composite_path,
            condition: step.condition.clone(),
            repeat: step.repeat.clone(),
        });
    }
    // A condition reads one validated output: not the many outputs of a
    // forEach step.
    for s in &steps {
        if let Some(c) = &s.condition
            && steps.iter().any(|p| p.step == c.source.step && matches!(p.repeat, Some(StepRepeat::ForEach { .. })))
        {
            blockers.push(blocker(
                "workflow.conditionSource",
                format!(
                    "“{}”: its condition reads a step that runs once per value; it has no single output to test",
                    s.label
                ),
                Some(&s.step),
                None,
            ));
        }
    }

    // ----- budgets -----
    let model_steps: Vec<String> =
        steps.iter().filter(|s| s.containment == Containment::ReadOnlyModel).map(|s| s.step.clone()).collect();
    let native_steps: Vec<String> =
        steps.iter().filter(|s| s.containment == Containment::NativeAccount).map(|s| s.step.clone()).collect();
    let restricted_steps: Vec<&str> =
        steps.iter().filter(|s| s.containment == Containment::RestrictedContainer).map(|s| s.label.as_str()).collect();
    // Every instance of a repeated step counts like a step.
    let n = |s: &WorkflowPlanStep| instances_of(s.repeat.as_ref());
    let budgets = WorkflowRunBudgets {
        max_steps: steps.iter().map(n).sum(),
        max_total_seconds: steps
            .iter()
            .map(|s| s.time_limit_seconds.saturating_mul(n(s)))
            .fold(0u32, u32::saturating_add)
            .clamp(1, MAX_RUN_SECONDS),
        max_output_bytes: steps
            .iter()
            .map(|s| s.max_output_bytes.saturating_mul(u64::from(n(s))))
            .fold(0u64, u64::saturating_add)
            .min(MAX_RUN_OUTPUT_BYTES),
        max_model_requests: steps.iter().map(|s| s.max_model_requests * n(s)).sum(),
    };
    let max_parallel = max_parallel_of(def);
    // The restricted backend and image bind the plan only when a step uses
    // them. A runner with its own image (R, Julia) binds that image and its
    // identity instead of the default one.
    let restricted = !restricted_steps.is_empty();
    let mut runner_images: Vec<WorkflowRunnerImage> = Vec::new();
    let mut default_image_steps: Vec<&str> = Vec::new();
    for s in steps.iter().filter(|s| s.containment == Containment::RestrictedContainer) {
        match facts.adapters.get(&s.runner).and_then(|a| a.restricted_image()) {
            None => default_image_steps.push(s.label.as_str()),
            Some(Ok((image, image_id))) => {
                if !runner_images.iter().any(|b| b.runner == s.runner) {
                    runner_images.push(WorkflowRunnerImage { runner: s.runner.clone(), image, image_id });
                }
            }
            // The step is unavailable; its availability blocker says why.
            Some(Err(_)) => {}
        }
    }
    let default_restricted = !default_image_steps.is_empty();
    let execution = WorkflowPlanExecution {
        restricted_backend: facts.restricted_backend.clone().filter(|_| restricted),
        restricted_image: facts.restricted_image.clone().filter(|_| default_restricted),
        restricted_image_id: if default_restricted { (facts.restricted_image_id)() } else { None },
        trusted_folder: facts.trusted,
        native_steps: native_steps.clone(),
        model_steps: model_steps.clone(),
        runner_images: (!runner_images.is_empty()).then(|| runner_images.clone()),
    };

    // ----- notices -----
    // Repeated steps run their declared instances; nothing else repeats.
    let repeats = if steps.iter().any(|s| s.repeat.is_some()) {
        "nothing is retried, and repeated steps run only their declared instances"
    } else {
        "nothing is retried or repeated"
    };
    let mut notices = vec![if max_parallel <= 1 {
        format!(
            "Nothing runs until you start the run. Steps run one at a time in the order shown; a failed step stops the workflow, and {repeats}."
        )
    } else {
        let l = facts.limits;
        format!(
            "Nothing runs until you start the run. Up to {max_parallel} independent steps run at the same time (a step starts once everything it depends on succeeded; across all runs at most {} steps at once: {} restricted, {} native, {} agent, {} Lean). Budgets are reserved before each step starts. A failed step stops the workflow: nothing new starts, steps already running finish, and {repeats}.",
            l.max_in_flight_steps, l.restricted, l.native, l.agent, l.lean
        )
    }];
    let restricted_notice = |labels: &[&str], image: Option<&str>| {
        format!(
            "{} run{} in the restricted container ({}): only the declared inputs and code are staged read-only, with no network and no credentials; outputs are validated before anything uses them.",
            labels.iter().map(|l| format!("“{l}”")).collect::<Vec<_>>().join(", "),
            if labels.len() == 1 { "s" } else { "" },
            match (&facts.restricted_backend, image) {
                (Some(b), Some(i)) => format!("{b}, image {i}"),
                (Some(b), None) => format!("{b}, no image configured"),
                _ => "not configured".to_owned(),
            }
        )
    };
    if default_restricted {
        notices.push(restricted_notice(&default_image_steps, facts.restricted_image.as_deref()));
    }
    for bound in &runner_images {
        let labels: Vec<&str> = steps.iter().filter(|s| s.runner == bound.runner).map(|s| s.label.as_str()).collect();
        notices.push(restricted_notice(&labels, Some(&bound.image)));
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
    // Bounded control flow, in plain words.
    let label_of =
        |id: &str| steps.iter().find(|s| s.step == id).map(|s| s.label.clone()).unwrap_or_else(|| id.to_owned());
    for s in &steps {
        if let Some(c) = &s.condition {
            notices.push(format!(
                "“{}” runs only when {} (output “{}” of “{}”); otherwise it does not run (condition not met), and the steps that need it are skipped.",
                s.label,
                crate::condition::describe_as(&c.test, c.negate),
                c.source.port,
                label_of(&c.source.step)
            ));
        }
        match &s.repeat {
            Some(StepRepeat::ForEach { parameter, values }) => notices.push(format!(
                "“{}” runs once for each of {} values of {parameter} (each instance counts as a step).",
                s.label,
                values.len()
            )),
            Some(StepRepeat::Until { port, test, negate, max_iterations, on_exhausted }) => notices.push(format!(
                "“{}” runs again, at most {max_iterations} times, until {} for its output “{port}”; if that never happens, {}.",
                s.label,
                crate::condition::describe_as(test, *negate),
                match on_exhausted {
                    exposition_graph::workflow::Exhausted::Fail => "the step fails",
                    exposition_graph::workflow::Exhausted::UseLast => "the last iteration's outputs are used",
                }
            )),
            None => {}
        }
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
    // A restricted plan binds the image's identity, not just its name.
    if default_restricted && execution.restricted_image_id.is_none() && steps.iter().all(|s| s.available) {
        blockers.push(blocker(
            "workflow.restrictedUnavailable",
            "The restricted image's identity could not be determined (Docker did not report it); restricted steps run only in an identified image. Nothing runs natively instead.",
            None,
            None,
        ));
    }
    for bound in runner_images.iter().filter(|b| b.image_id.is_none()) {
        if steps.iter().all(|s| s.available) {
            blockers.push(blocker(
                "workflow.restrictedUnavailable",
                format!(
                    "The identity of the restricted image {} could not be determined (Docker did not report it); restricted steps run only in an identified image. Nothing runs natively instead.",
                    bound.image
                ),
                None,
                None,
            ));
        }
    }
    if let Some(message) = &facts.active_run {
        blockers.push(blocker("workflow.activeRun", message.clone(), None, None));
    }

    let outcomes = def
        .outcomes
        .iter()
        .map(|o| {
            // An outcome of a composite step is realized by a step inside it.
            let from = flat_def
                .outcomes
                .iter()
                .find(|x| x.id == o.id)
                .map(|x| x.from.clone())
                .unwrap_or_else(|| o.from.clone());
            WorkflowRunOutcome {
                id: o.id.clone(),
                label: o.label.clone(),
                optional: o.optional,
                step: from.step,
                port: from.port,
                value: None,
                reason: None,
            }
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
        max_parallel,
        limits: facts.limits.clone(),
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
    if approved.execution.restricted_image == now.execution.restricted_image
        && approved.execution.restricted_backend == now.execution.restricted_backend
        && approved.execution.restricted_image_id != now.execution.restricted_image_id
    {
        changes.push(format!(
            "the restricted image {} is now a different image ({} → {})",
            now.execution.restricted_image.as_deref().unwrap_or("none"),
            short_id(approved.execution.restricted_image_id.as_deref()),
            short_id(now.execution.restricted_image_id.as_deref())
        ));
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
    let images = |p: &WorkflowRunPlan| p.execution.runner_images.clone().unwrap_or_default();
    for bound in images(approved) {
        match images(now).iter().find(|n| n.runner == bound.runner) {
            None => changes.push(format!("the restricted image {} is no longer configured", bound.image)),
            Some(n) if n.image != bound.image => {
                changes.push(format!("the restricted image changed ({} → {})", bound.image, n.image))
            }
            Some(n) if n.image_id != bound.image_id => changes.push(format!(
                "the restricted image {} is now a different image ({} → {})",
                n.image,
                short_id(bound.image_id.as_deref()),
                short_id(n.image_id.as_deref())
            )),
            Some(_) => {}
        }
    }
    for added in images(now).iter().filter(|n| !images(approved).iter().any(|b| b.runner == n.runner)) {
        changes.push(format!(
            "the restricted image {} ({}) is now configured",
            added.image,
            short_id(added.image_id.as_deref())
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
    if approved.max_parallel != now.max_parallel {
        changes.push(format!(
            "the steps that may run at the same time changed ({} → {})",
            approved.max_parallel, now.max_parallel
        ));
    }
    if approved.limits != now.limits {
        changes.push("the application's concurrency limits changed".to_owned());
    }
    if approved.project != now.project {
        changes.push("the open project changed".to_owned());
    }
    if changes.is_empty() {
        changes.push("the plan's steps, budgets, or execution settings changed".to_owned());
    }
    format!("The plan is out of date: {}. Review the new plan before starting.", changes.join("; "))
}
