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
use exposition_workspace::ops::{OsTrash, TrashBin, TrashedItem};
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
    fn delete(&self, paths: &[PathBuf]) -> Vec<Result<Option<TrashedItem>, String>> {
        let mut out = Vec::new();
        for (i, p) in paths.iter().enumerate() {
            if p.to_string_lossy().contains("fail") {
                out.push(Err("the Recycle Bin is not available on this drive".into()));
                continue;
            }
            let aside = p.with_file_name(format!("trashed-{i}-{}", now_millis()));
            let moved = fs::rename(p, aside.with_extension("gone")).map(|_| None).map_err(|e| e.to_string());
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
    let plan = ops.plan(&fx.ws, &data, request(OpKind::Delete { permanent: false }, &["a.txt", "notes"])).unwrap();
    assert!(!plan.needs_decision, "graph membership alone is never a reason: {:?}", plan.impacts);
    assert!(plan.choices.is_empty() && plan.refusals.is_empty());
    let result = apply_plan(&ops, &fx, &data, &plan).unwrap();
    assert_eq!(result.outcomes.len(), 2);
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
    assert!(!fx.exists("b.txt"));
    let restored = ops.restore(&fx.ws, &data, OpRestore { restore: token.clone(), on_collision: None }).unwrap();
    assert_eq!(restored.outcomes[0].status, OpStatus::Succeeded, "{:?}", restored.outcomes);
    assert_eq!(fs::read_to_string(fx.file("b.txt")).unwrap(), "b");
    assert_eq!(data.after.lock().unwrap().last().unwrap()[0].created, Some(path("b.txt")));
    let again = ops.restore(&fx.ws, &data, OpRestore { restore: token, on_collision: None }).unwrap_err();
    assert_eq!(again.code, "ops.restoreUnknown");
}
