//! Agent build, research, and attachment tools on the real application
//! services (typesetting, TeX builds, research sessions, the workspace and
//! execution services). The scripted provider — development only — drives the
//! loop; every build, run, and attachment is real.
//!
//! LaTeX and Python checks skip (printing SKIPPED) when pdfLaTeX or a Python
//! environment with ipykernel/matplotlib is not installed.
#![cfg(debug_assertions)]

use std::sync::Arc;
use std::time::Duration;

use exposition_agents::credentials::MemoryStore;
use exposition_agents::tools::execution::AppToolServices;
use exposition_agents::*;
use exposition_core::path::WorkspacePath;
use exposition_execution::{ExecutionConfig, ExecutionService, ExecutionSettings, Located};
use exposition_research::launcher::ExecutionLauncher;
use exposition_research::types::{Availability, RuntimeLanguage};
use exposition_research::{ResearchConfig, ResearchService};
use exposition_tex::{TexService, TexServiceConfig};
use exposition_typesetting::{Fonts, TypesettingService};
use exposition_workspace::{DocumentService, WorkspaceService};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::broadcast;

const MAIN: &str = "= Notes\n\nThe squares grow quickly.\n";
const PAPER: &str = "\\documentclass{article}\n\\begin{document}\nHello \\LaTeX.\n\\end{document}\n";

struct Harness {
    service: AgentService,
    execution: ExecutionService,
    research: Arc<ResearchService>,
    documents: Arc<DocumentService>,
    project: TempDir,
    events: broadcast::Receiver<AgentEvent>,
    _data: TempDir,
}

fn harness() -> Harness {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("main.typ"), MAIN).unwrap();
    std::fs::write(project.path().join("paper.tex"), PAPER).unwrap();
    std::fs::write(project.path().join("notes.md"), "# Notes\n").unwrap();
    let data = tempfile::tempdir().unwrap();
    let documents = Arc::new(DocumentService::new());
    let workspace = WorkspaceService::new(Arc::clone(&documents), data.path().join("workspace"));
    workspace.open_workspace(project.path()).unwrap();
    let mut config = ExecutionConfig::new(data.path());
    config.settings = Some(ExecutionSettings::default());
    config.too_broad_to_trust = Some(Vec::new());
    let execution = ExecutionService::with_config(config).unwrap();
    execution.register_workspace_root(project.path()).unwrap();
    let runtime = tokio::runtime::Handle::current();
    let tex =
        TexService::new(execution.clone(), Arc::clone(&workspace), runtime.clone(), TexServiceConfig::new(data.path()))
            .unwrap();
    let typesetting = Arc::new(TypesettingService::new(Arc::clone(&workspace), Fonts::with_system));
    let research = Arc::new(ResearchService::new(ResearchConfig::new(
        data.path().join("research"),
        Arc::new(ExecutionLauncher::new(execution.clone())),
        Arc::clone(&documents),
    )));
    let services = AppToolServices::new(execution.clone())
        .with_workspace(Arc::clone(&workspace))
        .with_typesetting(typesetting)
        .with_tex(tex)
        .with_research(Arc::clone(&research));
    let service = AgentService::new(AgentConfig {
        database: None,
        documents: Arc::clone(&documents),
        workspace: Some(workspace),
        credentials: Arc::new(MemoryStore::default()),
        services: Arc::new(services),
        runtime,
        allow_scripted: true,
        anthropic_base_url: None,
        retry: None,
    })
    .unwrap();
    service
        .update_settings(ProviderSettings { provider: ProviderKind::Scripted, ..ProviderSettings::default() })
        .unwrap();
    let events = service.subscribe();
    Harness { service, execution, research, documents, project, events, _data: data }
}

fn request(h: &Harness, execution: ExecutionMode) -> CreateTaskRequest {
    CreateTaskRequest {
        prompt: "Work on the paper".into(),
        workspace_root: h.project.path().display().to_string(),
        scope: TaskScope::Workspace,
        permission: PermissionProfile::AutoApply,
        execution,
        consent: None,
        budgets: None,
    }
}

fn results(h: &Harness, id: &TaskId) -> Vec<(bool, Value)> {
    h.service
        .task_detail(id)
        .unwrap()
        .transcript
        .into_iter()
        .filter_map(|item| match item {
            TranscriptItem::ToolCall { is_error, result, .. } => {
                Some((is_error, result.and_then(|r| serde_json::from_str(&r).ok()).unwrap_or(Value::Null)))
            }
            _ => None,
        })
        .collect()
}

async fn run_task(h: &mut Harness, script: Value, execution: ExecutionMode, approve: bool) -> TaskSummary {
    h.service.load_script(script).unwrap();
    let task = h.service.create_task(request(h, execution)).unwrap();
    let id = task.id.clone();
    let done = tokio::time::timeout(Duration::from_secs(240), async {
        loop {
            match h.events.recv().await {
                Ok(AgentEvent::ApprovalRequested { approval }) if approval.task_id == id => {
                    h.service.resolve_approval(&id, &approval.id, approve).unwrap();
                }
                Ok(AgentEvent::TaskChanged { task }) if task.id == id && !task.status.is_active() => break,
                _ => {}
            }
        }
    })
    .await;
    assert!(done.is_ok(), "task finished in time");
    h.service.join(&id).await;
    h.service.task(&id).unwrap()
}

fn build(entry: &str, id: &str) -> Value {
    json!({"toolUse": {"id": id, "name": "build_document", "input": {"entry": entry, "engine": "auto"}}})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn typst_builds_report_real_success_and_failure_diagnostics() {
    let mut h = harness();
    let script = json!({"turns": [
        {"steps": [build("main.typ", "b1"), {"toolUse": {"id": "r", "name": "read_file", "input": {"path": "main.typ"}}}]},
        {"steps": [{"toolUse": {"id": "e", "name": "edit_document", "input": {"path": "main.typ", "base_revision": "$ref:r/revision",
            "edits": [{"old_text": "quickly.", "new_text": "quickly. #undefined-function()"}]}}}]},
        {"steps": [build("main.typ", "b2"), build("notes.md", "b3")]},
        {"steps": [{"text": "The build fails; see the diagnostics."}]}
    ]});
    // Builds are allowed even with process execution disabled: Typst compiles in-process.
    let task = run_task(&mut h, script, ExecutionMode::Disabled, false).await;
    assert_eq!(task.status, TaskStatus::Completed);
    let out = results(&h, &task.id);
    let (ok_err, ok) = &out[0];
    assert!(!ok_err, "{ok}");
    assert_eq!(ok["status"], "succeeded");
    assert!(ok["pages"].as_u64().unwrap() >= 1);
    let (failed_err, failed) = &out[3];
    assert!(failed_err, "a failed build is an error result: {failed}");
    assert_eq!(failed["status"], "failed");
    let diagnostic = &failed["diagnostics"][0];
    assert_eq!(diagnostic["severity"], "error");
    assert_eq!(diagnostic["path"], "main.typ");
    assert_eq!(diagnostic["line"], 3, "{diagnostic}");
    assert!(diagnostic["message"].as_str().unwrap().contains("unknown variable"), "{diagnostic}");
    let (md_err, md) = &out[4];
    assert!(md_err);
    assert!(md["error"].as_str().unwrap().contains("Markdown builds is not available in this build"), "{md}");
    // Typst builds do not count as process launches.
    assert_eq!(task.usage.commands, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn latex_builds_need_a_trusted_folder_and_report_refusals() {
    let mut h = harness();
    let latex = json!({"turns": [{"steps": [build("paper.tex", "t")]}, {"steps": [{"text": "done"}]}]});
    // Untrusted folder: the TeX service refuses; the refusal is the tool result.
    let task = run_task(&mut h, latex.clone(), ExecutionMode::ApproveEach, true).await;
    let (is_error, refused) = &results(&h, &task.id)[0];
    assert!(is_error);
    assert_eq!(refused["status"], "refused", "{refused}");
    assert!(refused["reason"].as_str().unwrap().contains("not trusted"), "{refused}");

    // Builds that launch TeX tools are refused when the task disables execution.
    let task = run_task(&mut h, latex.clone(), ExecutionMode::Disabled, true).await;
    assert!(results(&h, &task.id)[0].1["error"].as_str().unwrap().contains("disabled"));

    if !matches!(h.execution.locate_tool("pdflatex"), Located::Found { .. }) {
        eprintln!("SKIPPED: pdflatex not found");
        return;
    }
    h.execution.grant_trust(&h.project.path().display().to_string()).unwrap();
    let task = run_task(&mut h, latex, ExecutionMode::ApproveEach, true).await;
    let (is_error, built) = &results(&h, &task.id)[0];
    assert!(!is_error, "{built}");
    assert_eq!(built["status"], "succeeded");
    assert_eq!(built["engine"], "pdfLatex");
    assert!(built["pages"].as_u64().unwrap() >= 1);
    assert_eq!(task.usage.commands, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn python_runs_in_an_agent_session_and_its_figure_is_attached_and_built() {
    let mut h = harness();
    let report = h.research.environments(false, Vec::new()).await.unwrap();
    let usable = report.runtimes.iter().any(|r| {
        r.language == RuntimeLanguage::Python
            && matches!(r.availability, Availability::Available)
            && r.packages.iter().any(|p| p.name == "matplotlib")
    });
    if !usable {
        eprintln!("SKIPPED: no Python environment with ipykernel and matplotlib");
        return;
    }
    h.execution.grant_trust(&h.project.path().display().to_string()).unwrap();
    let code = "import matplotlib.pyplot as plt\nplt.plot([0, 1, 2, 3], [0, 1, 4, 9])\nplt.title('squares')\nplt.show()\nprint(6 * 7)";
    let script = json!({"turns": [
        {"steps": [{"toolUse": {"id": "p", "name": "run_python", "input": {"code": code}}}]},
        {"steps": [{"text": "Computed."}]}
    ]});
    let task = run_task(&mut h, script, ExecutionMode::ApproveEach, true).await;
    assert_eq!(task.status, TaskStatus::Completed);
    let (is_error, run) = results(&h, &task.id).remove(0);
    assert!(!is_error, "{run}");
    assert_eq!(run["status"], "succeeded");
    assert_eq!(run["stdout"].as_str().unwrap().trim(), "42");
    let run_id = run["run"].as_str().unwrap().to_owned();
    // Provenance: the run manifest names the agent as the session owner.
    let manifest = h.research.run_manifest(&run_id).unwrap();
    assert_eq!(manifest.session.owner, exposition_research::types::SessionOwner::Agent { task_id: task.id.0.clone() });
    assert!(h.project.path().join(run["provenance"].as_str().unwrap()).exists());
    // The agent's session was released when the task ended; no user session was touched.
    assert!(h.research.sessions().is_empty());
    let figure =
        run["artifacts"].as_array().unwrap().iter().find(|a| a["kind"] == "figure").expect("figure artifact").clone();

    // A second task inserts the figure into main.typ and builds it.
    let script = json!({"turns": [
        {"steps": [{"toolUse": {"id": "i", "name": "insert_artifact", "input": {
            "path": "main.typ", "run_id": run_id, "artifact_id": figure["id"], "after_text": "grow quickly.\n",
            "caption": "Squares of small integers", "update_key": ""}}}]},
        {"steps": [build("main.typ", "b")]},
        {"steps": [{"text": "Inserted and built."}]}
    ]});
    let task = run_task(&mut h, script, ExecutionMode::Disabled, true).await;
    let out = results(&h, &task.id);
    assert!(!out[0].0, "{}", out[0].1);
    let key = out[0].1["attachmentKey"].as_str().unwrap().to_owned();
    assert!(!out[1].0, "the document with the figure builds: {}", out[1].1);
    let id = h.documents.find_by_path(&WorkspacePath::parse("main.typ").unwrap()).unwrap();
    let text = h.documents.state(&id).unwrap().text;
    assert!(text.contains(&format!("exposition:begin {key}")) && text.contains("Squares of small integers"), "{text}");
    // The insertion is an agent change under review; reverting restores the source.
    let review = h.service.review(&task.id).unwrap();
    assert_eq!(review.files[0].path.as_str(), "main.typ");
    assert!(h.service.revert(&task.id, None).unwrap()[0].ok);
    assert_eq!(h.documents.state(&id).unwrap().text, MAIN);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn create_file_goes_through_the_workspace_service() {
    let mut h = harness();
    let script = json!({"turns": [
        {"steps": [
            {"toolUse": {"id": "c", "name": "create_file", "input": {"path": "sections/intro.typ", "content": "= Intro\n"}}},
            {"toolUse": {"id": "d", "name": "create_file", "input": {"path": "main.typ", "content": "x"}}}
        ]},
        {"steps": [{"text": "done"}]}
    ]});
    let task = run_task(&mut h, script, ExecutionMode::Disabled, true).await;
    let out = results(&h, &task.id);
    assert!(!out[0].0, "{}", out[0].1);
    assert_eq!(std::fs::read_to_string(h.project.path().join("sections/intro.typ")).unwrap(), "= Intro\n");
    let opened = h.documents.find_by_path(&WorkspacePath::parse("sections/intro.typ").unwrap()).unwrap();
    assert_eq!(h.documents.info(&opened).unwrap().saved_revision, Some(1));
    assert!(out[1].0 && out[1].1["error"].as_str().unwrap().contains("already exists"));
    assert_eq!(std::fs::read_to_string(h.project.path().join("main.typ")).unwrap(), MAIN);
}
