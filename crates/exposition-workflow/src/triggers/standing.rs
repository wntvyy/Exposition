//! Standing plans: what a trigger approves, and how a later plan is compared.
//!
//! The standing hash is the run plan's approval hash
//! ([`crate::plan::approval_hash`]) computed over a copy of the plan in which
//! the watched inputs' content (SHA-256, size, review text) is blanked. Every
//! other part of the plan — revision and definition hash, the other inputs'
//! content (code above all), steps with their payloads, runners, containment,
//! effects, availability, budgets, the restricted image and its identity,
//! folder trust for native steps, the project — stays bound.

use std::path::Path;

use exposition_graph::workflow::registry::PortType;

use super::model::{
    MAX_DEBOUNCE_SECONDS, MAX_INTERVAL_MINUTES, MAX_MODEL_REQUESTS_PER_DAY, MAX_RUNS_PER_DAY, MAX_RUNS_PER_HOUR,
    MAX_WATCHED_INPUTS, MIN_DEBOUNCE_SECONDS, MIN_INTERVAL_MINUTES, TriggerKind, TriggerWatchedInput,
    WorkflowTriggerSettings,
};
use crate::model::{WorkflowRunError, WorkflowRunPlan};
use crate::plan;

/// The plan with the watched inputs' content blanked.
pub fn blanked(plan: &WorkflowRunPlan, watched: &[String]) -> WorkflowRunPlan {
    let mut standing = plan.clone();
    for input in &mut standing.inputs {
        if watched.iter().any(|w| w == &input.id) {
            input.sha256.clear();
            input.bytes = 0;
            input.text = None;
        }
    }
    standing
}

/// The standing hash of `plan` in the project at `root`.
pub fn standing_hash(plan: &WorkflowRunPlan, root: &Path, watched: &[String]) -> String {
    plan::approval_hash(&blanked(plan, watched), root)
}

/// What changed between the approved standing plan and a plan made now
/// (the watched inputs' content never counts).
pub fn describe(approved: &WorkflowRunPlan, now: &WorkflowRunPlan, watched: &[String]) -> String {
    let text = plan::describe_changes(&blanked(approved, watched), &blanked(now, watched));
    let text = text.strip_suffix(" Review the new plan before starting.").unwrap_or(&text);
    let text = text.strip_prefix("The plan is out of date: ").unwrap_or(text);
    format!(
        "The standing plan no longer matches: {text} The trigger is suspended until you review and enable it again."
    )
}

/// Inputs that are code: scripts, and Lean sources (elaborated natively by
/// Lean, where `#eval` runs code). They are never watched, the review shows
/// them in full, and any change to one suspends the trigger.
pub fn is_code(port_type: PortType) -> bool {
    matches!(port_type, PortType::CodeSource | PortType::FormalSource)
}

fn refuse(code: &str, message: impl Into<String>) -> WorkflowRunError {
    WorkflowRunError::new(code, message)
}

/// Check the kind against the plan: the watched inputs must be resolved
/// external inputs of the plan, never code, never pinned.
pub fn watched_inputs(
    plan: &WorkflowRunPlan,
    kind: &TriggerKind,
) -> Result<Vec<TriggerWatchedInput>, WorkflowRunError> {
    let inputs = kind.watched();
    match kind {
        TriggerKind::InputsChanged { inputs } if inputs.is_empty() => {
            return Err(refuse("trigger.noInputs", "Choose at least one input whose saved changes start a run."));
        }
        TriggerKind::Interval { minutes, .. } if !(MIN_INTERVAL_MINUTES..=MAX_INTERVAL_MINUTES).contains(minutes) => {
            return Err(refuse(
                "trigger.invalidInterval",
                format!(
                    "An interval trigger runs every {MIN_INTERVAL_MINUTES} to {MAX_INTERVAL_MINUTES} minutes (asked: {minutes})."
                ),
            ));
        }
        _ => {}
    }
    if inputs.len() > MAX_WATCHED_INPUTS {
        return Err(refuse("trigger.tooManyInputs", format!("A trigger watches at most {MAX_WATCHED_INPUTS} inputs.")));
    }
    let mut watched: Vec<TriggerWatchedInput> = Vec::new();
    for id in inputs {
        if watched.iter().any(|w| &w.input == id) {
            return Err(refuse("trigger.duplicateInput", format!("Input “{id}” is listed twice.")));
        }
        let Some(input) = plan.inputs.iter().find(|i| &i.id == id) else {
            return Err(refuse(
                "trigger.unknownInput",
                format!(
                    "“{id}” is not a resolved external input of this revision; only its declared input files can be watched."
                ),
            ));
        };
        if is_code(input.port_type) {
            return Err(refuse(
                "trigger.codeInputWatched",
                format!(
                    "“{}” ({}) is code{}. Code never runs without your review: a change to it suspends the trigger instead of starting a run, so it cannot be watched.",
                    input.label,
                    input.path,
                    if input.port_type == PortType::FormalSource {
                        " (Lean elaborates it natively, and `#eval` runs code)"
                    } else {
                        ""
                    }
                ),
            ));
        }
        if input.pinned {
            return Err(refuse(
                "trigger.inputPinned",
                format!(
                    "“{}” ({}) is pinned to one exact version by the workflow, so a change to it could never run; watch an input resolved at run start.",
                    input.label, input.path
                ),
            ));
        }
        watched.push(TriggerWatchedInput {
            input: input.id.clone(),
            label: input.label.clone(),
            path: input.path.clone(),
            port_type: input.port_type,
        });
    }
    Ok(watched)
}

/// Labels of the plan's native steps.
pub fn native_labels(plan: &WorkflowRunPlan) -> Vec<String> {
    plan.execution
        .native_steps
        .iter()
        .filter_map(|id| plan.steps.iter().find(|s| &s.step == id).map(|s| s.label.clone()))
        .collect()
}

/// Check the settings against the bounds and the plan.
pub fn check_settings(
    settings: &WorkflowTriggerSettings,
    plan: &WorkflowRunPlan,
    kind: &TriggerKind,
) -> Result<(), WorkflowRunError> {
    let invalid = |m: String| refuse("trigger.invalidSettings", m);
    if matches!(kind, TriggerKind::InputsChanged { .. })
        && !(MIN_DEBOUNCE_SECONDS..=MAX_DEBOUNCE_SECONDS).contains(&settings.debounce_seconds)
    {
        return Err(invalid(format!(
            "The quiet period must be {MIN_DEBOUNCE_SECONDS} s to {MAX_DEBOUNCE_SECONDS} s (asked: {} s).",
            settings.debounce_seconds
        )));
    }
    if !(1..=MAX_RUNS_PER_HOUR).contains(&settings.max_runs_per_hour) {
        return Err(invalid(format!("Runs per hour must be 1 to {MAX_RUNS_PER_HOUR}.")));
    }
    if !(1..=MAX_RUNS_PER_DAY).contains(&settings.max_runs_per_day) {
        return Err(invalid(format!("Runs per day must be 1 to {MAX_RUNS_PER_DAY}.")));
    }
    if settings.max_runs_per_hour > settings.max_runs_per_day {
        return Err(invalid("Runs per hour cannot exceed runs per day.".into()));
    }
    if settings.max_model_requests_per_day > MAX_MODEL_REQUESTS_PER_DAY {
        return Err(invalid(format!("Model requests per day must be at most {MAX_MODEL_REQUESTS_PER_DAY}.")));
    }
    if !plan.execution.native_steps.is_empty() && !settings.allow_native {
        return Err(refuse(
            "trigger.nativeNotAllowed",
            format!(
                "{} run{} natively with your account's privileges (not a sandbox). A trigger starts them without asking only if you explicitly allow native steps.",
                native_labels(plan).iter().map(|l| format!("“{l}”")).collect::<Vec<_>>().join(", "),
                if plan.execution.native_steps.len() == 1 { "s" } else { "" }
            ),
        ));
    }
    let per_run = plan.budgets.max_model_requests;
    if !plan.execution.model_steps.is_empty() && settings.max_model_requests_per_day < per_run {
        return Err(refuse(
            "trigger.modelCapTooLow",
            format!(
                "One run may send up to {per_run} model requests; the daily cap ({}) must allow at least one run.",
                settings.max_model_requests_per_day
            ),
        ));
    }
    Ok(())
}

/// Suggested settings for a plan.
pub fn default_settings(plan: &WorkflowRunPlan) -> WorkflowTriggerSettings {
    let per_run = plan.budgets.max_model_requests;
    WorkflowTriggerSettings {
        debounce_seconds: 10,
        max_runs_per_hour: 4,
        max_runs_per_day: 24,
        allow_native: false,
        max_model_requests_per_day: (per_run.saturating_mul(4)).min(MAX_MODEL_REQUESTS_PER_DAY),
    }
}

fn short(id: &str) -> String {
    id.chars().take("sha256:".len() + 12).collect()
}

/// Exactly what may change without asking.
pub fn may_change(watched: &[TriggerWatchedInput]) -> Vec<String> {
    if watched.is_empty() {
        return vec!["Nothing: every input, step, and setting must stay exactly as reviewed.".to_owned()];
    }
    watched
        .iter()
        .map(|w| format!("The saved content of “{}” ({}, {}).", w.label, w.path, w.port_type.title()))
        .collect()
}

/// What suspends the trigger.
pub fn suspends_on(plan: &WorkflowRunPlan, watched: &[String]) -> Vec<String> {
    let mut list =
        vec![format!("A new revision of “{}” (this trigger runs revision {} only).", plan.title, plan.revision)];
    for input in plan.inputs.iter().filter(|i| !watched.contains(&i.id)) {
        list.push(if is_code(input.port_type) {
            format!("Any change to the code in “{}” ({}).", input.label, input.path)
        } else {
            format!("Any change to “{}” ({}).", input.label, input.path)
        });
    }
    if plan.execution.restricted_image.is_some() || plan.execution.restricted_image_id.is_some() {
        list.push(format!(
            "The restricted image {} becoming a different image (approved: {}).",
            plan.execution.restricted_image.as_deref().unwrap_or("(none configured)"),
            plan.execution.restricted_image_id.as_deref().map(short).unwrap_or_else(|| "unknown".into())
        ));
    }
    // R and Julia steps run in their own images, bound the same way.
    for bound in plan.execution.runner_images.iter().flatten() {
        list.push(format!(
            "The restricted image {} becoming a different image (approved: {}).",
            bound.image,
            bound.image_id.as_deref().map(short).unwrap_or_else(|| "unknown".into())
        ));
    }
    if !plan.execution.native_steps.is_empty() {
        list.push(
            "The project folder's trust: native steps run with your account's privileges and need a trusted folder."
                .into(),
        );
    }
    list.push("Trust being revoked for the project folder: every trigger of the folder is suspended.".into());
    list.push("Any step's runner becoming unavailable (or available), and any step, budget, or limit changing.".into());
    list
}

/// What enabling the trigger means.
pub fn notices(plan: &WorkflowRunPlan, kind: &TriggerKind, watched: &[TriggerWatchedInput]) -> Vec<String> {
    let mut notices = Vec::new();
    match kind {
        TriggerKind::InputsChanged { .. } => notices.push(format!(
            "A run starts without asking after saved changes to {} once no further change arrives for the quiet period, within the hourly and daily limits.",
            watched.iter().map(|w| format!("“{}”", w.label)).collect::<Vec<_>>().join(", ")
        )),
        TriggerKind::Interval { minutes, .. } => notices.push(format!(
            "A run starts without asking every {minutes} minutes while the project is open, within the hourly and daily limits."
        )),
    }
    notices.push(
        "Nothing runs when the project opens or while it is indexed, and changes made while it was closed never start a run. Closing the project stops the trigger until it is open again."
            .into(),
    );
    notices.push(
        "If a run of this workflow is active, one follow-up waits for it to finish (the latest saved content wins)."
            .into(),
    );
    let native = native_labels(plan);
    if !native.is_empty() {
        notices.push(format!(
            "{} run{} natively with your account's privileges (not a sandbox); enabling requires allowing native steps explicitly.",
            native.iter().map(|l| format!("“{l}”")).collect::<Vec<_>>().join(", "),
            if native.len() == 1 { "s" } else { "" }
        ));
    }
    if !plan.execution.model_steps.is_empty() {
        notices.push(format!(
            "Each run may send up to {} model requests (the declared material goes to the model provider); they count against the trigger's daily cap.",
            plan.budgets.max_model_requests
        ));
    }
    notices
}
