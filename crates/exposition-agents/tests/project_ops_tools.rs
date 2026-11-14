//! `delete_paths` and `copy_paths` through the central gate and execution,
//! against the REAL file engine (`exposition_workspace::ops`) on disposable
//! folders, with a test trash that can restore (`DirTrash`; the OS Recycle Bin
//! is exercised by the engine's own tests). The orchestrator's data side
//! (graph, workflows) is not part of this crate: `EngineOps` plans and
//! applies the file part as the application's orchestrator does
//! (re-validating the plan, default choices, restore tokens).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use exposition_agents::review;
use exposition_agents::store::{Store, TaskRow};
use exposition_agents::tools::exec::{self, Gate, ToolEnv};
use exposition_agents::tools::services::{
    BuildRequest, CommandRequest, LeanRequest, ProjectOperations, PythonRequest, ServiceError, ServiceKind,
    ToolServices,
};
use exposition_agents::tools::{self, ToolCall};
use exposition_agents::workspace::LocalProjectFiles;
use exposition_agents::*;
use exposition_core::document::{
    ConflictPolicy, DocumentEdit, EditOrigin, TextPatch, TransactionId, TransactionRequest, UndoBehavior,
};
use exposition_core::ops::{OpError, OpOutcome, OpPlan, OpRequest, OpResult};
use exposition_core::path::WorkspacePath;
use exposition_core::text::TextRange;
use exposition_workspace::ops::{ApplyOptions, TrashBin, TrashFailure, TrashFailureKind, TrashedItem};
use exposition_workspace::{DocumentService, WorkspaceService};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

fn p(s: &str) -> WorkspacePath {
    WorkspacePath::parse(s).unwrap()
}

/// A trash folder next to the project: moves entries aside and back.
struct DirTrash {
    dir: PathBuf,
    next: AtomicU64,
}

impl TrashBin for DirTrash {
    fn name(&self) -> &'static str {
        "the test trash"
    }
    fn delete(&self, paths: &[PathBuf]) -> Vec<Result<Option<TrashedItem>, TrashFailure>> {
        paths
            .iter()
            .map(|path| {
                let id = self.next.fetch_add(1, Ordering::Relaxed).to_string();
                std::fs::rename(path, self.dir.join(&id))
                    .map_err(|e| TrashFailure::new(TrashFailureKind::Other, e.to_string()))?;
                Ok(Some(TrashedItem {
                    id,
                    name: path.file_name().unwrap().to_string_lossy().into_owned(),
                    original_parent: path.parent().unwrap().to_path_buf(),
                    deleted_at: 0,
                }))
            })
            .collect()
    }
    fn contains(&self, item: &TrashedItem) -> Result<bool, String> {
        Ok(self.dir.join(&item.id).exists())
    }
    fn restore_to(&self, item: &TrashedItem, parent: &Path, name: &str) -> Result<(), String> {
        std::fs::rename(self.dir.join(&item.id), parent.join(name)).map_err(|e| e.to_string())
    }
    fn purge(&self, _items: &[TrashedItem]) -> Result<(), String> {
        Ok(())
    }
}

/// The file part of the application's orchestrator, over the real engine.
struct EngineOps {
    ws: Arc<WorkspaceService>,
    trash: DirTrash,
    restores: Mutex<HashMap<String, (WorkspacePath, TrashedItem)>>,
    applied: Mutex<Vec<OpRequest>>,
}

impl EngineOps {
    fn compute(&self, request: OpRequest) -> OpPlan {
        let paths = self.ws.plan_paths(&request.kind, &request.targets, &request.actor);
        let needs_decision = paths.impacts.iter().any(|i| i.kind.needs_decision());
        OpPlan {
            plan: "plan".into(),
            request,
            effects: paths.effects,
            refusals: paths.refusals,
            impacts: paths.impacts,
            needs_decision,
            choices: paths.choices,
            plan_hash: paths.fingerprint.join("\n"),
            expires_at_ms: u64::MAX,
        }
    }
}

impl ProjectOperations for EngineOps {
    fn plan(&self, request: OpRequest) -> Result<OpPlan, OpError> {
        Ok(self.compute(request))
    }
    fn apply(&self, plan: &OpPlan) -> Result<OpResult, OpError> {
        let fresh = self.ws.plan_paths(&plan.request.kind, &plan.request.targets, &plan.request.actor);
        if fresh.fingerprint.join("\n") != plan.plan_hash {
            return Err(OpError::new("ops.planStale", "changed"));
        }
        self.applied.lock().unwrap().push(plan.request.clone());
        let choices: BTreeMap<String, String> =
            plan.choices.iter().map(|c| (c.key.clone(), c.default.clone())).collect();
        let outcomes = self.ws.apply_paths(
            &fresh,
            &ApplyOptions { choices: &choices, trash: &self.trash, skip: &plan.refusals, tag: "t" },
            &mut |_| {},
        );
        let mut out: Vec<OpOutcome> = Vec::new();
        for (n, o) in outcomes.into_iter().enumerate() {
            let mut outcome = o.outcome;
            if let (Some(item), Some(path)) = (o.trashed, o.path) {
                let token = format!("trash-{n}-{}", item.id);
                self.restores.lock().unwrap().insert(token.clone(), (path, item));
                outcome.restore = Some(token);
            }
            out.push(outcome);
        }
        Ok(OpResult { plan: plan.plan.clone(), outcomes: out, notices: Vec::new() })
    }
    fn restore(&self, token: &str) -> Result<OpResult, OpError> {
        let (path, item) =
            self.restores.lock().unwrap().remove(token).ok_or_else(|| OpError::new("ops.restoreUnknown", "unknown"))?;
        let outcome = self.ws.restore_trashed(&path, &item, None, &self.trash, "r");
        Ok(OpResult { plan: token.into(), outcomes: vec![outcome], notices: Vec::new() })
    }
}

struct Services {
    ops: Arc<EngineOps>,
}

#[async_trait]
impl ToolServices for Services {
    fn available(&self, kind: ServiceKind) -> bool {
        kind == ServiceKind::ProjectOps
    }
    fn restricted_backend(&self) -> Option<String> {
        None
    }
    async fn build_document(&self, _: BuildRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Err(ServiceError::NotAvailable(ServiceKind::Typesetting))
    }
    async fn run_python(&self, _: PythonRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Err(ServiceError::NotAvailable(ServiceKind::Research))
    }
    async fn check_lean(&self, _: LeanRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Err(ServiceError::NotAvailable(ServiceKind::Lean))
    }
    async fn run_command(&self, _: CommandRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Err(ServiceError::NotAvailable(ServiceKind::Execution))
    }
    fn project_operations(&self) -> Option<Arc<dyn ProjectOperations>> {
        Some(self.ops.clone())
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    ws: Arc<WorkspaceService>,
    files: LocalProjectFiles,
    services: Services,
    store: Store,
    task: TaskId,
}

impl Fixture {
    fn exists(&self, path: &str) -> bool {
        let mut abs = self.root.clone();
        abs.extend(path.split('/'));
        abs.exists()
    }
    fn spec(&self, permission: PermissionProfile, scope: TaskScope) -> TaskSpec {
        TaskSpec {
            prompt: "ops".into(),
            workspace_root: self.root.display().to_string(),
            scope,
            permission,
            execution: ExecutionMode::Disabled,
            consent: ContextConsent::MetadataOnly,
            budgets: Budgets::default(),
            provider: ProviderChoice {
                provider: ProviderKind::Scripted,
                model: "scripted".into(),
                effort: Effort::Low,
            },
        }
    }
    fn env<'a>(&'a self, spec: &'a TaskSpec) -> ToolEnv<'a> {
        ToolEnv {
            task_id: &self.task,
            spec,
            files: &self.files,
            documents: self.ws.documents(),
            workspace: Some(&self.ws),
            services: &self.services,
            store: &self.store,
            graph: None,
        }
    }
    async fn call(&self, spec: &TaskSpec, name: &str, input: Value) -> (Gate, Option<(bool, Value)>) {
        let call = ToolCall::parse(name, &input).unwrap();
        let env = self.env(spec);
        let gate = exec::gate(&call, &env);
        let ran = match &gate {
            Gate::Allowed(authorization) => {
                let mut seen = exec::SeenTexts::default();
                let out = exec::run(call, &env, &mut seen, "t1", authorization.clone(), CancellationToken::new()).await;
                Some((out.is_error, out.content))
            }
            _ => None,
        };
        (gate, ran)
    }
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap().join("project");
    for d in ["notes", "figures", ".exposition/runs"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    std::fs::write(root.join("main.typ"), "= Main\n").unwrap();
    std::fs::write(root.join("notes/a.typ"), "= A\n").unwrap();
    std::fs::write(root.join("figures/plot.png"), "PNG").unwrap();
    std::fs::write(root.join(".exposition/runs/r1.json"), "{}").unwrap();
    let trash = root.parent().unwrap().join("trash");
    std::fs::create_dir_all(&trash).unwrap();
    let ws = WorkspaceService::new(Arc::new(DocumentService::new()), root.parent().unwrap().join("data"));
    ws.open_workspace(&root).unwrap();
    let ops = Arc::new(EngineOps {
        ws: Arc::clone(&ws),
        trash: DirTrash { dir: trash, next: AtomicU64::new(0) },
        restores: Mutex::new(HashMap::new()),
        applied: Mutex::new(Vec::new()),
    });
    let store = Store::open_in_memory().unwrap();
    let task = TaskId::new();
    let files = LocalProjectFiles::open(&root).unwrap();
    let fx = Fixture { _dir: dir, root, ws, files, services: Services { ops }, store, task };
    let spec = fx.spec(PermissionProfile::AutoApply, TaskScope::Workspace);
    fx.store
        .insert_task(&TaskRow {
            id: fx.task.clone(),
            title: "ops".into(),
            created_at_ms: 1,
            updated_at_ms: 1,
            spec,
            status: TaskStatus::Running,
            usage: TaskUsage::default(),
        })
        .unwrap();
    fx
}

fn dirty(fx: &Fixture, path: &str) {
    let id = fx.ws.open_document(&p(path)).unwrap().state.info.id;
    fx.ws
        .documents()
        .apply(TransactionRequest {
            id: TransactionId("t".into()),
            origin: EditOrigin::Source,
            edits: vec![DocumentEdit {
                document: id,
                base_revision: 1,
                patches: vec![TextPatch { range: TextRange::new(0, 0), text: "unsaved ".into() }],
            }],
            undo: UndoBehavior::NewGroup,
            conflict: ConflictPolicy::Reject,
            label: None,
            selection: None,
        })
        .unwrap();
}

fn refused_code(gate: &Gate) -> String {
    match gate {
        Gate::Refused(out) => out.content["code"].as_str().unwrap_or_default().to_owned(),
        other => panic!("not refused: {other:?}"),
    }
}

fn review_row(fx: &Fixture, path: &str) -> exposition_agents::store::TaskFileRow {
    fx.store.task_file(&fx.task, &p(path)).unwrap().expect("recorded for review")
}

#[tokio::test]
async fn an_agent_deletion_goes_to_the_trash_is_reviewable_and_reverts_by_its_restore_token() {
    let fx = fixture();
    let spec = fx.spec(PermissionProfile::AutoApply, TaskScope::Workspace);
    let (gate, ran) = fx.call(&spec, tools::DELETE_PATHS, json!({"paths": ["notes/a.typ"]})).await;
    assert!(matches!(gate, Gate::Allowed(_)));
    let (is_error, content) = ran.unwrap();
    assert!(!is_error, "{content}");
    assert_eq!(content["outcomes"][0]["status"], "succeeded");
    assert!(!fx.exists("notes/a.typ"));
    let row = review_row(&fx, "notes/a.typ");
    assert_eq!(row.kind, ReviewChangeKind::Deleted);
    assert!(row.op_ref.is_some(), "the restore token is kept for revert");
    let ops: &dyn ProjectOperations = fx.services.ops.as_ref();
    let result =
        review::revert_file(&fx.store, &fx.task, fx.ws.documents(), Some(&fx.ws), &fx.files, Some(ops), row).unwrap();
    assert!(result.ok, "{}", result.message);
    assert_eq!(std::fs::read_to_string(fx.root.join("notes/a.typ")).unwrap(), "= A\n");
    assert_eq!(review_row(&fx, "notes/a.typ").state, ReviewState::Reverted);
}

#[tokio::test]
async fn consequences_need_the_user_and_are_shown_on_the_existing_approval() {
    let fx = fixture();
    dirty(&fx, "main.typ");
    // A task that applies changes directly: refused with the consequences.
    let auto = fx.spec(PermissionProfile::AutoApply, TaskScope::Workspace);
    let (gate, _) = fx.call(&auto, tools::DELETE_PATHS, json!({"paths": ["main.typ"]})).await;
    assert_eq!(refused_code(&gate), exec::codes::NEEDS_DECISION);
    if let Gate::Refused(out) = &gate {
        assert!(out.content["impacts"][0]["consequence"].as_str().unwrap().contains("unsaved"), "{}", out.content);
    }
    assert!(fx.exists("main.typ") && fx.services.ops.applied.lock().unwrap().is_empty(), "nothing changed");
    // A task that reviews changes: one approval showing them (no extra prompt).
    let review = fx.spec(PermissionProfile::ReviewEdits, TaskScope::Workspace);
    let call = ToolCall::parse(tools::DELETE_PATHS, &json!({"paths": ["main.typ"]})).unwrap();
    let env = fx.env(&review);
    let Gate::NeedsApproval { title, detail, authorization } = exec::gate(&call, &env) else { panic!("approval") };
    assert!(title.contains("Recycle Bin"), "{title}");
    assert!(detail.contains("main.typ") && detail.contains("unsaved changes"), "{detail}");
    assert!(detail.contains("Keep the unsaved text open in the editor"), "the default that applies: {detail}");
    let confirmed = exec::confirm(&call, &env, authorization).unwrap();
    let mut seen = exec::SeenTexts::default();
    let out = exec::run(call, &env, &mut seen, "t2", confirmed, CancellationToken::new()).await;
    assert!(!out.is_error, "{}", out.content);
    assert!(!fx.exists("main.typ"));
    let doc = fx.ws.documents().find_by_path(&p("main.typ")).expect("the unsaved text is kept in the editor");
    assert!(fx.ws.is_suspended(&doc));
}

#[tokio::test]
async fn agent_operations_stay_within_scope_path_policy_and_the_trash() {
    let fx = fixture();
    let auto = fx.spec(PermissionProfile::AutoApply, TaskScope::Workspace);
    // Application data: refused by the gate's path policy before any plan.
    let (gate, _) = fx.call(&auto, tools::DELETE_PATHS, json!({"paths": [".exposition/runs/r1.json"]})).await;
    assert_eq!(refused_code(&gate), exec::codes::SERVICE_OWNED);
    // Out of the task's scope (as requested).
    let narrow = fx.spec(PermissionProfile::AutoApply, TaskScope::Files { paths: vec![p("notes")] });
    let (gate, _) = fx.call(&narrow, tools::DELETE_PATHS, json!({"paths": ["main.typ"]})).await;
    assert_eq!(refused_code(&gate), exec::codes::OUT_OF_SCOPE);
    // A copy into the project root from a narrow task creates outside its scope.
    let (gate, _) = fx.call(&narrow, tools::COPY_PATHS, json!({"paths": ["notes/a.typ"], "destination": ""})).await;
    assert_eq!(refused_code(&gate), exec::codes::OUT_OF_SCOPE);
    // Absolute or escaping paths are not project paths.
    let (gate, _) = fx.call(&auto, tools::DELETE_PATHS, json!({"paths": ["../outside.txt"]})).await;
    assert!(matches!(gate, Gate::Refused(_)));
    // Read-only tasks change nothing.
    let read_only = fx.spec(PermissionProfile::ReadOnly, TaskScope::Workspace);
    let (gate, _) = fx.call(&read_only, tools::DELETE_PATHS, json!({"paths": ["main.typ"]})).await;
    assert_eq!(refused_code(&gate), exec::codes::READ_ONLY);
    // Nothing deletable: refused with each item's reason.
    let (gate, _) = fx.call(&auto, tools::DELETE_PATHS, json!({"paths": ["notes/missing.typ"]})).await;
    assert_eq!(refused_code(&gate), exec::codes::OPERATION_REFUSED);
    // Never permanent: the request the orchestrator received.
    fx.call(&auto, tools::DELETE_PATHS, json!({"paths": ["figures/plot.png"]})).await;
    let applied = fx.services.ops.applied.lock().unwrap().clone();
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].kind, exposition_core::ops::OpKind::Delete { permanent: false });
    assert_eq!(applied[0].actor, exposition_core::ops::OpActor::Agent { task: fx.task.0.clone() });
    assert!(fx.exists("main.typ") && fx.exists(".exposition/runs/r1.json"));
}

#[tokio::test]
async fn an_agent_copy_is_reviewable_and_reverts_only_while_unchanged() {
    let fx = fixture();
    let auto = fx.spec(PermissionProfile::AutoApply, TaskScope::Workspace);
    let (_, ran) = fx.call(&auto, tools::COPY_PATHS, json!({"paths": ["main.typ"], "destination": "notes"})).await;
    let (is_error, content) = ran.unwrap();
    assert!(!is_error, "{content}");
    assert_eq!(content["outcomes"][0]["created"], "notes/main.typ");
    let row = review_row(&fx, "notes/main.typ");
    assert_eq!(row.kind, ReviewChangeKind::Copied);
    let ops: &dyn ProjectOperations = fx.services.ops.as_ref();
    // Changed since: not removed.
    std::fs::write(fx.root.join("notes/main.typ"), "edited by the user").unwrap();
    let refused =
        review::revert_file(&fx.store, &fx.task, fx.ws.documents(), Some(&fx.ws), &fx.files, Some(ops), row.clone())
            .unwrap();
    assert!(!refused.ok && refused.message.contains("changed"), "{}", refused.message);
    assert!(fx.exists("notes/main.typ"));
    // A second copy, unchanged: reverting moves it to the trash.
    let (_, ran) =
        fx.call(&auto, tools::COPY_PATHS, json!({"paths": ["figures/plot.png"], "destination": "notes"})).await;
    assert!(!ran.unwrap().0);
    let row = review_row(&fx, "notes/plot.png");
    let result =
        review::revert_file(&fx.store, &fx.task, fx.ws.documents(), Some(&fx.ws), &fx.files, Some(ops), row).unwrap();
    assert!(result.ok, "{}", result.message);
    assert!(!fx.exists("notes/plot.png") && fx.exists("figures/plot.png"));
}
