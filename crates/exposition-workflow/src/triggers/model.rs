//! Trigger contract (shared with the UI through generated bindings).
//!
//! A trigger is an explicit, user-only standing approval: the user reviews a
//! *standing plan* ([`WorkflowTriggerPlan`]) — the exact revision, runners,
//! image identity, trust, and budgets of a run plan — and enables it with its
//! `standing_hash`. Only the declared watched inputs may change without
//! asking; any other change to the plan suspends the trigger until the user
//! reviews and enables it again.

use exposition_core::path::WorkspacePath;
use exposition_graph::workflow::registry::PortType;
use exposition_graph::{WorkflowId, WorkflowPlanId, WorkflowRunId};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::model::{ChangedInput, WorkflowPlanBlocker, WorkflowRunPlan};

/// Shortest quiet period after the last saved change before a trigger fires.
pub const MIN_DEBOUNCE_SECONDS: u32 = 2;
/// Longest quiet period.
pub const MAX_DEBOUNCE_SECONDS: u32 = 3600;
/// Shortest schedule of an interval trigger.
pub const MIN_INTERVAL_MINUTES: u32 = 15;
/// Longest schedule of an interval trigger (one week).
pub const MAX_INTERVAL_MINUTES: u32 = 7 * 24 * 60;
/// Most triggered runs per hour (a trigger may set fewer).
pub const MAX_RUNS_PER_HOUR: u32 = 12;
/// Most triggered runs per day (a trigger may set fewer).
pub const MAX_RUNS_PER_DAY: u32 = 100;
/// Most model requests a trigger's runs may use per day.
pub const MAX_MODEL_REQUESTS_PER_DAY: u32 = 1000;
/// Most inputs one trigger may watch.
pub const MAX_WATCHED_INPUTS: usize = 16;
/// History entries kept per trigger (oldest dropped first).
pub const MAX_TRIGGER_HISTORY: usize = 50;
/// Triggers kept per project (disabled ones included).
pub const MAX_TRIGGERS_PER_PROJECT: usize = 32;

/// Stable identity of a trigger (`wtrg-<32 hex>`), app-local.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export, type = "string")]
#[serde(try_from = "String", into = "String")]
pub struct WorkflowTriggerId(String);

impl WorkflowTriggerId {
    pub const PREFIX: &'static str = "wtrg-";

    pub fn mint() -> Self {
        Self(format!("{}{}", Self::PREFIX, uuid::Uuid::new_v4().simple()))
    }

    /// Validate an identifier from an untrusted source.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let valid = raw.len() <= 64
            && raw.len() > Self::PREFIX.len()
            && raw.starts_with(Self::PREFIX)
            && raw[Self::PREFIX.len()..].chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        if valid { Ok(Self(raw.to_owned())) } else { Err("invalid workflow trigger identifier".into()) }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for WorkflowTriggerId {
    type Error = String;
    fn try_from(raw: String) -> Result<Self, String> {
        Self::parse(&raw)
    }
}

impl From<WorkflowTriggerId> for String {
    fn from(id: WorkflowTriggerId) -> String {
        id.0
    }
}

impl std::fmt::Display for WorkflowTriggerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What makes a trigger fire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum TriggerKind {
    /// Saved changes to the content of these external inputs (ids). They are
    /// also the only inputs allowed to change without asking.
    InputsChanged { inputs: Vec<String> },
    /// On a schedule (at least every [`MIN_INTERVAL_MINUTES`]) while the
    /// project is open. `inputs` lists the inputs allowed to change between
    /// runs without asking (none by default: every input stays as reviewed).
    Interval {
        minutes: u32,
        #[serde(default)]
        inputs: Vec<String>,
    },
}

impl TriggerKind {
    /// The inputs allowed to change without asking.
    pub fn watched(&self) -> &[String] {
        match self {
            TriggerKind::InputsChanged { inputs } | TriggerKind::Interval { inputs, .. } => inputs,
        }
    }
}

/// An input the trigger watches (allowed to change without asking).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TriggerWatchedInput {
    pub input: String,
    pub label: String,
    pub path: WorkspacePath,
    pub port_type: PortType,
}

/// The user's settings of a trigger (bounded; checked when it is enabled).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowTriggerSettings {
    /// Quiet period after the last saved change ([`MIN_DEBOUNCE_SECONDS`]..=[`MAX_DEBOUNCE_SECONDS`]).
    pub debounce_seconds: u32,
    /// 1..=[`MAX_RUNS_PER_HOUR`].
    pub max_runs_per_hour: u32,
    /// 1..=[`MAX_RUNS_PER_DAY`].
    pub max_runs_per_day: u32,
    /// Required (explicitly true) when the plan has steps that run natively
    /// with the user's account privileges.
    pub allow_native: bool,
    /// Model requests the trigger's runs may use per day (each run counts its
    /// full model-request budget); at least one run's budget when the plan has
    /// model steps, at most [`MAX_MODEL_REQUESTS_PER_DAY`].
    pub max_model_requests_per_day: u32,
}

/// The bounds the settings must respect (for the UI).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowTriggerLimits {
    pub min_debounce_seconds: u32,
    pub max_debounce_seconds: u32,
    pub min_interval_minutes: u32,
    pub max_interval_minutes: u32,
    pub max_runs_per_hour: u32,
    pub max_runs_per_day: u32,
    pub max_model_requests_per_day: u32,
    pub max_watched_inputs: u32,
}

impl WorkflowTriggerLimits {
    pub fn current() -> Self {
        Self {
            min_debounce_seconds: MIN_DEBOUNCE_SECONDS,
            max_debounce_seconds: MAX_DEBOUNCE_SECONDS,
            min_interval_minutes: MIN_INTERVAL_MINUTES,
            max_interval_minutes: MAX_INTERVAL_MINUTES,
            max_runs_per_hour: MAX_RUNS_PER_HOUR,
            max_runs_per_day: MAX_RUNS_PER_DAY,
            max_model_requests_per_day: MAX_MODEL_REQUESTS_PER_DAY,
            max_watched_inputs: MAX_WATCHED_INPUTS as u32,
        }
    }
}

/// Review a standing plan for a trigger of the latest revision of `workflow`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowTriggerPlanRequest {
    pub workflow: WorkflowId,
    pub kind: TriggerKind,
}

/// A standing plan the user reviews before enabling a trigger.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowTriggerPlan {
    /// The complete run plan (every payload, runner, containment, effect,
    /// budget) exactly as each triggered run will be checked against.
    pub plan: WorkflowRunPlan,
    pub kind: TriggerKind,
    pub watched: Vec<TriggerWatchedInput>,
    /// The plan's approval hash with the watched inputs' content blanked.
    /// `workflow_trigger_enable` requires it.
    pub standing_hash: String,
    /// Exactly what may change without asking.
    pub may_change: Vec<String>,
    /// Anything here changing suspends the trigger until you enable it again.
    pub suspends_on: Vec<String>,
    /// Labels of steps that run natively with your account's privileges
    /// (enabling needs `allowNative`).
    pub native_steps: Vec<String>,
    /// Model requests one run may use (counted against the daily cap).
    pub model_requests_per_run: u32,
    /// Suggested settings.
    pub defaults: WorkflowTriggerSettings,
    pub limits: WorkflowTriggerLimits,
    /// What enabling means (unattended starts, limits, open/close behaviour).
    pub notices: Vec<String>,
    /// Non-empty: the trigger cannot be enabled (the plan's own blockers,
    /// except another run being active).
    pub blockers: Vec<WorkflowPlanBlocker>,
    pub expires_at_ms: u64,
}

/// Enable a trigger from a reviewed standing plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnableWorkflowTriggerRequest {
    /// The `plan.plan` id of the reviewed [`WorkflowTriggerPlan`].
    pub plan: WorkflowPlanId,
    pub standing_hash: String,
    pub settings: WorkflowTriggerSettings,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum TriggerState {
    /// Listening (while its project is open and indexed).
    Active,
    /// Stopped until the user reviews and enables it again (`code`: why).
    Suspended { code: String, message: String },
    /// Turned off by the user.
    Disabled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum TriggerHistoryEvent {
    /// Enabled (or re-enabled) by the user for this revision.
    Enabled {
        revision: u64,
    },
    /// A run started (`changed`: the watched inputs whose content changed,
    /// old → new SHA-256; empty for a scheduled run).
    Fired {
        run: WorkflowRunId,
        changed: Vec<ChangedInput>,
    },
    /// A run of the workflow was active; one follow-up waits for it.
    Queued,
    /// A rate limit was reached; the next check waits until `until_ms`.
    RateLimited {
        until_ms: u64,
    },
    /// The run service refused to start the run (the trigger stays enabled).
    StartRefused {
        code: String,
        message: String,
    },
    Suspended {
        code: String,
        message: String,
    },
    Disabled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TriggerHistoryEntry {
    pub at_ms: u64,
    pub event: TriggerHistoryEvent,
}

/// A trigger as the UI shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowTrigger {
    pub id: WorkflowTriggerId,
    pub workflow: WorkflowId,
    /// The exact revision the standing plan approved.
    pub revision: u64,
    pub title: String,
    /// Display name of the project folder.
    pub project: String,
    pub kind: TriggerKind,
    pub watched: Vec<TriggerWatchedInput>,
    pub debounce_seconds: u32,
    pub max_runs_per_hour: u32,
    pub max_runs_per_day: u32,
    pub allow_native: bool,
    pub max_model_requests_per_day: u32,
    pub model_requests_per_run: u32,
    pub standing_hash: String,
    pub state: TriggerState,
    pub created_at_ms: u64,
    /// When the user last enabled it.
    pub enabled_at_ms: u64,
    pub last_fired_ms: Option<u64>,
    /// One follow-up waits for an active run of the workflow to end.
    pub queued: bool,
    /// When the trigger next checks (quiet period end, schedule, or rate
    /// limit), while listening.
    pub next_check_ms: Option<u64>,
    /// Active, and its project is open and finished indexing.
    pub listening: bool,
    pub runs_last_hour: u32,
    pub runs_last_day: u32,
    pub model_requests_last_day: u32,
    /// Bounded, oldest first.
    pub history: Vec<TriggerHistoryEntry>,
    /// The trigger store could not be saved (or read): changes shown here
    /// may not survive a restart until it can.
    #[serde(default)]
    pub store_problem: Option<String>,
}

/// Emitted on `workflow-trigger` whenever a trigger changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowTriggerEvent {
    pub trigger: WorkflowTriggerId,
    pub workflow: WorkflowId,
    pub state: TriggerState,
    pub queued: bool,
    pub listening: bool,
    pub last_fired_ms: Option<u64>,
    /// The run it just started, if any.
    pub run: Option<WorkflowRunId>,
}
