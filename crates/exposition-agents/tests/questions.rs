//! Structured questions (`ask_user`), "Chat about it" discussions, and
//! figure references, through the task loop with the development-only
//! scripted provider and the real document service.
//!
//! The scripted provider cannot establish live model behavior; these tests
//! verify the application side: the worker blocks like an approval wait, the
//! rest of the tool batch is held, answers are validated server-side
//! (revision, options, idempotency), a discussion is one bounded turn in the
//! same worker and budgets, cancel and restart supersede, and figures are
//! recorded from the run manifest and served only to their own task.
#![cfg(debug_assertions)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use exposition_agents::credentials::MemoryStore;
use exposition_agents::tools::services::{
    BuildRequest, CommandRequest, LeanRequest, PythonRequest, ServiceError, ServiceKind, ToolServices,
    UnavailableServices,
};
use exposition_agents::*;
use exposition_core::artifact::{ArtifactId, ArtifactKind, ArtifactRecord};
use exposition_core::path::WorkspacePath;
use exposition_workspace::DocumentService;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

struct Harness {
    service: AgentService,
    project: TempDir,
    events: broadcast::Receiver<AgentEvent>,
    data: TempDir,
}

fn open(data: &TempDir, services: Arc<dyn ToolServices>) -> AgentService {
    let service = AgentService::new(AgentConfig {
        database: Some(data.path().join("agents.sqlite3")),
        documents: Arc::new(DocumentService::new()),
        workspace: None,
        credentials: Arc::new(MemoryStore::default()),
        services,
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
    service
}

fn harness_with(services: Arc<dyn ToolServices>) -> Harness {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("main.typ"), "= Notes\nThe cat sat on the mat.\n").unwrap();
    let data = tempfile::tempdir().unwrap();
    let service = open(&data, services);
    let events = service.subscribe();
    Harness { service, project, events, data }
}

fn harness() -> Harness {
    harness_with(Arc::new(UnavailableServices))
}

fn request(h: &Harness, permission: PermissionProfile) -> CreateTaskRequest {
    CreateTaskRequest {
        prompt: "Tidy the notes".into(),
        workspace_root: h.project.path().display().to_string(),
        scope: TaskScope::Workspace,
        permission,
        execution: ExecutionMode::Disabled,
        consent: None,
        budgets: None,
        approval_mode: None,
        provider: None,
        attachments: None,
    }
}

fn option(id: &str, label: &str) -> Value {
    json!({"id": id, "label": label, "consequence": "", "recommended": false})
}

/// An `ask_user` call with one question (options a, b).
fn ask(id: &str) -> Value {
    json!({"toolUse": {"id": id, "name": "ask_user", "input": {
        "questions": [{"prompt": "Which tone?", "explanation": "The notes are informal now.",
            "options": [option("a", "Formal"), {"id": "b", "label": "Keep informal", "consequence": "No rewording", "recommended": true}]}],
        "allowCustom": true}}})
}

async fn wait_for(events: &mut broadcast::Receiver<AgentEvent>, pred: impl Fn(&AgentEvent) -> bool) -> AgentEvent {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match events.recv().await {
                Ok(event) if pred(&event) => return event,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(e) => panic!("event channel closed: {e}"),
            }
        }
    })
    .await
    .expect("expected event")
}

async fn requested(events: &mut broadcast::Receiver<AgentEvent>) -> QuestionRequest {
    match wait_for(events, |e| matches!(e, AgentEvent::QuestionRequested { .. })).await {
        AgentEvent::QuestionRequested { request } => request,
        _ => unreachable!(),
    }
}

async fn finish(h: &Harness, id: &TaskId) -> TaskSummary {
    tokio::time::timeout(Duration::from_secs(20), h.service.join(id)).await.expect("task finished");
    h.service.task(id).unwrap()
}

/// Poll the task detail until `pred` holds (bounded).
async fn until(h: &Harness, id: &TaskId, pred: impl Fn(&TaskDetail) -> bool) -> TaskDetail {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let detail = h.service.task_detail(id).unwrap();
            if pred(&detail) {
                return detail;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("condition reached")
}

fn pick(question: &str, option: &str) -> QuestionAnswer {
    QuestionAnswer { question_id: question.into(), value: QuestionValue::Option { option_id: option.into() } }
}

fn call<'a>(detail: &'a TaskDetail, id: &str) -> Option<&'a TranscriptItem> {
    detail.transcript.iter().find(|i| matches!(i, TranscriptItem::ToolCall { tool_use_id, .. } if tool_use_id == id))
}

fn call_status(detail: &TaskDetail, id: &str) -> Option<ToolCallStatus> {
    match call(detail, id) {
        Some(TranscriptItem::ToolCall { status, .. }) => Some(*status),
        _ => None,
    }
}

fn call_result(detail: &TaskDetail, id: &str) -> Value {
    match call(detail, id) {
        Some(TranscriptItem::ToolCall { result, .. }) => {
            result.as_deref().and_then(|r| serde_json::from_str(r).ok()).unwrap_or(Value::Null)
        }
        _ => Value::Null,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ask_user_waits_holds_the_batch_and_validates_answers() {
    let mut h = harness();
    h.service
        .load_script(json!({"turns": [
            {"steps": [{"text": "One question first."}, ask("t_ask"),
                {"toolUse": {"id": "t_read", "name": "read_file", "input": {"path": "main.typ"}}}]},
            {"steps": [{"text": "Keeping it informal."}]}
        ]}))
        .unwrap();
    let task = h.service.create_task(request(&h, PermissionProfile::ReadOnly)).unwrap();
    let question = requested(&mut h.events).await;
    assert_eq!((question.revision, question.tool_use_id.as_str()), (1, "t_ask"));
    assert_eq!(question.questions[0].id, "q1");
    assert!(question.questions[0].options[1].recommended);
    assert_eq!(question.state, QuestionRequestState::Pending);

    let detail = until(&h, &task.id, |d| d.summary.status == TaskStatus::AwaitingAnswer).await;
    assert!(detail.summary.status.is_active(), "waiting for an answer is not terminal");
    // The rest of the batch is held: the read did not run past the question.
    assert_eq!(call_status(&detail, "t_read"), Some(ToolCallStatus::Pending));
    assert_eq!(detail.questions, vec![question.clone()]);

    let refused = |r: std::result::Result<(), AgentError>| r.unwrap_err().code;
    assert_eq!(
        refused(h.service.answer_question(&task.id, &question.id, 1, vec![pick("q1", "zzz")])),
        "agents.invalidAnswer"
    );
    assert_eq!(refused(h.service.answer_question(&task.id, &question.id, 1, vec![])), "agents.invalidAnswer");
    assert_eq!(
        refused(h.service.answer_question(&task.id, &question.id, 2, vec![pick("q1", "a")])),
        "agents.questionStale"
    );
    assert_eq!(
        refused(h.service.answer_question(&task.id, "question-unknown", 1, vec![pick("q1", "a")])),
        "agents.noQuestion"
    );
    h.service.answer_question(&task.id, &question.id, 1, vec![pick("q1", "b")]).unwrap();
    // A duplicate delivery (the other view) is accepted and changes nothing.
    h.service.answer_question(&task.id, &question.id, 1, vec![pick("q1", "b")]).unwrap();
    // A different answer after resolution is refused.
    assert_eq!(
        refused(h.service.answer_question(&task.id, &question.id, 1, vec![pick("q1", "a")])),
        "agents.questionResolved"
    );

    let done = finish(&h, &task.id).await;
    assert_eq!(done.status, TaskStatus::Completed, "{:?}", done.status);
    let detail = h.service.task_detail(&task.id).unwrap();
    assert_eq!(call_result(&detail, "t_ask"), json!({"q1": {"optionId": "b"}}));
    assert_eq!(call_status(&detail, "t_ask"), Some(ToolCallStatus::Succeeded));
    assert_eq!(call_status(&detail, "t_read"), Some(ToolCallStatus::Succeeded), "the held call ran after the answer");
    assert_eq!(detail.questions[0].state, QuestionRequestState::Answered { answers: vec![pick("q1", "b")] });
    // Still idempotent after the run ended (read from the store).
    h.service.answer_question(&task.id, &question.id, 1, vec![pick("q1", "b")]).unwrap();
    assert_eq!(
        refused(h.service.answer_question(&task.id, &question.id, 1, vec![pick("q1", "a")])),
        "agents.questionResolved"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discussion_is_one_bounded_turn_that_revises_options_while_the_batch_stays_held() {
    let mut h = harness();
    h.service
        .load_script(json!({"turns": [
            {"steps": [ask("t_ask"),
                {"toolUse": {"id": "t_create", "name": "create_file", "input": {"path": "tone.txt", "content": "decided"}}}]},
            // The discussion: reads context, explains, and adds an option.
            {"steps": [{"delayMs": 300}, {"text": "Formal suits a paper; a third choice mixes both."},
                {"toolUse": {"id": "d_read", "name": "read_file", "input": {"path": "main.typ"}}},
                {"toolUse": {"id": "d_edit", "name": "edit_document", "input": {"path": "main.typ", "base_revision": 1, "edits": [{"old_text": "cat", "new_text": "dog"}]}}},
                {"toolUse": {"id": "d_revise", "name": "revise_question", "input": {"request": "$ref:t_ask/request", "questions": [
                    {"prompt": "Which tone?", "explanation": "", "options": [option("a", "Formal"), option("b", "Keep informal"), option("c", "Mixed")]}]}}}]},
            {"steps": [{"text": "I added a mixed option."}]},
            {"steps": [{"text": "Mixed it is."}]}
        ]}))
        .unwrap();
    let task = h.service.create_task(request(&h, PermissionProfile::AutoApply)).unwrap();
    let question = requested(&mut h.events).await;
    let turns_before = h.service.task(&task.id).unwrap().usage.turns;

    h.service.discuss_question(&task.id, &question.id, 1, "Why would formal be better?").unwrap();
    // One discussion at a time.
    assert_eq!(h.service.discuss_question(&task.id, &question.id, 1, "And?").unwrap_err().code, "agents.questionBusy");
    let detail = until(&h, &task.id, |d| d.questions[0].state == QuestionRequestState::Pending).await;
    let revised = &detail.questions[0];
    assert_eq!(revised.revision, 2, "the revision bumped with the options");
    let ids: Vec<&str> = revised.questions[0].options.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(ids, ["a", "b", "c"], "existing option ids are kept");
    assert_eq!(detail.summary.status, TaskStatus::AwaitingAnswer);
    // Counted in the task's budgets: two discussion requests.
    assert_eq!(detail.summary.usage.turns, turns_before + 2);
    // The batch stayed held: nothing was created, and the discussion could
    // read but not edit.
    assert!(!h.project.path().join("tone.txt").exists());
    assert_eq!(call_status(&detail, "t_create"), Some(ToolCallStatus::Pending));
    assert_eq!(call_status(&detail, "d_read"), Some(ToolCallStatus::Succeeded));
    assert_eq!(call_status(&detail, "d_edit"), Some(ToolCallStatus::NotRun));
    assert!(call_result(&detail, "d_edit")["error"].as_str().unwrap().contains("discuss"));
    // The discussion is in the same conversation, marked with its question.
    assert!(detail.transcript.iter().any(|i| matches!(i,
        TranscriptItem::UserText { text, question: Some(q), .. } if text == "Why would formal be better?" && q == &question.id)));
    assert!(detail.transcript.iter().any(|i| matches!(i,
        TranscriptItem::AssistantText { text, .. } if text.contains("a third choice"))));

    assert_eq!(
        h.service.answer_question(&task.id, &question.id, 1, vec![pick("q1", "c")]).unwrap_err().code,
        "agents.questionStale"
    );
    h.service.answer_question(&task.id, &question.id, 2, vec![pick("q1", "c")]).unwrap();
    let done = finish(&h, &task.id).await;
    assert_eq!(done.status, TaskStatus::Completed, "{:?}", done.status);
    assert_eq!(std::fs::read_to_string(h.project.path().join("tone.txt")).unwrap(), "decided");
    let detail = h.service.task_detail(&task.id).unwrap();
    assert_eq!(call_result(&detail, "t_ask"), json!({"q1": {"optionId": "c"}}));
    // The provider history carries the answer and, beside it, the discussion
    // and the final options (the model revised them outside the history).
    let store = exposition_agents::store::Store::open(&h.data.path().join("agents.sqlite3")).unwrap();
    let messages = store.messages(&task.id).unwrap();
    let results = &messages[2].1;
    assert_eq!(results[0]["tool_use_id"], "t_ask");
    let context =
        results.iter().filter_map(|b| b["text"].as_str()).find(|t| t.starts_with("<question_discussion")).unwrap();
    assert!(context.contains("Why would formal be better?") && context.contains("Mixed"), "{context}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_supersedes_and_continue_is_a_normal_follow_up() {
    let mut h = harness();
    h.service
        .load_script(json!({"turns": [
            {"steps": [ask("t_ask"), {"toolUse": {"id": "t_read", "name": "read_file", "input": {"path": "main.typ"}}}]},
            {"steps": [{"text": "Continuing without the answer."}]}
        ]}))
        .unwrap();
    let task = h.service.create_task(request(&h, PermissionProfile::ReadOnly)).unwrap();
    let question = requested(&mut h.events).await;
    h.service.cancel_task(&task.id).unwrap();
    let done = finish(&h, &task.id).await;
    assert_eq!(done.status, TaskStatus::Canceled);
    let detail = h.service.task_detail(&task.id).unwrap();
    assert_eq!(detail.questions[0].state, QuestionRequestState::Superseded { reason: "The task was canceled".into() });
    assert_eq!(call_status(&detail, "t_read"), Some(ToolCallStatus::NotRun));
    let err = h.service.answer_question(&task.id, &question.id, 1, vec![pick("q1", "a")]).unwrap_err();
    assert_eq!(err.code, "agents.questionResolved");
    assert_eq!(
        h.service.discuss_question(&task.id, &question.id, 1, "hm").unwrap_err().code,
        "agents.questionResolved"
    );
    // Continue is an ordinary follow-up.
    let summary = h.service.continue_task(&task.id, "Go on", None).unwrap();
    assert!(summary.status.is_active());
    assert_eq!(finish(&h, &task.id).await.status, TaskStatus::Completed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_supersedes_open_questions_and_old_answers_are_refused() {
    let mut h = harness();
    h.service.load_script(json!({"turns": [{"steps": [ask("t_ask")]}, {"steps": [{"text": "ok"}]}]})).unwrap();
    let task = h.service.create_task(request(&h, PermissionProfile::ReadOnly)).unwrap();
    let question = requested(&mut h.events).await;
    // A second process opening the same database (the first one is gone).
    let restarted = open(&h.data, Arc::new(UnavailableServices));
    let detail = restarted.task_detail(&task.id).unwrap();
    assert_eq!(detail.summary.status, TaskStatus::Interrupted);
    assert_eq!(detail.questions[0].state, QuestionRequestState::Superseded { reason: "Exposition restarted".into() });
    assert_eq!(
        restarted.answer_question(&task.id, &question.id, 1, vec![pick("q1", "a")]).unwrap_err().code,
        "agents.questionResolved"
    );
    h.service.cancel_task(&task.id).unwrap();
    h.service.join(&task.id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn waiting_for_an_answer_is_not_counted_against_the_time_budget() {
    let mut h = harness();
    h.service.load_script(json!({"turns": [{"steps": [ask("t_ask")]}, {"steps": [{"text": "Done."}]}]})).unwrap();
    let mut req = request(&h, PermissionProfile::ReadOnly);
    req.budgets = Some(Budgets { max_wall_ms: 1_000, ..Budgets::default() });
    let task = h.service.create_task(req).unwrap();
    let question = requested(&mut h.events).await;
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    h.service.answer_question(&task.id, &question.id, 1, vec![pick("q1", "a")]).unwrap();
    let done = finish(&h, &task.id).await;
    assert_eq!(done.status, TaskStatus::Completed, "{:?}", done.status);
    assert!(done.usage.elapsed_ms < 1_000, "{} ms counted", done.usage.elapsed_ms);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ask_user_is_refused_without_a_user_in_the_loop() {
    let h = harness();
    let script = json!({"turns": [{"steps": [ask("t_ask")]}, {"steps": [{"text": "Done."}]}]});
    // A workflow review (created without a user in the loop).
    h.service.load_script(script.clone()).unwrap();
    let review = h.service.create_unattended_task(request(&h, PermissionProfile::ReadOnly)).unwrap();
    assert_eq!(finish(&h, &review.id).await.status, TaskStatus::Completed);
    let detail = h.service.task_detail(&review.id).unwrap();
    assert!(detail.questions.is_empty());
    assert_eq!(call_status(&detail, "t_ask"), Some(ToolCallStatus::Failed));
    assert!(call_result(&detail, "t_ask")["error"].as_str().unwrap().contains("not available"));
    // A restricted-only (unattended execution) run.
    h.service.load_script(script).unwrap();
    let mut req = request(&h, PermissionProfile::ReadOnly);
    req.execution = ExecutionMode::Restricted;
    let task = h.service.create_task(req).unwrap();
    assert_eq!(finish(&h, &task.id).await.status, TaskStatus::Completed);
    assert!(h.service.task_detail(&task.id).unwrap().questions.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_ask_user_input_is_an_ordinary_tool_error() {
    let h = harness();
    h.service
        .load_script(json!({"turns": [
            {"steps": [{"toolUse": {"id": "t_ask", "name": "ask_user", "input": {"questions": [
                {"prompt": "Only one option?", "explanation": "", "options": [option("a", "A")]}], "allowCustom": true}}}]},
            {"steps": [{"text": "Fine."}]}
        ]}))
        .unwrap();
    let task = h.service.create_task(request(&h, PermissionProfile::ReadOnly)).unwrap();
    assert_eq!(finish(&h, &task.id).await.status, TaskStatus::Completed);
    let detail = h.service.task_detail(&task.id).unwrap();
    assert!(detail.questions.is_empty());
    assert!(call_result(&detail, "t_ask")["error"].as_str().unwrap().contains("options"));
}

// ---- figures

/// Native Python through a fake research service: the run records two
/// figures (PNG and SVG) and a data file; artifact bytes are served from memory.
struct FigureServices {
    bytes: Mutex<std::collections::HashMap<String, Vec<u8>>>,
    purged: Mutex<bool>,
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfake";
const SVG: &[u8] = b"<svg xmlns='http://www.w3.org/2000/svg'/>";

fn record(id: &str, kind: ArtifactKind, path: &str, media: &str, bytes: &[u8]) -> ArtifactRecord {
    use sha2::Digest;
    let hex: String = sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect();
    ArtifactRecord {
        id: ArtifactId(id.into()),
        kind,
        path: Some(WorkspacePath::parse(path).unwrap()),
        scratch_path: None,
        media_type: media.into(),
        size_bytes: bytes.len() as u64,
        content_hash: format!("sha256:{hex}"),
    }
}

fn records() -> Vec<ArtifactRecord> {
    vec![
        record("fig-1", ArtifactKind::Figure, ".exposition/artifacts/run-1/plot.png", "image/png", PNG),
        record("data-1", ArtifactKind::Data, ".exposition/artifacts/run-1/table.csv", "text/csv", b"a,b\n"),
        record("fig-2", ArtifactKind::Figure, ".exposition/artifacts/run-1/curve.svg", "image/svg+xml", SVG),
    ]
}

#[async_trait]
impl ToolServices for FigureServices {
    fn available(&self, kind: ServiceKind) -> bool {
        kind == ServiceKind::Research
    }
    fn restricted_backend(&self) -> Option<String> {
        None
    }
    async fn build_document(&self, _: BuildRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Err(ServiceError::NotAvailable(ServiceKind::Typesetting))
    }
    async fn run_python(&self, _: PythonRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Ok(json!({"status": "succeeded", "run": "run-1", "stdout": "", "artifacts": []}))
    }
    async fn check_lean(&self, _: LeanRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Err(ServiceError::NotAvailable(ServiceKind::Lean))
    }
    async fn run_command(&self, _: CommandRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Err(ServiceError::NotAvailable(ServiceKind::Execution))
    }
    fn run_artifacts(&self, _root: &std::path::Path, run_id: &str) -> Result<Vec<ArtifactRecord>, ServiceError> {
        if run_id == "run-1" { Ok(records()) } else { Err(ServiceError::Failed("unknown run".into())) }
    }
    fn artifact_bytes(
        &self,
        _root: &std::path::Path,
        run_id: &str,
        artifact_id: &str,
    ) -> Result<(ArtifactRecord, Vec<u8>), ServiceError> {
        if *self.purged.lock().unwrap() {
            return Err(exposition_agents::tools::services::research_outcome("research.unknownRun", "unknown run"));
        }
        let record = records().into_iter().find(|r| r.id.0 == artifact_id && run_id == "run-1");
        let bytes = self.bytes.lock().unwrap().get(artifact_id).cloned();
        match (record, bytes) {
            (Some(record), Some(bytes)) => Ok((record, bytes)),
            _ => Err(exposition_agents::tools::services::research_outcome("research.unknownArtifact", "unknown")),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn figures_are_recorded_from_the_run_manifest_and_served_only_to_their_task() {
    let services = Arc::new(FigureServices {
        bytes: Mutex::new([("fig-1".to_owned(), PNG.to_vec()), ("fig-2".to_owned(), SVG.to_vec())].into()),
        purged: Mutex::new(false),
    });
    let mut h = harness_with(services.clone());
    let python = json!({"turns": [
        {"steps": [{"toolUse": {"id": "t_py", "name": "run_python", "input": {"code": "plot()"}}}]},
        {"steps": [{"text": "Plotted."}]}
    ]});
    h.service.load_script(python).unwrap();
    let mut req = request(&h, PermissionProfile::AutoApply);
    req.execution = ExecutionMode::ApproveEach;
    let task = h.service.create_task(req).unwrap();
    let approval = match wait_for(&mut h.events, |e| matches!(e, AgentEvent::ApprovalRequested { .. })).await {
        AgentEvent::ApprovalRequested { approval } => approval,
        _ => unreachable!(),
    };
    h.service.resolve_approval(&task.id, &approval.id, true).unwrap();
    assert_eq!(finish(&h, &task.id).await.status, TaskStatus::Completed);
    let detail = h.service.task_detail(&task.id).unwrap();
    let Some(TranscriptItem::ToolCall { figures, .. }) = call(&detail, "t_py") else { panic!("no call") };
    assert_eq!(
        figures,
        &vec![
            FigureRef {
                run_id: "run-1".into(),
                artifact_id: "fig-1".into(),
                media_type: "image/png".into(),
                name: "plot.png".into(),
                caption: None
            },
            FigureRef {
                run_id: "run-1".into(),
                artifact_id: "fig-2".into(),
                media_type: "image/svg+xml".into(),
                name: "curve.svg".into(),
                caption: None
            },
        ],
        "figures only: the data file is not one"
    );
    assert_eq!(h.service.figure(&task.id, "run-1", "fig-1").unwrap(), PNG);
    assert_eq!(h.service.figure(&task.id, "run-1", "fig-2").unwrap(), SVG);
    let unavailable = |r: std::result::Result<Vec<u8>, AgentError>| {
        let e = r.unwrap_err();
        assert_eq!(e.code, "agents.figureUnavailable");
        e.message
    };
    // Not a figure of this task's calls (the data artifact; another run).
    assert!(unavailable(h.service.figure(&task.id, "run-1", "data-1")).contains("not this task's"));
    assert!(unavailable(h.service.figure(&task.id, "run-2", "fig-1")).contains("not this task's"));
    // Another task cannot read this task's figure.
    h.service.load_script(json!({"turns": [{"steps": [{"text": "Hi."}]}]})).unwrap();
    let other = h.service.create_task(request(&h, PermissionProfile::ReadOnly)).unwrap();
    finish(&h, &other.id).await;
    assert!(unavailable(h.service.figure(&other.id, "run-1", "fig-1")).contains("not this task's"));
    // Changed bytes (hash and size are verified), then a purged run.
    services.bytes.lock().unwrap().insert("fig-1".into(), b"tampered".to_vec());
    assert!(unavailable(h.service.figure(&task.id, "run-1", "fig-1")).contains("changed"));
    *services.purged.lock().unwrap() = true;
    assert!(unavailable(h.service.figure(&task.id, "run-1", "fig-2")).contains("purged"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_says_whether_an_open_file_is_saved() {
    let h = harness();
    h.service
        .load_script(json!({"turns": [
            {"steps": [{"toolUse": {"id": "r", "name": "read_file", "input": {"path": "main.typ"}}}]},
            {"steps": [{"toolUse": {"id": "e", "name": "edit_document", "input": {"path": "main.typ", "base_revision": "$ref:r/revision",
                "edits": [{"old_text": "cat", "new_text": "dog"}]}}},
                {"toolUse": {"id": "c", "name": "create_file", "input": {"path": "new.typ", "content": "x"}}}]},
            {"steps": [{"text": "Done."}]}
        ]}))
        .unwrap();
    let task = h.service.create_task(request(&h, PermissionProfile::AutoApply)).unwrap();
    assert_eq!(finish(&h, &task.id).await.status, TaskStatus::Completed);
    let review = h.service.review(&task.id).unwrap();
    let unsaved = |path: &str| review.files.iter().find(|f| f.path.as_str() == path).unwrap().unsaved;
    assert_eq!(unsaved("main.typ"), Some(true), "edited in the open buffer, not saved");
    // A created file is written to disk (saved); it is open or not.
    assert_ne!(unsaved("new.typ"), Some(true));
}
