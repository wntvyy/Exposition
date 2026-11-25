//! Orchestrator tests: a real workspace service over disposable folders, the
//! real operation journal, and a data side double (`FakeData`: lane L2's
//! stores are not part of this lane). Trash: `FakeTrash` where a failure or
//! a crash must be produced on demand (said per test), the real Recycle Bin
//! for the restore token.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use exposition_core::document::{
    ConflictPolicy, DocumentEdit, EditOrigin, TextPatch, TransactionId, TransactionRequest, UndoBehavior,
};
use exposition_core::ops::{
    ImpactKind, OpActor, OpApply, OpConsumer, OpDestination, OpImpact, OpKind, OpOutcome, OpPlan, OpRefusal, OpRequest,
    OpRestore, OpStatus, OpTarget,
};
use exposition_core::path::WorkspacePath;
use exposition_core::text::TextRange;
use exposition_graph::OperationState;
use exposition_workspace::ops::{OsTrash, TrashBin, TrashFailure, TrashFailureKind, TrashedItem};
use exposition_workspace::{DocumentService, WorkspaceService};

use super::*;

/// A disposable folder (src-tauri has no tempfile dev-dependency).
struct Temp(PathBuf);

impl Temp {
    fn new() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "exposition-project-ops-test-{}-{}-{}",
            std::process::id(),
            now_millis(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&p).unwrap();
        Temp(fs::canonicalize(&p).unwrap())
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct FakeData {
    /// Impacts reported for these paths (by path text).
    impacts: Vec<(String, ImpactKind)>,
    /// Refusals reported by `impacts_for_paths`.
    refuse: Vec<String>,
    /// Paths `prepare_paths` cannot prepare.
    unprepared: Vec<String>,
    fingerprint: Mutex<Vec<String>>,
    after: Mutex<Vec<Vec<OpOutcome>>>,
    crash_after_paths: bool,
    /// Paths the project graph keeps as resources.
    resources: Vec<String>,
    /// Data restore tokens this side owns (`<kind>:<id>`).
    data_restores: Mutex<Vec<String>>,
    /// Graph entities the source side asks about.
    details: Vec<exposition_graph::model::EntityDetail>,
}

fn wp(s: &str) -> WorkspacePath {
    WorkspacePath::parse(s).unwrap()
}

fn path(s: &str) -> OpTarget {
    OpTarget::Path { path: wp(s) }
}

impl DataSide for FakeData {
    fn impacts_for_paths(&self, paths: &[WorkspacePath], _kind: &OpKind) -> DataPlan {
        let mut plan = DataPlan { fingerprint: self.fingerprint.lock().unwrap().clone(), ..DataPlan::default() };
        for p in paths {
            for (name, kind) in &self.impacts {
                if p.as_str() == name {
                    plan.impacts.push(OpImpact {
                        kind: *kind,
                        target: OpTarget::Path { path: p.clone() },
                        consumers: vec![OpConsumer {
                            kind: "workflow".into(),
                            id: "wf-1".into(),
                            label: "Fit".into(),
                            path: None,
                        }],
                        message: format!("Workflow Fit needs {p}."),
                    });
                }
            }
            if self.refuse.iter().any(|r| r == p.as_str()) {
                plan.refusals.push(OpRefusal {
                    target: OpTarget::Path { path: p.clone() },
                    code: "ops.protected".into(),
                    message: "kept by the data side".into(),
                });
            }
        }
        plan
    }
    fn plan_data(&self, request: &OpRequest) -> DataPlan {
        crate::project_data::DataPlan {
            refusals: request
                .targets
                .iter()
                .filter(|t| !matches!(t, OpTarget::Path { .. } | OpTarget::External { .. }))
                .map(|t| OpRefusal { target: t.clone(), code: "ops.notSupported".into(), message: "no".into() })
                .collect(),
            ..DataPlan::default()
        }
    }
    fn apply_data(&self, _plan: &OpPlan, _choices: &BTreeMap<String, String>) -> Vec<OpOutcome> {
        Vec::new()
    }
    fn prepare_paths(&self, plan: &OpPlan, _choices: &BTreeMap<String, String>) -> Vec<OpRefusal> {
        plan.request
            .targets
            .iter()
            .filter(|t| matches!(t, OpTarget::Path { path } if self.unprepared.iter().any(|u| u == path.as_str())))
            .map(|t| OpRefusal {
                target: t.clone(),
                code: "ops.activeWork".into(),
                message: "a run could not be stopped".into(),
            })
            .collect()
    }
    fn restore_data(&self, restore: &OpRestore) -> Option<OpOutcome> {
        self.data_restores.lock().unwrap().push(restore.restore.clone());
        restore.restore.starts_with("workflow:").then(|| OpOutcome {
            target: OpTarget::Recoverable { id: restore.restore.clone() },
            status: OpStatus::Succeeded,
            code: Some("ops.restored".into()),
            message: None,
            created: Some(OpTarget::Workflow { workflow: "wf-1".into() }),
            recovery: exposition_core::ops::OpRecovery::None,
            restore: None,
        })
    }
    fn graph_resource(&self, path: &WorkspacePath) -> bool {
        self.resources.iter().any(|r| r == path.as_str())
    }
    fn entity_detail(&self, entity: &str) -> Option<exposition_graph::model::EntityDetail> {
        self.details.iter().find(|d| d.entity.id.as_str() == entity).cloned()
    }
    fn after_paths(&self, outcomes: &[OpOutcome]) {
        self.after.lock().unwrap().push(outcomes.to_vec());
        if self.crash_after_paths {
            panic!("simulated crash between the disk change and the graph update");
        }
    }
}

/// Test trash (said per test where used): moves entries aside, fails for
/// names containing `fail`, or "crashes" after moving the first entry.
#[derive(Default)]
struct FakeTrash {
    crash_after_first: bool,
}

impl TrashBin for FakeTrash {
    fn name(&self) -> &'static str {
        "the test trash"
    }
    fn delete(&self, paths: &[PathBuf]) -> Vec<Result<Option<TrashedItem>, TrashFailure>> {
        let mut out = Vec::new();
        for (i, p) in paths.iter().enumerate() {
            if p.to_string_lossy().contains("fail") {
                out.push(Err(TrashFailure::new(
                    TrashFailureKind::Unavailable,
                    "the Recycle Bin is not available on this drive",
                )));
                continue;
            }
            let aside = p.with_file_name(format!("trashed-{i}-{}", now_millis()));
            let moved = fs::rename(p, aside.with_extension("gone"))
                .map(|_| None)
                .map_err(|e| TrashFailure::new(TrashFailureKind::Other, e.to_string()));
            if self.crash_after_first {
                panic!("simulated crash during file work");
            }
            out.push(moved);
        }
        out
    }
    fn contains(&self, _item: &TrashedItem) -> Result<bool, String> {
        Ok(false)
    }
    fn restore_to(&self, _item: &TrashedItem, _parent: &Path, _name: &str) -> Result<(), String> {
        Err("unsupported".into())
    }
    fn purge(&self, _items: &[TrashedItem]) -> Result<(), String> {
        Ok(())
    }
}

struct Fixture {
    dir: Temp,
    ws: Arc<WorkspaceService>,
}

impl Fixture {
    fn project(&self) -> PathBuf {
        self.dir.0.join("project")
    }
    fn data(&self) -> PathBuf {
        self.dir.0.join("data")
    }
    fn file(&self, p: &str) -> PathBuf {
        let mut out = self.project();
        out.extend(p.split('/'));
        out
    }
    fn exists(&self, p: &str) -> bool {
        fs::symlink_metadata(self.file(p)).is_ok()
    }
    fn ops(&self, trash: impl TrashBin + 'static) -> ProjectOps {
        ProjectOps::new(&self.data(), Arc::new(trash))
    }
    fn restart(&self) -> Arc<WorkspaceService> {
        let ws = WorkspaceService::new(Arc::new(DocumentService::new()), self.data());
        ws.open_workspace(&self.project()).unwrap();
        ws
    }
}

fn fixture() -> Fixture {
    let dir = Temp::new();
    let project = dir.0.join("project");
    for d in ["notes", "figures", ".exposition"] {
        fs::create_dir_all(project.join(d)).unwrap();
    }
    for (p, c) in [
        ("a.txt", "a"),
        ("b.txt", "b"),
        ("fail.txt", "f"),
        ("notes/n1.md", "n1"),
        ("notes/n2.md", "n2"),
        ("figures/plot.png", "PNG"),
        (".exposition/graph.sqlite3", "db"),
    ] {
        fs::write(project.join(p), c).unwrap();
    }
    let ws = WorkspaceService::new(Arc::new(DocumentService::new()), dir.0.join("data"));
    ws.open_workspace(&project).unwrap();
    Fixture { dir, ws }
}

fn request(kind: OpKind, targets: &[&str]) -> OpRequest {
    OpRequest { kind, targets: targets.iter().map(|t| path(t)).collect(), actor: OpActor::User }
}

fn apply_plan(ops: &ProjectOps, fx: &Fixture, data: &FakeData, plan: &OpPlan) -> Result<OpResult, OpError> {
    ops.apply(
        &fx.ws,
        data,
        OpApply { plan: plan.plan.clone(), plan_hash: plan.plan_hash.clone(), choices: BTreeMap::new() },
    )
}

fn records(ops: &ProjectOps, fx: &Fixture) -> Vec<OperationRecord> {
    ops.journal.list(&fx.ws.open_root().unwrap())
}

#[test]
fn routine_deletion_needs_no_decision_and_reports_what_happened() {
    let fx = fixture();
    let ops = fx.ops(FakeTrash::default());
    let data = FakeData::default();
    let plan = ops
        .plan(&fx.ws, &data, request(OpKind::Delete { permanent: false }, &["a.txt", "notes", "notes/n1.md"]))
        .unwrap();
    assert!(!plan.needs_decision, "graph membership alone is never a reason: {:?}", plan.impacts);
    assert!(plan.choices.is_empty() && plan.refusals.is_empty());
    let result = apply_plan(&ops, &fx, &data, &plan).unwrap();
    assert_eq!(result.outcomes.len(), 3, "one outcome per selected target");
    assert_eq!(result.outcomes[2].message.as_deref(), Some("Included in notes."));
    assert!(result.outcomes.iter().all(|o| o.status == OpStatus::Succeeded), "{:?}", result.outcomes);
    assert_eq!(result.outcomes[0].target, path("a.txt"), "request order");
    assert!(!fx.exists("a.txt") && !fx.exists("notes"));
    // The data side got the actual outcomes once.
    let after = data.after.lock().unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].len(), 2);
    // The intent record is complete.
    assert_eq!(records(&ops, &fx)[0].state, OperationState::Completed);
    // A plan is single-use.
    let again = apply_plan(&ops, &fx, &data, &plan).unwrap_err();
    assert_eq!(again.code, "ops.planUnknown");
}

#[test]
fn a_stale_plan_changes_nothing() {
    let fx = fixture();
    let ops = fx.ops(FakeTrash::default());
    let data = FakeData::default();
    // A file appears in the folder after planning: the plan no longer says
    // what would be deleted.
    let plan = ops.plan(&fx.ws, &data, request(OpKind::Delete { permanent: true }, &["notes"])).unwrap();
    fs::write(fx.file("notes/late.md"), "late").unwrap();
    let e = apply_plan(&ops, &fx, &data, &plan).unwrap_err();
    assert_eq!(e.code, "ops.planStale");
    assert!(fx.exists("notes/n1.md") && fx.exists("notes/late.md"), "nothing applied");
    assert!(data.after.lock().unwrap().is_empty());
    assert!(records(&ops, &fx).is_empty(), "nothing recorded");
    // A wrong hash is stale too.
    let plan = ops.plan(&fx.ws, &data, request(OpKind::Delete { permanent: false }, &["a.txt"])).unwrap();
    let e = ops
        .apply(&fx.ws, &data, OpApply { plan: plan.plan.clone(), plan_hash: "0".repeat(64), choices: BTreeMap::new() })
        .unwrap_err();
    assert_eq!(e.code, "ops.planStale");
    // An expired plan is stale.
    let plan = ops.plan(&fx.ws, &data, request(OpKind::Delete { permanent: false }, &["a.txt"])).unwrap();
    lock(&ops.plans).get_mut(&plan.plan).unwrap().plan.expires_at_ms = 1;
    assert_eq!(apply_plan(&ops, &fx, &data, &plan).unwrap_err().code, "ops.planStale");
    // A buffer that became dirty after planning is material.
    let plan = ops.plan(&fx.ws, &data, request(OpKind::Delete { permanent: false }, &["b.txt"])).unwrap();
    let id = fx.ws.open_document(&wp("b.txt")).unwrap().state.info.id;
    fx.ws
        .documents()
        .apply(TransactionRequest {
            id: TransactionId("t".into()),
            origin: EditOrigin::Source,
            edits: vec![DocumentEdit {
                document: id,
                base_revision: 1,
                patches: vec![TextPatch { range: TextRange::new(0, 0), text: "x".into() }],
            }],
            undo: UndoBehavior::NewGroup,
            conflict: ConflictPolicy::Reject,
            label: None,
            selection: None,
        })
        .unwrap();
    assert_eq!(apply_plan(&ops, &fx, &data, &plan).unwrap_err().code, "ops.planStale");
    assert!(fx.exists("a.txt") && fx.exists("b.txt"));
}

#[test]
fn data_impacts_join_one_combined_decision_and_their_changes_make_a_plan_stale() {
    let fx = fixture();
    let ops = fx.ops(FakeTrash::default());
    let data = FakeData {
        impacts: vec![("figures/plot.png".into(), ImpactKind::ManuscriptDependency)],
        ..FakeData::default()
    };
    let id = fx.ws.open_document(&wp("a.txt")).unwrap().state.info.id;
    fx.ws
        .documents()
        .apply(TransactionRequest {
            id: TransactionId("t".into()),
            origin: EditOrigin::Source,
            edits: vec![DocumentEdit {
                document: id,
                base_revision: 1,
                patches: vec![TextPatch { range: TextRange::new(0, 0), text: "x".into() }],
            }],
            undo: UndoBehavior::NewGroup,
            conflict: ConflictPolicy::Reject,
            label: None,
            selection: None,
        })
        .unwrap();
    let plan =
        ops.plan(&fx.ws, &data, request(OpKind::Delete { permanent: false }, &["figures/plot.png", "a.txt"])).unwrap();
    assert!(plan.needs_decision);
    let kinds: Vec<ImpactKind> = plan.impacts.iter().map(|i| i.kind).collect();
    assert!(
        kinds.contains(&ImpactKind::ManuscriptDependency) && kinds.contains(&ImpactKind::UnsavedBuffer),
        "{kinds:?}"
    );
    // Something the data side's decision rests on changes.
    data.fingerprint.lock().unwrap().push("workflow wf-1 revision 2".into());
    assert_eq!(apply_plan(&ops, &fx, &data, &plan).unwrap_err().code, "ops.planStale");
    assert!(fx.exists("figures/plot.png"));
}

#[test]
fn choices_are_validated_and_default_to_the_safe_option() {
    let fx = fixture();
    let ops = fx.ops(FakeTrash::default());
    let data = FakeData::default();
    let id = fx.ws.open_document(&wp("a.txt")).unwrap().state.info.id;
    fx.ws
        .documents()
        .apply(TransactionRequest {
            id: TransactionId("t".into()),
            origin: EditOrigin::Source,
            edits: vec![DocumentEdit {
                document: id.clone(),
                base_revision: 1,
                patches: vec![TextPatch { range: TextRange::new(0, 0), text: "x".into() }],
            }],
            undo: UndoBehavior::NewGroup,
            conflict: ConflictPolicy::Reject,
            label: None,
            selection: None,
        })
        .unwrap();
    let req = || request(OpKind::Delete { permanent: false }, &["a.txt"]);
    for (key, value) in [("nope", "preserve"), ("unsavedBuffers", "overwrite")] {
        let plan = ops.plan(&fx.ws, &data, req()).unwrap();
        let e = ops
            .apply(
                &fx.ws,
                &data,
                OpApply {
                    plan: plan.plan.clone(),
                    plan_hash: plan.plan_hash.clone(),
                    choices: BTreeMap::from([(key.to_owned(), value.to_owned())]),
                },
            )
            .unwrap_err();
        assert_eq!(e.code, "ops.invalidRequest");
        assert!(fx.exists("a.txt"));
    }
    let plan = ops.plan(&fx.ws, &data, req()).unwrap();
    apply_plan(&ops, &fx, &data, &plan).unwrap();
    assert!(!fx.exists("a.txt"));
    assert!(fx.ws.is_suspended(&id), "default: the unsaved text is kept in the editor");
}

#[test]
fn partial_success_and_unprepared_paths_are_reported_as_they_are() {
    let fx = fixture();
    let ops = fx.ops(FakeTrash::default());
    let data = FakeData { unprepared: vec!["b.txt".into()], refuse: vec!["notes".into()], ..FakeData::default() };
    let plan = ops
        .plan(&fx.ws, &data, request(OpKind::Delete { permanent: false }, &["a.txt", "b.txt", "fail.txt", "notes"]))
        .unwrap();
    assert!(plan.refusals.iter().any(|r| r.target == path("notes") && r.code == "ops.protected"));
    let result = apply_plan(&ops, &fx, &data, &plan).unwrap();
    let status: Vec<(String, OpStatus, Option<String>)> =
        result.outcomes.iter().map(|o| (super::target_text(&o.target), o.status, o.code.clone())).collect();
    assert_eq!(
        status,
        [
            ("a.txt".into(), OpStatus::Succeeded, None),
            ("b.txt".into(), OpStatus::Skipped, Some("ops.activeWork".into())),
            ("fail.txt".into(), OpStatus::Failed, Some("ops.trashUnavailable".into())),
            ("notes".into(), OpStatus::Skipped, Some("ops.protected".into())),
        ]
    );
    assert!(!fx.exists("a.txt") && fx.exists("b.txt") && fx.exists("fail.txt") && fx.exists("notes/n1.md"));
    // The data side reconciles exactly what happened.
    let after = data.after.lock().unwrap();
    let succeeded: Vec<&OpTarget> =
        after[0].iter().filter(|o| o.status == OpStatus::Succeeded).map(|o| &o.target).collect();
    assert_eq!(succeeded, [&path("a.txt")]);
}

#[test]
fn an_interruption_between_disk_and_graph_update_is_reconciled_after_restart() {
    let fx = fixture();
    let ops = fx.ops(FakeTrash::default());
    let data = FakeData { crash_after_paths: true, ..FakeData::default() };
    let plan = ops.plan(&fx.ws, &data, request(OpKind::Delete { permanent: false }, &["a.txt", "notes"])).unwrap();
    let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| apply_plan(&ops, &fx, &data, &plan)));
    assert!(crashed.is_err(), "the simulated crash happened");
    assert!(!fx.exists("a.txt") && !fx.exists("notes"), "the disk changed");
    assert!(matches!(records(&ops, &fx)[0].state, OperationState::Pending), "intent left pending");

    // Restart: a new session reconciles once, with what really happened.
    std::thread::sleep(std::time::Duration::from_millis(5));
    let ws = fx.restart();
    let ops2 = fx.ops(FakeTrash::default());
    let data2 = FakeData::default();
    let results = ops2.reconcile_interrupted(&ws, &data2);
    assert_eq!(results.len(), 1);
    assert!(results[0].notices[0].contains("interrupted"), "{:?}", results[0].notices);
    let after = data2.after.lock().unwrap().clone();
    assert_eq!(after.len(), 1);
    let mut done: Vec<(String, OpStatus)> =
        after[0].iter().map(|o| (super::target_text(&o.target), o.status)).collect();
    done.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(done, [("a.txt".into(), OpStatus::Succeeded), ("notes".into(), OpStatus::Succeeded)]);
    assert!(matches!(ops2.journal.list(&ws.open_root().unwrap())[0].state, OperationState::Resolved { .. }));
    assert!(ops2.reconcile_interrupted(&ws, &data2).is_empty(), "once");
    let ops3 = fx.ops(FakeTrash::default());
    assert!(ops3.reconcile_interrupted(&ws, &data2).is_empty(), "resolved records are not reconciled again");
}

#[test]
fn an_interruption_during_file_work_is_reconciled_from_the_disk() {
    let fx = fixture();
    // Test trash that "crashes" right after moving the first entry.
    let ops = fx.ops(FakeTrash { crash_after_first: true });
    let data = FakeData::default();
    let plan = ops.plan(&fx.ws, &data, request(OpKind::Delete { permanent: false }, &["a.txt", "b.txt"])).unwrap();
    let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| apply_plan(&ops, &fx, &data, &plan)));
    assert!(crashed.is_err());
    assert!(!fx.exists("a.txt") && fx.exists("b.txt"));
    std::thread::sleep(std::time::Duration::from_millis(5));
    let ws = fx.restart();
    let ops2 = fx.ops(FakeTrash::default());
    let data2 = FakeData::default();
    ops2.reconcile_interrupted(&ws, &data2);
    let after = data2.after.lock().unwrap().clone();
    let mut done: Vec<(String, OpStatus)> =
        after[0].iter().map(|o| (super::target_text(&o.target), o.status)).collect();
    done.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(done, [("a.txt".into(), OpStatus::Succeeded), ("b.txt".into(), OpStatus::Failed)]);
}

#[test]
fn interface_requests_must_be_the_users() {
    let agent = OpRequest {
        kind: OpKind::Delete { permanent: true },
        targets: vec![path("a.txt")],
        actor: OpActor::Agent { task: "task-1".into() },
    };
    assert_eq!(require_user(&agent).unwrap_err().code, "ops.invalidRequest");
    assert!(require_user(&request(OpKind::Duplicate, &["a.txt"])).is_ok());
}

#[test]
fn copies_report_created_paths_and_the_saved_version_notice() {
    let fx = fixture();
    let ops = fx.ops(FakeTrash::default());
    let data = FakeData::default();
    let id = fx.ws.open_document(&wp("a.txt")).unwrap().state.info.id;
    fx.ws
        .documents()
        .apply(TransactionRequest {
            id: TransactionId("t".into()),
            origin: EditOrigin::Source,
            edits: vec![DocumentEdit {
                document: id,
                base_revision: 1,
                patches: vec![TextPatch { range: TextRange::new(0, 0), text: "x".into() }],
            }],
            undo: UndoBehavior::NewGroup,
            conflict: ConflictPolicy::Reject,
            label: None,
            selection: None,
        })
        .unwrap();
    let plan = ops
        .plan(
            &fx.ws,
            &data,
            request(OpKind::Paste { destination: OpDestination::Folder { folder: Some(wp("notes")) } }, &["a.txt"]),
        )
        .unwrap();
    assert!(!plan.needs_decision, "a saved-version notice is not a decision");
    let result = apply_plan(&ops, &fx, &data, &plan).unwrap();
    assert_eq!(result.outcomes[0].created, Some(path("notes/a.txt")));
    assert!(result.notices.iter().any(|n| n.contains("saved version")), "{:?}", result.notices);
    assert_eq!(fs::read_to_string(fx.file("notes/a.txt")).unwrap(), "a");
}

#[test]
fn the_clipboard_gets_only_copyable_project_entries() {
    let fx = fixture();
    let e = ProjectOps::clipboard_paths(&fx.ws, &[wp(".exposition/graph.sqlite3")]).unwrap_err();
    assert_eq!(e.code, "ops.protected");
    let paths = ProjectOps::clipboard_paths(&fx.ws, &[wp("a.txt"), wp("notes")]).unwrap();
    assert_eq!(paths.len(), 2);
    assert!(paths.iter().all(|p| p.is_absolute() && !p.display().to_string().starts_with(r"\\?\")));
}

#[test]
fn a_restore_token_brings_back_a_real_recycle_bin_item_and_reconciles_it() {
    let fx = fixture();
    let probe = fx.dir.0.join("probe.txt");
    fs::write(&probe, "p").unwrap();
    match OsTrash.delete(&[probe]).pop().unwrap() {
        Ok(Some(item)) => {
            let _ = OsTrash.purge(&[item]);
        }
        other => {
            eprintln!("SKIPPED: the Recycle Bin cannot be used or matched here: {other:?}");
            return;
        }
    }
    let ops = fx.ops(OsTrash);
    let data = FakeData::default();
    let plan = ops.plan(&fx.ws, &data, request(OpKind::Delete { permanent: false }, &["b.txt"])).unwrap();
    let result = apply_plan(&ops, &fx, &data, &plan).unwrap();
    let token = result.outcomes[0].restore.clone().expect("restorable");
    assert!(file_token_shape(&token), "{token}");
    assert!(!fx.exists("b.txt"));
    let restored = ops.restore(&fx.ws, &data, OpRestore { restore: token.clone(), on_collision: None }).unwrap();
    assert_eq!(restored.outcomes[0].status, OpStatus::Succeeded, "{:?}", restored.outcomes);
    assert_eq!(restored.outcomes[0].code.as_deref(), Some("ops.restored"));
    assert_eq!(data.data_restores.lock().unwrap().as_slice(), std::slice::from_ref(&token), "data side first");
    assert_eq!(fs::read_to_string(fx.file("b.txt")).unwrap(), "b");
    assert_eq!(data.after.lock().unwrap().last().unwrap()[0].created, Some(path("b.txt")));
    let again = ops.restore(&fx.ws, &data, OpRestore { restore: token, on_collision: None }).unwrap_err();
    assert_eq!(again.code, "ops.restoreUnknown");
}

/// File restore tokens can never be mistaken for the data side's
/// `<kind>:<id>` tokens.
fn file_token_shape(token: &str) -> bool {
    const DATA_KINDS: &[&str] = &["entity", "relation", "view", "workflow", "workflowRun", "researchRun", "agentTask"];
    !token.contains(':') && !DATA_KINDS.iter().any(|k| token.starts_with(k)) && token.starts_with("trash-")
}

#[test]
fn data_restore_tokens_are_handled_by_the_data_side_first() {
    let fx = fixture();
    let ops = fx.ops(FakeTrash::default());
    let data = FakeData::default();
    let result = ops.restore(&fx.ws, &data, OpRestore { restore: "workflow:wf-1".into(), on_collision: None }).unwrap();
    assert_eq!(result.outcomes[0].created, Some(OpTarget::Workflow { workflow: "wf-1".into() }));
    assert_eq!(result.outcomes[0].code.as_deref(), Some("ops.restored"));
    assert!(records(&ops, &fx).is_empty(), "no file work was recorded");
    // Unknown tokens: neither side knows them.
    let e = ops.restore(&fx.ws, &data, OpRestore { restore: "trash-0-0".into(), on_collision: None }).unwrap_err();
    assert_eq!(e.code, "ops.restoreUnknown");
    for t in ["restore-1-2", "relation:x", "workflowRun:wrun-1"] {
        assert!(!file_token_shape(t), "{t}");
    }
}

#[test]
fn retained_artifacts_are_deleted_through_the_path_plan_with_data_impacts() {
    let fx = fixture();
    let artifact = ".exposition/artifacts/r1/output-1.png";
    fs::create_dir_all(fx.file(".exposition/artifacts/r1")).unwrap();
    fs::write(fx.file(artifact), "PNG").unwrap();
    fs::create_dir_all(fx.file(".exposition/runs")).unwrap();
    fs::write(fx.file(".exposition/runs/r1.json"), "{}").unwrap();
    let ops = fx.ops(FakeTrash::default());
    let data = FakeData {
        resources: vec![artifact.into()],
        impacts: vec![(artifact.into(), ImpactKind::RetainedEvidence)],
        ..FakeData::default()
    };
    let plan = ops
        .plan(&fx.ws, &data, request(OpKind::Delete { permanent: false }, &[artifact, ".exposition/runs/r1.json"]))
        .unwrap();
    assert!(plan.needs_decision);
    assert!(plan.impacts.iter().any(|i| i.kind == ImpactKind::RetainedEvidence && i.target == path(artifact)));
    assert!(plan.refusals.iter().any(|r| r.target == path(".exposition/runs/r1.json") && r.code == "ops.protected"));
    let result = apply_plan(&ops, &fx, &data, &plan).unwrap();
    assert_eq!(result.outcomes[0].status, OpStatus::Succeeded);
    assert!(!fx.exists(artifact) && fx.exists(".exposition/runs/r1.json"));
    // Not a graph resource: still protected.
    fs::write(fx.file(".exposition/artifacts/r1/other.png"), "x").unwrap();
    let plan = ops
        .plan(&fx.ws, &data, request(OpKind::Delete { permanent: false }, &[".exposition/artifacts/r1/other.png"]))
        .unwrap();
    assert_eq!(plan.refusals[0].code, "ops.protected");
}

// ----- restore records that survive a restart ---------------------------------

/// A test trash folder that can restore (the real Recycle Bin is used below
/// where the restart test needs it): ids are unique across instances.
struct DirTrash {
    dir: PathBuf,
}

impl DirTrash {
    fn new(dir: &Path) -> Self {
        fs::create_dir_all(dir).unwrap();
        Self { dir: dir.to_path_buf() }
    }
}

impl TrashBin for DirTrash {
    fn name(&self) -> &'static str {
        "the test trash"
    }
    fn delete(&self, paths: &[PathBuf]) -> Vec<Result<Option<TrashedItem>, TrashFailure>> {
        static N: AtomicU64 = AtomicU64::new(0);
        paths
            .iter()
            .map(|path| {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                let id = format!("{}-{name}", N.fetch_add(1, Ordering::SeqCst));
                fs::rename(path, self.dir.join(&id))
                    .map_err(|e| TrashFailure::new(TrashFailureKind::Other, e.to_string()))?;
                Ok(Some(TrashedItem {
                    id,
                    name: path.file_name().unwrap().to_string_lossy().into_owned(),
                    original_parent: path.parent().unwrap().to_path_buf(),
                    deleted_at: 7,
                }))
            })
            .collect()
    }
    fn contains(&self, item: &TrashedItem) -> Result<bool, String> {
        Ok(self.dir.join(&item.id).exists())
    }
    fn restore_to(&self, item: &TrashedItem, parent: &Path, name: &str) -> Result<(), String> {
        fs::rename(self.dir.join(&item.id), parent.join(name)).map_err(|e| e.to_string())
    }
    fn purge(&self, _items: &[TrashedItem]) -> Result<(), String> {
        Ok(())
    }
}

fn delete_tokens(ops: &ProjectOps, fx: &Fixture, ws: &WorkspaceService, targets: &[&str]) -> Vec<String> {
    let data = FakeData::default();
    let plan = ops.plan(ws, &data, request(OpKind::Delete { permanent: false }, targets)).unwrap();
    let result = ops
        .apply(
            ws,
            &data,
            OpApply { plan: plan.plan.clone(), plan_hash: plan.plan_hash.clone(), choices: BTreeMap::new() },
        )
        .unwrap();
    let _ = fx;
    result.outcomes.iter().map(|o| o.restore.clone().expect("a restore token")).collect()
}

#[test]
fn restore_tokens_survive_a_restart_and_the_item_is_re_identified_first() {
    let fx = fixture();
    let trash = fx.dir.0.join("trash");
    let ops = fx.ops(DirTrash::new(&trash));
    let tokens = delete_tokens(&ops, &fx, &fx.ws, &["a.txt", "b.txt"]);
    assert!(!fx.exists("a.txt") && !fx.exists("b.txt"));
    drop(ops);

    // Restart: a new orchestrator over the same application data.
    let ws = fx.restart();
    let ops = fx.ops(DirTrash::new(&trash));
    let data = FakeData::default();
    let restored = ops.restore(&ws, &data, OpRestore { restore: tokens[0].clone(), on_collision: None }).unwrap();
    assert_eq!(restored.outcomes[0].status, OpStatus::Succeeded, "{:?}", restored.outcomes);
    assert_eq!(restored.outcomes[0].code.as_deref(), Some("ops.restored"));
    assert_eq!(fs::read_to_string(fx.file("a.txt")).unwrap(), "a");
    // Used once.
    let again = ops.restore(&ws, &data, OpRestore { restore: tokens[0].clone(), on_collision: None }).unwrap_err();
    assert_eq!(again.code, "ops.restoreUnknown");

    // Emptied from the bin since: an honest refusal, nothing created, not offered again.
    for entry in fs::read_dir(&trash).unwrap() {
        fs::remove_file(entry.unwrap().path()).unwrap();
    }
    let gone = ops.restore(&ws, &data, OpRestore { restore: tokens[1].clone(), on_collision: None }).unwrap();
    assert_eq!(gone.outcomes[0].status, OpStatus::Failed);
    assert_eq!(gone.outcomes[0].code.as_deref(), Some("ops.restoreUnavailable"));
    assert!(!fx.exists("b.txt"));
    let after = ops.restore(&ws, &data, OpRestore { restore: tokens[1].clone(), on_collision: None }).unwrap_err();
    assert_eq!(after.code, "ops.restoreUnknown");
    // Another restart: still spent.
    let ops = fx.ops(DirTrash::new(&trash));
    let e = ops.restore(&ws, &data, OpRestore { restore: tokens[0].clone(), on_collision: None }).unwrap_err();
    assert_eq!(e.code, "ops.restoreUnknown");
}

#[test]
fn a_persisted_restore_never_overwrites_the_original_path() {
    let fx = fixture();
    let trash = fx.dir.0.join("trash");
    let ops = fx.ops(DirTrash::new(&trash));
    let tokens = delete_tokens(&ops, &fx, &fx.ws, &["a.txt"]);
    fs::write(fx.file("a.txt"), "a NEW file").unwrap();
    let ws = fx.restart();
    let ops = fx.ops(DirTrash::new(&trash));
    let data = FakeData::default();
    let cancelled =
        ops.restore(&ws, &data, OpRestore { restore: tokens[0].clone(), on_collision: Some("cancel".into()) });
    assert_eq!(cancelled.unwrap().outcomes[0].code.as_deref(), Some("ops.restoreCollision"));
    let renamed = ops.restore(&ws, &data, OpRestore { restore: tokens[0].clone(), on_collision: None }).unwrap();
    assert_eq!(renamed.outcomes[0].created, Some(path("a (2).txt")));
    assert_eq!(fs::read_to_string(fx.file("a.txt")).unwrap(), "a NEW file");
    assert_eq!(fs::read_to_string(fx.file("a (2).txt")).unwrap(), "a");
}

#[test]
fn restore_records_are_bounded_and_pruned_once_their_item_is_gone() {
    let fx = fixture();
    let trash = fx.dir.0.join("trash");
    fs::create_dir_all(fx.file("many")).unwrap();
    let names: Vec<String> = (0..MAX_RESTORABLE + 3).map(|i| format!("many/f{i:04}.txt")).collect();
    for n in &names {
        fs::write(fx.file(n), "x").unwrap();
    }
    let ops = fx.ops(DirTrash::new(&trash));
    let targets: Vec<&str> = names.iter().map(String::as_str).collect();
    let tokens = delete_tokens(&ops, &fx, &fx.ws, &targets);
    let root = fx.ws.open_root().unwrap();
    let live = ops.persisted_tokens(&root);
    assert_eq!(live.len(), MAX_RESTORABLE, "capped");
    assert!(!live.contains(&tokens[0]) && live.contains(tokens.last().unwrap()), "the oldest go first");
    // An item that left the bin is pruned when the project is next reconciled.
    let gone = &tokens[tokens.len() - 1];
    let last_name = format!("-f{:04}.txt", MAX_RESTORABLE + 2);
    let item = fs::read_dir(&trash)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.to_string_lossy().ends_with(&last_name))
        .expect("the newest item");
    fs::remove_file(item).unwrap();
    let ws = fx.restart();
    let ops = fx.ops(DirTrash::new(&trash));
    ops.reconcile_interrupted(&ws, &FakeData::default());
    let live = ops.persisted_tokens(&root);
    assert_eq!(live.len(), MAX_RESTORABLE - 1);
    assert!(!live.contains(gone));
}

#[test]
fn a_restore_token_survives_a_restart_with_the_real_recycle_bin() {
    let fx = fixture();
    let probe = fx.dir.0.join("probe.txt");
    fs::write(&probe, "p").unwrap();
    match OsTrash.delete(&[probe]).pop().unwrap() {
        Ok(Some(item)) => {
            let _ = OsTrash.purge(&[item]);
        }
        other => {
            eprintln!("SKIPPED: the Recycle Bin cannot be used or matched here: {other:?}");
            return;
        }
    }
    let ops = fx.ops(OsTrash);
    let tokens = delete_tokens(&ops, &fx, &fx.ws, &["b.txt"]);
    drop(ops);
    let ws = fx.restart();
    let ops = fx.ops(OsTrash);
    let data = FakeData::default();
    let restored = ops.restore(&ws, &data, OpRestore { restore: tokens[0].clone(), on_collision: None }).unwrap();
    assert_eq!(restored.outcomes[0].status, OpStatus::Succeeded, "{:?}", restored.outcomes);
    assert_eq!(fs::read_to_string(fx.file("b.txt")).unwrap(), "b");
}

/// The application's adapter for agent tools, without a Tauri app.
struct TestAgentOps<'a> {
    ops: &'a ProjectOps,
    ws: &'a WorkspaceService,
    data: &'a FakeData,
}

impl exposition_agents::tools::services::ProjectOperations for TestAgentOps<'_> {
    fn plan(&self, request: OpRequest) -> Result<OpPlan, OpError> {
        self.ops.plan(self.ws, self.data, request)
    }
    fn apply(&self, plan: &OpPlan) -> Result<OpResult, OpError> {
        let apply = OpApply { plan: plan.plan.clone(), plan_hash: plan.plan_hash.clone(), choices: BTreeMap::new() };
        self.ops.apply(self.ws, self.data, apply)
    }
    fn restore(&self, token: &str) -> Result<OpResult, OpError> {
        self.ops.restore(self.ws, self.data, OpRestore { restore: token.to_owned(), on_collision: None })
    }
}

#[test]
fn an_agent_deletion_can_be_reverted_after_a_restart() {
    use exposition_agents::store::{Store, TaskRow};
    use exposition_agents::{
        Budgets, ContextConsent, Effort, ExecutionMode, PermissionProfile, ProviderChoice, ProviderKind, TaskId,
        TaskScope, TaskSpec, TaskStatus, TaskUsage,
    };
    let fx = fixture();
    let trash = fx.dir.0.join("trash");
    let task = TaskId::new();
    let store = Store::open_in_memory().unwrap();
    let spec = TaskSpec {
        prompt: "tidy".into(),
        workspace_root: fx.project().display().to_string(),
        scope: TaskScope::Workspace,
        permission: PermissionProfile::AutoApply,
        execution: ExecutionMode::Disabled,
        consent: ContextConsent::MetadataOnly,
        budgets: Budgets::default(),
        provider: ProviderChoice { provider: ProviderKind::Scripted, model: "scripted".into(), effort: Effort::Low },
        approval_mode: exposition_agents::ApprovalMode::Ask,
    };
    store
        .insert_task(&TaskRow {
            id: task.clone(),
            title: "tidy".into(),
            created_at_ms: 1,
            updated_at_ms: 1,
            spec,
            status: TaskStatus::Completed,
            usage: TaskUsage::default(),
        })
        .unwrap();
    {
        // The agent deletes through the orchestrator (its task as the actor).
        let ops = fx.ops(DirTrash::new(&trash));
        let data = FakeData::default();
        let agent = TestAgentOps { ops: &ops, ws: &fx.ws, data: &data };
        use exposition_agents::tools::services::ProjectOperations;
        let plan = agent
            .plan(OpRequest {
                kind: OpKind::Delete { permanent: false },
                targets: vec![path("notes/n1.md")],
                actor: OpActor::Agent { task: task.0.clone() },
            })
            .unwrap();
        let result = agent.apply(&plan).unwrap();
        let token = result.outcomes[0].restore.clone().expect("token");
        exposition_agents::review::record_delete(&store, &task, "t1", &wp("notes/n1.md"), Some(token)).unwrap();
    }
    assert!(!fx.exists("notes/n1.md"));
    // Restart, then the user reverts the agent's change from the review.
    let ws = fx.restart();
    let ops = fx.ops(DirTrash::new(&trash));
    let data = FakeData::default();
    let agent = TestAgentOps { ops: &ops, ws: &ws, data: &data };
    let files = exposition_agents::workspace::LocalProjectFiles::open(&fx.project()).unwrap();
    let row = store.task_file(&task, &wp("notes/n1.md")).unwrap().unwrap();
    let result =
        exposition_agents::review::revert_file(&store, &task, ws.documents(), Some(&ws), &files, Some(&agent), row)
            .unwrap();
    assert!(result.ok, "{}", result.message);
    assert_eq!(fs::read_to_string(fx.file("notes/n1.md")).unwrap(), "n1");
}

// ----- docs-audit fixes ------------------------------------------------------

/// A test trash that "crashes" (panics) right after moving an item out of
/// the bin during a restore, before it is put in place.
struct CrashingRestore(DirTrash);

impl TrashBin for CrashingRestore {
    fn name(&self) -> &'static str {
        self.0.name()
    }
    fn delete(&self, paths: &[PathBuf]) -> Vec<Result<Option<TrashedItem>, TrashFailure>> {
        self.0.delete(paths)
    }
    fn contains(&self, item: &TrashedItem) -> Result<bool, String> {
        self.0.contains(item)
    }
    fn restore_to(&self, item: &TrashedItem, parent: &Path, name: &str) -> Result<(), String> {
        self.0.restore_to(item, parent, name)?;
        panic!("simulated crash between leaving the bin and being put in place");
    }
    fn purge(&self, items: &[TrashedItem]) -> Result<(), String> {
        self.0.purge(items)
    }
}

fn staging_entries(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(exposition_workspace::fsutil::TEMP_MARKER))
        .collect()
}

#[test]
fn an_interrupted_restore_is_completed_after_a_restart_and_its_item_never_deleted() {
    for occupied in [false, true] {
        let fx = fixture();
        let trash = fx.dir.0.join("trash");
        let ops = fx.ops(CrashingRestore(DirTrash::new(&trash)));
        let tokens = delete_tokens(&ops, &fx, &fx.ws, &["notes/n1.md"]);
        let data = FakeData::default();
        let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ops.restore(&fx.ws, &data, OpRestore { restore: tokens[0].clone(), on_collision: None })
        }));
        assert!(crashed.is_err(), "the simulated crash happened");
        // The only copy is out of the bin under a hidden staging name.
        assert_eq!(staging_entries(&fx.file("notes")).len(), 1);
        assert!(!fx.exists("notes/n1.md"));
        if occupied {
            fs::write(fx.file("notes/n1.md"), "a NEW n1").unwrap();
        }
        drop(ops);

        std::thread::sleep(std::time::Duration::from_millis(5));
        let ws = fx.restart();
        let ops = fx.ops(DirTrash::new(&trash));
        let data = FakeData::default();
        let results = ops.reconcile_interrupted(&ws, &data);
        assert_eq!(results.len(), 1, "reported");
        assert!(staging_entries(&fx.file("notes")).is_empty(), "no hidden leftover");
        let expected = if occupied { "notes/n1 (2).md" } else { "notes/n1.md" };
        assert_eq!(fs::read_to_string(fx.file(expected)).unwrap(), "n1", "the restore was completed, never deleted");
        if occupied {
            assert_eq!(fs::read_to_string(fx.file("notes/n1.md")).unwrap(), "a NEW n1", "never overwritten");
        }
        let outcome = &results[0].outcomes[0];
        assert_eq!(outcome.status, OpStatus::Succeeded, "{outcome:?}");
        assert_eq!(outcome.code.as_deref(), Some("ops.restored"));
        assert_eq!(outcome.created, Some(path(expected)));
        let after = data.after.lock().unwrap();
        assert!(after.iter().flatten().any(|o| o.created == Some(path(expected))), "the data side was told");
    }
}

#[test]
fn a_retained_artifact_can_be_restored_into_its_run_folder() {
    let fx = fixture();
    let artifact = ".exposition/artifacts/r1/output-1.png";
    fs::create_dir_all(fx.file(".exposition/artifacts/r1")).unwrap();
    fs::write(fx.file(artifact), "PNG").unwrap();
    let trash = fx.dir.0.join("trash");
    let ops = fx.ops(DirTrash::new(&trash));
    let data = FakeData { resources: vec![artifact.into()], ..FakeData::default() };
    let plan = ops.plan(&fx.ws, &data, request(OpKind::Delete { permanent: false }, &[artifact])).unwrap();
    let result = apply_plan(&ops, &fx, &data, &plan).unwrap();
    let token = result.outcomes[0].restore.clone().expect("restorable");
    assert!(!fx.exists(artifact));
    let restored = ops.restore(&fx.ws, &data, OpRestore { restore: token, on_collision: None }).unwrap();
    assert_eq!(restored.outcomes[0].status, OpStatus::Succeeded, "{:?}", restored.outcomes);
    assert_eq!(fs::read_to_string(fx.file(artifact)).unwrap(), "PNG");
}

#[test]
fn a_restore_refused_for_good_spends_its_token() {
    let fx = fixture();
    let trash = fx.dir.0.join("trash");
    let ops = fx.ops(DirTrash::new(&trash));
    // A record (as an earlier session wrote it) for an item whose original
    // place is the project graph's own storage: never restorable there.
    fs::create_dir_all(&trash).unwrap();
    fs::write(trash.join("x-graph.sqlite3"), "db").unwrap();
    let root = fx.ws.open_root().unwrap();
    let record = ops
        .journal
        .begin(OperationKind::CrossService, &root, None, "Project operation: move to the trash 1 item", vec![])
        .unwrap();
    let item = TrashedItem {
        id: "x-graph.sqlite3".into(),
        name: "graph.sqlite3".into(),
        original_parent: root.join(".exposition"),
        deleted_at: 7,
    };
    let kept = serde_json::to_string(&vec![Restorable {
        token: "trash-test-1".into(),
        path: wp(".exposition/graph.sqlite3x"),
        item,
    }])
    .unwrap();
    ops.journal
        .record_step(&root, &record.id, "restorable", BTreeMap::from([("items".to_owned(), kept)]), true, None)
        .unwrap();
    ops.journal.complete(&root, &record.id).unwrap();
    let data = FakeData::default();
    let first = ops.restore(&fx.ws, &data, OpRestore { restore: "trash-test-1".into(), on_collision: None }).unwrap();
    assert_eq!(first.outcomes[0].code.as_deref(), Some("ops.protected"));
    let again = ops.restore(&fx.ws, &data, OpRestore { restore: "trash-test-1".into(), on_collision: None });
    assert_eq!(again.unwrap_err().code, "ops.restoreUnknown", "not offered again");
    assert!(trash.join("x-graph.sqlite3").exists(), "the item stays in the bin");
}

#[test]
fn messages_name_this_platforms_trash() {
    let fx = fixture();
    let ops = fx.ops(DirTrash::new(&fx.dir.0.join("trash")));
    let e = ops.restore(&fx.ws, &FakeData::default(), OpRestore { restore: "trash-0-0".into(), on_collision: None });
    let e = e.unwrap_err();
    assert_eq!(e.code, "ops.restoreUnknown");
    assert!(e.message.contains("the test trash") && !e.message.contains("Recycle Bin"), "{}", e.message);
}

#[test]
fn a_reconciliation_triggered_by_planning_is_announced() {
    let fx = fixture();
    let ops = fx.ops(FakeTrash::default());
    let data = FakeData { crash_after_paths: true, ..FakeData::default() };
    let plan = ops.plan(&fx.ws, &data, request(OpKind::Delete { permanent: false }, &["a.txt"])).unwrap();
    let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| apply_plan(&ops, &fx, &data, &plan)));
    assert!(crashed.is_err());
    std::thread::sleep(std::time::Duration::from_millis(5));
    let ws = fx.restart();
    let ops = fx.ops(FakeTrash::default());
    let announced: Arc<Mutex<Vec<OpResult>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&announced);
    ops.set_event_sink(Arc::new(move |r: &OpResult| sink.lock().unwrap().push(r.clone())));
    // The first plan of the session reconciles first, and says so.
    ops.plan(&ws, &FakeData::default(), request(OpKind::Duplicate, &["b.txt"])).unwrap();
    let announced = announced.lock().unwrap();
    assert_eq!(announced.len(), 1, "one project-op event for the reconciliation");
    assert!(announced[0].notices[0].contains("interrupted"));
}

// ----- items declared in a source: "Delete from Source" ----------------------------------

mod source_items {
    use exposition_graph::ids::{EntityId, RelationId};
    use exposition_graph::model::{
        AssertionStatus, BindingState, Entity, EntityDetail, Lifecycle, Origin, Relation, SourceBinding,
        SourceSelector, SourceVersion,
    };
    use exposition_graph::reconcile::extract::extract;
    use exposition_graph::reconcile::matching::encode_fingerprint;
    use exposition_graph::source_removal::content_hash;
    use exposition_graph::vocabulary::{EntityKind, RelationKind};

    use super::*;

    pub const PAPER: &str = "\\begin{document}\n\n\\begin{lemma}\\label{lem:closed}\nA closed subset is compact.\n\\end{lemma}\n\n\\begin{theorem}\\label{thm:main}\nThe image is compact.\n\\end{theorem}\n\n\\begin{proof}\nSee Lemma~\\ref{lem:closed}.\n\\end{proof}\n\n\\end{document}\n";
    pub const WITHOUT_THEOREM_AND_PROOF: &str = "\\begin{document}\n\n\\begin{lemma}\\label{lem:closed}\nA closed subset is compact.\n\\end{lemma}\n\n\\end{document}\n";

    pub fn entity(kind: EntityKind, label: &str, key: Option<&str>) -> Entity {
        Entity {
            id: EntityId::mint(),
            kind,
            roles: Vec::new(),
            label: label.into(),
            key: key.map(Into::into),
            lifecycle: Lifecycle::Active,
            tombstone_reason: None,
            revision: 1,
            origin: Origin::Extracted { extractor: "latex/1".into() },
            attributes: Default::default(),
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    pub fn relation(kind: RelationKind, from: &Entity, to: &Entity) -> Relation {
        Relation {
            id: RelationId::mint(),
            kind,
            from: from.id.clone(),
            to: to.id.clone(),
            origin: Origin::Extracted { extractor: "latex/1".into() },
            status: AssertionStatus::Observed,
            evidence_kind: None,
            evidence: Vec::new(),
            from_version: None,
            to_version: None,
            note: None,
            reviewed_statement: None,
            review: None,
            revision: 1,
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    /// The binding the graph would hold for a declaration of `text` at `path`.
    pub fn binding(e: &Entity, path: &str, text: &str, syntax: &str, name: Option<&str>) -> SourceBinding {
        let x = extract(&wp(path), text).unwrap();
        let d = x.declarations.iter().find(|d| d.syntax == syntax && d.name.as_deref() == name).unwrap();
        SourceBinding {
            entity: e.id.clone(),
            path: wp(path),
            selector: SourceSelector::Declaration { syntax: syntax.into(), name: name.map(Into::into), ordinal: None },
            range: Some(d.range),
            version: Some(SourceVersion { content_hash: content_hash(text), revision: None, saved: true }),
            fingerprint: Some(encode_fingerprint(d)),
            state: BindingState::Resolved,
            detail: None,
        }
    }

    pub fn detail(
        entity: Entity,
        bindings: Vec<SourceBinding>,
        incoming: Vec<Relation>,
        neighbors: Vec<Entity>,
    ) -> EntityDetail {
        EntityDetail {
            entity,
            bindings,
            outgoing: Vec::new(),
            incoming,
            neighbors,
            evidence: Vec::new(),
            cited_evidence: Vec::new(),
            evidence_freshness: Vec::new(),
            imported_evidence: Vec::new(),
            coverage: None,
            relations_truncated: false,
            neighbors_truncated: false,
            evidence_truncated: false,
            graph_revision: 1,
        }
    }

    pub struct Paper {
        pub data: FakeData,
        pub theorem: Entity,
        pub proof: Entity,
    }

    /// paper.tex with a theorem, its proof, and a curated claim that depends on the theorem.
    pub fn paper(fx: &Fixture) -> Paper {
        fs::write(fx.file("paper.tex"), PAPER).unwrap();
        let theorem = entity(EntityKind::Theorem, "Theorem thm:main", Some("thm:main"));
        let proof = entity(EntityKind::Proof, "Proof (paper.tex)", None);
        let mut claim = entity(EntityKind::Claim, "Claim C", None);
        claim.origin = Origin::User;
        let thm_binding = binding(&theorem, "paper.tex", PAPER, "latex.env:theorem", Some("thm:main"));
        let proof_binding = binding(&proof, "paper.tex", PAPER, "latex.env:proof", None);
        let proof_of = relation(RelationKind::ProofOf, &proof, &theorem);
        let mut depends = relation(RelationKind::DependsOn, &claim, &theorem);
        depends.origin = Origin::User;
        depends.status = AssertionStatus::Accepted;
        let data = FakeData {
            details: vec![
                detail(theorem.clone(), vec![thm_binding], vec![proof_of, depends], vec![proof.clone(), claim]),
                detail(proof.clone(), vec![proof_binding], Vec::new(), vec![theorem.clone()]),
            ],
            ..FakeData::default()
        };
        Paper { data, theorem, proof }
    }

    pub fn delete(targets: &[&Entity], permanent: bool, actor: OpActor) -> OpRequest {
        OpRequest {
            kind: OpKind::Delete { permanent },
            targets: targets.iter().map(|e| OpTarget::GraphEntity { entity: e.id.to_string() }).collect(),
            actor,
        }
    }

    pub fn buffer(fx: &Fixture, path: &str) -> exposition_core::document::DocumentState {
        let id = fx.ws.documents().find_by_path(&wp(path)).expect("the document is open");
        fx.ws.documents().state(&id).unwrap()
    }
}

use exposition_graph::model::{BindingState, Origin};
use exposition_graph::vocabulary::EntityKind;
use source_items::*;

#[test]
fn a_theorem_is_removed_from_its_source_with_its_proof_as_one_unsaved_undoable_edit() {
    let fx = fixture();
    let ops = fx.ops(FakeTrash::default());
    let p = paper(&fx);
    let plan = ops.plan(&fx.ws, &p.data, delete(&[&p.theorem], false, OpActor::User)).unwrap();
    assert!(plan.refusals.is_empty(), "the data side never sees it: {:?}", plan.refusals);
    assert_eq!(plan.effects.len(), 1);
    assert!(matches!(plan.effects[0].recovery, OpRecovery::SourceEdit { edit: None }));
    assert!(plan.effects[0].description.contains("paper.tex, lines 7–9"), "{}", plan.effects[0].description);
    assert!(plan.needs_decision, "its proof and a claim use it: one decision");
    let kinds: Vec<ImpactKind> = plan.impacts.iter().map(|i| i.kind).collect();
    assert_eq!(kinds, vec![ImpactKind::SourceDependents, ImpactKind::SourceDependents]);
    assert!(plan.impacts.iter().any(|i| i.message.contains("“Claim C” depends on it")), "{:?}", plan.impacts);
    let proofs = plan.choices.iter().find(|c| c.key == "proofs").expect("the proofs choice");
    assert_eq!(proofs.default, "remove");
    // Planning opened nothing and changed nothing.
    assert!(fx.ws.documents().find_by_path(&wp("paper.tex")).is_none());

    let result = apply_plan(&ops, &fx, &p.data, &plan).unwrap();
    assert_eq!(result.outcomes.len(), 1);
    let o = &result.outcomes[0];
    assert_eq!((o.status, o.code.as_deref()), (OpStatus::Succeeded, Some("ops.sourceEdited")), "{o:?}");
    let OpRecovery::SourceEdit { edit: Some(at) } = &o.recovery else { panic!("{o:?}") };
    assert_eq!(at.path, wp("paper.tex"));
    let state = buffer(&fx, "paper.tex");
    assert_eq!(state.text, WITHOUT_THEOREM_AND_PROOF, "theorem and proof removed, one blank line between blocks");
    assert_eq!((at.document.clone(), at.revision), (state.info.id.clone(), state.info.revision));
    assert_ne!(state.info.saved_revision, Some(state.info.revision), "unsaved");
    assert_eq!(fs::read_to_string(fx.file("paper.tex")).unwrap(), PAPER, "nothing written to disk");
    assert_eq!(&state.text[at.at as usize..at.at as usize + 14], "\\end{document}", "the removal point");
    // One undo step brings both back.
    fx.ws.documents().undo(&state.info.id, EditOrigin::Source).unwrap();
    assert_eq!(buffer(&fx, "paper.tex").text, PAPER);
}

#[test]
fn keeping_the_proof_removes_only_the_statement_and_an_edit_after_planning_changes_nothing() {
    let fx = fixture();
    let ops = fx.ops(FakeTrash::default());
    let p = paper(&fx);
    // An open buffer with unsaved text above the theorem: located there, not on disk.
    let opened = fx.ws.open_document(&wp("paper.tex")).unwrap().state;
    let typed = |rev: u64, at: u32, text: &str| TransactionRequest {
        id: TransactionId(format!("t-{rev}-{at}")),
        origin: EditOrigin::Source,
        edits: vec![DocumentEdit {
            document: opened.info.id.clone(),
            base_revision: rev,
            patches: vec![TextPatch { range: TextRange::empty(at), text: text.into() }],
        }],
        undo: UndoBehavior::NewGroup,
        conflict: ConflictPolicy::Reject,
        label: None,
        selection: None,
    };
    fx.ws.documents().apply(typed(opened.info.revision, 17, "% unsaved note\n")).unwrap();

    let plan = ops.plan(&fx.ws, &p.data, delete(&[&p.theorem], false, OpActor::User)).unwrap();
    // More typing after the plan: the plan is stale and nothing is removed.
    let rev = buffer(&fx, "paper.tex").info.revision;
    fx.ws.documents().apply(typed(rev, 0, "%")).unwrap();
    let e = apply_plan(&ops, &fx, &p.data, &plan).unwrap_err();
    assert_eq!(e.code, "ops.planStale");
    assert!(buffer(&fx, "paper.tex").text.contains("\\begin{theorem}"));

    let plan = ops.plan(&fx.ws, &p.data, delete(&[&p.theorem], false, OpActor::User)).unwrap();
    let result = ops
        .apply(
            &fx.ws,
            &p.data,
            OpApply {
                plan: plan.plan.clone(),
                plan_hash: plan.plan_hash.clone(),
                choices: BTreeMap::from([("proofs".to_owned(), "keep".to_owned())]),
            },
        )
        .unwrap();
    assert!(result.outcomes[0].message.as_deref().unwrap().contains("its proof stays"), "{:?}", result.outcomes);
    let text = buffer(&fx, "paper.tex").text;
    assert!(text.starts_with("%\\begin{document}\n% unsaved note\n"), "{text}");
    assert!(!text.contains("\\begin{theorem}") && text.contains("\\begin{proof}"), "{text}");
}

#[test]
fn permanent_agent_unlocated_and_covered_items_are_refused_without_changes() {
    let fx = fixture();
    let ops = fx.ops(FakeTrash::default());
    let p = paper(&fx);
    let permanent = ops.plan(&fx.ws, &p.data, delete(&[&p.theorem], true, OpActor::User)).unwrap();
    assert!(permanent.effects.is_empty());
    assert_eq!(permanent.refusals[0].code, "ops.notSupported");
    assert!(permanent.refusals[0].message.contains("use Delete"), "{}", permanent.refusals[0].message);
    let agent = ops.plan(&fx.ws, &p.data, delete(&[&p.theorem], false, OpActor::Agent { task: "t-1".into() })).unwrap();
    assert_eq!(agent.refusals[0].code, "ops.agentScope");

    // A binding the graph could not resolve: never guessed.
    let mut data = paper(&fx).data;
    data.details[0].bindings[0].state = BindingState::Missing;
    let theorem = data.details[0].entity.clone();
    let unresolved = ops.plan(&fx.ws, &data, delete(&[&theorem], false, OpActor::User)).unwrap();
    assert_eq!(unresolved.refusals[0].code, "ops.sourceDerived");

    // Its source file deleted in the same request: covered, the file goes to the trash.
    let mut req = delete(&[&p.theorem], false, OpActor::User);
    req.targets.push(path("paper.tex"));
    let plan = ops.plan(&fx.ws, &p.data, req).unwrap();
    assert_eq!(plan.refusals[0].code, "ops.covered");
    let result = apply_plan(&ops, &fx, &p.data, &plan).unwrap();
    assert!(!fx.exists("paper.tex"));
    assert!(fx.ws.documents().find_by_path(&wp("paper.tex")).is_none(), "no edit of a deleted file");
    assert_eq!(result.outcomes[0].code.as_deref(), Some("ops.covered"));

    // A proof selected with its theorem: one edit, no decision about the proof.
    let fx = fixture();
    let ops = fx.ops(FakeTrash::default());
    let p = paper(&fx);
    let plan = ops.plan(&fx.ws, &p.data, delete(&[&p.theorem, &p.proof], false, OpActor::User)).unwrap();
    assert!(plan.choices.iter().all(|c| c.key != "proofs"), "{:?}", plan.choices);
    let result = apply_plan(&ops, &fx, &p.data, &plan).unwrap();
    assert!(result.outcomes.iter().all(|o| o.status == OpStatus::Succeeded), "{:?}", result.outcomes);
    assert_eq!(buffer(&fx, "paper.tex").text, WITHOUT_THEOREM_AND_PROOF);
}

/// RL-9 (QC audit), recorded as the plan's present contract: Delete from
/// Source lists what refers to the item in the sources (references that
/// become undefined, its proofs, literal Lean mentions). It is not the
/// impact query: what the item itself supports or formalizes (its outgoing
/// `formalizes` / `evidenceFor`) is not a consequence the plan names, so no
/// decision is asked for it. Whether it should be named is a product
/// decision (see the lane report); this test fails if the behavior changes.
#[test]
fn a_source_removal_plan_names_what_refers_to_the_item_not_what_the_item_supports() {
    let fx = fixture();
    let ops = fx.ops(FakeTrash::default());
    let basel = "namespace LeanBasic\n\ntheorem basel : True := by\n  trivial\n\nend LeanBasic\n";
    fs::create_dir_all(fx.file("lean/LeanBasic")).unwrap();
    fs::write(fx.file("lean/LeanBasic/Basel.lean"), basel).unwrap();
    let mut decl = entity(EntityKind::FormalDeclaration, "LeanBasic.basel", Some("LeanBasic.basel"));
    decl.origin = Origin::Extracted { extractor: "lean/1".into() };
    let mut statement = entity(EntityKind::Theorem, "Basel theorem", None);
    statement.origin = Origin::User;
    let mut formalizes = relation(exposition_graph::vocabulary::RelationKind::Formalizes, &decl, &statement);
    formalizes.origin = Origin::User;
    formalizes.status = exposition_graph::model::AssertionStatus::Accepted;
    let b = binding(&decl, "lean/LeanBasic/Basel.lean", basel, "lean.theorem", Some("LeanBasic.basel"));
    let mut d = detail(decl.clone(), vec![b], Vec::new(), vec![statement.clone()]);
    d.outgoing = vec![formalizes];
    let data = FakeData { details: vec![d], ..FakeData::default() };
    let plan = ops.plan(&fx.ws, &data, delete(&[&decl], false, OpActor::User)).unwrap();
    assert!(plan.refusals.is_empty(), "{:?}", plan.refusals);
    assert!(plan.impacts.is_empty(), "the formalized statement is not named: {:?}", plan.impacts);
    assert!(!plan.needs_decision);
}

#[test]
fn a_lean_declaration_lists_literal_uses_of_its_name_in_other_lean_files() {
    let fx = fixture();
    let ops = fx.ops(FakeTrash::default());
    let basel =
        "namespace LeanBasic\n\n/-- The Basel sum. -/\ntheorem basel : True := by\n  trivial\n\nend LeanBasic\n";
    let user = "import LeanBasic.Basel\n\nnamespace Other\n\ntheorem uses : True := LeanBasic.basel\n\n-- LeanBasic.basel in a comment\nend Other\n";
    fs::create_dir_all(fx.file("lean/LeanBasic")).unwrap();
    fs::create_dir_all(fx.file("lean/.lake/packages")).unwrap();
    fs::write(fx.file("lean/LeanBasic/Basel.lean"), basel).unwrap();
    fs::write(fx.file("lean/User.lean"), user).unwrap();
    fs::write(fx.file("lean/.lake/packages/Dep.lean"), "theorem d : True := LeanBasic.basel\n").unwrap();
    let mut decl = entity(EntityKind::FormalDeclaration, "LeanBasic.basel", Some("LeanBasic.basel"));
    decl.origin = Origin::Extracted { extractor: "lean/1".into() };
    let b = binding(&decl, "lean/LeanBasic/Basel.lean", basel, "lean.theorem", Some("LeanBasic.basel"));
    let data = FakeData { details: vec![detail(decl.clone(), vec![b], Vec::new(), Vec::new())], ..FakeData::default() };
    let plan = ops.plan(&fx.ws, &data, delete(&[&decl], false, OpActor::User)).unwrap();
    assert!(plan.needs_decision);
    let impact = &plan.impacts[0];
    assert_eq!(impact.kind, ImpactKind::SourceDependents);
    let consumers: Vec<(&str, Option<&str>)> =
        impact.consumers.iter().map(|c| (c.label.as_str(), c.path.as_ref().map(|p| p.as_str()))).collect();
    assert_eq!(consumers, vec![("Other.uses", Some("lean/User.lean"))], "not the comment, not .lake: {impact:?}");
    let result = apply_plan(&ops, &fx, &data, &plan).unwrap();
    assert_eq!(result.outcomes[0].status, OpStatus::Succeeded);
    assert_eq!(buffer(&fx, "lean/LeanBasic/Basel.lean").text, "namespace LeanBasic\n\nend LeanBasic\n");
}

/// Delete from Source plans from what the graph read of the item: when that
/// read left relationships or their items out, the plan says its list of
/// uses is incomplete and asks for the decision, instead of reading as "used
/// by nothing" (here a claim depends on the theorem, but the bounded read
/// of its neighbors did not include the claim).
#[test]
fn a_removal_planned_from_an_incomplete_read_of_its_uses_says_so() {
    let fx = fixture();
    let ops = fx.ops(FakeTrash::default());
    fs::write(fx.file("paper.tex"), PAPER).unwrap();
    let theorem = entity(EntityKind::Theorem, "Theorem thm:main", Some("thm:main"));
    let mut claim = entity(EntityKind::Claim, "Claim C", None);
    claim.origin = Origin::User;
    let thm_binding = binding(&theorem, "paper.tex", PAPER, "latex.env:theorem", Some("thm:main"));
    let mut depends = relation(exposition_graph::vocabulary::RelationKind::DependsOn, &claim, &theorem);
    depends.origin = Origin::User;
    depends.status = exposition_graph::model::AssertionStatus::Accepted;
    for (relations_truncated, neighbors_truncated) in [(false, true), (true, false)] {
        let mut d = detail(theorem.clone(), vec![thm_binding.clone()], vec![depends.clone()], Vec::new());
        if relations_truncated {
            // The relation itself was not read.
            d.incoming.clear();
        }
        d.relations_truncated = relations_truncated;
        d.neighbors_truncated = neighbors_truncated;
        let data = FakeData { details: vec![d], ..FakeData::default() };
        let plan = ops.plan(&fx.ws, &data, delete(&[&theorem], false, OpActor::User)).unwrap();
        let case = format!("relations_truncated {relations_truncated}, neighbors_truncated {neighbors_truncated}");
        assert_eq!(plan.effects.len(), 1, "{case}: still removable as a reviewed, unsaved edit");
        assert!(plan.needs_decision, "{case}: an incomplete list of uses is a decision: {:?}", plan.impacts);
        let incomplete: Vec<&OpImpact> =
            plan.impacts.iter().filter(|i| i.message.starts_with("Not every use of “Theorem thm:main”")).collect();
        assert_eq!(incomplete.len(), 1, "{case}: {:?}", plan.impacts);
        assert_eq!(incomplete[0].kind, ImpactKind::SourceDependents);
        assert!(incomplete[0].message.contains("more relationships than this plan reads"), "{}", incomplete[0].message);
    }
}

/// A Lean declaration whose literal uses are looked for in the project's
/// Lean files: a file that cannot be read is not silently skipped.
#[test]
fn a_lean_file_that_cannot_be_read_makes_the_list_of_mentions_incomplete() {
    let fx = fixture();
    let ops = fx.ops(FakeTrash::default());
    let basel = "namespace LeanBasic\n\ntheorem basel : True := by\n  trivial\n\nend LeanBasic\n";
    fs::create_dir_all(fx.file("lean/LeanBasic")).unwrap();
    fs::write(fx.file("lean/LeanBasic/Basel.lean"), basel).unwrap();
    // Not UTF-8 text: the plan cannot look for mentions in it.
    fs::write(fx.file("lean/Broken.lean"), [0x74, 0x68, 0xff, 0xfe, 0x00, 0xc3, 0x28]).unwrap();
    let mut decl = entity(EntityKind::FormalDeclaration, "LeanBasic.basel", Some("LeanBasic.basel"));
    decl.origin = Origin::Extracted { extractor: "lean/1".into() };
    let b = binding(&decl, "lean/LeanBasic/Basel.lean", basel, "lean.theorem", Some("LeanBasic.basel"));
    let data = FakeData { details: vec![detail(decl.clone(), vec![b], Vec::new(), Vec::new())], ..FakeData::default() };
    let plan = ops.plan(&fx.ws, &data, delete(&[&decl], false, OpActor::User)).unwrap();
    assert!(plan.needs_decision, "{:?}", plan.impacts);
    let impact = plan.impacts.iter().find(|i| i.message.starts_with("Not every use of “LeanBasic.basel”"));
    let impact = impact.unwrap_or_else(|| panic!("{:?}", plan.impacts));
    assert_eq!(impact.kind, ImpactKind::SourceDependents);
    assert!(impact.message.contains("1 Lean file could not be read (lean/Broken.lean)"), "{}", impact.message);
}

/// The places that mention a removed Lean name are counted in full: the
/// decision names 25 of them and says how many more there are.
#[test]
fn the_count_of_places_that_mention_a_lean_name_is_complete() {
    let fx = fixture();
    let ops = fx.ops(FakeTrash::default());
    let basel = "namespace LeanBasic\n\ntheorem basel : True := by\n  trivial\n\nend LeanBasic\n";
    fs::create_dir_all(fx.file("lean/LeanBasic")).unwrap();
    fs::write(fx.file("lean/LeanBasic/Basel.lean"), basel).unwrap();
    for i in 0..30 {
        fs::write(fx.file(&format!("lean/U{i:02}.lean")), format!("theorem u{i} : True := LeanBasic.basel\n")).unwrap();
    }
    let mut decl = entity(EntityKind::FormalDeclaration, "LeanBasic.basel", Some("LeanBasic.basel"));
    decl.origin = Origin::Extracted { extractor: "lean/1".into() };
    let b = binding(&decl, "lean/LeanBasic/Basel.lean", basel, "lean.theorem", Some("LeanBasic.basel"));
    let data = FakeData { details: vec![detail(decl.clone(), vec![b], Vec::new(), Vec::new())], ..FakeData::default() };
    let plan = ops.plan(&fx.ws, &data, delete(&[&decl], false, OpActor::User)).unwrap();
    let impact = plan.impacts.iter().find(|i| i.message.contains("is mentioned by name in")).expect("the mentions");
    assert_eq!(impact.consumers.len(), 25, "the decision names a bounded number: {impact:?}");
    assert!(impact.message.contains(" and 25 more."), "5 shown of 30: {}", impact.message);
    assert!(plan.impacts.iter().all(|i| !i.message.starts_with("Not every use")), "{:?}", plan.impacts);
}

// ----- review: Delete from Source with an incomplete list of uses --------------------------

mod incomplete_uses_review {
    use super::*;

    const BASEL: &str = "namespace LeanBasic\n\ntheorem basel : True := by\n  trivial\n\nend LeanBasic\n";

    /// lean/LeanBasic/Basel.lean and the FakeData detail of its declaration.
    fn basel(fx: &Fixture) -> (FakeData, exposition_graph::model::Entity) {
        fs::create_dir_all(fx.file("lean/LeanBasic")).unwrap();
        fs::write(fx.file("lean/LeanBasic/Basel.lean"), BASEL).unwrap();
        let mut decl = entity(EntityKind::FormalDeclaration, "LeanBasic.basel", Some("LeanBasic.basel"));
        decl.origin = Origin::Extracted { extractor: "lean/1".into() };
        let b = binding(&decl, "lean/LeanBasic/Basel.lean", BASEL, "lean.theorem", Some("LeanBasic.basel"));
        (FakeData { details: vec![detail(decl.clone(), vec![b], Vec::new(), Vec::new())], ..FakeData::default() }, decl)
    }

    fn incomplete(plan: &OpPlan) -> Vec<&OpImpact> {
        plan.impacts.iter().filter(|i| i.message.starts_with("Not every use of")).collect()
    }

    /// Not UTF-8: the plan cannot look for mentions in it.
    const BROKEN: [u8; 7] = [0x74, 0x68, 0xff, 0xfe, 0x00, 0xc3, 0x28];

    /// The Lean walk through the whole plan: 2,000 Lean files (its bound)
    /// are all searched and the plan is complete (no decision: nothing
    /// mentions the name); 2,001 are not, and the plan says so.
    #[test]
    fn the_lean_file_bound_is_said_exactly_past_it() {
        for (total, capped) in [(2000usize, false), (2001, true)] {
            let fx = fixture();
            let ops = fx.ops(FakeTrash::default());
            let (data, decl) = basel(&fx);
            fs::create_dir_all(fx.file("lean/many")).unwrap();
            for i in 1..total {
                fs::write(fx.file(&format!("lean/many/F{i:04}.lean")), "theorem f : True := trivial\n").unwrap();
            }
            let plan = ops.plan(&fx.ws, &data, delete(&[&decl], false, OpActor::User)).unwrap();
            let found = incomplete(&plan);
            let case = format!("{total} Lean files: {:?}", plan.impacts);
            if capped {
                assert_eq!(found.len(), 1, "{case}");
                assert!(
                    found[0].message.contains("only the first 2000 Lean files of the project were searched"),
                    "{case}"
                );
                assert!(plan.needs_decision, "{case}");
            } else {
                assert!(found.is_empty(), "{case}");
                assert!(!plan.needs_decision, "{case}");
            }
        }
    }

    /// The incompleteness is part of what the decision rests on: when a Lean
    /// file becomes unreadable after planning, or readable again, the plan
    /// is stale and nothing is removed.
    #[test]
    fn a_change_in_what_could_be_read_makes_the_plan_stale() {
        let fx = fixture();
        let ops = fx.ops(FakeTrash::default());
        let (data, decl) = basel(&fx);
        // Complete when planned; a file that cannot be read appears before apply.
        let plan = ops.plan(&fx.ws, &data, delete(&[&decl], false, OpActor::User)).unwrap();
        assert!(incomplete(&plan).is_empty() && !plan.needs_decision, "{:?}", plan.impacts);
        fs::write(fx.file("lean/Broken.lean"), BROKEN).unwrap();
        assert_eq!(apply_plan(&ops, &fx, &data, &plan).unwrap_err().code, "ops.planStale");
        assert!(fx.ws.documents().find_by_path(&wp("lean/LeanBasic/Basel.lean")).is_none(), "nothing edited");
        // Incomplete when planned; the file becomes readable before apply.
        let plan = ops.plan(&fx.ws, &data, delete(&[&decl], false, OpActor::User)).unwrap();
        assert_eq!(incomplete(&plan).len(), 1, "{:?}", plan.impacts);
        fs::write(fx.file("lean/Broken.lean"), "theorem b : True := trivial\n").unwrap();
        assert_eq!(apply_plan(&ops, &fx, &data, &plan).unwrap_err().code, "ops.planStale");
        assert!(fx.ws.documents().find_by_path(&wp("lean/LeanBasic/Basel.lean")).is_none(), "nothing edited");
    }

    /// Warn, don't block: a plan whose list of uses is incomplete applies
    /// (after the decision) as one unsaved edit in the shared undo history;
    /// no file on disk changes, the unreadable file included.
    #[test]
    fn an_incomplete_plan_applies_only_as_an_unsaved_undoable_edit() {
        let fx = fixture();
        let ops = fx.ops(FakeTrash::default());
        let (data, decl) = basel(&fx);
        fs::write(fx.file("lean/Broken.lean"), BROKEN).unwrap();
        let plan = ops.plan(&fx.ws, &data, delete(&[&decl], false, OpActor::User)).unwrap();
        assert!(plan.needs_decision);
        assert_eq!(plan.effects.len(), 1);
        let result = apply_plan(&ops, &fx, &data, &plan).unwrap();
        assert_eq!(result.outcomes[0].status, OpStatus::Succeeded, "{:?}", result.outcomes);
        let state = buffer(&fx, "lean/LeanBasic/Basel.lean");
        assert_eq!(state.text, "namespace LeanBasic\n\nend LeanBasic\n");
        assert_ne!(state.info.saved_revision, Some(state.info.revision), "unsaved");
        assert_eq!(fs::read_to_string(fx.file("lean/LeanBasic/Basel.lean")).unwrap(), BASEL, "nothing on disk");
        assert_eq!(fs::read(fx.file("lean/Broken.lean")).unwrap(), BROKEN);
        fx.ws.documents().undo(&state.info.id, EditOrigin::Source).unwrap();
        assert_eq!(buffer(&fx, "lean/LeanBasic/Basel.lean").text, BASEL, "Undo brings it back");
    }

    /// An agent is refused Delete from Source whatever the plan could read;
    /// the refused item's consequences are not listed.
    #[test]
    fn an_agent_is_refused_even_when_the_read_was_incomplete() {
        let fx = fixture();
        let ops = fx.ops(FakeTrash::default());
        let (data, decl) = basel(&fx);
        fs::write(fx.file("lean/Broken.lean"), BROKEN).unwrap();
        let plan = ops.plan(&fx.ws, &data, delete(&[&decl], false, OpActor::Agent { task: "t-1".into() })).unwrap();
        assert_eq!(plan.refusals.len(), 1);
        assert_eq!(plan.refusals[0].code, "ops.agentScope");
        assert!(plan.effects.is_empty() && plan.impacts.is_empty() && !plan.needs_decision, "{plan:?}");
    }

    /// The wording: counts, plurals, "and N more", the per-file mention cap,
    /// and project-relative paths only.
    #[test]
    fn the_incompleteness_message_counts_and_names_project_paths() {
        let fx = fixture();
        let ops = fx.ops(FakeTrash::default());
        let (data, decl) = basel(&fx);
        for i in 0..4 {
            fs::write(fx.file(&format!("lean/B{i}.lean")), BROKEN).unwrap();
        }
        let crowded: String = (0..51).map(|i| format!("theorem u{i} : True := LeanBasic.basel\n")).collect();
        fs::write(fx.file("lean/Crowded.lean"), crowded).unwrap();
        let plan = ops.plan(&fx.ws, &data, delete(&[&decl], false, OpActor::User)).unwrap();
        let found = incomplete(&plan);
        assert_eq!(found.len(), 1, "{:?}", plan.impacts);
        assert_eq!(
            found[0].message,
            "Not every use of “LeanBasic.basel” could be checked: 4 Lean files could not be read (lean/B0.lean, \
             lean/B1.lean, lean/B2.lean and 1 more); only the first 50 mentions per file were read in \
             lean/Crowded.lean. Uses not listed here may break too; check the sources before you save."
        );
        let mentions = plan.impacts.iter().find(|i| i.message.contains("is mentioned by name in")).unwrap();
        assert!(mentions.message.contains(" and 45 more."), "50 places read, 5 named: {}", mentions.message);
        let root = fx.project().display().to_string();
        assert!(plan.impacts.iter().all(|i| !i.message.contains(&root)), "{:?}", plan.impacts);
    }

    /// A folder that cannot be listed (here: listing denied to everyone) is
    /// named by its project path.
    #[cfg(windows)]
    #[test]
    fn a_folder_that_cannot_be_listed_is_named_by_its_project_path() {
        struct Denied(PathBuf);
        impl Drop for Denied {
            fn drop(&mut self) {
                let _ = std::process::Command::new("icacls").arg(&self.0).args(["/remove:d", "*S-1-1-0"]).output();
            }
        }
        let fx = fixture();
        let ops = fx.ops(FakeTrash::default());
        let (data, decl) = basel(&fx);
        fs::create_dir_all(fx.file("lean/Locked")).unwrap();
        fs::write(fx.file("lean/Locked/Hidden.lean"), "theorem h : True := LeanBasic.basel\n").unwrap();
        let locked = Denied(fx.file("lean/Locked"));
        let out =
            std::process::Command::new("icacls").arg(&locked.0).args(["/deny", "*S-1-1-0:(RD)"]).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
        assert!(fs::read_dir(&locked.0).is_err(), "the folder cannot be listed");
        let plan = ops.plan(&fx.ws, &data, delete(&[&decl], false, OpActor::User)).unwrap();
        drop(locked);
        let found = incomplete(&plan);
        assert_eq!(found.len(), 1, "{:?}", plan.impacts);
        assert!(found[0].message.contains("1 folder could not be listed (lean/Locked)"), "{}", found[0].message);
        assert!(plan.needs_decision);
    }

    /// FINDING (low): the plan reads "it has more relationships than this
    /// plan reads" whenever the graph's detail read left neighbors out, also
    /// when those were only items the removed item itself uses (outgoing):
    /// every relationship TO it was read and its item is known.
    #[test]
    #[ignore = "review finding: neighbors_truncated from outgoing relations alone makes a complete plan read as incomplete"]
    fn neighbors_left_out_only_on_the_outgoing_side_do_not_make_the_uses_incomplete() {
        use exposition_graph::model::AssertionStatus;
        use exposition_graph::vocabulary::RelationKind;
        let fx = fixture();
        let ops = fx.ops(FakeTrash::default());
        fs::write(fx.file("paper.tex"), PAPER).unwrap();
        let theorem = entity(EntityKind::Theorem, "Theorem thm:main", Some("thm:main"));
        let mut claim = entity(EntityKind::Claim, "Claim C", None);
        claim.origin = Origin::User;
        let thm_binding = binding(&theorem, "paper.tex", PAPER, "latex.env:theorem", Some("thm:main"));
        let mut depends = relation(RelationKind::DependsOn, &claim, &theorem);
        depends.origin = Origin::User;
        depends.status = AssertionStatus::Accepted;
        // The theorem cites 501 sources: its detail lists 500 neighbors (the claim among them).
        let cited: Vec<_> = (0..501).map(|i| entity(EntityKind::Reference, &format!("Ref {i}"), None)).collect();
        let mut d = detail(theorem.clone(), vec![thm_binding], vec![depends], vec![claim.clone()]);
        d.outgoing = cited.iter().map(|c| relation(RelationKind::References, &theorem, c)).collect();
        d.neighbors.extend(cited.iter().take(499).cloned());
        d.neighbors_truncated = true;
        let data = FakeData { details: vec![d], ..FakeData::default() };
        let plan = ops.plan(&fx.ws, &data, delete(&[&theorem], false, OpActor::User)).unwrap();
        assert!(plan.impacts.iter().any(|i| i.message.contains("“Claim C” depends on it")), "{:?}", plan.impacts);
        assert!(incomplete(&plan).is_empty(), "every use was read: {:?}", plan.impacts);
    }
}
