//! Provider contracts through the agent service, against a local mock of the
//! OpenAI and Anthropic HTTP APIs (no real provider is contacted):
//! - an OpenAI agent completes a multi-turn tool workflow through the same
//!   tool loop, permission gate, and history as every provider;
//! - model catalogs (discovery, caching, failures, saved IDs);
//! - attachments (snapshot, payloads, preflight, persistence);
//! - switching provider/model between turns, and defaults during a run;
//! - settings saved before connections existed keep the user's choice.
//!
//! Mocked providers establish the application side only; live entitlement
//! and model behavior need the opt-in live scenarios.
#![cfg(debug_assertions)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use exposition_agents::attachments::{AttachmentKind, IngestOutcome};
use exposition_agents::catalog::{AgentSupport, CatalogState, DescriptorSource};
use exposition_agents::credentials::{MemoryStore, SecretStore};
use exposition_agents::tools::services::UnavailableServices;
use exposition_agents::*;
use exposition_workspace::DocumentService;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const OPENAI_KEY: &str = "sk-proj-mock-0123456789abcdefghij";
const ANTHROPIC_KEY: &str = "sk-ant-mock-0123456789abcdefghij";

/// A recorded request: method + path line, headers, body.
#[derive(Clone, Debug)]
struct Seen {
    line: String,
    head: String,
    body: Value,
}

type Routes = Arc<Mutex<VecDeque<(String, String)>>>;

/// A mock server answering queued responses in order (each with an expected
/// path prefix); records every request.
struct Mock {
    base: String,
    queue: Routes,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Mock {
    async fn start() -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let queue: Routes = Arc::new(Mutex::new(VecDeque::new()));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (q, s) = (Arc::clone(&queue), Arc::clone(&seen));
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else { return };
                let (q, s) = (Arc::clone(&q), Arc::clone(&s));
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 16384];
                    let (head, body) = loop {
                        let n = socket.read(&mut chunk).await.unwrap_or(0);
                        buf.extend_from_slice(&chunk[..n]);
                        let text = String::from_utf8_lossy(&buf).to_string();
                        if let Some(end) = text.find("\r\n\r\n") {
                            let len = text[..end]
                                .lines()
                                .find_map(|l| {
                                    l.to_ascii_lowercase()
                                        .strip_prefix("content-length:")
                                        .map(|v| v.trim().parse::<usize>().unwrap())
                                })
                                .unwrap_or(0);
                            if buf.len() >= end + 4 + len || n == 0 {
                                break (text[..end].to_owned(), text[end + 4..].to_owned());
                            }
                        }
                        if n == 0 {
                            return;
                        }
                    };
                    let line = head.lines().next().unwrap_or("").to_owned();
                    s.lock().unwrap().push(Seen {
                        line: line.clone(),
                        head: head.clone(),
                        body: serde_json::from_str(&body).unwrap_or(Value::Null),
                    });
                    let response = {
                        let mut queue = q.lock().unwrap();
                        let pos = queue.iter().position(|(path, _)| line.contains(path.as_str()));
                        pos.and_then(|i| queue.remove(i)).map(|(_, r)| r)
                    };
                    let response = response.unwrap_or_else(|| {
                        json_reply(
                            "500 Internal Server Error",
                            json!({"error": {"message": "no mock response queued"}}),
                        )
                    });
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        Mock { base, queue, seen }
    }

    fn reply(&self, path: &str, response: String) {
        self.queue.lock().unwrap().push_back((path.to_owned(), response));
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

fn json_reply(status: &str, body: Value) -> String {
    let text = body.to_string();
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{text}",
        text.len()
    )
}

fn sse(events: &[Value]) -> String {
    let body: String =
        events.iter().map(|e| format!("event: {}\ndata: {}\n\n", e["type"].as_str().unwrap(), e)).collect();
    format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{body}")
}

/// A complete OpenAI streamed response with the given output items.
fn openai_response(output: Value) -> String {
    sse(&[
        json!({"type": "response.created", "response": {"id": "resp_1", "model": "gpt-6.2-sol", "status": "in_progress"}}),
        json!({"type": "response.completed", "response": {"id": "resp_1", "model": "gpt-6.2-sol", "status": "completed", "output": output,
            "usage": {"input_tokens": 100, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 10}}}),
    ])
}

struct Harness {
    service: AgentService,
    project: TempDir,
    data: TempDir,
    openai: Arc<MemoryStore>,
    anthropic: Arc<MemoryStore>,
}

fn open(data: &TempDir, mock: &Mock, anthropic: Arc<MemoryStore>, openai: Arc<MemoryStore>) -> AgentService {
    let service = AgentService::new(AgentConfig {
        database: Some(data.path().join("agents.sqlite3")),
        documents: Arc::new(DocumentService::new()),
        workspace: None,
        credentials: anthropic,
        services: Arc::new(UnavailableServices),
        runtime: tokio::runtime::Handle::current(),
        allow_scripted: true,
        anthropic_base_url: Some(mock.base.clone()),
        retry: Some(provider::anthropic::RetryPolicy {
            max_retries: 0,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(1),
        }),
        graph: None,
    })
    .unwrap();
    service.configure_openai(openai, Some(mock.base.clone()));
    service
}

async fn harness(mock: &Mock) -> Harness {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("main.typ"), "= Notes\nThe cat sat on the mat.\n").unwrap();
    let data = tempfile::tempdir().unwrap();
    let anthropic = Arc::new(MemoryStore::default());
    let openai = Arc::new(MemoryStore::default());
    let service = open(&data, mock, Arc::clone(&anthropic), Arc::clone(&openai));
    Harness { service, project, data, openai, anthropic }
}

fn use_openai(h: &Harness, model: &str) {
    h.service.set_provider_key(ProviderKind::OpenAi, OPENAI_KEY).unwrap();
    let settings = ProviderSettings {
        provider: ProviderKind::OpenAi,
        model: model.into(),
        effort: Effort::Medium,
        ..ProviderSettings::default()
    };
    h.service.update_settings(settings).unwrap();
}

fn request(h: &Harness, prompt: &str) -> CreateTaskRequest {
    CreateTaskRequest {
        prompt: prompt.into(),
        workspace_root: h.project.path().display().to_string(),
        scope: TaskScope::Workspace,
        permission: PermissionProfile::ReadOnly,
        execution: ExecutionMode::Disabled,
        consent: None,
        budgets: None,
        approval_mode: None,
        provider: None,
        attachments: None,
    }
}

async fn finish(h: &Harness, id: &TaskId) -> TaskSummary {
    tokio::time::timeout(Duration::from_secs(20), h.service.join(id)).await.expect("task finished");
    h.service.task(id).unwrap()
}

fn png() -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    b.extend(4u32.to_be_bytes());
    b.extend(3u32.to_be_bytes());
    b.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
    b
}

fn added(outcome: IngestOutcome) -> attachments::AttachmentInfo {
    match outcome {
        IngestOutcome::Added { attachment } => attachment,
        other => panic!("not added: {other:?}"),
    }
}

#[tokio::test]
async fn an_openai_agent_completes_a_multi_turn_tool_workflow() {
    let mock = Mock::start().await;
    let h = harness(&mock).await;
    use_openai(&h, "gpt-6.2-sol");
    let reasoning = json!({"type": "reasoning", "id": "rs_1", "encrypted_content": "enc-1", "summary": [{"type": "summary_text", "text": "Read the file first."}]});
    let call = json!({"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "read_file", "arguments": "{\"path\":\"main.typ\"}"});
    mock.reply("/v1/responses", openai_response(json!([reasoning, call])));
    mock.reply("/v1/responses", openai_response(json!([{"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "The notes have a heading and one sentence."}]}])));
    let task = h.service.create_task(request(&h, "Summarize main.typ")).unwrap();
    let done = finish(&h, &task.id).await;
    assert_eq!(done.status, TaskStatus::Completed, "{:?}", done.status);
    assert_eq!(
        done.spec.provider,
        ProviderChoice { provider: ProviderKind::OpenAi, model: "gpt-6.2-sol".into(), effort: Effort::Medium }
    );
    assert_eq!(done.usage.turns, 2);

    let seen = mock.seen();
    assert_eq!(seen.len(), 2);
    assert!(seen[0].line.starts_with("POST /v1/responses"));
    assert!(seen[0].head.to_ascii_lowercase().contains("authorization: bearer"));
    let first = &seen[0].body;
    assert_eq!(first["model"], "gpt-6.2-sol");
    assert_eq!(first["store"], false);
    assert_eq!(first["reasoning"]["effort"], "medium");
    assert!(first["tools"].as_array().unwrap().iter().any(|t| t["name"] == "read_file"));
    // The second request replays the reasoning item with its call and the
    // tool's real result from the shared tool loop.
    let input = seen[1].body["input"].as_array().unwrap().clone();
    let types: Vec<&str> = input.iter().map(|i| i["type"].as_str().unwrap()).collect();
    assert_eq!(&types[types.len() - 3..], ["reasoning", "function_call", "function_call_output"]);
    let output = input.last().unwrap();
    assert_eq!(output["call_id"], "call_1");
    assert!(output["output"].as_str().unwrap().contains("The cat sat on the mat."));

    let detail = h.service.task_detail(&task.id).unwrap();
    assert!(
        detail
            .transcript
            .iter()
            .any(|i| matches!(i, TranscriptItem::Thinking { text, .. } if text == "Read the file first."))
    );
    assert!(detail.transcript.iter().any(
        |i| matches!(i, TranscriptItem::ToolCall { name, status: ToolCallStatus::Succeeded, .. } if name == "read_file")
    ));
    // No credential in anything persisted or shown.
    let raw = std::fs::read(h.data.path().join("agents.sqlite3")).unwrap();
    assert!(!String::from_utf8_lossy(&raw).contains(OPENAI_KEY));
    assert!(!serde_json::to_string(&detail).unwrap().contains(OPENAI_KEY));
}

#[tokio::test]
async fn connections_keep_separate_credentials() {
    let mock = Mock::start().await;
    let h = harness(&mock).await;
    h.service.set_provider_key(ProviderKind::OpenAi, OPENAI_KEY).unwrap();
    let status = h.service.provider_status();
    let conn = |p: ProviderKind| status.connections.iter().find(|c| c.provider == p).unwrap().clone();
    assert!(conn(ProviderKind::OpenAi).configured);
    assert!(!conn(ProviderKind::Anthropic).configured);
    assert_eq!(h.anthropic.get().unwrap(), None);
    h.service.set_api_key(ANTHROPIC_KEY).unwrap();
    h.service.clear_provider_key(ProviderKind::OpenAi).unwrap();
    assert_eq!(h.openai.get().unwrap(), None);
    assert_eq!(h.anthropic.get().unwrap().unwrap().expose(), ANTHROPIC_KEY);
    // The request context is validated.
    let mut settings = h.service.settings().unwrap();
    settings.openai = Some(OpenAiConnection { organization: Some("not an org".into()), project: None });
    assert_eq!(h.service.update_settings(settings).unwrap_err().code, "agents.invalidConnection");
}

#[tokio::test]
async fn catalogs_are_discovered_cached_and_never_drop_saved_choices() {
    let mock = Mock::start().await;
    let h = harness(&mock).await;
    // No key: unavailable, with a reason, and nothing fetched.
    let none = h.service.models(ProviderKind::OpenAi, false).await.unwrap();
    assert_eq!(none.state, CatalogState::Unavailable);
    assert!(none.error.unwrap().contains("No OpenAI API key"));
    assert!(mock.seen().is_empty());

    use_openai(&h, "gpt-legacy-saved");
    mock.reply(
        "/v1/models",
        json_reply(
            "200 OK",
            json!({"object": "list", "data": [
                {"id": "gpt-6.2-sol", "object": "model", "created": 1_790_000_000, "owned_by": "system"},
                {"id": "gpt-6.2-astra", "object": "model", "created": 1_791_000_000, "owned_by": "system"},
                {"id": "text-embedding-3-small", "object": "model", "created": 1, "owned_by": "system"}
            ]}),
        ),
    );
    let catalog = h.service.models(ProviderKind::OpenAi, false).await.unwrap();
    assert_eq!(catalog.state, CatalogState::Fresh);
    assert_eq!(catalog.models[0].id, "gpt-6.2-astra");
    // The saved model the catalog does not list stays as itself.
    let saved = catalog.models.iter().find(|m| m.id == "gpt-legacy-saved").unwrap();
    assert_eq!(saved.source, DescriptorSource::Saved);
    assert_eq!(catalog.suggested.as_deref(), Some("gpt-6.2-astra"));
    assert!(matches!(
        catalog.models.iter().find(|m| m.id == "text-embedding-3-small").unwrap().agent,
        AgentSupport::Unsupported { .. }
    ));
    // Cached: no second request.
    h.service.models(ProviderKind::OpenAi, false).await.unwrap();
    assert_eq!(mock.seen().iter().filter(|s| s.line.contains("/v1/models")).count(), 1);
    // A failed refresh keeps the cached list as stale, with a safe reason.
    mock.reply(
        "/v1/models",
        json_reply(
            "401 Unauthorized",
            json!({"error": {"message": format!("Incorrect API key provided: {OPENAI_KEY}")}}),
        ),
    );
    let stale = h.service.models(ProviderKind::OpenAi, true).await.unwrap();
    assert_eq!(stale.state, CatalogState::Stale);
    assert!(!stale.models.is_empty());
    let reason = stale.error.unwrap();
    assert!(reason.contains("rejected the API key") && !reason.contains(OPENAI_KEY), "{reason}");
    // A new key is a new connection: its catalog is not the old one's.
    h.service.set_provider_key(ProviderKind::OpenAi, "sk-proj-another-0123456789abcdef").unwrap();
    mock.reply("/v1/models", json_reply("500 Internal Server Error", json!({"error": {"message": "down"}})));
    let other = h.service.models(ProviderKind::OpenAi, false).await.unwrap();
    assert_eq!(other.state, CatalogState::Unavailable);
    assert!(other.models.iter().all(|m| m.source == DescriptorSource::Saved));
    // The saved setting itself was never rewritten.
    assert_eq!(h.service.settings().unwrap().model, "gpt-legacy-saved");
}

#[tokio::test]
async fn anthropic_catalog_follows_pagination() {
    let mock = Mock::start().await;
    let h = harness(&mock).await;
    h.service.set_api_key(ANTHROPIC_KEY).unwrap();
    mock.reply("/v1/models?limit=1000&after_id=claude-b", json_reply("200 OK", json!({"data": [
        {"type": "model", "id": "claude-c", "display_name": "Claude C", "created_at": "2026-01-01T00:00:00Z", "capabilities": null}
    ], "has_more": false, "first_id": "claude-c", "last_id": "claude-c"})));
    mock.reply("/v1/models?limit=1000", json_reply("200 OK", json!({"data": [
        {"type": "model", "id": "claude-a", "display_name": "Claude A", "created_at": "2026-03-01T00:00:00Z", "capabilities": null},
        {"type": "model", "id": "claude-b", "display_name": "Claude B", "created_at": "2026-02-01T00:00:00Z", "capabilities": null}
    ], "has_more": true, "first_id": "claude-a", "last_id": "claude-b"})));
    let catalog = h.service.models(ProviderKind::Anthropic, true).await.unwrap();
    let ids: Vec<&str> =
        catalog.models.iter().filter(|m| m.source == DescriptorSource::Catalog).map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["claude-a", "claude-b", "claude-c"]);
    let seen = mock.seen();
    assert!(seen.iter().all(|s| s.head.to_ascii_lowercase().contains("anthropic-version:")));
}

#[tokio::test]
async fn attachments_are_snapshots_sent_as_real_payloads_and_kept_after_reopening() {
    let mock = Mock::start().await;
    let h = harness(&mock).await;
    use_openai(&h, "gpt-6.2-sol");
    let image = added(h.service.add_attachment_bytes(None, Some("image/png"), &attachments::base64_encode(&png())));
    assert_eq!(
        (image.kind, image.width, image.height, image.name.as_str()),
        (AttachmentKind::Image, Some(4), Some(3), "Pasted image.png")
    );
    let notes = h.project.path().join("data.csv");
    std::fs::write(&notes, "x,y\n1,2\n").unwrap();
    let outcomes = h.service.add_attachment_files(&[notes.clone(), h.project.path().join("missing.txt")]);
    let text = added(outcomes[0].clone());
    assert!(matches!(&outcomes[1], IngestOutcome::Rejected { code, .. } if code == "attachments.unreadable"));
    // Editing the source afterwards does not change what is sent.
    std::fs::write(&notes, "changed\n").unwrap();
    // Duplicate names stay distinct attachments.
    let again = added(h.service.add_attachment_bytes(None, Some("image/png"), &attachments::base64_encode(&png())));
    assert_ne!(again.id, image.id);

    mock.reply(
        "/v1/responses",
        openai_response(json!([{"type": "message", "content": [{"type": "output_text", "text": "Seen."}]}])),
    );
    let mut req = request(&h, "Describe these");
    req.attachments = Some(vec![image.id.clone(), text.id.clone()]);
    let task = h.service.create_task(req).unwrap();
    assert_eq!(finish(&h, &task.id).await.status, TaskStatus::Completed);
    let body = &mock.seen()[0].body;
    let parts = body["input"].as_array().unwrap().last().unwrap()["content"].as_array().unwrap().clone();
    assert!(
        parts
            .iter()
            .any(|p| p["type"] == "input_image"
                && p["image_url"].as_str().unwrap().starts_with("data:image/png;base64,"))
    );
    let text_part = parts
        .iter()
        .find(|p| p["type"] == "input_text" && p["text"].as_str().unwrap().contains("<attachment name=\"data.csv\""))
        .unwrap();
    assert!(text_part["text"].as_str().unwrap().contains("x,y\n1,2"));
    assert!(!text_part["text"].as_str().unwrap().contains("changed"));

    // Reopened: the message still shows its attachments, with their identity.
    drop(h.service);
    let reopened = open(&h.data, &mock, Arc::clone(&h.anthropic), Arc::clone(&h.openai));
    let detail = reopened.task_detail(&task.id).unwrap();
    let shown = detail
        .transcript
        .iter()
        .find_map(|i| match i {
            TranscriptItem::UserText { attachments, .. } if !attachments.is_empty() => Some(attachments.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(shown.iter().map(|a| a.id.clone()).collect::<Vec<_>>(), vec![image.id.clone(), text.id.clone()]);
    assert!(shown.iter().all(|a| !a.missing));
    assert!(reopened.attachment_preview(&image.id).unwrap().unwrap().starts_with("data:image/png;base64,"));
}

#[tokio::test]
async fn preflight_explains_incompatible_models_without_dropping_anything() {
    let mock = Mock::start().await;
    let h = harness(&mock).await;
    h.service.set_provider_key(ProviderKind::OpenAi, OPENAI_KEY).unwrap();
    let image =
        added(h.service.add_attachment_bytes(Some("plot.png"), Some("image/png"), &attachments::base64_encode(&png())));
    let mut req = request(&h, "Look");
    req.attachments = Some(vec![image.id.clone()]);
    req.provider =
        Some(ProviderChoice { provider: ProviderKind::OpenAi, model: "ft:custom-model".into(), effort: Effort::Low });
    let e = h.service.create_task(req.clone()).unwrap_err();
    assert_eq!(e.code, "agents.attachmentUnsupported");
    assert!(e.message.contains("images"), "{}", e.message);
    req.provider = Some(ProviderChoice {
        provider: ProviderKind::OpenAi,
        model: "text-embedding-3-small".into(),
        effort: Effort::Low,
    });
    assert_eq!(h.service.create_task(req.clone()).unwrap_err().code, "agents.modelUnsupported");
    req.provider =
        Some(ProviderChoice { provider: ProviderKind::OpenAi, model: "gpt-5.4".into(), effort: Effort::Max });
    assert_eq!(h.service.create_task(req).unwrap_err().code, "agents.effortUnsupported");
    // Nothing was created by the refused requests.
    assert!(h.service.list_tasks().unwrap().is_empty());
}

#[tokio::test]
async fn switching_models_between_turns_keeps_the_task_and_drops_private_reasoning() {
    let mock = Mock::start().await;
    let h = harness(&mock).await;
    h.service.set_provider_key(ProviderKind::OpenAi, OPENAI_KEY).unwrap();
    let settings = ProviderSettings { provider: ProviderKind::Scripted, ..ProviderSettings::default() };
    h.service.update_settings(settings).unwrap();
    h.service
        .load_script(json!({"turns": [{"steps": [{"thinking": "secret scratchpad"}, {"text": "First answer."}]}]}))
        .unwrap();
    let task = h.service.create_task(request(&h, "First")).unwrap();
    assert_eq!(finish(&h, &task.id).await.status, TaskStatus::Completed);

    mock.reply(
        "/v1/responses",
        openai_response(json!([{"type": "message", "content": [{"type": "output_text", "text": "Second answer."}]}])),
    );
    let to = ProviderChoice { provider: ProviderKind::OpenAi, model: "gpt-6.2-sol".into(), effort: Effort::High };
    h.service
        .continue_task_with(
            &task.id,
            ContinueTaskRequest {
                prompt: "Second".into(),
                budgets: None,
                attachments: vec![],
                provider: Some(to.clone()),
            },
        )
        .unwrap();
    let done = finish(&h, &task.id).await;
    assert_eq!(done.status, TaskStatus::Completed);
    assert_eq!(done.id, task.id);
    assert_eq!(done.spec.provider, to);
    assert_eq!(done.spec.permission, PermissionProfile::ReadOnly);
    let body = serde_json::to_string(&mock.seen()[0].body).unwrap();
    assert!(body.contains("First answer.") && body.contains("Second"));
    assert!(!body.contains("secret scratchpad"), "another provider's reasoning must not be forwarded");
    let detail = h.service.task_detail(&task.id).unwrap();
    assert!(
        detail
            .transcript
            .iter()
            .any(|i| matches!(i, TranscriptItem::Notice { message, .. } if message.contains("Model changed from")))
    );
}

#[tokio::test]
async fn a_switch_that_cannot_carry_the_history_is_refused() {
    let mock = Mock::start().await;
    let h = harness(&mock).await;
    let settings = ProviderSettings { provider: ProviderKind::Scripted, ..ProviderSettings::default() };
    h.service.update_settings(settings).unwrap();
    h.service.set_provider_key(ProviderKind::OpenAi, OPENAI_KEY).unwrap();
    let pdf = added(h.service.add_attachment_bytes(
        Some("paper.pdf"),
        Some("application/pdf"),
        &attachments::base64_encode(b"%PDF-1.7\n%%EOF\n"),
    ));
    h.service.load_script(json!({"turns": [{"steps": [{"text": "Read it."}]}]})).unwrap();
    let mut req = request(&h, "Read the paper");
    req.attachments = Some(vec![pdf.id.clone()]);
    let task = h.service.create_task(req).unwrap();
    finish(&h, &task.id).await;
    let before = h.service.task(&task.id).unwrap().spec.provider;
    let to = ProviderChoice { provider: ProviderKind::OpenAi, model: "ft:custom-model".into(), effort: Effort::Low };
    let e = h
        .service
        .continue_task_with(
            &task.id,
            ContinueTaskRequest { prompt: "More".into(), budgets: None, attachments: vec![], provider: Some(to) },
        )
        .unwrap_err();
    assert_eq!(e.code, "agents.attachmentUnsupported");
    assert!(e.message.starts_with("This conversation cannot continue on ft:custom-model"), "{}", e.message);
    assert_eq!(h.service.task(&task.id).unwrap().spec.provider, before);
}

#[tokio::test]
async fn changing_the_default_during_a_run_affects_later_tasks_only() {
    let mock = Mock::start().await;
    let h = harness(&mock).await;
    let settings = ProviderSettings { provider: ProviderKind::Scripted, ..ProviderSettings::default() };
    h.service.update_settings(settings).unwrap();
    h.service.load_script(json!({"turns": [{"steps": [{"delayMs": 300}, {"text": "done"}]}]})).unwrap();
    let running = h.service.create_task(request(&h, "Slow")).unwrap();
    let recorded = running.spec.provider.clone();
    h.service.set_provider_key(ProviderKind::OpenAi, OPENAI_KEY).unwrap();
    use_openai(&h, "gpt-6.2-astra");
    assert_eq!(finish(&h, &running.id).await.spec.provider, recorded);
    mock.reply(
        "/v1/responses",
        openai_response(json!([{"type": "message", "content": [{"type": "output_text", "text": "ok"}]}])),
    );
    let later = h.service.create_task(request(&h, "Next")).unwrap();
    assert_eq!(later.spec.provider.model, "gpt-6.2-astra");
    finish(&h, &later.id).await;
}

#[tokio::test]
async fn settings_saved_before_connections_keep_the_exact_model() {
    let mock = Mock::start().await;
    let data = tempfile::tempdir().unwrap();
    {
        let first = open(&data, &mock, Arc::new(MemoryStore::default()), Arc::new(MemoryStore::default()));
        drop(first);
    }
    // A settings record written by an earlier version (no `openai` field).
    let conn = rusqlite::Connection::open(data.path().join("agents.sqlite3")).unwrap();
    conn.execute(
        "INSERT INTO settings (key, value) VALUES ('provider', ?1) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [r#"{"provider":"anthropic","model":"claude-opus-4-1-20250805","effort":"high","consent":"scopeContent"}"#],
    )
    .unwrap();
    drop(conn);
    let service = open(&data, &mock, Arc::new(MemoryStore::default()), Arc::new(MemoryStore::default()));
    let settings = service.settings().unwrap();
    assert_eq!(
        (settings.provider, settings.model.as_str(), settings.effort),
        (ProviderKind::Anthropic, "claude-opus-4-1-20250805", Effort::High)
    );
    assert_eq!(settings.openai, None);
    assert_eq!(service.provider_status().connections.len(), 3);
}

fn text_reply(text: &str) -> String {
    openai_response(json!([{"type": "message", "content": [{"type": "output_text", "text": text}]}]))
}

fn header(seen: &Seen, name: &str) -> Option<String> {
    seen.head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim().to_owned())
    })
}

#[tokio::test]
async fn switching_to_a_model_without_reasoning_sends_no_reasoning_items_and_respects_its_output_limit() {
    let mock = Mock::start().await;
    let h = harness(&mock).await;
    use_openai(&h, "gpt-6.2-sol");
    let reasoning = json!({"type": "reasoning", "id": "rs_1", "encrypted_content": "enc-1", "summary": []});
    let message = json!({"type": "message", "content": [{"type": "output_text", "text": "First."}]});
    mock.reply("/v1/responses", openai_response(json!([reasoning, message])));
    let task = h.service.create_task(request(&h, "One")).unwrap();
    assert_eq!(finish(&h, &task.id).await.status, TaskStatus::Completed);
    mock.reply("/v1/responses", text_reply("Second."));
    let to = ProviderChoice { provider: ProviderKind::OpenAi, model: "gpt-4.1".into(), effort: Effort::Medium };
    h.service
        .continue_task_with(
            &task.id,
            ContinueTaskRequest { prompt: "Two".into(), budgets: None, attachments: vec![], provider: Some(to) },
        )
        .unwrap();
    assert_eq!(finish(&h, &task.id).await.status, TaskStatus::Completed);
    let second = &mock.seen()[1].body;
    assert_eq!(second["model"], "gpt-4.1");
    assert!(second.get("reasoning").is_none() && second.get("include").is_none());
    assert!(second["input"].as_array().unwrap().iter().all(|i| i["type"] != "reasoning"));
    assert_eq!(second["max_output_tokens"], 16_384);
}

#[tokio::test]
async fn a_task_keeps_the_connection_context_it_started_with_until_it_switches() {
    let mock = Mock::start().await;
    let h = harness(&mock).await;
    use_openai(&h, "gpt-6.2-sol");
    let with_project = |project: &str| {
        let mut settings = h.service.settings().unwrap();
        settings.openai = Some(OpenAiConnection { organization: None, project: Some(project.into()) });
        h.service.update_settings(settings).unwrap();
    };
    with_project("proj_a");
    mock.reply("/v1/responses", text_reply("one"));
    let task = h.service.create_task(request(&h, "One")).unwrap();
    finish(&h, &task.id).await;
    with_project("proj_b");
    mock.reply("/v1/responses", text_reply("two"));
    h.service.continue_task(&task.id, "Two", None).unwrap();
    finish(&h, &task.id).await;
    mock.reply("/v1/responses", text_reply("three"));
    let to = ProviderChoice { provider: ProviderKind::OpenAi, model: "gpt-6.2-astra".into(), effort: Effort::Medium };
    h.service
        .continue_task_with(
            &task.id,
            ContinueTaskRequest { prompt: "Three".into(), budgets: None, attachments: vec![], provider: Some(to) },
        )
        .unwrap();
    finish(&h, &task.id).await;
    let projects: Vec<Option<String>> = mock.seen().iter().map(|s| header(s, "OpenAI-Project")).collect();
    assert_eq!(projects, [Some("proj_a".to_owned()), Some("proj_a".to_owned()), Some("proj_b".to_owned())]);
}

#[tokio::test]
async fn key_changes_change_the_connection_version_and_attachment_only_messages_show_no_placeholder() {
    let mock = Mock::start().await;
    let h = harness(&mock).await;
    let version = |h: &Harness| {
        h.service.provider_status().connections.iter().find(|c| c.provider == ProviderKind::OpenAi).unwrap().version
    };
    let before = version(&h);
    h.service.set_provider_key(ProviderKind::OpenAi, OPENAI_KEY).unwrap();
    let after_set = version(&h);
    h.service.set_provider_key(ProviderKind::OpenAi, "sk-proj-second-account-0123456789").unwrap();
    assert!(before < after_set && after_set < version(&h), "every key change is visible to views");

    use_openai(&h, "gpt-6.2-sol");
    let note = added(h.service.add_attachment_bytes(Some("n.txt"), None, &attachments::base64_encode(b"note\n")));
    mock.reply("/v1/responses", text_reply("ok"));
    let mut req = request(&h, "");
    req.attachments = Some(vec![note.id.clone()]);
    let task = h.service.create_task(req).unwrap();
    finish(&h, &task.id).await;
    let shown = h.service.task_detail(&task.id).unwrap().transcript.into_iter().find_map(|i| match i {
        TranscriptItem::UserText { text, attachments, .. } if !attachments.is_empty() => Some(text),
        _ => None,
    });
    assert_eq!(shown.as_deref(), Some(""), "the request text of an attachment-only message is not shown as the user's");
}

/// Structured questions through a real provider adapter (mocked HTTP):
/// `ask_user` is offered only with a user in the loop; a "Chat about it"
/// request offers read-only tools plus `revise_question`, holds the batch,
/// and carries the user's message; the answer comes back as the call's result.
#[tokio::test]
async fn questions_are_offered_with_a_user_and_a_discussion_offers_read_only_tools() {
    let mock = Mock::start().await;
    let h = harness(&mock).await;
    use_openai(&h, "gpt-6.2-sol");
    let options = json!([
        {"id": "a", "label": "Formal", "consequence": "", "recommended": false},
        {"id": "b", "label": "Informal", "consequence": "", "recommended": true}
    ]);
    let arguments =
        json!({"questions": [{"prompt": "Which tone?", "explanation": "", "options": options}], "allowCustom": true});
    let ask = json!({"type": "function_call", "id": "fc_1", "call_id": "call_ask", "name": "ask_user", "arguments": arguments.to_string()});
    let read = json!({"type": "function_call", "id": "fc_2", "call_id": "call_read", "name": "read_file", "arguments": "{\"path\":\"main.typ\"}"});
    mock.reply("/v1/responses", openai_response(json!([ask, read])));
    mock.reply("/v1/responses", text_reply("Formal reads like a paper."));
    mock.reply("/v1/responses", text_reply("Informal it is."));
    let mut events = h.service.subscribe();
    let task = h.service.create_task(request(&h, "Tidy the notes")).unwrap();
    let question = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(AgentEvent::QuestionRequested { request }) = events.recv().await {
                return request;
            }
        }
    })
    .await
    .expect("question asked");
    h.service.discuss_question(&task.id, &question.id, 1, "Why formal?").unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        while h.service.task_detail(&task.id).unwrap().questions[0].state != QuestionRequestState::Pending {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("discussion finished");
    let b = QuestionAnswer { question_id: "q1".into(), value: QuestionValue::Option { option_id: "b".into() } };
    h.service.answer_question(&task.id, &question.id, 1, vec![b]).unwrap();
    assert_eq!(finish(&h, &task.id).await.status, TaskStatus::Completed);

    let seen: Vec<Seen> = mock.seen().into_iter().filter(|s| s.line.contains("/v1/responses")).collect();
    assert_eq!(seen.len(), 3, "the request, one discussion request, the request after the answer");
    let tools = |s: &Seen| -> Vec<String> {
        s.body["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_owned()).collect()
    };
    assert!(tools(&seen[0]).contains(&"ask_user".to_owned()));
    assert!(!tools(&seen[0]).contains(&"revise_question".to_owned()));
    assert_eq!(tools(&seen[1]), ["list_files", "read_file", "graph_query", "revise_question"]);
    let input = seen[1].body["input"].as_array().unwrap().clone();
    let output = |items: &[Value], call: &str| -> String {
        items
            .iter()
            .find(|i| i["type"] == "function_call_output" && i["call_id"] == call)
            .and_then(|i| i["output"].as_str())
            .unwrap_or_default()
            .to_owned()
    };
    assert!(output(&input, "call_ask").contains("awaitingAnswer"));
    assert!(output(&input, "call_read").starts_with("Held"), "the batch is held");
    assert_eq!(input.last().unwrap()["content"][0]["text"], "Why formal?");
    let after = seen[2].body["input"].as_array().unwrap().clone();
    assert_eq!(serde_json::from_str::<Value>(&output(&after, "call_ask")).unwrap(), json!({"q1": {"optionId": "b"}}));
    assert!(output(&after, "call_read").contains("The cat sat"), "the held call ran after the answer");
    let texts: Vec<String> = after
        .iter()
        .filter(|i| i["type"] == "message" && i["role"] == "user")
        .flat_map(|i| i["content"].as_array().cloned().unwrap_or_default())
        .filter_map(|c| c["text"].as_str().map(str::to_owned))
        .collect();
    assert!(texts.iter().any(|t| t.starts_with("<question_discussion") && t.contains("Why formal?")));

    // A task without a user in the loop is not offered ask_user.
    mock.reply("/v1/responses", text_reply("Reviewed."));
    let review = h.service.create_unattended_task(request(&h, "Review the notes")).unwrap();
    finish(&h, &review.id).await;
    let last = mock.seen().into_iter().rfind(|s| s.line.contains("/v1/responses")).unwrap();
    assert!(!tools(&last).contains(&"ask_user".to_owned()));
}
