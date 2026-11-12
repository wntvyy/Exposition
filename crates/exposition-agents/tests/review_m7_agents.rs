//! R-m7 independent review: agent file tools against the graph's
//! service-owned paths, through the real task loop (scripted development
//! provider), the real graph service and the real file system.
//!
//! `review_defect_*` tests FAIL while the defect exists.
#![cfg(debug_assertions)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use exposition_agents::credentials::MemoryStore;
use exposition_agents::store::Store;
use exposition_agents::tools;
use exposition_agents::tools::services::{
    BuildRequest, CommandRequest, LeanRequest, PythonRequest, ServiceError, ServiceKind, ToolServices,
};
use exposition_agents::*;
use exposition_core::artifact::{ArtifactId, ArtifactKind, ArtifactRecord};
use exposition_core::path::WorkspacePath;
use exposition_graph::{GraphConfig, GraphService, GraphState};
use exposition_workspace::DocumentService;
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

fn p(s: &str) -> WorkspacePath {
    WorkspacePath::parse(s).unwrap()
}

fn write(root: &Path, path: &str, text: &str) {
    let full = root.join(path);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, text).unwrap();
}

struct Recorder;

#[async_trait]
impl ToolServices for Recorder {
    fn available(&self, _kind: ServiceKind) -> bool {
        true
    }
    fn restricted_backend(&self) -> Option<String> {
        None
    }
    async fn build_document(&self, _: BuildRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Ok(json!({}))
    }
    async fn run_python(&self, _: PythonRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Ok(json!({}))
    }
    async fn check_lean(&self, _: LeanRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Ok(json!({}))
    }
    async fn run_command(&self, _: CommandRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Ok(json!({}))
    }
    fn research_artifact(&self, _run: &str, _artifact: &str) -> Result<ArtifactRecord, ServiceError> {
        Ok(ArtifactRecord {
            id: ArtifactId("fig".into()),
            kind: ArtifactKind::Figure,
            path: Some(p(".exposition/artifacts/run-1/fig.png")),
            scratch_path: None,
            media_type: "image/png".into(),
            size_bytes: 4,
            content_hash: "sha256:00".into(),
        })
    }
}

struct Harness {
    service: AgentService,
    events: broadcast::Receiver<AgentEvent>,
    database: PathBuf,
}

fn harness(graph: Arc<GraphService>, data: &Path) -> Harness {
    let database = data.join("agents-review-m7.sqlite3");
    let service = AgentService::new(AgentConfig {
        database: Some(database.clone()),
        documents: Arc::new(DocumentService::new()),
        workspace: None,
        credentials: Arc::new(MemoryStore::default()),
        services: Arc::new(Recorder),
        runtime: tokio::runtime::Handle::current(),
        allow_scripted: true,
        anthropic_base_url: None,
        retry: None,
        graph: Some(graph),
    })
    .unwrap();
    service
        .update_settings(ProviderSettings { provider: ProviderKind::Scripted, ..ProviderSettings::default() })
        .unwrap();
    service.set_graph_settle(Duration::from_millis(150));
    let events = service.subscribe();
    Harness { service, events, database }
}

fn tool(id: &str, name: &str, input: Value) -> Value {
    json!({"toolUse": {"id": id, "name": name, "input": input}})
}

async fn run(h: &mut Harness, root: &Path, calls: Vec<Value>) -> TaskId {
    let mut turns: Vec<Value> = calls.into_iter().map(|c| json!({"steps": [c]})).collect();
    turns.push(json!({"steps": [{"text": "done"}]}));
    h.service.load_script(json!({ "turns": turns })).unwrap();
    let task = h
        .service
        .create_task(CreateTaskRequest {
            prompt: "review".into(),
            workspace_root: root.display().to_string(),
            scope: TaskScope::Workspace,
            permission: PermissionProfile::AutoApply,
            execution: ExecutionMode::Disabled,
            consent: Some(ContextConsent::ScopeContent),
            budgets: None,
        })
        .unwrap();
    let id = task.id.clone();
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            match h.events.recv().await {
                Ok(AgentEvent::ApprovalRequested { approval }) if approval.task_id == id => {
                    h.service.resolve_approval(&id, &approval.id, true).unwrap();
                }
                Ok(AgentEvent::TaskChanged { task }) if task.id == id && !task.status.is_active() => break,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(e) => panic!("events closed: {e}"),
            }
        }
    })
    .await
    .expect("task finished");
    h.service.join(&id).await;
    id
}

fn result(h: &Harness, id: &TaskId, tool_use_id: &str) -> (bool, String) {
    let store = Store::open(&h.database).unwrap();
    store
        .tool_calls(id)
        .unwrap()
        .into_iter()
        .find(|c| c.tool_use_id == tool_use_id)
        .map(|c| (c.is_error, c.result.unwrap_or_default()))
        .unwrap_or_else(|| panic!("no call {tool_use_id}"))
}

/// R-M7-A1. §7/§9 and the gate's own rule: agents never write or create
/// service-owned records (`.exposition/runs/`, `.exposition/artifacts/`,
/// application metadata) and never touch the graph's storage. The rule
/// compares paths case-sensitively; on Windows `.exposition/Runs/x.json` and
/// `.EXPOSITION/runs/x.json` are the same folder, so an agent forges a run
/// record and an artifact there.
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_defect_case_variants_bypass_the_service_owned_path_rule() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    write(&root, "main.typ", "= Paper\n");
    write(&root, ".exposition/runs/run-1.json", "{\"run\": 1}\n");
    std::fs::create_dir_all(root.join(".exposition/artifacts")).unwrap();
    let data = tempfile::tempdir().unwrap();
    let graph = GraphService::new(GraphConfig { data_dir: data.path().join("graph") });
    assert!(matches!(graph.attach(&root).state, GraphState::Attached));
    let mut h = harness(Arc::clone(&graph), data.path());
    let calls = vec![
        // Control: the exact spelling is refused (held).
        tool("exact", tools::CREATE_FILE, json!({"path": ".exposition/runs/run-7.json", "content": "{}"})),
        tool(
            "runs",
            tools::CREATE_FILE,
            json!({"path": ".exposition/Runs/run-8.json", "content": "{\"forged\":true}"}),
        ),
        tool(
            "upper",
            tools::CREATE_FILE,
            json!({"path": ".EXPOSITION/runs/run-9.json", "content": "{\"forged\":true}"}),
        ),
        tool(
            "art",
            tools::CREATE_FILE,
            json!({"path": ".Exposition/artifacts/run-9/fig.svg", "content": "<svg xmlns=\"http://www.w3.org/2000/svg\"/>"}),
        ),
        tool("meta", tools::CREATE_FILE, json!({"path": ".EXPOSITION/settings-forged.json", "content": "{}"})),
        tool("db", tools::READ_FILE, json!({"path": ".EXPOSITION/graph.sqlite3"})),
    ];
    let id = run(&mut h, &root, calls).await;
    let mut defects = Vec::new();
    let (err, out) = result(&h, &id, "exact");
    println!("{} exact spelling refused — {}", if err { "ok:" } else { "DEFECT:" }, &out[..out.len().min(160)]);
    for (call, file) in [
        ("runs", ".exposition/runs/run-8.json"),
        ("upper", ".exposition/runs/run-9.json"),
        ("art", ".exposition/artifacts/run-9/fig.svg"),
        ("meta", ".exposition/settings-forged.json"),
    ] {
        let (err, out) = result(&h, &id, call);
        let exists = file.split('/').fold(root.clone(), |p, c| p.join(c)).exists();
        if exists || !err {
            println!(
                "DEFECT: agent create_file ({call}) wrote {file}: is_error={err} result={}",
                &out[..out.len().min(200)]
            );
            defects.push(file);
        } else {
            println!("ok: {call} refused — {}", &out[..out.len().min(160)]);
        }
    }
    let (err, out) = result(&h, &id, "db");
    let refused_as_storage = err && out.contains("agents.graphStorage");
    println!(
        "{} read of .EXPOSITION/graph.sqlite3 — is_error={err} {}",
        if refused_as_storage { "ok:" } else { "info:" },
        &out[..out.len().min(200)]
    );
    assert!(defects.is_empty(), "agent wrote service-owned records: {defects:?}");
}
