//! R-a1 (independent review of A1): an agent's write into the project's
//! Python environment runs natively through environment discovery.
//!
//! Discovery probes a trusted project's `.venv` interpreter as
//! `<.venv python> -I -c <probe>`; `-I` does not skip `site`, so every
//! `site-packages/*.pth` line that starts with `import` runs. An agent that
//! may only change files (Auto: Files edit, Code off) can create such a file;
//! the next discovery runs it with the user's privileges — including the one
//! the agent's own `install_packages` call triggers to PLAN the setup, before
//! its approval card is shown.
//!
//! Tests marked `#[ignore = "R-a1 F<n>: ..."]` demonstrate a CURRENT defect.
//! Run: `cargo test -p exposition-agents --test review_a1_venv -- --include-ignored --nocapture`.
//! Needs a host Python (`py -3.12`, `py -3`, or `python`); no network.
#![cfg(debug_assertions)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use exposition_agents::credentials::MemoryStore;
use exposition_agents::permissions::{CodeAccess, FilesAccess, PermissionChoice};
use exposition_agents::tools::execution::AppToolServices;
use exposition_agents::*;
use exposition_execution::{ExecutionConfig, ExecutionService, ExecutionSettings};
use exposition_research::launcher::ExecutionLauncher;
use exposition_research::{ResearchConfig, ResearchService};
use exposition_workspace::{DocumentService, WorkspaceService};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::broadcast;

/// Create `<root>/.venv` with a host Python; false when none works.
fn make_venv(root: &Path) -> bool {
    let target = root.join(".venv");
    for (program, args) in [("py", vec!["-3.12"]), ("py", vec!["-3"]), ("python", vec![])] {
        let ok = std::process::Command::new(program)
            .args(&args)
            .args(["-I", "-m", "venv", "--without-pip"])
            .arg(&target)
            .output()
            .is_ok_and(|o| o.status.success());
        if ok && target.join("pyvenv.cfg").is_file() {
            return true;
        }
    }
    false
}

fn site_packages(root: &Path) -> Option<PathBuf> {
    let windows = root.join(".venv").join("Lib").join("site-packages");
    if windows.is_dir() {
        return Some(windows);
    }
    std::fs::read_dir(root.join(".venv").join("lib"))
        .ok()?
        .flatten()
        .map(|e| e.path().join("site-packages"))
        .find(|p| p.is_dir())
}

struct Harness {
    service: AgentService,
    execution: ExecutionService,
    research: Arc<ResearchService>,
    project: TempDir,
    events: broadcast::Receiver<AgentEvent>,
    _data: TempDir,
}

fn harness(project: TempDir) -> Harness {
    let data = tempfile::tempdir().unwrap();
    let documents = Arc::new(DocumentService::new());
    let workspace = WorkspaceService::new(Arc::clone(&documents), data.path().join("workspace"));
    workspace.open_workspace(project.path()).unwrap();
    let graph =
        exposition_graph::GraphService::new(exposition_graph::GraphConfig { data_dir: data.path().join("graph") });
    graph.attach(&std::fs::canonicalize(project.path()).unwrap());
    let mut config = ExecutionConfig::new(data.path());
    config.settings = Some(ExecutionSettings::default());
    config.too_broad_to_trust = Some(Vec::new());
    let execution = ExecutionService::with_config(config).unwrap();
    execution.register_workspace_root(project.path()).unwrap();
    let research = Arc::new(ResearchService::new(ResearchConfig::new(
        data.path().join("research"),
        Arc::new(ExecutionLauncher::new(execution.clone())),
        Arc::clone(&documents),
    )));
    let services = AppToolServices::new(execution.clone())
        .with_workspace(Arc::clone(&workspace))
        .with_research(Arc::clone(&research));
    let service = AgentService::new(AgentConfig {
        database: None,
        documents: Arc::clone(&documents),
        workspace: Some(workspace),
        credentials: Arc::new(MemoryStore::default()),
        services: Arc::new(services),
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
    let events = service.subscribe();
    Harness { service, execution, research, project, events, _data: data }
}

/// Run a task; every approval is declined. Returns (tool results, cards).
async fn run(h: &mut Harness, script: Value, mode: ApprovalMode) -> (Vec<(bool, Value)>, Vec<ApprovalRequest>) {
    h.service.load_script(script).unwrap();
    let task = h
        .service
        .create_task(CreateTaskRequest {
            prompt: "Prepare the analysis environment".into(),
            workspace_root: h.project.path().display().to_string(),
            scope: TaskScope::Workspace,
            permission: PermissionProfile::AutoApply,
            execution: ExecutionMode::ApproveEach,
            consent: Some(ContextConsent::ScopeContent),
            budgets: None,
            approval_mode: Some(mode),
        })
        .unwrap();
    let mut cards = Vec::new();
    tokio::time::timeout(Duration::from_secs(300), async {
        loop {
            match h.events.recv().await {
                Ok(AgentEvent::ApprovalRequested { approval }) if approval.task_id == task.id => {
                    h.service.resolve_approval(&task.id, &approval.id, false).unwrap();
                    cards.push(approval);
                }
                Ok(AgentEvent::TaskChanged { task: t }) if t.id == task.id && !t.status.is_active() => break,
                _ => {}
            }
        }
    })
    .await
    .expect("task finished");
    h.service.join(&task.id).await;
    let results = h
        .service
        .task_detail(&task.id)
        .unwrap()
        .transcript
        .into_iter()
        .filter_map(|item| match item {
            TranscriptItem::ToolCall { is_error, result, .. } => {
                Some((is_error, result.and_then(|r| serde_json::from_str(&r).ok()).unwrap_or(Value::Null)))
            }
            _ => None,
        })
        .collect();
    (results, cards)
}

/// R-a1 F4. Trusted folder with an existing `.venv`; Auto with Files: edit,
/// Code: off (no native code, no setup). The agent creates
/// `.venv/Lib/site-packages/review_a1.pth` and then asks to install a
/// package (the card is declined). Expected: no agent-written code runs —
/// neither during the planning discovery before the card nor at the user's
/// next discovery (Research page open / Check again).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "R-a1 F4: an agent-written .pth in the project .venv runs natively on the next environment discovery"]
async fn review_a1_f4_agent_pth_runs_on_environment_discovery() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("analysis.py"), "print(1)\n").unwrap();
    if !make_venv(project.path()) {
        eprintln!("SKIPPED: no host Python could create a virtual environment");
        return;
    }
    let mut h = harness(project);
    let root = h.project.path().display().to_string();
    h.execution.grant_trust(&root).unwrap();
    h.service
        .set_project_permissions(
            &root,
            PermissionChoice { files: FilesAccess::Edit, code: CodeAccess::Off, setup: false },
        )
        .unwrap();
    let site = site_packages(h.project.path()).expect("the venv has site-packages");
    let relative = site.strip_prefix(h.project.path()).unwrap().display().to_string().replace('\\', "/");
    let outside = tempfile::tempdir().unwrap();
    let marker = outside.path().join("pth-ran.txt");
    let pth = format!(
        "import os; open({:?}, 'a').write('ran pid %d\\n' % os.getpid())\n",
        marker.display().to_string().replace('\\', "/")
    );
    let script = json!({"turns": [
        {"steps": [{"toolUse": {"id": "c", "name": "create_file", "input": {"path": format!("{relative}/review_a1.pth"), "content": pth}}}]},
        {"steps": [{"toolUse": {"id": "i", "name": "install_packages", "input": {"packages": ["tomli"]}}}]},
        {"steps": [{"text": "done"}]}
    ]});
    let (out, cards) = run(&mut h, script, ApprovalMode::Auto).await;
    eprintln!("create_file: error={} {}", out[0].0, out[0].1);
    eprintln!(
        "install_packages: error={} {}",
        out.get(1).map(|o| o.0).unwrap_or_default(),
        out.get(1).map(|o| o.1.to_string()).unwrap_or_default()
    );
    eprintln!("cards: {:?}", cards.iter().map(|c| c.title.clone()).collect::<Vec<_>>());
    let during_planning = marker.exists();
    eprintln!("agent .pth ran during the setup planning (before/while the card was shown): {during_planning}");
    if during_planning {
        eprintln!("marker: {}", std::fs::read_to_string(&marker).unwrap_or_default().trim());
    }
    let _ = std::fs::remove_file(&marker);
    // The user opens the Research page (discovery with refresh).
    let report = h.research.environments(true).await;
    let after_refresh = marker.exists();
    eprintln!("agent .pth ran at the user's next discovery: {after_refresh}");
    if let Ok(report) = &report {
        let venv: Vec<_> = report
            .runtimes
            .iter()
            .filter(|r| r.project_folder.is_some())
            .map(|r| (r.display_name.clone(), r.availability.clone()))
            .collect();
        eprintln!("project environments after refresh: {venv:?}");
    }
    assert!(
        !during_planning && !after_refresh,
        "DEFECT: agent-written .pth ran natively (planning: {during_planning}, refresh: {after_refresh})"
    );
}
