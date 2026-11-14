//! P1 project operations, data side (lane L2), against the trigger engine's
//! fixture: a real project graph, the real run service (fake batch runner,
//! which can hold a batch active), the real trigger store and a manual clock;
//! research run records and agent tasks behind in-memory fakes of their
//! record traits (their stores are tested in their own crates).

use std::collections::BTreeMap;
use std::sync::Mutex;

use exposition_core::ops::{
    ImpactKind, OpActor, OpError, OpKind, OpOutcome, OpPlan, OpRequest, OpRestore, OpStatus, OpTarget,
};
use exposition_core::path::WorkspacePath;
use exposition_research::records::OutputFile;

use super::*;
use crate::model::StartWorkflowRunRequest;
use crate::project_data::*;

// ---------------------------------------------------------------- fakes ----

#[derive(Default)]
struct Research {
    records: Mutex<Vec<ResearchRecord>>,
    removed_outputs: Mutex<Vec<String>>,
    stopped: Mutex<Vec<String>>,
}

impl ResearchRecords for Research {
    fn records(&self) -> Result<Vec<ResearchRecord>, OpError> {
        Ok(self.records.lock().unwrap().iter().filter(|r| r.deleted_at_ms.is_none()).cloned().collect())
    }
    fn deleted(&self) -> Result<Vec<ResearchRecord>, OpError> {
        Ok(self.records.lock().unwrap().iter().filter(|r| r.deleted_at_ms.is_some()).cloned().collect())
    }
    fn record(&self, id: &str) -> Result<ResearchRecord, OpError> {
        self.records
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.id == id)
            .cloned()
            .ok_or_else(|| OpError::new("research.unknownRun", "unknown run"))
    }
    fn delete(&self, id: &str, permanent: bool) -> Result<(), OpError> {
        let mut records = self.records.lock().unwrap();
        let i = records.iter().position(|r| r.id == id).ok_or_else(|| OpError::new("research.unknownRun", "x"))?;
        if records[i].active {
            return Err(OpError::new("research.runActive", "active"));
        }
        if permanent {
            records.remove(i);
        } else {
            records[i].deleted_at_ms = Some(1);
        }
        Ok(())
    }
    fn restore(&self, id: &str) -> Result<(), OpError> {
        let mut records = self.records.lock().unwrap();
        let r = records.iter_mut().find(|r| r.id == id).ok_or_else(|| OpError::new("research.unknownRun", "x"))?;
        r.deleted_at_ms = None;
        Ok(())
    }
    fn remove_outputs(&self, id: &str, keep: &dyn Fn(&WorkspacePath) -> bool) -> Result<OutputsRemoved, OpError> {
        self.removed_outputs.lock().unwrap().push(id.to_owned());
        let mut done = OutputsRemoved::default();
        let mut records = self.records.lock().unwrap();
        if let Some(r) = records.iter_mut().find(|r| r.id == id) {
            for o in std::mem::take(&mut r.outputs) {
                if keep(&o.path) {
                    done.kept.push(o.path.clone());
                    r.outputs.push(o);
                } else {
                    done.bytes += o.bytes;
                    done.removed.push(o.path);
                }
            }
        }
        Ok(done)
    }
    fn stop(&self, id: &str) -> bool {
        if let Some(r) = self.records.lock().unwrap().iter_mut().find(|r| r.id == id && r.active) {
            r.active = false;
            self.stopped.lock().unwrap().push(id.to_owned());
        }
        true
    }
}

#[derive(Default)]
struct Tasks {
    records: Mutex<Vec<TaskRecord>>,
    paths: Mutex<BTreeMap<String, Vec<WorkspacePath>>>,
    stoppable: AtomicBool,
}

impl TaskRecords for Tasks {
    fn records(&self) -> Result<Vec<TaskRecord>, OpError> {
        Ok(self.records.lock().unwrap().iter().filter(|t| t.deleted_at_ms.is_none()).cloned().collect())
    }
    fn deleted(&self) -> Result<Vec<TaskRecord>, OpError> {
        Ok(self.records.lock().unwrap().iter().filter(|t| t.deleted_at_ms.is_some()).cloned().collect())
    }
    fn record(&self, id: &str) -> Result<TaskRecord, OpError> {
        self.records
            .lock()
            .unwrap()
            .iter()
            .find(|t| t.id.0 == id)
            .cloned()
            .ok_or_else(|| OpError::new("agents.notFound", "unknown task"))
    }
    fn paths(&self, id: &str) -> Vec<WorkspacePath> {
        self.paths.lock().unwrap().get(id).cloned().unwrap_or_default()
    }
    fn delete(&self, id: &str, permanent: bool) -> Result<(), OpError> {
        let mut records = self.records.lock().unwrap();
        let i = records.iter().position(|t| t.id.0 == id).ok_or_else(|| OpError::new("agents.notFound", "x"))?;
        if records[i].active {
            return Err(OpError::new("agents.taskActive", "running"));
        }
        if permanent {
            records.remove(i);
        } else {
            records[i].deleted_at_ms = Some(1);
        }
        Ok(())
    }
    fn restore(&self, id: &str) -> Result<(), OpError> {
        let mut records = self.records.lock().unwrap();
        let t = records.iter_mut().find(|t| t.id.0 == id).ok_or_else(|| OpError::new("agents.notFound", "x"))?;
        t.deleted_at_ms = None;
        Ok(())
    }
    fn stop(&self, id: &str) -> bool {
        if !self.stoppable.load(Ordering::SeqCst) {
            return false;
        }
        if let Some(t) = self.records.lock().unwrap().iter_mut().find(|t| t.id.0 == id) {
            t.active = false;
        }
        true
    }
    fn compact(&self) -> Option<(Option<u64>, Option<u64>)> {
        Some((Some(4096), Some(4096)))
    }
}

fn task(id: &str, active: bool) -> TaskRecord {
    TaskRecord {
        id: exposition_agents::TaskId(id.into()),
        title: format!("Task {id}"),
        created_at_ms: 1,
        workspace_root: "C:/project".into(),
        bytes: 1234,
        active,
        pending_review: 0,
        deleted_at_ms: None,
    }
}

fn research_run(id: &str, active: bool, inputs: &[&str], outputs: &[&str]) -> ResearchRecord {
    ResearchRecord {
        id: id.into(),
        label: format!("print({id})"),
        started_at_ms: 1,
        record_bytes: 300,
        outputs: outputs.iter().map(|p| OutputFile { path: path(p), bytes: 10 }).collect(),
        active,
        deleted_at_ms: None,
        inputs: inputs.iter().map(|p| path(p)).collect(),
    }
}

struct Env {
    fx: Fx,
    research: Research,
    tasks: Tasks,
}

fn env() -> Env {
    let fx = fixture();
    fx.graph
        .reconcile(&exposition_graph::reconcile::ReconcileOptions {
            scope: exposition_graph::reconcile::Scope::Full,
            force: false,
        })
        .unwrap();
    fx.open_ready();
    Env { fx, research: Research::default(), tasks: Tasks::default() }
}

impl Env {
    fn cx(&self) -> DataContext<'_> {
        DataContext {
            graph: &self.fx.graph,
            runs: &self.fx.runs,
            triggers: Some(&self.fx.triggers),
            research: Some(&self.research),
            tasks: Some(&self.tasks),
            file_exists: None,
            root: Some(self.fx.root.clone()),
        }
    }

    fn request(&self, kind: OpKind, targets: Vec<OpTarget>) -> OpRequest {
        OpRequest { kind, targets, actor: OpActor::User }
    }

    /// Plan a request the way the orchestrator does (data side only).
    fn plan(&self, request: OpRequest) -> OpPlan {
        let paths: Vec<WorkspacePath> = request
            .targets
            .iter()
            .filter_map(|t| match t {
                OpTarget::Path { path } => Some(path.clone()),
                _ => None,
            })
            .collect();
        let mut data = impacts_for_paths(&self.cx(), &paths, &request.kind);
        let more = plan_data(&self.cx(), &request);
        data.effects.extend(more.effects);
        data.refusals.extend(more.refusals);
        data.impacts.extend(more.impacts);
        for c in more.choices {
            if !data.choices.iter().any(|d| d.key == c.key) {
                data.choices.push(c);
            }
        }
        OpPlan {
            plan: "plan-test".into(),
            needs_decision: data.impacts.iter().any(|i| i.kind.needs_decision()),
            request,
            effects: data.effects,
            refusals: data.refusals,
            impacts: data.impacts,
            choices: data.choices,
            plan_hash: data.fingerprint.join("\n"),
            expires_at_ms: 0,
        }
    }

    fn apply(&self, plan: &OpPlan, choices: &[(&str, &str)]) -> Vec<OpOutcome> {
        let choices: BTreeMap<String, String> = choices.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        apply_data(&self.cx(), plan, &choices)
    }

    fn prepare(&self, plan: &OpPlan, choices: &[(&str, &str)]) -> Vec<exposition_core::ops::OpRefusal> {
        let choices: BTreeMap<String, String> = choices.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        prepare_paths(&self.cx(), plan, &choices)
    }

    fn workflow(&self) -> WorkflowId {
        self.fx.save(definition(false, false))
    }

    /// Start a run of `id` whose batch stays active until `release`.
    fn hold_run(&self, id: &WorkflowId) -> (WorkflowRunId, Arc<AtomicBool>) {
        let release = Arc::new(AtomicBool::new(false));
        *self.fx.batch.gate.lock().unwrap() = Some(release.clone());
        let plan = self.fx.runs.plan(id, None).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        let run = self
            .fx
            .runs
            .start(&StartWorkflowRunRequest { plan: plan.plan, approval_hash: plan.approval_hash })
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while self.fx.batch.runs.load(Ordering::SeqCst) == 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(self.fx.runs.is_active(&run.id));
        (run.id, release)
    }

    fn trigger_state(&self, id: &WorkflowId) -> TriggerState {
        self.fx.triggers.list(Some(id)).unwrap().remove(0).state
    }
}

fn kinds(plan: &OpPlan) -> Vec<ImpactKind> {
    let mut k: Vec<ImpactKind> = plan.impacts.iter().map(|i| i.kind).collect();
    k.sort();
    k
}

fn delete(permanent: bool) -> OpKind {
    OpKind::Delete { permanent }
}

fn p(s: &str) -> OpTarget {
    OpTarget::Path { path: path(s) }
}

// -------------------------------------------------------------- paths ----

#[test]
fn an_unused_file_needs_no_decision_and_copies_have_no_data_impacts() {
    let e = env();
    e.workflow();
    let plan = e.plan(e.request(delete(false), vec![p("notes.md")]));
    assert_eq!(plan.impacts, vec![]);
    assert!(!plan.needs_decision);
    assert!(plan.choices.is_empty());
    let copy = e.plan(e.request(OpKind::Duplicate, vec![p("data/obs.csv")]));
    assert_eq!(copy.impacts, vec![], "copying is never a consequence for consumers");
}

#[test]
fn a_watched_workflow_input_needs_one_decision_and_cancelling_changes_nothing() {
    let e = env();
    let id = e.workflow();
    e.fx.enable(&id, watch(&["data"]));
    let revision = e.fx.graph.status().revision;
    let plan = e.plan(e.request(delete(false), vec![p("data/obs.csv")]));
    assert_eq!(kinds(&plan), vec![ImpactKind::WorkflowInput, ImpactKind::EnabledTrigger]);
    assert!(plan.needs_decision);
    let trigger = plan.impacts.iter().find(|i| i.kind == ImpactKind::EnabledTrigger).unwrap();
    assert_eq!(trigger.consumers[0].kind, "trigger");
    assert!(plan.choices.iter().any(|c| c.key == CHOICE_TRIGGERS && c.default == "disable"));
    // The user cancels: nothing was applied, nothing changed.
    assert_eq!(e.trigger_state(&id), TriggerState::Active);
    assert_eq!(e.fx.graph.status().revision, revision);
    assert!(e.fx.root.join("data/obs.csv").is_file());
    assert!(e.fx.graph.workflows().unwrap().iter().any(|w| w.id == id));
}

#[test]
fn proceeding_turns_the_trigger_off_first_and_it_never_fires_for_the_deleted_input() {
    let e = env();
    let id = e.workflow();
    e.fx.enable(&id, watch(&["data"]));
    let plan = e.plan(e.request(delete(false), vec![p("data/obs.csv")]));

    // Keeping the trigger keeps the file.
    let kept = e.prepare(&plan, &[(CHOICE_TRIGGERS, "skip")]);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].code, "ops.triggerEnabled");
    assert_eq!(e.trigger_state(&id), TriggerState::Active);

    // Proceeding (the default) turns it off before the file goes.
    assert_eq!(e.prepare(&plan, &[]), vec![]);
    assert_eq!(e.trigger_state(&id), TriggerState::Disabled);
    std::fs::remove_file(e.fx.root.join("data/obs.csv")).unwrap();
    e.fx.triggers.files_changed(&[path("data/obs.csv")], false);
    e.fx.advance(10 * SECOND);
    e.fx.advance(10 * SECOND);
    assert!(e.fx.starter.started.lock().unwrap().is_empty(), "no run was started for the deleted input");
    assert_eq!(e.trigger_state(&id), TriggerState::Disabled, "nothing turned it back on");
    // The workflow stays (honest missing input), nothing cascaded.
    after_paths(
        &e.cx(),
        &[OpOutcome {
            target: p("data/obs.csv"),
            status: OpStatus::Succeeded,
            code: None,
            message: None,
            created: None,
            recovery: exposition_core::ops::OpRecovery::OsTrash,
            restore: None,
        }],
    );
    assert!(e.fx.graph.workflows().unwrap().iter().any(|w| w.id == id));
    assert!(e.fx.graph.resource(&path("data/obs.csv")).unwrap().is_none(), "reconciled with the actual outcome");
}

#[test]
fn active_work_on_a_path_is_coordinated_before_deletion() {
    let e = env();
    let id = e.workflow();
    let (run, release) = e.hold_run(&id);
    e.tasks.records.lock().unwrap().push(task("task-1", true));
    e.tasks.paths.lock().unwrap().insert("task-1".into(), vec![path("data")]);
    e.research.records.lock().unwrap().push(research_run("run-live", true, &["data/obs.csv"], &[]));
    let plan = e.plan(e.request(delete(false), vec![p("data/obs.csv")]));
    let active = plan.impacts.iter().find(|i| i.kind == ImpactKind::ActiveWork).expect("active work");
    let ids: Vec<&str> = active.consumers.iter().map(|c| c.id.as_str()).collect();
    assert!(ids.contains(&run.as_str()) && ids.contains(&"task-1") && ids.contains(&"run-live"), "{ids:?}");
    assert!(plan.choices.iter().any(|c| c.key == CHOICE_ACTIVE_WORK && c.default == "skip"));

    // Default: the work keeps running and the file is kept.
    let refused = e.prepare(&plan, &[]);
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0].code, "ops.activeWork");
    assert!(e.fx.runs.is_active(&run), "nothing was stopped");

    // A task that cannot be stopped keeps the file too (no worker writes into a deleted target).
    let refused = e.prepare(&plan, &[(CHOICE_ACTIVE_WORK, "stop")]);
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert!(refused[0].message.contains("could not be stopped"), "{}", refused[0].message);
    // Everything stoppable is stopped first; then nothing is refused.
    e.tasks.stoppable.store(true, Ordering::SeqCst);
    assert_eq!(e.prepare(&plan, &[(CHOICE_ACTIVE_WORK, "stop")]), vec![]);
    assert!(!e.fx.runs.is_active(&run));
    assert_eq!(e.fx.runs.get(&run).unwrap().state, WorkflowRunState::Canceled);
    assert_eq!(e.research.stopped.lock().unwrap().as_slice(), ["run-live"]);
    release.store(true, Ordering::SeqCst);
}

// ----------------------------------------------------------- workflows ----

#[test]
fn deleting_a_workflow_turns_its_trigger_off_and_restoring_keeps_it_off() {
    let e = env();
    let id = e.workflow();
    e.fx.enable(&id, watch(&["data"]));
    let target = OpTarget::Workflow { workflow: id.to_string() };
    let plan = e.plan(e.request(delete(false), vec![target.clone()]));
    assert_eq!(kinds(&plan), vec![ImpactKind::EnabledTrigger]);
    let outcomes = e.apply(&plan, &[]);
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].status, OpStatus::Succeeded, "{outcomes:?}");
    assert_eq!(outcomes[0].recovery, exposition_core::ops::OpRecovery::AppManaged);
    let token = outcomes[0].restore.clone().expect("restorable");
    assert_eq!(e.trigger_state(&id), TriggerState::Disabled);
    assert!(e.fx.graph.workflows().unwrap().iter().all(|w| w.id != id));
    // A watched input changes: the deleted workflow never runs.
    e.fx.save_file("data/obs.csv", "x\n9\n");
    e.fx.advance(10 * SECOND);
    assert!(e.fx.starter.started.lock().unwrap().is_empty());

    let restored = restore_data(&e.cx(), &OpRestore { restore: token, on_collision: None }).unwrap();
    assert_eq!(restored.status, OpStatus::Succeeded);
    assert_eq!(restored.created, Some(target));
    assert!(e.fx.graph.workflows().unwrap().iter().any(|w| w.id == id));
    assert_eq!(e.trigger_state(&id), TriggerState::Disabled, "a restored workflow keeps its trigger off");
    e.fx.save_file("data/obs.csv", "x\n10\n");
    e.fx.advance(10 * SECOND);
    assert!(e.fx.starter.started.lock().unwrap().is_empty(), "restoring never restarts anything");
    assert!(e.fx.runs.runs(Some(&id)).unwrap().is_empty());
}

#[test]
fn a_workflow_with_an_active_run_is_kept_until_the_run_is_stopped() {
    let e = env();
    let id = e.workflow();
    let (run, release) = e.hold_run(&id);
    let target = OpTarget::Workflow { workflow: id.to_string() };
    let plan = e.plan(e.request(delete(false), vec![target]));
    assert_eq!(kinds(&plan), vec![ImpactKind::ActiveWork]);
    let kept = e.apply(&plan, &[]);
    assert_eq!(kept[0].status, OpStatus::Skipped);
    assert_eq!(kept[0].code.as_deref(), Some("ops.activeWork"));
    assert!(e.fx.runs.is_active(&run));
    assert!(e.fx.graph.workflows().unwrap().iter().any(|w| w.id == id));
    let done = e.apply(&plan, &[(CHOICE_ACTIVE_WORK, "stop")]);
    assert_eq!(done[0].status, OpStatus::Succeeded, "{done:?}");
    assert!(!e.fx.runs.is_active(&run));
    assert!(e.fx.graph.workflow_retired(&id));
    release.store(true, Ordering::SeqCst);
}

#[test]
fn deleting_a_workflow_permanently_keeps_revisions_other_workflows_run_unless_chosen() {
    let e = env();
    let child = e.workflow();
    // A parent running the child's revision 1.
    let mut parent: WorkflowDefinition = serde_json::from_value(definition(false, false)).unwrap();
    parent.schema = exposition_graph::workflow::WORKFLOW_SCHEMA_V2.into();
    parent.title = "Parent".into();
    parent.steps[0].operation = exposition_graph::workflow::StepOperation {
        id: exposition_graph::workflow::COMPOSITE_OPERATION.into(),
        version: 1,
    };
    parent.steps[0].composite = Some(exposition_graph::workflow::CompositeRef { workflow: child.clone(), revision: 1 });
    let parent_id =
        e.fx.graph
            .save_workflow(
                &Origin::User,
                &exposition_graph::SaveWorkflowRequest { id: None, expected_revision: None, definition: parent },
                &ValidationContext::default(),
            )
            .unwrap()
            .id;
    e.fx.enable(&child, watch(&["data"]));
    let target = OpTarget::Workflow { workflow: child.to_string() };
    let plan = e.plan(e.request(delete(true), vec![target.clone()]));
    assert_eq!(kinds(&plan), vec![ImpactKind::PinnedRevision, ImpactKind::EnabledTrigger, ImpactKind::RecoveryLoss]);
    let pinned = plan.impacts.iter().find(|i| i.kind == ImpactKind::PinnedRevision).unwrap();
    assert!(pinned.consumers[0].label.contains("Parent"), "{:?}", pinned.consumers);
    assert!(plan.choices.iter().any(|c| c.key == CHOICE_PINNED && c.default == "keep"));
    let outcomes = e.apply(&plan, &[]);
    assert_eq!(outcomes[0].status, OpStatus::Succeeded, "{outcomes:?}");
    assert_eq!(outcomes[0].restore, None, "permanent: nothing to restore");
    assert!(outcomes[0].message.as_deref().unwrap_or("").contains("kept"), "{outcomes:?}");
    assert!(e.fx.graph.workflow_definition(&child, 1).is_some(), "kept for the parent");
    assert!(e.fx.triggers.list(Some(&child)).unwrap().is_empty(), "its triggers are removed");
    assert_eq!(e.fx.graph.workflow(&parent_id, None).unwrap().revision, 1, "the parent is untouched");
    // The retained revision is history in Storage & History, marked as used.
    let overview = storage_overview(&e.cx());
    let revisions = overview.categories.iter().find(|c| c.id == "workflowRevisions").unwrap();
    let item = revisions
        .items
        .iter()
        .find(|i| i.target == OpTarget::WorkflowRevision { workflow: child.to_string(), revision: 1 })
        .expect("listed");
    assert!(item.referenced && item.bytes.unwrap_or(0) > 0);
    // Explicitly purging that revision after the decision.
    let revision = OpTarget::WorkflowRevision { workflow: child.to_string(), revision: 1 };
    let purge = e.plan(e.request(delete(true), vec![revision]));
    assert_eq!(kinds(&purge), vec![ImpactKind::PinnedRevision, ImpactKind::RecoveryLoss]);
    assert_eq!(e.apply(&purge, &[])[0].status, OpStatus::Succeeded);
    assert!(e.fx.graph.workflow_definition(&child, 1).is_none());
}

#[test]
fn duplicating_a_workflow_carries_no_triggers_runs_or_approvals() {
    let e = env();
    let id = e.workflow();
    e.fx.enable(&id, watch(&["data"]));
    let run = {
        let plan = e.fx.runs.plan(&id, None).unwrap();
        e.fx.runs.start(&StartWorkflowRunRequest { plan: plan.plan, approval_hash: plan.approval_hash }).unwrap()
    };
    assert!(e.fx.runs.wait_idle(Duration::from_secs(10)));
    let plan = e.plan(e.request(OpKind::Duplicate, vec![OpTarget::Workflow { workflow: id.to_string() }]));
    assert_eq!(plan.impacts, vec![]);
    let outcomes = e.apply(&plan, &[]);
    let Some(OpTarget::Workflow { workflow }) = outcomes[0].created.clone() else { panic!("{outcomes:?}") };
    let copy = WorkflowId::parse(&workflow).unwrap();
    assert_ne!(copy, id);
    assert!(e.fx.triggers.list(Some(&copy)).unwrap().is_empty(), "no trigger");
    assert!(e.fx.runs.runs(Some(&copy)).unwrap().is_empty(), "no run history");
    let saved = e.fx.graph.workflow(&copy, None).unwrap();
    assert_eq!(saved.revision, 1);
    assert!(!saved.validation.runnable);
    assert_eq!(e.fx.runs.get(&run.id).unwrap().workflow, id, "the original's history is untouched");
    assert!(e.fx.starter.started.lock().unwrap().is_empty(), "nothing ran");
    // A run is history: never duplicated as a new result.
    let dup_run = e.plan(e.request(OpKind::Duplicate, vec![OpTarget::WorkflowRun { run: run.id.to_string() }]));
    assert_eq!(dup_run.refusals[0].code, "ops.notSupported");
}

// ---------------------------------------------------------------- runs ----

#[test]
fn run_records_are_deleted_recoverably_restored_and_purged_with_outputs_as_their_own_scope() {
    let e = env();
    let id = e.workflow();
    let plan = e.fx.runs.plan(&id, None).unwrap();
    let run = e.fx.runs.start(&StartWorkflowRunRequest { plan: plan.plan, approval_hash: plan.approval_hash }).unwrap();
    assert!(e.fx.runs.wait_idle(Duration::from_secs(10)));
    let finished = e.fx.runs.get(&run.id).unwrap();
    let (research_runs, _) = crate::service::records::run_outputs(&finished);
    assert_eq!(research_runs.len(), 1, "the batch run produced outputs");
    e.research.records.lock().unwrap().push(research_run(
        &research_runs[0],
        false,
        &[],
        &[&format!(".exposition/artifacts/{}/dataset.csv", research_runs[0])],
    ));
    let target = OpTarget::WorkflowRun { run: run.id.to_string() };

    // Routine: no decision; recoverable.
    let plan = e.plan(e.request(delete(false), vec![target.clone()]));
    assert!(!plan.needs_decision, "{:?}", plan.impacts);
    let outcomes = e.apply(&plan, &[]);
    let token = outcomes[0].restore.clone().expect("restorable");
    assert!(e.fx.runs.runs(None).unwrap().iter().all(|r| r.id != run.id));
    let overview = storage_overview(&e.cx());
    let recent = overview.categories.iter().find(|c| c.id == "recoverableDeletions").unwrap();
    assert!(recent.items.iter().any(|i| i.target == OpTarget::Recoverable { id: token.clone() }));
    assert!(e.research.removed_outputs.lock().unwrap().is_empty(), "outputs untouched");
    restore_data(&e.cx(), &OpRestore { restore: token, on_collision: None }).unwrap();
    assert!(e.fx.runs.runs(None).unwrap().iter().any(|r| r.id == run.id));

    // Permanent: one decision (loss of recovery), outputs only when chosen.
    let plan = e.plan(e.request(delete(true), vec![target.clone()]));
    assert_eq!(kinds(&plan), vec![ImpactKind::RecoveryLoss]);
    assert!(plan.choices.iter().any(|c| c.key == CHOICE_OUTPUTS && c.default == "keep"));
    let only = e.apply(&plan, &[(CHOICE_OUTPUTS, "only")]);
    assert_eq!(only[0].status, OpStatus::Succeeded);
    assert!(e.fx.runs.get(&run.id).is_ok(), "the record stays when only outputs are removed");
    assert_eq!(e.research.removed_outputs.lock().unwrap().as_slice(), [research_runs[0].clone()]);
    let gone = e.apply(&plan, &[(CHOICE_OUTPUTS, "keep")]);
    assert_eq!(gone[0].status, OpStatus::Succeeded);
    assert!(e.fx.runs.run_any(&run.id).is_err(), "purged");
    let message = gone[0].message.clone().unwrap_or_default();
    assert!(message.contains("gave back") || message.contains("did not shrink"), "{message}");
}

#[test]
fn an_active_run_record_is_never_deleted_under_its_worker() {
    let e = env();
    let id = e.workflow();
    let (run, release) = e.hold_run(&id);
    let target = OpTarget::WorkflowRun { run: run.to_string() };
    let plan = e.plan(e.request(delete(false), vec![target]));
    assert_eq!(kinds(&plan), vec![ImpactKind::ActiveWork]);
    assert_eq!(e.apply(&plan, &[])[0].code.as_deref(), Some("ops.activeWork"));
    assert!(e.fx.runs.is_active(&run) && e.fx.runs.get(&run).is_ok());
    let done = e.apply(&plan, &[(CHOICE_ACTIVE_WORK, "stop")]);
    assert_eq!(done[0].status, OpStatus::Succeeded, "{done:?}");
    assert!(e.fx.runs.get(&run).is_err(), "deleted after it stopped");
    assert_eq!(e.fx.runs.run_any(&run).unwrap().0.state, WorkflowRunState::Canceled);
    release.store(true, Ordering::SeqCst);
}

#[test]
fn agent_tasks_and_research_runs_follow_the_same_rules() {
    let e = env();
    e.tasks.records.lock().unwrap().push(task("task-a", true));
    e.research.records.lock().unwrap().push(research_run(
        "run-r1",
        false,
        &[],
        &[".exposition/artifacts/run-r1/a.csv"],
    ));
    let t = OpTarget::AgentTask { task: "task-a".into() };
    let plan = e.plan(e.request(delete(false), vec![t.clone()]));
    assert_eq!(kinds(&plan), vec![ImpactKind::ActiveWork]);
    assert_eq!(e.apply(&plan, &[])[0].status, OpStatus::Skipped);
    e.tasks.stoppable.store(true, Ordering::SeqCst);
    let done = e.apply(&plan, &[(CHOICE_ACTIVE_WORK, "stop")]);
    assert_eq!(done[0].restore.as_deref(), Some("agentTask:task-a"));
    let purge = e.plan(e.request(delete(true), vec![OpTarget::Recoverable { id: "agentTask:task-a".into() }]));
    assert_eq!(kinds(&purge), vec![ImpactKind::RecoveryLoss]);
    assert_eq!(e.apply(&purge, &[])[0].status, OpStatus::Succeeded);
    assert!(e.tasks.records.lock().unwrap().is_empty());

    let r = OpTarget::ResearchRun { run: "run-r1".into() };
    let plan = e.plan(e.request(delete(false), vec![r.clone()]));
    assert!(!plan.needs_decision);
    assert_eq!(e.apply(&plan, &[])[0].restore.as_deref(), Some("researchRun:run-r1"));
    // Already deleted: only restore or permanent removal remain.
    assert_eq!(e.plan(e.request(delete(false), vec![r])).refusals[0].code, "ops.alreadyDeleted");
}

#[test]
fn agents_keep_their_grants() {
    let e = env();
    let id = e.workflow();
    e.fx.enable(&id, watch(&["data"]));
    let request = OpRequest {
        kind: delete(false),
        targets: vec![OpTarget::Workflow { workflow: id.to_string() }, p("data/obs.csv")],
        actor: OpActor::Agent { task: "task-x".into() },
    };
    let plan = e.plan(request);
    assert_eq!(plan.refusals.len(), 1);
    assert_eq!(plan.refusals[0].code, "ops.agentScope");
    // An agent cannot turn the user's trigger off to delete a watched file.
    let refused = e.prepare(&plan, &[(CHOICE_TRIGGERS, "disable")]);
    assert_eq!(refused[0].code, "ops.agentScope");
    assert_eq!(e.trigger_state(&id), TriggerState::Active);
    assert_eq!(e.apply(&plan, &[])[0].status, OpStatus::Skipped);
    assert!(e.fx.graph.workflows().unwrap().iter().any(|w| w.id == id));
}

// ----------------------------------------------------- storage & history ----

#[test]
fn storage_overview_lists_categories_with_sizes_references_and_exclusions() {
    let e = env();
    let id = e.workflow();
    let plan = e.fx.runs.plan(&id, None).unwrap();
    e.fx.runs.start(&StartWorkflowRunRequest { plan: plan.plan, approval_hash: plan.approval_hash }).unwrap();
    assert!(e.fx.runs.wait_idle(Duration::from_secs(10)));
    e.tasks.records.lock().unwrap().push(task("task-b", false));
    e.research.records.lock().unwrap().push(research_run("run-o", false, &[], &["data/obs.csv", "out/x.csv"]));
    std::fs::create_dir_all(e.fx.root.join(".exposition/graph-backups")).unwrap();
    std::fs::write(e.fx.root.join(".exposition/graph-backups/graph-1-r1-manual.sqlite3"), b"backup").unwrap();
    let overview = storage_overview(&e.cx());
    let ids: Vec<&str> = overview.categories.iter().map(|c| c.id.as_str()).collect();
    for want in [
        "workflowRuns",
        "researchRuns",
        "generatedOutputs",
        "agentTasks",
        "workflowRevisions",
        "recoverableDeletions",
        "caches",
    ] {
        assert!(ids.contains(&want), "{want} in {ids:?}");
    }
    let runs = overview.categories.iter().find(|c| c.id == "workflowRuns").unwrap();
    assert_eq!(runs.items.len(), 1);
    assert!(runs.items[0].bytes.unwrap_or(0) > 0 && runs.bytes.unwrap_or(0) > 0);
    assert!(runs.note.contains("record only"));
    let outputs = overview.categories.iter().find(|c| c.id == "generatedOutputs").unwrap();
    assert!(outputs.items[0].referenced, "an output the project keeps (a live resource) is marked");
    assert!(overview.excluded.iter().any(|x| x.contains("graph backups")), "{:?}", overview.excluded);
}

#[test]
fn graph_state_follows_the_actual_file_outcomes() {
    let e = env();
    let done = |target: OpTarget, created: Option<OpTarget>, recovery: exposition_core::ops::OpRecovery| OpOutcome {
        target,
        status: OpStatus::Succeeded,
        code: None,
        message: None,
        created,
        recovery,
        restore: None,
    };
    let original = e.fx.graph.resource(&path("data/extra.csv")).unwrap().unwrap();
    // A copy: a fresh identity with copied-from provenance, nothing inherited.
    std::fs::copy(e.fx.root.join("data/extra.csv"), e.fx.root.join("data/extra (2).csv")).unwrap();
    after_paths(
        &e.cx(),
        &[done(p("data/extra.csv"), Some(p("data/extra (2).csv")), exposition_core::ops::OpRecovery::None)],
    );
    let copy = e.fx.graph.resource(&path("data/extra (2).csv")).unwrap().expect("indexed at once");
    assert_ne!(copy.id, original.id);
    assert_eq!(copy.attributes.get("copy.from").map(String::as_str), Some("data/extra.csv"));
    assert!(e.fx.graph.entity_detail(&copy.id).unwrap().evidence.is_empty());

    // A failed or skipped outcome changes nothing in the graph.
    std::fs::remove_file(e.fx.root.join("notes.md")).unwrap();
    let mut not_done = done(p("notes.md"), None, exposition_core::ops::OpRecovery::OsTrash);
    not_done.status = OpStatus::Failed;
    after_paths(&e.cx(), &[not_done]);
    assert!(e.fx.graph.resource(&path("notes.md")).unwrap().is_some(), "only actual successes are reconciled");

    // A permanent deletion leaves no unreferenced metadata behind.
    let notes = e.fx.graph.resource(&path("notes.md")).unwrap().unwrap();
    after_paths(&e.cx(), &[done(p("notes.md"), None, exposition_core::ops::OpRecovery::None)]);
    assert!(e.fx.graph.entity_detail(&notes.id).is_err(), "unreferenced tombstone cleaned up");

    // A trashed file keeps its tombstone (an undo can bring the identity back).
    let extra = e.fx.graph.resource(&path("data/extra (2).csv")).unwrap().unwrap();
    std::fs::remove_file(e.fx.root.join("data/extra (2).csv")).unwrap();
    after_paths(&e.cx(), &[done(p("data/extra (2).csv"), None, exposition_core::ops::OpRecovery::OsTrash)]);
    assert_eq!(e.fx.graph.entity_detail(&extra.id).unwrap().entity.lifecycle, exposition_graph::Lifecycle::Tombstoned);
    // Restored at the same path with the same content: the same identity.
    std::fs::copy(e.fx.root.join("data/extra.csv"), e.fx.root.join("data/extra (2).csv")).unwrap();
    after_paths(
        &e.cx(),
        &[done(p("data/extra (2).csv"), Some(p("data/extra (2).csv")), exposition_core::ops::OpRecovery::OsTrash)],
    );
    assert_eq!(e.fx.graph.resource(&path("data/extra (2).csv")).unwrap().unwrap().id, extra.id);
}

/// A run history written before P1 (no `deleted_at`) stays loadable and
/// gains recoverable deletion.
#[test]
fn an_earlier_run_history_keeps_its_runs_when_it_gains_recoverable_deletion() {
    let e = env();
    let id = e.workflow();
    let plan = e.fx.runs.plan(&id, None).unwrap();
    let run = e.fx.runs.start(&StartWorkflowRunRequest { plan: plan.plan, approval_hash: plan.approval_hash }).unwrap();
    assert!(e.fx.runs.wait_idle(Duration::from_secs(10)));
    let finished = e.fx.runs.get(&run.id).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("workflow-runs.sqlite3");
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE runs (id TEXT PRIMARY KEY, project TEXT NOT NULL, workflow TEXT NOT NULL, state TEXT NOT NULL,
             started_at INTEGER NOT NULL, body TEXT NOT NULL);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO runs (id, project, workflow, state, started_at, body) VALUES (?1, 'p', ?2, ?3, 1, ?4)",
            rusqlite::params![
                finished.id.as_str(),
                finished.workflow.as_str(),
                crate::store::state_name(finished.state),
                serde_json::to_string(&finished).unwrap()
            ],
        )
        .unwrap();
    }
    let store = crate::store::RunStore::open(Some(&db)).unwrap();
    assert_eq!(store.list("p", None, 10).unwrap().len(), 1, "earlier runs stay loadable");
    assert!(store.set_deleted(&finished.id, Some(5)).unwrap());
    assert!(store.list("p", None, 10).unwrap().is_empty());
    assert!(store.get(&finished.id).unwrap().is_none(), "a deleted run is not looked up");
    assert_eq!(store.deleted("p").unwrap()[0].deleted_at_ms, 5);
    assert!(store.set_deleted(&finished.id, None).unwrap());
    assert!(store.get(&finished.id).unwrap().is_some());
    drop(store);
    // Opening again does not migrate twice.
    let store = crate::store::RunStore::open(Some(&db)).unwrap();
    assert_eq!(store.list("p", None, 10).unwrap().len(), 1);
    assert!(store.remove(&finished.id).unwrap());
    let (before, after) = store.compact().unwrap();
    assert!(before.is_some() && after.is_some());
}

#[test]
fn graph_items_go_through_the_same_plan_and_come_back_by_token() {
    use exposition_graph::{ApplyOutcome, ChangeRequest, EntityKind, EntityRef, GraphOperation, RelationKind};
    let e = env();
    let create = |label: &str| -> exposition_graph::EntityId {
        let base = e.fx.graph.status().revision;
        let op = GraphOperation::CreateEntity {
            temp_id: "t".into(),
            kind: EntityKind::Claim,
            roles: vec![],
            label: label.into(),
            key: None,
            attributes: BTreeMap::new(),
            binding: None,
        };
        let request = ChangeRequest { base_revision: base, operations: vec![op], summary: None };
        match e.fx.graph.propose_and_apply(Origin::User, &request).unwrap().1 {
            ApplyOutcome::Applied { created, .. } => created[0].id.clone(),
            other => panic!("{other:?}"),
        }
    };
    let (a, b) = (create("Claim A"), create("Claim B"));
    let base = e.fx.graph.status().revision;
    let relate = GraphOperation::AddRelation {
        kind: RelationKind::DependsOn,
        from: EntityRef::Id { id: a.clone() },
        to: EntityRef::Id { id: b.clone() },
        evidence_kind: None,
        evidence: vec![],
        note: None,
    };
    e.fx.graph
        .propose_and_apply(
            Origin::User,
            &ChangeRequest { base_revision: base, operations: vec![relate], summary: None },
        )
        .unwrap();

    // Routine deletion of a curated item: no decision, restorable.
    let ta = OpTarget::GraphEntity { entity: a.to_string() };
    let plan = e.plan(e.request(delete(false), vec![ta.clone()]));
    assert!(!plan.needs_decision, "{:?}", plan.impacts);
    assert!(plan.effects[0].description.contains("1 relationship"), "{}", plan.effects[0].description);
    let token = e.apply(&plan, &[])[0].restore.clone().expect("restorable");
    assert!(e.fx.graph.entity_detail(&a).is_err());
    // Permanent deletion explains the loss of recovery.
    let tb = OpTarget::GraphEntity { entity: b.to_string() };
    let plan = e.plan(e.request(delete(true), vec![tb]));
    assert_eq!(kinds(&plan), vec![ImpactKind::RecoveryLoss]);
    assert_eq!(e.apply(&plan, &[])[0].restore, None);
    // Restoring A: same identity; the relationship to the purged B cannot come back.
    let restored = restore_data(&e.cx(), &OpRestore { restore: token, on_collision: None }).unwrap();
    assert_eq!(restored.status, OpStatus::Succeeded);
    assert_eq!(restored.created, Some(ta.clone()));
    assert!(restored.message.unwrap_or_default().contains("not restored"));
    assert!(e.fx.graph.entity_detail(&a).unwrap().outgoing.is_empty());
    // Duplicate: a fresh identity.
    let plan = e.plan(e.request(OpKind::Duplicate, vec![ta]));
    let Some(OpTarget::GraphEntity { entity }) = e.apply(&plan, &[])[0].created.clone() else { panic!() };
    assert_ne!(entity, a.to_string());
    // A file-backed resource is deleted as its source file.
    let file = e.fx.graph.resource(&path("notes.md")).unwrap().unwrap();
    let plan = e.plan(e.request(delete(false), vec![OpTarget::GraphEntity { entity: file.id.to_string() }]));
    assert_eq!(plan.refusals[0].code, "ops.useSourceFile");
    // Unknown tokens belong to the file side.
    assert!(restore_data(&e.cx(), &OpRestore { restore: "trash-1234".into(), on_collision: None }).is_none());
}
