//! Every way a model response can end, with the development-only scripted
//! provider standing in for the model (no provider call is made).
//!
//! Only a normal end (`end_turn`/`stop_sequence` without tool calls) completes
//! a task, and only a `tool_use` stop runs tools. Every other stop ends the run
//! with an honest status and a user-visible reason; the response's tool calls
//! are recorded as not run (never executed), and a continuation answers them.
#![cfg(debug_assertions)]

use std::sync::Arc;
use std::time::Duration;

use exposition_agents::credentials::MemoryStore;
use exposition_agents::store::Store;
use exposition_agents::tools::services::UnavailableServices;
use exposition_agents::*;
use exposition_workspace::DocumentService;
use serde_json::{Value, json};
use tempfile::TempDir;

struct Harness {
    service: AgentService,
    project: TempDir,
    data: TempDir,
}

fn harness() -> Harness {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("main.typ"), "= Notes\n").unwrap();
    let data = tempfile::tempdir().unwrap();
    let service = AgentService::new(AgentConfig {
        database: Some(data.path().join("agents.sqlite3")),
        documents: Arc::new(DocumentService::new()),
        workspace: None,
        credentials: Arc::new(MemoryStore::default()),
        services: Arc::new(UnavailableServices),
        runtime: tokio::runtime::Handle::current(),
        allow_scripted: true,
        anthropic_base_url: None,
        retry: None,
        graph: None,
    })
    .unwrap();
    service
        .update_settings(ProviderSettings { provider: ProviderKind::Scripted, ..ProviderSettings::default() })
        .unwrap();
    Harness { service, project, data }
}

fn request(h: &Harness, budgets: Option<Budgets>) -> CreateTaskRequest {
    CreateTaskRequest {
        prompt: "Write the notes".into(),
        workspace_root: h.project.path().display().to_string(),
        scope: TaskScope::Workspace,
        permission: PermissionProfile::AutoApply,
        execution: ExecutionMode::Disabled,
        consent: None,
        budgets,
    }
}

async fn run(h: &Harness, script: Value, budgets: Option<Budgets>) -> TaskSummary {
    h.service.load_script(script).unwrap();
    let task = h.service.create_task(request(h, budgets)).unwrap();
    tokio::time::timeout(Duration::from_secs(20), h.service.join(&task.id)).await.expect("task finished");
    h.service.task(&task.id).unwrap()
}

/// A turn that writes some text and asks to create a file, ending with `stop`.
fn create_turn(stop: Option<&str>) -> Value {
    let mut turn = json!({"steps": [
        {"text": "Creating the summary file"},
        {"toolUse": {"id": "toolu_create", "name": "create_file",
            "input": {"path": "summary.typ", "content": "= Summary\n"}}}
    ]});
    if let Some(stop) = stop {
        turn["stopReason"] = json!(stop);
    }
    turn
}

/// The create_file call of the task's transcript: (status, is_error, result).
fn create_call(h: &Harness, id: &TaskId) -> (ToolCallStatus, bool, String) {
    h.service
        .task_detail(id)
        .unwrap()
        .transcript
        .into_iter()
        .find_map(|item| match item {
            TranscriptItem::ToolCall { tool_use_id, status, is_error, result, .. } if tool_use_id == "toolu_create" => {
                Some((status, is_error, result.unwrap_or_default()))
            }
            _ => None,
        })
        .expect("the create_file call is in the transcript")
}

fn notices(h: &Harness, id: &TaskId) -> Vec<(NoticeLevel, String)> {
    h.service
        .task_detail(id)
        .unwrap()
        .transcript
        .into_iter()
        .filter_map(|item| match item {
            TranscriptItem::Notice { level, message, .. } => Some((level, message)),
            _ => None,
        })
        .collect()
}

fn stopped(task: &TaskSummary) -> (&str, &str) {
    match &task.status {
        TaskStatus::Stopped { stop_reason, message } => (stop_reason, message),
        other => panic!("expected Stopped, got {other:?}"),
    }
}

/// The call was recorded as not run, with a reason, and nothing was created.
fn assert_not_run(h: &Harness, id: &TaskId, reason: &str) {
    let (status, is_error, result) = create_call(h, id);
    assert_eq!(status, ToolCallStatus::NotRun, "{result}");
    assert!(is_error && result.contains("Not executed") && result.contains(reason), "{result}");
    assert!(!h.project.path().join("summary.typ").exists(), "the cut-off tool call never ran");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn max_tokens_mid_tool_use_stops_without_running_it_and_continues_cleanly() {
    let h = harness();
    let script = json!({"turns": [create_turn(Some("max_tokens")), {"steps": [{"text": "Resumed."}]}]});
    let task = run(&h, script, None).await;
    let (reason, message) = stopped(&task);
    assert_eq!(reason, "max_tokens");
    assert!(message.contains("per-turn output limit") && message.contains("tool call was not run"), "{message}");
    assert_not_run(&h, &task.id, "cut off at the per-turn output limit");
    assert!(
        notices(&h, &task.id).iter().any(|(l, m)| *l == NoticeLevel::Warning && m.starts_with("Stopped: ")),
        "{:?}",
        notices(&h, &task.id)
    );

    // Continuing answers the unrun call with its recorded reason, so the
    // history stays valid (every tool_use has a tool_result).
    h.service.continue_task(&task.id, "Go on", None).unwrap();
    tokio::time::timeout(Duration::from_secs(20), h.service.join(&task.id)).await.unwrap();
    assert_eq!(h.service.task(&task.id).unwrap().status, TaskStatus::Completed);
    let store = Store::open(&h.data.path().join("agents.sqlite3")).unwrap();
    let messages = store.messages(&task.id).unwrap();
    let answered = messages
        .iter()
        .filter(|(role, _)| role == "user")
        .flat_map(|(_, content)| content.iter())
        .find(|b| b["type"] == "tool_result" && b["tool_use_id"] == "toolu_create")
        .expect("the cut-off call is answered on continuation");
    assert_eq!(answered["is_error"], json!(true));
    assert!(answered["content"].as_str().unwrap().contains("cut off at the per-turn output limit"), "{answered}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn max_tokens_limited_by_the_output_budget_is_a_budget_stop_and_runs_nothing() {
    let h = harness();
    let budgets = Budgets { max_output_tokens: 2048, ..Budgets::default() };
    let task = run(&h, json!({"turns": [create_turn(Some("max_tokens"))]}), Some(budgets)).await;
    assert!(
        matches!(&task.status, TaskStatus::BudgetExceeded { budget: BudgetKind::OutputTokens, .. }),
        "{:?}",
        task.status
    );
    assert_not_run(&h, &task.id, "output-token budget ran out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_window_exceeded_stops_and_never_reports_completed() {
    let h = harness();
    let task = run(&h, json!({"turns": [create_turn(Some("model_context_window_exceeded"))]}), None).await;
    let (reason, message) = stopped(&task);
    assert_eq!(reason, "model_context_window_exceeded");
    assert!(message.contains("context window") && message.contains("narrower scope"), "{message}");
    assert_not_run(&h, &task.id, "context window");

    // Cut off without a tool call: still not completed.
    let task = run(
        &h,
        json!({"turns": [{"steps": [{"text": "The answer is"}], "stopReason": "model_context_window_exceeded"}]}),
        None,
    )
    .await;
    let (_, message) = stopped(&task);
    assert!(!message.contains("tool call"), "{message}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pause_turn_stops_for_the_user_instead_of_resending_a_prefill() {
    let h = harness();
    let script = json!({"turns": [create_turn(Some("pause_turn")), {"steps": [{"text": "Resumed."}]}]});
    let task = run(&h, script, None).await;
    let (reason, message) = stopped(&task);
    assert_eq!(reason, "pause_turn");
    assert!(message.contains("paused") && message.contains("Continue the task"), "{message}");
    assert_not_run(&h, &task.id, "paused");
    assert_eq!(task.usage.turns, 1, "the paused turn was not silently re-sent");
    h.service.continue_task(&task.id, "Go on", None).unwrap();
    tokio::time::timeout(Duration::from_secs(20), h.service.join(&task.id)).await.unwrap();
    assert_eq!(h.service.task(&task.id).unwrap().status, TaskStatus::Completed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_and_inconsistent_stop_reasons_stop_honestly() {
    let h = harness();
    // A stop reason added to the API later: named in the message, sanitized.
    let task = run(&h, json!({"turns": [create_turn(Some("future_reason<b>\"x\""))]}), None).await;
    let (reason, message) = stopped(&task);
    assert_eq!(reason, "future_reasonbx");
    assert!(message.contains("\"future_reasonbx\"") && message.contains("Continue the task"), "{message}");
    assert_not_run(&h, &task.id, "future_reasonbx");

    // end_turn while asking for a tool: the call is not run, and not "completed".
    let task = run(&h, json!({"turns": [create_turn(Some("end_turn"))]}), None).await;
    assert_eq!(stopped(&task).0, "end_turn");
    assert_not_run(&h, &task.id, "end_turn");

    // tool_use without any tool call.
    let task = run(&h, json!({"turns": [{"steps": [{"text": "Let me"}], "stopReason": "tool_use"}]}), None).await;
    assert_eq!(stopped(&task).0, "tool_use");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refusal_mid_tool_use_runs_nothing_and_normal_ends_complete() {
    let h = harness();
    let script = json!({"turns": [{"steps": [
        {"toolUse": {"id": "toolu_create", "name": "create_file", "input": {"path": "summary.typ", "content": "x"}}},
        {"refusal": {"category": "cyber", "explanation": "declined"}}
    ]}]});
    let task = run(&h, script, None).await;
    assert!(matches!(&task.status, TaskStatus::Refused { category: Some(c), .. } if c == "cyber"), "{:?}", task.status);
    assert!(!h.project.path().join("summary.typ").exists());

    for stop in ["end_turn", "stop_sequence"] {
        let task = run(&h, json!({"turns": [{"steps": [{"text": "All done."}], "stopReason": stop}]}), None).await;
        assert_eq!(task.status, TaskStatus::Completed, "{stop}");
    }
}
