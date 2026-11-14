//! P1 project operations — the data side: graph records, saved workflows and
//! their revisions, workflow runs, research run records and outputs, agent
//! tasks, triggers, and Settings → Storage & History. The application's
//! orchestrator (`src-tauri/src/project_ops.rs`) calls these through the seam
//! `src-tauri/src/project_data.rs`; everything here goes through the owning
//! services and stores (no second graph engine, scheduler or undo system).
//!
//! Decisions (P1_PLAN 1, 3–5):
//! - Impacts are concrete consequences only (see [`ImpactKind`]); ordinary
//!   deletion of app records is app-managed and recoverable (listed in
//!   Storage & History, restorable with the same identity); permanent
//!   deletion and purge of retained content carry a `RecoveryLoss` impact.
//! - Active work is coordinated, never deleted under a worker: the
//!   `activeWork` choice `stop` stops it through its own cancellation and
//!   waits; the default `skip` leaves it running and the item is not deleted.
//! - An enabled trigger is turned off before what it watches or runs is
//!   deleted (`triggers`: `disable`, the default, or `skip`); nothing turns a
//!   trigger back on (a restored workflow's triggers stay off).
//! - Outputs of runs are a separate, explicit scope (`outputs`: `keep` the
//!   default, `delete` with the record, `only` without it); outputs something
//!   keeps (retained, attached) are never removed here.
//! - Agents keep their grants: data targets are refused for agent actors, and
//!   an agent cannot stop the user's work or turn off a trigger.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use exposition_core::ops::{
    ImpactKind, OpActor, OpChoice, OpChoiceOption, OpConsumer, OpEffect, OpError, OpImpact, OpKind, OpOutcome, OpPlan,
    OpRecovery, OpRefusal, OpRequest, OpRestore, OpStatus, OpTarget, StorageCategory, StorageItem, StorageOverview,
};
use exposition_core::path::WorkspacePath;
use exposition_graph::project_ops::{
    RECOVERABLE_ENTITY, RECOVERABLE_RELATION, RECOVERABLE_VIEW, RECOVERABLE_WORKFLOW, path_under, recoverable_id,
    split_recoverable,
};
use exposition_graph::{
    EntityId, GraphService, Origin, RelationId, ValidationContext, ViewId, WorkflowId, WorkflowRunId,
};

use crate::service::WorkflowRunService;
use crate::triggers::{TriggerService, TriggerState, WorkflowTriggerId};

/// Choice keys and values.
pub const CHOICE_ACTIVE_WORK: &str = "activeWork";
pub const CHOICE_TRIGGERS: &str = "triggers";
pub const CHOICE_OUTPUTS: &str = "outputs";
pub const CHOICE_PINNED: &str = "pinnedRevisions";

/// Recoverable kinds kept outside the graph.
pub const RECOVERABLE_WORKFLOW_RUN: &str = "workflowRun";
pub const RECOVERABLE_RESEARCH_RUN: &str = "researchRun";
pub const RECOVERABLE_AGENT_TASK: &str = "agentTask";
/// Storage item: unreferenced tombstones (metadata of removed files).
pub const TOMBSTONES: &str = "tombstones:unreferenced";

/// How long stopping active work may take before the item is not deleted.
const STOP_TIMEOUT: Duration = Duration::from_secs(15);

/// The data side's part of a plan (re-exported by the seam).
#[derive(Default)]
pub struct DataPlan {
    pub effects: Vec<OpEffect>,
    pub refusals: Vec<OpRefusal>,
    pub impacts: Vec<OpImpact>,
    pub choices: Vec<OpChoice>,
    /// Material the plan hash must cover (revisions, states, consumers) so a
    /// change between plan and apply is detected.
    pub fingerprint: Vec<String>,
}

// ------------------------------------------------------------ services ----

/// A research run record (interactive run or workflow batch).
pub use exposition_research::records::{OutputsRemoved, RunRecordInfo as ResearchRecord};

/// Research run records as the data side uses them.
pub trait ResearchRecords: Send + Sync {
    fn records(&self) -> Result<Vec<ResearchRecord>, OpError>;
    fn deleted(&self) -> Result<Vec<ResearchRecord>, OpError>;
    fn record(&self, id: &str) -> Result<ResearchRecord, OpError>;
    fn delete(&self, id: &str, permanent: bool) -> Result<(), OpError>;
    fn restore(&self, id: &str) -> Result<(), OpError>;
    fn remove_outputs(&self, id: &str, keep: &dyn Fn(&WorkspacePath) -> bool) -> Result<OutputsRemoved, OpError>;
    /// Cancel an active run and wait (bounded) until it ended.
    fn stop(&self, id: &str) -> bool;
}

/// An agent task record.
pub use exposition_agents::service::TaskRecord;

/// Agent task records as the data side uses them.
pub trait TaskRecords: Send + Sync {
    fn records(&self) -> Result<Vec<TaskRecord>, OpError>;
    fn deleted(&self) -> Result<Vec<TaskRecord>, OpError>;
    fn record(&self, id: &str) -> Result<TaskRecord, OpError>;
    /// Project files the task may write or touched.
    fn paths(&self, id: &str) -> Vec<WorkspacePath>;
    fn delete(&self, id: &str, permanent: bool) -> Result<(), OpError>;
    fn restore(&self, id: &str) -> Result<(), OpError>;
    fn stop(&self, id: &str) -> bool;
    /// Compact the task database; (before, after) on-disk sizes.
    fn compact(&self) -> Option<(Option<u64>, Option<u64>)>;
}

/// The research service, canceling through its async API on `runtime`.
pub struct ResearchServiceRecords {
    pub service: Arc<exposition_research::ResearchService>,
    pub runtime: tokio::runtime::Handle,
}

fn research_error(e: exposition_research::ResearchError) -> OpError {
    OpError::new(&e.code(), e.to_string())
}

impl ResearchRecords for ResearchServiceRecords {
    fn records(&self) -> Result<Vec<ResearchRecord>, OpError> {
        self.service.run_records().map_err(research_error)
    }
    fn deleted(&self) -> Result<Vec<ResearchRecord>, OpError> {
        self.service.deleted_run_records().map_err(research_error)
    }
    fn record(&self, id: &str) -> Result<ResearchRecord, OpError> {
        self.service.run_record(id).map_err(research_error)
    }
    fn delete(&self, id: &str, permanent: bool) -> Result<(), OpError> {
        self.service.delete_run_record(id, permanent).map_err(research_error)
    }
    fn restore(&self, id: &str) -> Result<(), OpError> {
        self.service.restore_run_record(id).map(|_| ()).map_err(research_error)
    }
    fn remove_outputs(&self, id: &str, keep: &dyn Fn(&WorkspacePath) -> bool) -> Result<OutputsRemoved, OpError> {
        self.service.remove_run_outputs(id, keep).map_err(research_error)
    }
    fn stop(&self, id: &str) -> bool {
        if !self.service.run_active(id) {
            return true;
        }
        let (service, runtime, run) = (Arc::clone(&self.service), self.runtime.clone(), id.to_owned());
        // Off any async context: the cancel waits for the kernel.
        let _ = std::thread::spawn(move || runtime.block_on(service.cancel_run(&run))).join();
        let deadline = std::time::Instant::now() + STOP_TIMEOUT;
        while std::time::Instant::now() < deadline {
            if !self.service.run_active(id) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        !self.service.run_active(id)
    }
}

fn agent_error(e: exposition_agents::AgentError) -> OpError {
    OpError::new(&e.code, e.message)
}

impl TaskRecords for exposition_agents::AgentService {
    fn records(&self) -> Result<Vec<TaskRecord>, OpError> {
        self.task_records().map_err(agent_error)
    }
    fn deleted(&self) -> Result<Vec<TaskRecord>, OpError> {
        self.deleted_task_records().map_err(agent_error)
    }
    fn record(&self, id: &str) -> Result<TaskRecord, OpError> {
        self.task_record(&exposition_agents::TaskId(id.to_owned())).map_err(agent_error)
    }
    fn paths(&self, id: &str) -> Vec<WorkspacePath> {
        self.task_paths(&exposition_agents::TaskId(id.to_owned())).unwrap_or_default()
    }
    fn delete(&self, id: &str, permanent: bool) -> Result<(), OpError> {
        self.delete_task(&exposition_agents::TaskId(id.to_owned()), permanent).map_err(agent_error)
    }
    fn restore(&self, id: &str) -> Result<(), OpError> {
        self.restore_task(&exposition_agents::TaskId(id.to_owned())).map(|_| ()).map_err(agent_error)
    }
    fn stop(&self, id: &str) -> bool {
        self.stop_and_wait(&exposition_agents::TaskId(id.to_owned()), STOP_TIMEOUT)
    }
    fn compact(&self) -> Option<(Option<u64>, Option<u64>)> {
        self.compact_tasks().ok()
    }
}

/// What the data side works with (the application builds it per call).
pub struct DataContext<'a> {
    pub graph: &'a GraphService,
    pub runs: &'a WorkflowRunService,
    pub triggers: Option<&'a TriggerService>,
    pub research: Option<&'a dyn ResearchRecords>,
    pub tasks: Option<&'a dyn TaskRecords>,
    /// Whether a project file exists (validating duplicated workflows).
    pub file_exists: Option<&'a dyn Fn(&WorkspacePath) -> bool>,
    /// The open project's root.
    pub root: Option<PathBuf>,
}

// ------------------------------------------------------------- helpers ----

fn origin_of(actor: &OpActor) -> Origin {
    match actor {
        OpActor::User => Origin::User,
        OpActor::Agent { task } => Origin::Agent { task_id: task.clone() },
    }
}

fn refusal(target: &OpTarget, code: &str, message: impl Into<String>) -> OpRefusal {
    OpRefusal { target: target.clone(), code: code.into(), message: message.into() }
}

fn refused(target: &OpTarget, e: OpError) -> OpRefusal {
    OpRefusal { target: target.clone(), code: e.code, message: e.message }
}

fn quoted(s: &str) -> String {
    format!("“{s}”")
}

fn option(value: &str, label: &str) -> OpChoiceOption {
    OpChoiceOption { value: value.into(), label: label.into() }
}

fn active_work_choice() -> OpChoice {
    OpChoice {
        key: CHOICE_ACTIVE_WORK.into(),
        label: "Work that is running now".into(),
        options: vec![option("skip", "Leave it running and keep these items"), option("stop", "Stop it, then delete")],
        default: "skip".into(),
    }
}

fn triggers_choice() -> OpChoice {
    OpChoice {
        key: CHOICE_TRIGGERS.into(),
        label: "Enabled triggers".into(),
        options: vec![
            option("disable", "Turn the triggers off, then delete"),
            option("skip", "Keep the triggers and these items"),
        ],
        default: "disable".into(),
    }
}

fn outputs_choice() -> OpChoice {
    OpChoice {
        key: CHOICE_OUTPUTS.into(),
        label: "Generated outputs".into(),
        options: vec![
            option("keep", "Keep the outputs (delete the record only)"),
            option("delete", "Delete the record and the outputs nothing keeps"),
            option("only", "Delete only the outputs nothing keeps (keep the record)"),
        ],
        default: "keep".into(),
    }
}

fn pinned_choice() -> OpChoice {
    OpChoice {
        key: CHOICE_PINNED.into(),
        label: "Revisions other workflows run".into(),
        options: vec![
            option("keep", "Keep the revisions other workflows run"),
            option("purge", "Delete them too (those workflows show the step missing and cannot run)"),
        ],
        default: "keep".into(),
    }
}

fn push_choice(choices: &mut Vec<OpChoice>, choice: OpChoice) {
    if !choices.iter().any(|c| c.key == choice.key) {
        choices.push(choice);
    }
}

/// The value of a choice: the user's pick when it is one of the plan's
/// options, else the plan's default (never a destructive one), else
/// `fallback`.
fn choice(plan: &OpPlan, choices: &BTreeMap<String, String>, key: &str, fallback: &str) -> String {
    let offered = plan.choices.iter().find(|c| c.key == key);
    match (choices.get(key), offered) {
        (Some(v), Some(c)) if c.options.iter().any(|o| &o.value == v) => v.clone(),
        (_, Some(c)) => c.default.clone(),
        (_, None) => fallback.to_owned(),
    }
}

fn under_any(path: &WorkspacePath, targets: &[WorkspacePath]) -> Option<usize> {
    targets.iter().position(|t| path_under(path, t))
}

/// `a` lies under `b` or `b` under `a` (a task scoped to a folder that
/// contains the target, or a target folder containing the task's file).
fn overlaps(a: &WorkspacePath, b: &WorkspacePath) -> bool {
    path_under(a, b) || path_under(b, a)
}

fn consumer(kind: &str, id: &str, label: &str) -> OpConsumer {
    OpConsumer { kind: kind.into(), id: id.into(), label: label.into(), path: None }
}

fn fingerprint_of(impacts: &[OpImpact]) -> Vec<String> {
    impacts
        .iter()
        .map(|i| {
            let mut ids: Vec<&str> = i.consumers.iter().map(|c| c.id.as_str()).collect();
            ids.sort();
            format!("{:?}|{}|{}", i.kind, serde_json::to_string(&i.target).unwrap_or_default(), ids.join(","))
        })
        .collect()
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["bytes", "KiB", "MiB", "GiB"];
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{n} bytes") } else { format!("{v:.1} {}", UNITS[unit]) }
}

/// Honest wording of what compaction returned to the file system.
fn reclaimed(before: Option<u64>, after: Option<u64>) -> String {
    match (before, after) {
        (Some(b), Some(a)) if b > a => format!("The history database gave back {} of disk space.", human_bytes(b - a)),
        (Some(_), Some(_)) => {
            "The record was removed; the database did not shrink now (its free space is reused by later records)."
                .into()
        }
        _ => "The record was removed.".into(),
    }
}

fn outcome(target: &OpTarget, status: OpStatus) -> OpOutcome {
    OpOutcome {
        target: target.clone(),
        status,
        code: None,
        message: None,
        created: None,
        recovery: OpRecovery::None,
        restore: None,
    }
}

fn failed(target: &OpTarget, e: OpError) -> OpOutcome {
    OpOutcome { code: Some(e.code), message: Some(e.message), ..outcome(target, OpStatus::Failed) }
}

fn skipped(target: &OpTarget, code: &str, message: impl Into<String>) -> OpOutcome {
    OpOutcome { code: Some(code.into()), message: Some(message.into()), ..outcome(target, OpStatus::Skipped) }
}

fn is_data_target(t: &OpTarget) -> bool {
    !matches!(t, OpTarget::Path { .. } | OpTarget::External { .. })
}

fn recovery_loss(target: &OpTarget, what: &str) -> OpImpact {
    OpImpact {
        kind: ImpactKind::RecoveryLoss,
        target: target.clone(),
        consumers: Vec::new(),
        message: format!("{what} is deleted permanently: it cannot be restored afterwards."),
    }
}

fn workflow_run_id(raw: &str) -> Result<WorkflowRunId, OpError> {
    WorkflowRunId::parse(raw).map_err(|_| OpError::new("ops.notFound", "That workflow run does not exist."))
}

fn workflow_id(raw: &str) -> Result<WorkflowId, OpError> {
    WorkflowId::parse(raw).map_err(|_| OpError::new("ops.notFound", "That workflow does not exist."))
}

fn run_error(e: crate::model::WorkflowRunError) -> OpError {
    OpError::new(&e.code, e.message)
}

// ------------------------------------------------------- path impacts ----

/// Concrete consequences of deleting these project paths for data
/// consumers (manuscripts, workflows, retained evidence, triggers, active
/// work). Copies have none here.
pub fn impacts_for_paths(cx: &DataContext<'_>, paths: &[WorkspacePath], kind: &OpKind) -> DataPlan {
    let mut plan = DataPlan::default();
    if !matches!(kind, OpKind::Delete { .. }) || paths.is_empty() {
        return plan;
    }
    match cx.graph.path_impacts(paths) {
        Ok(impacts) => plan.impacts.extend(impacts),
        // An unavailable graph cannot report consumers; editing files keeps
        // working, and the fingerprint records that nothing was known.
        Err(e) => plan.fingerprint.push(format!("graph:{}", e.code)),
    }
    // Active work using the paths.
    let mut active: BTreeMap<usize, Vec<OpConsumer>> = BTreeMap::new();
    for run in cx.runs.active_run_inputs() {
        for input in &run.inputs {
            if let Some(t) = under_any(input, paths) {
                active.entry(t).or_default().push(consumer("run", run.run.as_str(), &run.title));
            }
        }
    }
    if let Some(research) = cx.research
        && let Ok(records) = research.records()
    {
        for r in records.iter().filter(|r| r.active) {
            for input in &r.inputs {
                if let Some(t) = under_any(input, paths) {
                    active.entry(t).or_default().push(consumer("run", &r.id, &r.label));
                }
            }
        }
    }
    if let Some(tasks) = cx.tasks
        && let Ok(records) = tasks.records()
    {
        for task in records.iter().filter(|t| t.active) {
            for p in tasks.paths(task.id.0.as_str()) {
                for (t, target) in paths.iter().enumerate() {
                    if overlaps(&p, target) {
                        active.entry(t).or_default().push(consumer("task", &task.id.0, &task.title));
                    }
                }
            }
        }
    }
    for (t, mut consumers) in active {
        consumers.sort_by(|a, b| a.id.cmp(&b.id));
        consumers.dedup_by(|a, b| a.id == b.id);
        let names: Vec<String> = consumers.iter().take(3).map(|c| quoted(&c.label)).collect();
        plan.impacts.push(OpImpact {
            kind: ImpactKind::ActiveWork,
            target: OpTarget::Path { path: paths[t].clone() },
            message: format!(
                "{} is running and uses {}. Stop it before deleting, or keep the file.",
                names.join(", "),
                quoted(paths[t].as_str())
            ),
            consumers,
        });
        push_choice(&mut plan.choices, active_work_choice());
    }
    // Enabled triggers watching the paths.
    if let Some(triggers) = cx.triggers {
        let mut watching: BTreeMap<usize, Vec<OpConsumer>> = BTreeMap::new();
        for trigger in triggers.watching(paths) {
            for w in &trigger.watched {
                if let Some(t) = under_any(&w.path, paths) {
                    watching.entry(t).or_default().push(OpConsumer {
                        kind: "trigger".into(),
                        id: trigger.id.to_string(),
                        label: trigger.title.clone(),
                        path: Some(w.path.clone()),
                    });
                }
            }
        }
        for (t, mut consumers) in watching {
            consumers.dedup_by(|a, b| a.id == b.id);
            let names: Vec<String> = consumers.iter().take(3).map(|c| quoted(&c.label)).collect();
            plan.impacts.push(OpImpact {
                kind: ImpactKind::EnabledTrigger,
                target: OpTarget::Path { path: paths[t].clone() },
                message: format!(
                    "The trigger of {} watches {}; deleting it would make the trigger run or stop. Turn the trigger off first.",
                    names.join(", "),
                    quoted(paths[t].as_str())
                ),
                consumers,
            });
            push_choice(&mut plan.choices, triggers_choice());
        }
    }
    plan.fingerprint.extend(fingerprint_of(&plan.impacts));
    plan
}

/// Before file work: stop the work and turn off the triggers the user chose
/// to (coordinated, so no worker writes into a deleted target). Paths whose
/// work could not be stopped are refused (not deleted).
pub fn prepare_paths(cx: &DataContext<'_>, plan: &OpPlan, choices: &BTreeMap<String, String>) -> Vec<OpRefusal> {
    let agent = matches!(plan.request.actor, OpActor::Agent { .. });
    let stop = choice(plan, choices, CHOICE_ACTIVE_WORK, "skip") == "stop";
    let disable = choice(plan, choices, CHOICE_TRIGGERS, "disable") == "disable";
    let mut refusals: Vec<OpRefusal> = Vec::new();
    let mut refuse = |target: &OpTarget, code: &str, message: String| {
        if !refusals.iter().any(|r| &r.target == target) {
            refusals.push(refusal(target, code, message));
        }
    };
    for impact in &plan.impacts {
        if !matches!(impact.target, OpTarget::Path { .. }) {
            continue;
        }
        match impact.kind {
            ImpactKind::ActiveWork if agent => refuse(
                &impact.target,
                "ops.agentScope",
                "Work is running on this item; an agent cannot stop it. Ask the user.".into(),
            ),
            ImpactKind::ActiveWork if !stop => refuse(
                &impact.target,
                "ops.activeWork",
                format!("{} It was left running, so this item was kept.", impact.message),
            ),
            ImpactKind::ActiveWork => {
                for c in &impact.consumers {
                    if !stop_consumer(cx, c) {
                        refuse(
                            &impact.target,
                            "ops.activeWork",
                            format!("{} could not be stopped, so this item was kept.", quoted(&c.label)),
                        );
                    }
                }
            }
            ImpactKind::EnabledTrigger if agent => refuse(
                &impact.target,
                "ops.agentScope",
                "An enabled trigger watches this item; only the user can turn triggers off.".into(),
            ),
            ImpactKind::EnabledTrigger if !disable => refuse(
                &impact.target,
                "ops.triggerEnabled",
                format!("{} The trigger was kept, so this item was kept.", impact.message),
            ),
            ImpactKind::EnabledTrigger => {
                for c in &impact.consumers {
                    let ok = cx
                        .triggers
                        .is_some_and(|t| WorkflowTriggerId::parse(&c.id).is_ok_and(|id| t.disable(&id).is_ok()));
                    if !ok {
                        refuse(
                            &impact.target,
                            "ops.triggerEnabled",
                            format!(
                                "The trigger of {} could not be turned off, so this item was kept.",
                                quoted(&c.label)
                            ),
                        );
                    }
                }
            }
            _ => {}
        }
    }
    refusals
}

fn stop_consumer(cx: &DataContext<'_>, c: &OpConsumer) -> bool {
    match c.kind.as_str() {
        "run" if c.id.starts_with("wrun-") => {
            WorkflowRunId::parse(&c.id).is_ok_and(|id| cx.runs.stop_and_wait(&id, STOP_TIMEOUT))
        }
        "run" => cx.research.is_some_and(|r| r.stop(&c.id)),
        "task" => cx.tasks.is_some_and(|t| t.stop(&c.id)),
        _ => false,
    }
}

/// After file work: reconcile project data with the ACTUAL outcomes.
/// Deleted paths are indexed at once (tombstones; surviving consumers then
/// show them missing); permanently deleted ones also drop metadata nothing
/// refers to. Copies are indexed as fresh identities with copied-from
/// provenance. Restored paths are indexed (an unrelated file at an old path
/// gets a new identity; evidence is re-evaluated by content).
pub fn after_paths(cx: &DataContext<'_>, outcomes: &[OpOutcome]) {
    let mut deleted: Vec<WorkspacePath> = Vec::new();
    let mut permanent: Vec<WorkspacePath> = Vec::new();
    let mut copies: Vec<(WorkspacePath, WorkspacePath)> = Vec::new();
    let mut touched: Vec<WorkspacePath> = Vec::new();
    for o in outcomes.iter().filter(|o| o.status == OpStatus::Succeeded) {
        match (&o.target, &o.created) {
            (OpTarget::Path { path }, None) => {
                if !exists(cx, path) {
                    deleted.push(path.clone());
                    if o.recovery == OpRecovery::None {
                        permanent.push(path.clone());
                    }
                } else {
                    touched.push(path.clone());
                }
            }
            (OpTarget::Path { path: source }, Some(OpTarget::Path { path: created })) => {
                if exists(cx, source) && source != created {
                    copies.push((source.clone(), created.clone()));
                } else {
                    // A restore (the source is the deleted original).
                    touched.push(created.clone());
                    if source != created {
                        touched.push(source.clone());
                    }
                }
            }
            (OpTarget::External { .. }, Some(OpTarget::Path { path: created })) => touched.push(created.clone()),
            _ => {}
        }
    }
    let mut reindex = deleted.clone();
    reindex.extend(touched);
    if !reindex.is_empty() {
        let _ = cx.graph.reconcile(&exposition_graph::reconcile::ReconcileOptions {
            scope: exposition_graph::reconcile::Scope::Paths(reindex),
            force: false,
        });
    }
    if !permanent.is_empty() {
        let _ = cx.graph.cleanup_tombstones(Some(&permanent));
    }
    if !copies.is_empty() {
        let _ = cx.graph.record_copies(&copies);
    }
}

fn exists(cx: &DataContext<'_>, path: &WorkspacePath) -> bool {
    let Some(root) = &cx.root else { return false };
    let abs = path.components().fold(root.clone(), |p, c| p.join(c));
    std::fs::symlink_metadata(abs).is_ok()
}

// ------------------------------------------------------------ planning ----

/// Plan the non-path targets of `request`.
pub fn plan_data(cx: &DataContext<'_>, request: &OpRequest) -> DataPlan {
    let mut plan = DataPlan::default();
    let agent = matches!(request.actor, OpActor::Agent { .. });
    for target in request.targets.iter().filter(|t| is_data_target(t)) {
        if agent {
            plan.refusals.push(refusal(
                target,
                "ops.agentScope",
                "Agents cannot delete or copy this kind of item; ask the user.",
            ));
            continue;
        }
        let result = match &request.kind {
            OpKind::Delete { permanent } => plan_delete(cx, target, *permanent, &mut plan),
            OpKind::Duplicate | OpKind::Paste { destination: exposition_core::ops::OpDestination::Project } => {
                plan_copy(cx, target, &mut plan)
            }
            OpKind::Paste { .. } => {
                Err(OpError::new("ops.notSupported", "This item is pasted into the project, not into a folder."))
            }
        };
        if let Err(e) = result {
            plan.refusals.push(refused(target, e));
        }
    }
    plan
}

fn effect(target: &OpTarget, description: String, recovery: OpRecovery) -> OpEffect {
    OpEffect { target: target.clone(), description, destination: None, recovery }
}

fn plan_delete(cx: &DataContext<'_>, target: &OpTarget, permanent: bool, plan: &mut DataPlan) -> Result<(), OpError> {
    let recovery = if permanent { OpRecovery::None } else { OpRecovery::AppManaged };
    let restorable = "restorable in Settings → Storage & History";
    match target {
        OpTarget::Workflow { workflow } => {
            let id = workflow_id(workflow)?;
            let check = cx.graph.check_workflow_delete(&id)?;
            if check.retired && !permanent {
                return Err(OpError::new(
                    "ops.alreadyDeleted",
                    "That workflow is already deleted; restore it or delete it permanently in Storage & History.",
                ));
            }
            let mut fp = format!("wf:{id}:{}:{}:{}", check.latest, check.revisions.len(), check.retired);
            if cx.runs.workflow_active(&id) {
                let runs: Vec<OpConsumer> = cx
                    .runs
                    .active_run_inputs()
                    .into_iter()
                    .filter(|r| cx.runs.run_any(&r.run).is_ok_and(|(run, _)| run.workflow == id))
                    .map(|r| consumer("run", r.run.as_str(), &r.title))
                    .collect();
                fp.push_str(&format!(":active{}", runs.len()));
                plan.impacts.push(OpImpact {
                    kind: ImpactKind::ActiveWork,
                    target: target.clone(),
                    consumers: runs,
                    message: format!(
                        "A run of {} is active. Stop it before deleting the workflow.",
                        quoted(&check.title)
                    ),
                });
                push_choice(&mut plan.choices, active_work_choice());
            }
            let triggers: Vec<OpConsumer> = cx
                .triggers
                .and_then(|t| t.list(Some(&id)).ok())
                .unwrap_or_default()
                .into_iter()
                .filter(|t| t.state == TriggerState::Active)
                .map(|t| consumer("trigger", t.id.as_str(), &t.title))
                .collect();
            if !triggers.is_empty() {
                fp.push_str(&format!(":triggers{}", triggers.len()));
                plan.impacts.push(OpImpact {
                    kind: ImpactKind::EnabledTrigger,
                    target: target.clone(),
                    consumers: triggers,
                    message: format!(
                        "{} has an enabled trigger that starts runs unattended; it is turned off with the workflow (a restored workflow keeps it off).",
                        quoted(&check.title)
                    ),
                });
                push_choice(&mut plan.choices, triggers_choice());
            }
            let description = if permanent {
                plan.impacts
                    .push(recovery_loss(target, &format!("The workflow {} and its revisions", quoted(&check.title))));
                if !check.pins.is_empty() {
                    let mut consumers: Vec<OpConsumer> = check
                        .pins
                        .iter()
                        .map(|p| OpConsumer {
                            kind: "workflowRevision".into(),
                            id: format!("{}@{}", p.by, p.by_revision),
                            label: format!("{} (revision {})", p.by_title, p.by_revision),
                            path: None,
                        })
                        .collect();
                    consumers.dedup();
                    fp.push_str(&format!(":pins{}", consumers.len()));
                    let revisions: BTreeSet<u64> = check.pins.iter().map(|p| p.revision).collect();
                    plan.impacts.push(OpImpact {
                        kind: ImpactKind::PinnedRevision,
                        target: target.clone(),
                        consumers,
                        message: format!(
                            "Other workflows run revision {} of {}. Those revisions are kept unless you choose to delete them too; then those workflows show the step missing and cannot run.",
                            revisions.iter().map(u64::to_string).collect::<Vec<_>>().join(", "),
                            quoted(&check.title)
                        ),
                    });
                    push_choice(&mut plan.choices, pinned_choice());
                }
                format!(
                    "Delete the workflow {} and its revisions permanently; its triggers are removed. Run history stays.",
                    quoted(&check.title)
                )
            } else {
                format!(
                    "Delete the workflow {} ({restorable}); its triggers are turned off. Run history stays.",
                    quoted(&check.title)
                )
            };
            plan.effects.push(effect(target, description, recovery));
            plan.fingerprint.push(fp);
        }
        OpTarget::WorkflowRevision { workflow, revision } => {
            if !permanent {
                return Err(OpError::new(
                    "ops.notSupported",
                    "A single revision can only be deleted permanently (or delete the whole workflow).",
                ));
            }
            let id = workflow_id(workflow)?;
            let pins = cx.graph.check_revision_purge(&id, *revision)?;
            let check = cx.graph.check_workflow_delete(&id)?;
            plan.impacts.push(recovery_loss(target, &format!("Revision {revision} of {}", quoted(&check.title))));
            if !pins.is_empty() {
                plan.impacts.push(OpImpact {
                    kind: ImpactKind::PinnedRevision,
                    target: target.clone(),
                    consumers: pins
                        .iter()
                        .map(|p| OpConsumer {
                            kind: "workflowRevision".into(),
                            id: format!("{}@{}", p.by, p.by_revision),
                            label: format!("{} (revision {})", p.by_title, p.by_revision),
                            path: None,
                        })
                        .collect(),
                    message: format!(
                        "Other workflows run revision {revision} of {}; after deleting it they show the step missing and cannot run until rebound.",
                        quoted(&check.title)
                    ),
                });
            }
            plan.effects.push(effect(
                target,
                format!("Delete revision {revision} of {} permanently", quoted(&check.title)),
                OpRecovery::None,
            ));
            plan.fingerprint.push(format!("rev:{id}:{revision}:{}:{}", check.latest, pins.len()));
        }
        OpTarget::WorkflowRun { run } => {
            let id = workflow_run_id(run)?;
            let (record, deleted) = cx.runs.run_any(&id).map_err(run_error)?;
            if deleted.is_some() && !permanent {
                return Err(OpError::new("ops.alreadyDeleted", "That run is already deleted (see Storage & History)."));
            }
            let active = cx.runs.is_active(&id);
            if active {
                plan.impacts.push(OpImpact {
                    kind: ImpactKind::ActiveWork,
                    target: target.clone(),
                    consumers: vec![consumer("run", run, &record.title)],
                    message: format!(
                        "The run of {} is active; it is stopped before its record is deleted.",
                        quoted(&record.title)
                    ),
                });
                push_choice(&mut plan.choices, active_work_choice());
            }
            let description = if permanent {
                plan.impacts.push(recovery_loss(target, &format!("The run record of {}", quoted(&record.title))));
                push_choice(&mut plan.choices, outputs_choice());
                format!(
                    "Delete the run record of {} permanently (outputs: as chosen; outputs something keeps are never removed)",
                    quoted(&record.title)
                )
            } else {
                format!("Delete the run record of {} ({restorable}); its outputs stay", quoted(&record.title))
            };
            plan.effects.push(effect(target, description, recovery));
            plan.fingerprint.push(format!("wrun:{run}:{:?}:{active}:{}", record.state, deleted.is_some()));
        }
        OpTarget::ResearchRun { run } => {
            let research = cx.research.ok_or_else(|| OpError::new("ops.notSupported", "Research is not available."))?;
            let record = research.record(run)?;
            if record.deleted_at_ms.is_some() && !permanent {
                return Err(OpError::new("ops.alreadyDeleted", "That run is already deleted (see Storage & History)."));
            }
            if record.active {
                plan.impacts.push(OpImpact {
                    kind: ImpactKind::ActiveWork,
                    target: target.clone(),
                    consumers: vec![consumer("run", run, &record.label)],
                    message: format!(
                        "The run {} is active; it is canceled before its record is deleted.",
                        quoted(&record.label)
                    ),
                });
                push_choice(&mut plan.choices, active_work_choice());
            }
            let description = if permanent {
                plan.impacts.push(recovery_loss(target, &format!("The run record {}", quoted(&record.label))));
                let evidence = cx.graph.evidence_for_operation("researchRun", run).unwrap_or_default();
                let users: Vec<OpConsumer> = evidence
                    .iter()
                    .filter_map(|(e, subject)| {
                        subject.as_ref().map(|s| OpConsumer {
                            kind: "evidence".into(),
                            id: e.to_string(),
                            label: format!("Evidence for {}", quoted(&s.label)),
                            path: s.key.as_deref().and_then(|k| WorkspacePath::parse(k).ok()),
                        })
                    })
                    .collect();
                if !users.is_empty() {
                    plan.impacts.push(OpImpact {
                        kind: ImpactKind::RetainedEvidence,
                        target: target.clone(),
                        consumers: users,
                        message: "Retained evidence was recorded from this run; without its record (code, transcript) the evidence cannot be reproduced. The evidence and its recorded results stay as history.".into(),
                    });
                }
                if !record.outputs.is_empty() {
                    push_choice(&mut plan.choices, outputs_choice());
                }
                format!(
                    "Delete the run record {} permanently (outputs: as chosen; outputs something keeps are never removed)",
                    quoted(&record.label)
                )
            } else {
                format!("Delete the run record {} ({restorable}); its outputs stay", quoted(&record.label))
            };
            plan.effects.push(effect(target, description, recovery));
            plan.fingerprint.push(format!(
                "rrun:{run}:{}:{}:{}",
                record.active,
                record.deleted_at_ms.is_some(),
                record.outputs.len()
            ));
        }
        OpTarget::AgentTask { task } => {
            let tasks = cx.tasks.ok_or_else(|| OpError::new("ops.notSupported", "Agent tasks are not available."))?;
            let record = tasks.record(task)?;
            if record.deleted_at_ms.is_some() && !permanent {
                return Err(OpError::new(
                    "ops.alreadyDeleted",
                    "That task is already deleted (see Storage & History).",
                ));
            }
            if record.active {
                plan.impacts.push(OpImpact {
                    kind: ImpactKind::ActiveWork,
                    target: target.clone(),
                    consumers: vec![consumer("task", task, &record.title)],
                    message: format!(
                        "The agent task {} is running; it is stopped before its record is deleted.",
                        quoted(&record.title)
                    ),
                });
                push_choice(&mut plan.choices, active_work_choice());
            }
            let description = if permanent {
                let mut loss = recovery_loss(
                    target,
                    &format!("The agent task {} (transcript and change records)", quoted(&record.title)),
                );
                if record.pending_review > 0 {
                    loss.message.push_str(&format!(
                        " {} file(s) still await review; their changes can no longer be reverted from the task.",
                        record.pending_review
                    ));
                }
                plan.impacts.push(loss);
                format!("Delete the agent task {} permanently", quoted(&record.title))
            } else {
                format!("Delete the agent task {} ({restorable})", quoted(&record.title))
            };
            plan.effects.push(effect(target, description, recovery));
            plan.fingerprint.push(format!("task:{task}:{}:{}", record.active, record.deleted_at_ms.is_some()));
        }
        OpTarget::GraphEntity { entity } => {
            let id = EntityId::parse(entity).map_err(|_| OpError::new("ops.notFound", "That item does not exist."))?;
            let check = cx.graph.check_entity_delete(&id)?;
            plan.impacts.extend(check.impacts.iter().cloned());
            if permanent {
                plan.impacts.push(recovery_loss(target, &quoted(&check.entity.label)));
            }
            plan.effects.push(effect(
                target,
                format!(
                    "Delete {} and its {} relationship(s){}",
                    quoted(&check.entity.label),
                    check.relations,
                    if permanent { " permanently".to_owned() } else { format!(" ({restorable})") }
                ),
                recovery,
            ));
            plan.fingerprint.push(format!("ent:{id}:{}:{}", check.entity.revision, check.relations));
        }
        OpTarget::GraphRelation { relation } => {
            let id = RelationId::parse(relation)
                .map_err(|_| OpError::new("ops.notFound", "That relationship does not exist."))?;
            let check = cx.graph.check_relation_delete(&id)?;
            plan.impacts.extend(check.impacts.iter().cloned());
            if permanent {
                plan.impacts.push(recovery_loss(target, &format!("The relationship “{}”", check.description)));
            }
            plan.effects.push(effect(
                target,
                format!(
                    "Delete the relationship “{}”{}; reindexing does not bring it back",
                    check.description,
                    if permanent { " permanently".to_owned() } else { format!(" ({restorable})") }
                ),
                recovery,
            ));
            plan.fingerprint.push(format!("rel:{id}:{}", check.relation.revision));
        }
        OpTarget::GraphView { view } => {
            let id =
                ViewId::parse(view).map_err(|_| OpError::new("ops.notFound", "That saved view does not exist."))?;
            let saved = cx.graph.view(&id).map_err(|e| OpError::new("ops.notFound", e.message))?;
            if permanent {
                plan.impacts.push(recovery_loss(target, &format!("The saved view {}", quoted(&saved.name))));
            }
            plan.effects.push(effect(
                target,
                format!(
                    "Delete the saved view {} (layout only; nothing in the graph changes){}",
                    quoted(&saved.name),
                    if permanent { "" } else { "; restorable" }
                ),
                recovery,
            ));
            plan.fingerprint.push(format!("view:{id}:{}", saved.revision));
        }
        OpTarget::Recoverable { id } => {
            if !permanent {
                return Err(OpError::new(
                    "ops.alreadyDeleted",
                    "This item is already deleted; restore it, or delete it permanently.",
                ));
            }
            plan_purge(cx, target, id, plan)?;
        }
        OpTarget::Path { .. } | OpTarget::External { .. } => {}
    }
    Ok(())
}

fn plan_purge(cx: &DataContext<'_>, target: &OpTarget, id: &str, plan: &mut DataPlan) -> Result<(), OpError> {
    if id == TOMBSTONES {
        let n = cx.graph.unreferenced_tombstones(None)?.len();
        plan.effects.push(effect(
            target,
            format!("Remove the records of {n} deleted file(s) and declarations that nothing refers to any more"),
            OpRecovery::None,
        ));
        plan.fingerprint.push(format!("tomb:{n}"));
        return Ok(());
    }
    let (kind, record) =
        split_recoverable(id).ok_or_else(|| OpError::new("ops.notFound", "That deleted item no longer exists."))?;
    match kind {
        RECOVERABLE_ENTITY | RECOVERABLE_RELATION | RECOVERABLE_VIEW | RECOVERABLE_WORKFLOW => {
            let found = cx.graph.recoverables()?.into_iter().find(|r| r.id == id);
            let r = found.ok_or_else(|| OpError::new("ops.notFound", "That deleted item no longer exists."))?;
            plan.impacts.push(recovery_loss(target, &quoted(&r.label)));
            if kind == RECOVERABLE_WORKFLOW {
                let wf = workflow_id(record)?;
                let pins = cx.graph.workflow_pins(&wf)?;
                if !pins.is_empty() {
                    plan.impacts.push(OpImpact {
                        kind: ImpactKind::PinnedRevision,
                        target: target.clone(),
                        consumers: pins
                            .iter()
                            .map(|p| OpConsumer {
                                kind: "workflowRevision".into(),
                                id: format!("{}@{}", p.by, p.by_revision),
                                label: format!("{} (revision {})", p.by_title, p.by_revision),
                                path: None,
                            })
                            .collect(),
                        message: format!(
                            "Other workflows run revisions of {}; they are kept unless you choose to delete them too.",
                            quoted(&r.label)
                        ),
                    });
                    push_choice(&mut plan.choices, pinned_choice());
                }
                plan.fingerprint.push(format!("pins:{}", pins.len()));
            }
            plan.effects.push(effect(target, format!("Purge {} for good", quoted(&r.label)), OpRecovery::None));
            plan.fingerprint.push(format!("rec:{id}:{}", r.deleted_at_ms));
        }
        RECOVERABLE_WORKFLOW_RUN => {
            let run = workflow_run_id(record)?;
            let (record, deleted) = cx.runs.run_any(&run).map_err(run_error)?;
            if deleted.is_none() {
                return Err(OpError::new("ops.notFound", "That run is not in recoverable deletions."));
            }
            plan.impacts.push(recovery_loss(target, &format!("The run record of {}", quoted(&record.title))));
            push_choice(&mut plan.choices, outputs_choice());
            plan.effects.push(effect(
                target,
                format!("Purge the run record of {}", quoted(&record.title)),
                OpRecovery::None,
            ));
            plan.fingerprint.push(format!("rec:{id}"));
        }
        RECOVERABLE_RESEARCH_RUN => {
            let research = cx.research.ok_or_else(|| OpError::new("ops.notSupported", "Research is not available."))?;
            let r = research.record(record)?;
            if r.deleted_at_ms.is_none() {
                return Err(OpError::new("ops.notFound", "That run is not in recoverable deletions."));
            }
            plan.impacts.push(recovery_loss(target, &format!("The run record {}", quoted(&r.label))));
            if !r.outputs.is_empty() {
                push_choice(&mut plan.choices, outputs_choice());
            }
            plan.effects.push(effect(target, format!("Purge the run record {}", quoted(&r.label)), OpRecovery::None));
            plan.fingerprint.push(format!("rec:{id}"));
        }
        RECOVERABLE_AGENT_TASK => {
            let tasks = cx.tasks.ok_or_else(|| OpError::new("ops.notSupported", "Agent tasks are not available."))?;
            let r = tasks.record(record)?;
            if r.deleted_at_ms.is_none() {
                return Err(OpError::new("ops.notFound", "That task is not in recoverable deletions."));
            }
            plan.impacts.push(recovery_loss(target, &format!("The agent task {}", quoted(&r.title))));
            plan.effects.push(effect(target, format!("Purge the agent task {}", quoted(&r.title)), OpRecovery::None));
            plan.fingerprint.push(format!("rec:{id}"));
        }
        _ => return Err(OpError::new("ops.notFound", "That deleted item no longer exists.")),
    }
    Ok(())
}

fn plan_copy(cx: &DataContext<'_>, target: &OpTarget, plan: &mut DataPlan) -> Result<(), OpError> {
    match target {
        OpTarget::Workflow { workflow } => {
            let id = workflow_id(workflow)?;
            let check = cx.graph.check_workflow_delete(&id)?;
            if check.retired {
                return Err(OpError::new("ops.notFound", "That workflow was deleted; restore it first."));
            }
            plan.effects.push(effect(
                target,
                format!(
                    "Copy as a new workflow “{}” (revision 1; no triggers, runs or approvals; nothing runs)",
                    exposition_graph::project_ops::copy_label(&check.title, 200)
                ),
                OpRecovery::None,
            ));
            plan.fingerprint.push(format!("dupwf:{id}:{}", check.latest));
        }
        OpTarget::WorkflowRevision { workflow, revision } => {
            let id = workflow_id(workflow)?;
            if cx.graph.workflow_definition(&id, *revision).is_none() || cx.graph.workflow_retired(&id) {
                return Err(OpError::new("ops.notFound", "That workflow revision no longer exists."));
            }
            plan.effects.push(effect(
                target,
                format!("Copy revision {revision} as a new workflow (revision 1; no triggers, runs or approvals)"),
                OpRecovery::None,
            ));
            plan.fingerprint.push(format!("duprev:{id}:{revision}"));
        }
        OpTarget::GraphEntity { entity } => {
            let id = EntityId::parse(entity).map_err(|_| OpError::new("ops.notFound", "That item does not exist."))?;
            let e = cx.graph.check_entity_duplicate(&id)?;
            plan.effects.push(effect(
                target,
                format!(
                    "Copy {} as a new item (its relationships within the selection are copied; nothing uses the copy; no evidence or verification is copied)",
                    quoted(&e.label)
                ),
                OpRecovery::None,
            ));
            plan.fingerprint.push(format!("dupent:{id}:{}", e.revision));
        }
        OpTarget::GraphView { view } => {
            let id =
                ViewId::parse(view).map_err(|_| OpError::new("ops.notFound", "That saved view does not exist."))?;
            let saved = cx.graph.view(&id).map_err(|e| OpError::new("ops.notFound", e.message))?;
            plan.effects.push(effect(target, format!("Copy the saved view {}", quoted(&saved.name)), OpRecovery::None));
            plan.fingerprint.push(format!("dupview:{id}:{}", saved.revision));
        }
        OpTarget::WorkflowRun { .. } | OpTarget::ResearchRun { .. } | OpTarget::AgentTask { .. } => {
            return Err(OpError::new(
                "ops.notSupported",
                "A finished run or task is history: copy its details or start a new one (it is never duplicated as a new result).",
            ));
        }
        OpTarget::GraphRelation { .. } => {
            return Err(OpError::new(
                "ops.notSupported",
                "A relationship cannot be duplicated; copy its items instead.",
            ));
        }
        OpTarget::Recoverable { .. } => {
            return Err(OpError::new("ops.notSupported", "Restore a deleted item before copying it."));
        }
        OpTarget::Path { .. } | OpTarget::External { .. } => {}
    }
    Ok(())
}

// -------------------------------------------------------------- applying ----

/// Apply the non-path part of an approved plan. One outcome per non-path
/// target, in request order (targets refused at plan time are Skipped).
pub fn apply_data(cx: &DataContext<'_>, plan: &OpPlan, choices: &BTreeMap<String, String>) -> Vec<OpOutcome> {
    let origin = origin_of(&plan.request.actor);
    let mut outcomes = Vec::new();
    let mut entity_copies: Vec<(OpTarget, EntityId)> = Vec::new();
    for target in plan.request.targets.iter().filter(|t| is_data_target(t)) {
        if let Some(r) = plan.refusals.iter().find(|r| &r.target == target) {
            outcomes.push(skipped(target, &r.code, r.message.clone()));
            continue;
        }
        if matches!(plan.request.actor, OpActor::Agent { .. }) {
            outcomes.push(skipped(target, "ops.agentScope", "Agents cannot delete or copy this kind of item."));
            continue;
        }
        let result = match &plan.request.kind {
            OpKind::Delete { permanent } => apply_delete(cx, plan, choices, &origin, target, *permanent),
            OpKind::Duplicate | OpKind::Paste { .. } => {
                if let OpTarget::GraphEntity { entity } = target
                    && let Ok(id) = EntityId::parse(entity)
                {
                    entity_copies.push((target.clone(), id));
                    continue;
                }
                apply_copy(cx, &origin, target)
            }
        };
        outcomes.push(result.unwrap_or_else(|e| failed(target, e)));
    }
    if !entity_copies.is_empty() {
        let ids: Vec<EntityId> = entity_copies.iter().map(|(_, id)| id.clone()).collect();
        match cx.graph.duplicate_entities(&origin, &ids) {
            Ok(map) => {
                for (target, id) in &entity_copies {
                    match map.iter().find(|(old, _)| old == id) {
                        Some((_, new)) => outcomes.push(OpOutcome {
                            created: Some(OpTarget::GraphEntity { entity: new.to_string() }),
                            ..outcome(target, OpStatus::Succeeded)
                        }),
                        None => outcomes.push(failed(target, OpError::new("ops.invalid", "The copy was not made."))),
                    }
                }
            }
            Err(e) => {
                for (target, _) in &entity_copies {
                    outcomes.push(failed(target, e.clone()));
                }
            }
        }
    }
    // Keep request order.
    let order: Vec<&OpTarget> = plan.request.targets.iter().filter(|t| is_data_target(t)).collect();
    outcomes.sort_by_key(|o| order.iter().position(|t| **t == o.target).unwrap_or(usize::MAX));
    outcomes
}

/// Stop active work on a data target first when the user chose so. `Some`:
/// the target is not deleted (left running, or it could not be stopped).
fn coordinate(
    plan: &OpPlan,
    choices: &BTreeMap<String, String>,
    target: &OpTarget,
    stop: impl FnOnce() -> bool,
    active: bool,
) -> Option<OpOutcome> {
    if !active {
        return None;
    }
    if choice(plan, choices, CHOICE_ACTIVE_WORK, "skip") != "stop" {
        return Some(skipped(target, "ops.activeWork", "It is still running; it was left running and kept."));
    }
    if !stop() {
        return Some(failed(target, OpError::new("ops.activeWork", "It could not be stopped, so it was kept.")));
    }
    None
}

fn apply_delete(
    cx: &DataContext<'_>,
    plan: &OpPlan,
    choices: &BTreeMap<String, String>,
    origin: &Origin,
    target: &OpTarget,
    permanent: bool,
) -> Result<OpOutcome, OpError> {
    let recovery = if permanent { OpRecovery::None } else { OpRecovery::AppManaged };
    let done = |restore: Option<String>, message: Option<String>| OpOutcome {
        recovery: if restore.is_some() { OpRecovery::AppManaged } else { recovery.clone() },
        restore,
        message,
        ..outcome(target, OpStatus::Succeeded)
    };
    match target {
        OpTarget::Workflow { workflow } => {
            let id = workflow_id(workflow)?;
            if let Some(o) = coordinate(
                plan,
                choices,
                target,
                || {
                    let runs: Vec<WorkflowRunId> = cx
                        .runs
                        .active_run_inputs()
                        .into_iter()
                        .filter(|r| cx.runs.run_any(&r.run).is_ok_and(|(run, _)| run.workflow == id))
                        .map(|r| r.run)
                        .collect();
                    runs.iter().all(|r| cx.runs.stop_and_wait(r, STOP_TIMEOUT))
                },
                cx.runs.workflow_active(&id),
            ) {
                return Ok(o);
            }
            if let Some(triggers) = cx.triggers {
                let enabled = triggers
                    .list(Some(&id))
                    .map(|l| l.iter().any(|t| t.state == TriggerState::Active))
                    .unwrap_or(false);
                if enabled && choice(plan, choices, CHOICE_TRIGGERS, "disable") != "disable" {
                    return Ok(skipped(target, "ops.triggerEnabled", "Its trigger is enabled; the workflow was kept."));
                }
                if let Err(e) = triggers.disable_workflow(&id) {
                    return Err(OpError::new(
                        "ops.triggerEnabled",
                        format!("Its triggers could not be turned off ({}); the workflow was kept.", e.message),
                    ));
                }
            }
            let purge_pinned = choice(plan, choices, CHOICE_PINNED, "keep") == "purge";
            let deletion = cx.graph.delete_workflow(origin, &id, permanent, purge_pinned)?;
            if permanent && let Some(triggers) = cx.triggers {
                let _ = triggers.remove_workflow(&id);
            }
            let message = if permanent && !deletion.kept.is_empty() {
                Some(format!(
                    "Deleted. Revision(s) {} are kept because other workflows run them.",
                    deletion.kept.iter().map(u64::to_string).collect::<Vec<_>>().join(", ")
                ))
            } else {
                None
            };
            Ok(done(deletion.recoverable, message))
        }
        OpTarget::WorkflowRevision { workflow, revision } => {
            let id = workflow_id(workflow)?;
            cx.graph.purge_workflow_revision(origin, &id, *revision)?;
            Ok(done(None, None))
        }
        OpTarget::WorkflowRun { run } => {
            let id = workflow_run_id(run)?;
            if let Some(o) =
                coordinate(plan, choices, target, || cx.runs.stop_and_wait(&id, STOP_TIMEOUT), cx.runs.is_active(&id))
            {
                return Ok(o);
            }
            delete_workflow_run(cx, plan, choices, &id, permanent, target)
        }
        OpTarget::ResearchRun { run } => {
            let research = cx.research.ok_or_else(|| OpError::new("ops.notSupported", "Research is not available."))?;
            let active = research.record(run)?.active;
            if let Some(o) = coordinate(plan, choices, target, || research.stop(run), active) {
                return Ok(o);
            }
            delete_research_run(cx, plan, choices, run, permanent, target)
        }
        OpTarget::AgentTask { task } => {
            let tasks = cx.tasks.ok_or_else(|| OpError::new("ops.notSupported", "Agent tasks are not available."))?;
            let active = tasks.record(task)?.active;
            if let Some(o) = coordinate(plan, choices, target, || tasks.stop(task), active) {
                return Ok(o);
            }
            tasks.delete(task, permanent)?;
            if permanent {
                let message = tasks.compact().map(|(b, a)| reclaimed(b, a));
                return Ok(done(None, message));
            }
            Ok(done(Some(recoverable_id(RECOVERABLE_AGENT_TASK, task)), None))
        }
        OpTarget::GraphEntity { entity } => {
            let id = EntityId::parse(entity).map_err(|_| OpError::new("ops.notFound", "That item does not exist."))?;
            Ok(done(cx.graph.delete_entity(origin, &id, permanent)?, None))
        }
        OpTarget::GraphRelation { relation } => {
            let id = RelationId::parse(relation)
                .map_err(|_| OpError::new("ops.notFound", "That relationship does not exist."))?;
            Ok(done(cx.graph.delete_relation(origin, &id, permanent)?, None))
        }
        OpTarget::GraphView { view } => {
            let id =
                ViewId::parse(view).map_err(|_| OpError::new("ops.notFound", "That saved view does not exist."))?;
            Ok(done(cx.graph.delete_view_recoverable(&id, permanent)?, None))
        }
        OpTarget::Recoverable { id } => purge(cx, plan, choices, origin, target, id),
        OpTarget::Path { .. } | OpTarget::External { .. } => Ok(outcome(target, OpStatus::Skipped)),
    }
}

/// Files something keeps: a live graph resource (retained or attached).
fn kept_by_graph(graph: &GraphService) -> impl Fn(&WorkspacePath) -> bool + '_ {
    move |p: &WorkspacePath| graph.resource(p).ok().flatten().is_some()
}

fn delete_workflow_run(
    cx: &DataContext<'_>,
    plan: &OpPlan,
    choices: &BTreeMap<String, String>,
    id: &WorkflowRunId,
    permanent: bool,
    target: &OpTarget,
) -> Result<OpOutcome, OpError> {
    let outputs = if permanent { choice(plan, choices, CHOICE_OUTPUTS, "keep") } else { "keep".into() };
    let mut notes = Vec::new();
    if outputs != "keep" {
        let (run, _) = cx.runs.run_any(id).map_err(run_error)?;
        let (research_runs, _) = crate::service::records::run_outputs(&run);
        let mut removed = OutputsRemoved::default();
        if let Some(research) = cx.research {
            let keep = kept_by_graph(cx.graph);
            for r in &research_runs {
                if let Ok(done) = research.remove_outputs(r, &keep) {
                    removed.bytes += done.bytes;
                    removed.removed.extend(done.removed);
                    removed.kept.extend(done.kept);
                }
            }
        }
        notes.push(format!(
            "Removed {} output file(s) ({}){}.",
            removed.removed.len(),
            human_bytes(removed.bytes),
            if removed.kept.is_empty() {
                String::new()
            } else {
                format!("; {} output(s) are kept because the project keeps or uses them", removed.kept.len())
            }
        ));
    }
    if outputs == "only" {
        return Ok(OpOutcome { message: Some(notes.join(" ")), ..outcome(target, OpStatus::Succeeded) });
    }
    cx.runs.delete_run(id, permanent).map_err(run_error)?;
    if permanent {
        if let Ok((before, after)) = cx.runs.compact_history() {
            notes.push(reclaimed(before, after));
        }
        return Ok(OpOutcome { message: Some(notes.join(" ")), ..outcome(target, OpStatus::Succeeded) });
    }
    Ok(OpOutcome {
        recovery: OpRecovery::AppManaged,
        restore: Some(recoverable_id(RECOVERABLE_WORKFLOW_RUN, id.as_str())),
        ..outcome(target, OpStatus::Succeeded)
    })
}

fn delete_research_run(
    cx: &DataContext<'_>,
    plan: &OpPlan,
    choices: &BTreeMap<String, String>,
    run: &str,
    permanent: bool,
    target: &OpTarget,
) -> Result<OpOutcome, OpError> {
    let research = cx.research.ok_or_else(|| OpError::new("ops.notSupported", "Research is not available."))?;
    let outputs = if permanent { choice(plan, choices, CHOICE_OUTPUTS, "keep") } else { "keep".into() };
    let mut notes = Vec::new();
    if outputs != "keep" {
        let done = research.remove_outputs(run, &kept_by_graph(cx.graph))?;
        notes.push(format!(
            "Removed {} output file(s) ({}){}.",
            done.removed.len(),
            human_bytes(done.bytes),
            if done.kept.is_empty() {
                String::new()
            } else {
                format!("; {} output(s) are kept because the project keeps or uses them", done.kept.len())
            }
        ));
    }
    if outputs == "only" {
        return Ok(OpOutcome { message: Some(notes.join(" ")), ..outcome(target, OpStatus::Succeeded) });
    }
    research.delete(run, permanent)?;
    if permanent {
        notes.push("The run record (manifest and transcript) was removed from the project.".into());
        return Ok(OpOutcome { message: Some(notes.join(" ")), ..outcome(target, OpStatus::Succeeded) });
    }
    Ok(OpOutcome {
        recovery: OpRecovery::AppManaged,
        restore: Some(recoverable_id(RECOVERABLE_RESEARCH_RUN, run)),
        ..outcome(target, OpStatus::Succeeded)
    })
}

fn purge(
    cx: &DataContext<'_>,
    plan: &OpPlan,
    choices: &BTreeMap<String, String>,
    origin: &Origin,
    target: &OpTarget,
    id: &str,
) -> Result<OpOutcome, OpError> {
    let ok = |message: Option<String>| OpOutcome { message, ..outcome(target, OpStatus::Succeeded) };
    if id == TOMBSTONES {
        let n = cx.graph.cleanup_tombstones(None)?;
        return Ok(ok(Some(format!(
            "Removed {n} record(s) of deleted files nothing referred to. The project graph file keeps its size until it is compacted."
        ))));
    }
    let (kind, record) =
        split_recoverable(id).ok_or_else(|| OpError::new("ops.notFound", "That deleted item no longer exists."))?;
    match kind {
        RECOVERABLE_ENTITY | RECOVERABLE_RELATION | RECOVERABLE_VIEW | RECOVERABLE_WORKFLOW => {
            let purge_pinned = choice(plan, choices, CHOICE_PINNED, "keep") == "purge";
            let deletion = cx.graph.purge_recoverable(origin, id, purge_pinned)?;
            if kind == RECOVERABLE_WORKFLOW
                && let (Some(triggers), Ok(wf)) = (cx.triggers, WorkflowId::parse(record))
            {
                let _ = triggers.remove_workflow(&wf);
            }
            let message = if deletion.kept.is_empty() {
                "Purged. The project graph file keeps its size until it is compacted.".to_owned()
            } else {
                format!(
                    "Purged. Revision(s) {} are kept because other workflows run them.",
                    deletion.kept.iter().map(u64::to_string).collect::<Vec<_>>().join(", ")
                )
            };
            Ok(ok(Some(message)))
        }
        RECOVERABLE_WORKFLOW_RUN => {
            let run = workflow_run_id(record)?;
            delete_workflow_run(cx, plan, choices, &run, true, target)
        }
        RECOVERABLE_RESEARCH_RUN => delete_research_run(cx, plan, choices, record, true, target),
        RECOVERABLE_AGENT_TASK => {
            let tasks = cx.tasks.ok_or_else(|| OpError::new("ops.notSupported", "Agent tasks are not available."))?;
            tasks.delete(record, true)?;
            Ok(ok(tasks.compact().map(|(b, a)| reclaimed(b, a))))
        }
        _ => Err(OpError::new("ops.notFound", "That deleted item no longer exists.")),
    }
}

fn apply_copy(cx: &DataContext<'_>, origin: &Origin, target: &OpTarget) -> Result<OpOutcome, OpError> {
    let resolve = |id: &WorkflowId, revision: u64| cx.graph.workflow_definition(id, revision);
    let context = ValidationContext {
        ceiling: None,
        file_exists: cx.file_exists,
        resolve_workflow: Some(&resolve),
        workflow_id: None,
    };
    let created = match target {
        OpTarget::Workflow { workflow } => {
            let copy = cx.graph.duplicate_workflow(origin, &workflow_id(workflow)?, None, &context)?;
            OpTarget::Workflow { workflow: copy.id.to_string() }
        }
        OpTarget::WorkflowRevision { workflow, revision } => {
            let copy = cx.graph.duplicate_workflow(origin, &workflow_id(workflow)?, Some(*revision), &context)?;
            OpTarget::Workflow { workflow: copy.id.to_string() }
        }
        OpTarget::GraphView { view } => {
            let id =
                ViewId::parse(view).map_err(|_| OpError::new("ops.notFound", "That saved view does not exist."))?;
            OpTarget::GraphView { view: cx.graph.duplicate_view(&id)?.id.to_string() }
        }
        _ => return Err(OpError::new("ops.notSupported", "This item cannot be copied.")),
    };
    Ok(OpOutcome { created: Some(created), ..outcome(target, OpStatus::Succeeded) })
}

// --------------------------------------------------------------- restore ----

/// Restore an app-managed deletion by its token (a recoverable id). `None`
/// when the token is not the data side's (a file restore). Nothing is
/// overwritten; nothing restarts; a restored workflow's triggers stay off.
pub fn restore_data(cx: &DataContext<'_>, restore: &OpRestore) -> Option<OpOutcome> {
    let (kind, record) = split_recoverable(&restore.restore)?;
    let target = OpTarget::Recoverable { id: restore.restore.clone() };
    let mut notes: Vec<String> = Vec::new();
    let result: Result<OpTarget, OpError> = match kind {
        RECOVERABLE_ENTITY | RECOVERABLE_RELATION | RECOVERABLE_VIEW | RECOVERABLE_WORKFLOW => {
            cx.graph.restore_recoverable(&Origin::User, &restore.restore).map(|r| {
                if r.skipped_relations > 0 {
                    notes.push(format!(
                        "{} relationship(s) were not restored: an item they connect no longer exists, or the same relationship exists again.",
                        r.skipped_relations
                    ));
                }
                r.target
            })
        }
        RECOVERABLE_WORKFLOW_RUN => workflow_run_id(record).and_then(|id| {
            cx.runs.restore_run(&id).map(|_| OpTarget::WorkflowRun { run: id.to_string() }).map_err(run_error)
        }),
        RECOVERABLE_RESEARCH_RUN => match cx.research {
            Some(r) => r.restore(record).map(|()| OpTarget::ResearchRun { run: record.to_owned() }),
            None => Err(OpError::new("ops.notSupported", "Research is not available.")),
        },
        RECOVERABLE_AGENT_TASK => match cx.tasks {
            Some(t) => t.restore(record).map(|()| OpTarget::AgentTask { task: record.to_owned() }),
            None => Err(OpError::new("ops.notSupported", "Agent tasks are not available.")),
        },
        _ => return None,
    };
    if kind == RECOVERABLE_WORKFLOW {
        notes.insert(0, "Restored. Its triggers stay off until you turn them on again.".to_owned());
    }
    Some(match result {
        Ok(created) => OpOutcome {
            created: Some(created),
            recovery: OpRecovery::AppManaged,
            message: (!notes.is_empty()).then(|| notes.join(" ")),
            ..outcome(&target, OpStatus::Succeeded)
        },
        Err(e) => failed(&target, e),
    })
}

// ----------------------------------------------------- storage & history ----

fn date_label(ms: u64) -> String {
    // Days since the epoch → a civil date (UTC), without a date library.
    let days = (ms / 86_400_000) as i64;
    let (z, era_days) = (days + 719_468, 146_097);
    let era = z.div_euclid(era_days);
    let doe = z - era * era_days;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn category(id: &str, label: &str, note: &str, items: Vec<StorageItem>) -> StorageCategory {
    let bytes = items.iter().map(|i| i.bytes).sum::<Option<u64>>();
    StorageCategory { id: id.into(), label: label.into(), items, bytes, note: note.into() }
}

fn dir_bytes(dir: &Path, depth: u32) -> (u64, usize) {
    let Ok(entries) = std::fs::read_dir(dir) else { return (0, 0) };
    let (mut bytes, mut files) = (0, 0);
    for entry in entries.flatten() {
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else { continue };
        if meta.is_file() {
            bytes += meta.len();
            files += 1;
        } else if meta.is_dir() && depth > 0 {
            let (b, f) = dir_bytes(&entry.path(), depth - 1);
            bytes += b;
            files += f;
        }
    }
    (bytes, files)
}

/// Settings → Storage & History for the open project (and the app's agent
/// tasks): sizes where measurable, referenced or shared items marked (kept
/// by default), and what a purge cannot reach disclosed.
pub fn storage_overview(cx: &DataContext<'_>) -> StorageOverview {
    let mut categories = Vec::new();
    let mut recoverable: Vec<StorageItem> = Vec::new();

    // Workflow run records.
    if let Ok(records) = cx.runs.run_records() {
        let items = records
            .iter()
            .map(|r| StorageItem {
                target: OpTarget::WorkflowRun { run: r.summary.id.to_string() },
                label: format!("{} — {:?}, {}", r.summary.title, r.summary.state, date_label(r.summary.started_at_ms)),
                bytes: Some(r.bytes),
                created_at_ms: Some(r.summary.started_at_ms),
                referenced: false,
                active: r.active,
            })
            .collect();
        categories.push(category(
            "workflowRuns",
            "Workflow run records",
            "Deleting removes the run record only (restorable); its outputs stay unless you include them when deleting permanently.",
            items,
        ));
    }
    if let Ok(deleted) = cx.runs.deleted_runs() {
        recoverable.extend(deleted.iter().map(|d| StorageItem {
            target: OpTarget::Recoverable { id: recoverable_id(RECOVERABLE_WORKFLOW_RUN, d.summary.id.as_str()) },
            label: format!("Workflow run of {}", quoted(&d.summary.title)),
            bytes: Some(d.bytes),
            created_at_ms: Some(d.deleted_at_ms),
            referenced: false,
            active: false,
        }));
    }

    // Research run records and their generated outputs.
    if let Some(research) = cx.research {
        let evidence_of =
            |run: &str| cx.graph.evidence_for_operation("researchRun", run).map(|e| !e.is_empty()).unwrap_or(false);
        let kept = kept_by_graph(cx.graph);
        if let Ok(records) = research.records() {
            let runs = records
                .iter()
                .map(|r| StorageItem {
                    target: OpTarget::ResearchRun { run: r.id.clone() },
                    label: format!("{} — {}", r.label, date_label(r.started_at_ms)),
                    bytes: Some(r.record_bytes),
                    created_at_ms: Some(r.started_at_ms),
                    referenced: evidence_of(&r.id),
                    active: r.active,
                })
                .collect();
            categories.push(category(
                "researchRuns",
                "Research run records",
                "Deleting removes the record (code, transcript) only (restorable); generated outputs are a separate choice. Records retained evidence came from are marked.",
                runs,
            ));
            let outputs = records
                .iter()
                .filter(|r| !r.outputs.is_empty())
                .map(|r| StorageItem {
                    target: OpTarget::ResearchRun { run: r.id.clone() },
                    label: format!("Outputs of {} ({} file(s))", r.label, r.outputs.len()),
                    bytes: Some(r.outputs.iter().map(|o| o.bytes).sum()),
                    created_at_ms: Some(r.started_at_ms),
                    referenced: r.outputs.iter().any(|o| kept(&o.path)),
                    active: r.active,
                })
                .collect();
            categories.push(category(
                "generatedOutputs",
                "Generated outputs",
                "Delete with the outputs choice “only”: the files nothing keeps are removed and the record stays. Outputs that are retained or attached to a manuscript are never removed here.",
                outputs,
            ));
        }
        if let Ok(deleted) = research.deleted() {
            recoverable.extend(deleted.iter().map(|r| StorageItem {
                target: OpTarget::Recoverable { id: recoverable_id(RECOVERABLE_RESEARCH_RUN, &r.id) },
                label: format!("Research run {}", quoted(&r.label)),
                bytes: Some(r.record_bytes),
                created_at_ms: r.deleted_at_ms,
                referenced: evidence_of(&r.id),
                active: false,
            }));
        }
    }

    // Agent tasks (every project's, in the app's task database).
    if let Some(tasks) = cx.tasks {
        if let Ok(records) = tasks.records() {
            let items = records
                .iter()
                .map(|t| StorageItem {
                    target: OpTarget::AgentTask { task: t.id.0.clone() },
                    label: format!(
                        "{} — {}{}",
                        t.title,
                        date_label(t.created_at_ms),
                        Path::new(&t.workspace_root)
                            .file_name()
                            .map(|n| format!(" ({})", n.to_string_lossy()))
                            .unwrap_or_default()
                    ),
                    bytes: Some(t.bytes),
                    created_at_ms: Some(t.created_at_ms),
                    referenced: t.pending_review > 0,
                    active: t.active,
                })
                .collect();
            categories.push(category(
                "agentTasks",
                "Agent task records",
                "Deleting removes the task's transcript and change records (restorable). Tasks with changes still awaiting review are marked: after a permanent deletion those changes can no longer be reverted from the task.",
                items,
            ));
        }
        if let Ok(deleted) = tasks.deleted() {
            recoverable.extend(deleted.iter().map(|t| StorageItem {
                target: OpTarget::Recoverable { id: recoverable_id(RECOVERABLE_AGENT_TASK, &t.id.0) },
                label: format!("Agent task {}", quoted(&t.title)),
                bytes: Some(t.bytes),
                created_at_ms: t.deleted_at_ms,
                referenced: t.pending_review > 0,
                active: false,
            }));
        }
    }

    // Workflow revisions (history; the current revision of a workflow in use
    // is the workflow itself, not listed here).
    if let Ok(revisions) = cx.graph.workflow_revision_sizes() {
        let items = revisions
            .iter()
            .filter(|r| !r.current)
            .map(|r| StorageItem {
                target: OpTarget::WorkflowRevision { workflow: r.workflow.to_string(), revision: r.revision },
                label: format!(
                    "{} — revision {}{}{}",
                    r.title,
                    r.revision,
                    if r.retired { " (deleted workflow)" } else { "" },
                    if r.pinned_by.is_empty() { String::new() } else { format!(", run by {}", r.pinned_by.join(", ")) }
                ),
                bytes: Some(r.bytes),
                created_at_ms: Some(r.saved_at_ms),
                referenced: !r.pinned_by.is_empty(),
                active: false,
            })
            .collect();
        categories.push(category(
            "workflowRevisions",
            "Workflow revisions",
            "Earlier revisions of saved workflows. Revisions other workflows run are marked and kept unless you include them.",
            items,
        ));
    }

    // App-managed recoverable deletions.
    if let Ok(records) = cx.graph.recoverables() {
        recoverable.extend(records.iter().map(|r| StorageItem {
            target: OpTarget::Recoverable { id: r.id.clone() },
            label: format!(
                "{} {}",
                match r.kind.as_str() {
                    RECOVERABLE_ENTITY => "Item",
                    RECOVERABLE_RELATION => "Relationship",
                    RECOVERABLE_VIEW => "Saved view",
                    _ => "Workflow",
                },
                quoted(&r.label)
            ),
            bytes: Some(r.bytes),
            created_at_ms: Some(r.deleted_at_ms),
            referenced: false,
            active: false,
        }));
    }
    recoverable.sort_by_key(|i| std::cmp::Reverse(i.created_at_ms));
    categories.push(category(
        "recoverableDeletions",
        "Recently deleted",
        "Items deleted in Exposition that can still be restored. Deleting them here removes them for good.",
        recoverable,
    ));

    // Caches: metadata of removed files nothing refers to.
    let tombstones = cx.graph.unreferenced_tombstones(None).map(|t| t.len()).unwrap_or(0);
    let items = if tombstones > 0 {
        vec![StorageItem {
            target: OpTarget::Recoverable { id: TOMBSTONES.into() },
            label: format!("Records of {tombstones} removed file(s) or declaration(s) nothing refers to"),
            bytes: None,
            created_at_ms: None,
            referenced: false,
            active: false,
        }]
    } else {
        Vec::new()
    };
    categories.push(StorageCategory {
        id: "caches".into(),
        label: "Disposable records".into(),
        items,
        bytes: None,
        note: "Metadata kept for files that no longer exist; removing it changes nothing you can see. Its size inside the project graph is not measured separately.".into(),
    });

    // What a purge does not reach.
    let mut excluded = Vec::new();
    if let Some(root) = &cx.root {
        let backups = root.join(".exposition").join("graph-backups");
        let (bytes, files) = dir_bytes(&backups, 0);
        if files > 0 {
            excluded.push(format!(
                "Project graph backups ({files} file(s), {} in .exposition/graph-backups) may still contain deleted or purged items, workflows and relationships; the newest 8 are kept and older ones are replaced over time.",
                human_bytes(bytes)
            ));
        }
    }
    excluded.push(
        "Files deleted to the Recycle Bin stay there until you empty it; items purged here are not copied anywhere else."
            .into(),
    );
    excluded.push(
        "Workflow bundles and run manifests you exported, and copies in version control or sync folders, are outside the app and are not removed."
            .into(),
    );
    excluded.push("Deleting removes records and files from disk; it is not secure erasure.".into());
    if let Some(bytes) = cx.runs.history_bytes() {
        excluded.push(format!(
            "The workflow run history database currently uses {} on disk; it is compacted after records are deleted permanently.",
            human_bytes(bytes)
        ));
    }
    StorageOverview { categories, excluded }
}
