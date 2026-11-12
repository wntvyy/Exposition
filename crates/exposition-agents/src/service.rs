//! The agent service: task lifecycle, the task loop, approvals, and review.
//!
//! One task run is a loop of model requests and tool executions:
//! stream a response (progress events as it arrives) → append it to the
//! append-only history → execute every tool call it requested, in order →
//! return all results in one user message → repeat until the model ends its
//! turn, a budget is reached, the user cancels, or an error occurs. Budgets are
//! enforced here, outside model prose. Cancellation aborts the stream or the
//! running tool immediately; changes made so far stay recorded and revertible.

use std::collections::HashMap;
#[cfg(debug_assertions)]
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use exposition_core::path::WorkspacePath;
use exposition_core::text::utf16_range_to_bytes;
use exposition_workspace::{DocumentService, WorkspaceService};
use serde_json::{Value, json};
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::credentials::{SecretStore, normalize_api_key};
use crate::model::*;
use crate::provider::anthropic::{API_BASE, AnthropicProvider, MAX_TOKENS_PER_TURN, RetryPolicy};
use crate::provider::{
    Provider, ProviderError, ProviderMessage, ProviderRequest, StreamEvent, block_type, sanitize_after_fallback,
};
use crate::store::{Store, StoreError, TaskRow, ToolCallRow, TranscriptEntry, now_ms};
use crate::tools::exec::{self, Gate, SeenTexts, ToolEnv, ToolOutput};
use crate::tools::services::ToolServices;
use crate::tools::{self, ToolCall};
use crate::workspace::{LocalProjectFiles, ProjectFiles, content_hash};
use crate::{prompt, review};

const SETTINGS_KEY: &str = "provider";
/// Bound on result text kept for transcript display.
const TRANSCRIPT_RESULT_LIMIT: usize = 4000;
const MAX_PROMPT_CHARS: usize = 100_000;

pub struct AgentConfig {
    /// SQLite database path; `None` keeps everything in memory (tests).
    pub database: Option<PathBuf>,
    pub documents: Arc<DocumentService>,
    /// The application's workspace service. When present, tasks may only act
    /// on the open workspace folder and documents open through it.
    pub workspace: Option<Arc<WorkspaceService>>,
    pub credentials: Arc<dyn SecretStore>,
    pub services: Arc<dyn ToolServices>,
    pub runtime: tokio::runtime::Handle,
    /// Allow the development-only scripted provider. Ignored in release builds.
    pub allow_scripted: bool,
    /// Provider base URL override for tests; the application uses the default.
    pub anthropic_base_url: Option<String>,
    /// Provider retry policy override for tests.
    pub retry: Option<RetryPolicy>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct AgentError {
    /// Stable machine-readable code, e.g. `agents.notConfigured`.
    pub code: String,
    /// Human-readable message, safe to show (never contains secrets).
    pub message: String,
}

impl AgentError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self { code: code.to_owned(), message: message.into() }
    }
}

impl From<StoreError> for AgentError {
    fn from(e: StoreError) -> Self {
        AgentError::new("agents.storage", e.to_string())
    }
}

type Result<T, E = AgentError> = std::result::Result<T, E>;

#[derive(Clone)]
pub struct AgentService {
    inner: Arc<Inner>,
}

struct Inner {
    store: Store,
    documents: Arc<DocumentService>,
    workspace: Option<Arc<WorkspaceService>>,
    credentials: Arc<dyn SecretStore>,
    services: Arc<dyn ToolServices>,
    runtime: tokio::runtime::Handle,
    allow_scripted: bool,
    anthropic_base_url: String,
    retry: RetryPolicy,
    events: broadcast::Sender<AgentEvent>,
    running: Mutex<HashMap<TaskId, Running>>,
    #[cfg(debug_assertions)]
    scripts: Mutex<ScriptState>,
}

#[cfg(debug_assertions)]
#[derive(Default)]
struct ScriptState {
    queue: VecDeque<crate::provider::scripted::Script>,
    by_task: HashMap<TaskId, (crate::provider::scripted::Script, Arc<std::sync::atomic::AtomicUsize>)>,
}

struct Running {
    cancel: CancellationToken,
    approvals: HashMap<String, (ApprovalRequest, oneshot::Sender<bool>)>,
    join: Option<JoinHandle<()>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl AgentService {
    pub fn new(config: AgentConfig) -> Result<Self> {
        let store = match &config.database {
            Some(path) => Store::open(path)?,
            None => Store::open_in_memory()?,
        };
        // Tasks left running by a previous process were interrupted.
        for id in store.mark_interrupted()? {
            store.append_notice(
                &id,
                NoticeLevel::Warning,
                "Exposition closed while this task was running. Its changes so far remain reviewable; continue the \
                 task to resume.",
            )?;
        }
        let (events, _) = broadcast::channel(4096);
        Ok(Self {
            inner: Arc::new(Inner {
                store,
                documents: config.documents,
                workspace: config.workspace,
                credentials: config.credentials,
                services: config.services,
                runtime: config.runtime,
                allow_scripted: config.allow_scripted && cfg!(debug_assertions),
                anthropic_base_url: config.anthropic_base_url.unwrap_or_else(|| API_BASE.to_owned()),
                retry: config.retry.unwrap_or_default(),
                events,
                running: Mutex::new(HashMap::new()),
                #[cfg(debug_assertions)]
                scripts: Mutex::new(ScriptState::default()),
            }),
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.inner.events.subscribe()
    }

    // ---- provider settings and credentials

    pub fn settings(&self) -> Result<ProviderSettings> {
        self.inner.settings()
    }

    pub fn provider_status(&self) -> ProviderStatus {
        let settings = self.inner.settings().unwrap_or_default();
        let (configured, credential_error) = match self.inner.credentials.get() {
            Ok(secret) => (secret.is_some(), None),
            Err(e) => (false, Some(e.to_string())),
        };
        ProviderStatus {
            settings,
            configured,
            credential_store: self.inner.credentials.describe(),
            credential_error,
            scripted_available: self.inner.allow_scripted,
        }
    }

    pub fn update_settings(&self, settings: ProviderSettings) -> Result<ProviderStatus> {
        let model = settings.model.trim();
        let valid = !model.is_empty()
            && model.len() <= 100
            && model.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '@'));
        if !valid {
            return Err(AgentError::new("agents.invalidModel", "The model identifier is not valid."));
        }
        if settings.provider == ProviderKind::Scripted && !self.inner.allow_scripted {
            return Err(AgentError::new(
                "agents.scriptedUnavailable",
                "The scripted provider is only available in development builds.",
            ));
        }
        let settings = ProviderSettings { model: model.to_owned(), ..settings };
        let raw = serde_json::to_string(&settings).map_err(|e| AgentError::new("agents.storage", e.to_string()))?;
        self.inner.store.set_setting(SETTINGS_KEY, &raw)?;
        Ok(self.provider_status())
    }

    /// Store the API key in OS credential storage. The key is never returned.
    pub fn set_api_key(&self, raw: &str) -> Result<ProviderStatus> {
        let key = normalize_api_key(raw).map_err(|e| AgentError::new("agents.invalidKey", e.to_string()))?;
        self.inner.credentials.set(&key).map_err(|e| AgentError::new("agents.credentials", e.to_string()))?;
        Ok(self.provider_status())
    }

    pub fn clear_api_key(&self) -> Result<ProviderStatus> {
        self.inner.credentials.delete().map_err(|e| AgentError::new("agents.credentials", e.to_string()))?;
        Ok(self.provider_status())
    }

    /// Queue a development script for the next scripted task (debug builds only).
    #[cfg(debug_assertions)]
    pub fn load_script(&self, script: Value) -> Result<()> {
        if !self.inner.allow_scripted {
            return Err(AgentError::new("agents.scriptedUnavailable", "The scripted provider is disabled."));
        }
        let script: crate::provider::scripted::Script = serde_json::from_value(script)
            .map_err(|e| AgentError::new("agents.invalidScript", format!("invalid development script: {e}")))?;
        lock(&self.inner.scripts).queue.push_back(script);
        Ok(())
    }

    // ---- tasks

    pub fn list_tasks(&self) -> Result<Vec<TaskSummary>> {
        let rows = self.inner.store.tasks()?;
        rows.iter().map(|row| self.inner.summary_of(row)).collect()
    }

    pub fn task(&self, id: &TaskId) -> Result<TaskSummary> {
        self.inner.summary(id)
    }

    pub fn task_detail(&self, id: &TaskId) -> Result<TaskDetail> {
        let summary = self.inner.summary(id)?;
        let transcript = self.inner.transcript(id)?;
        let approvals = lock(&self.inner.running)
            .get(id)
            .map(|r| r.approvals.values().map(|(a, _)| a.clone()).collect())
            .unwrap_or_default();
        Ok(TaskDetail { summary, transcript, approvals })
    }

    pub fn create_task(&self, request: CreateTaskRequest) -> Result<TaskSummary> {
        let prompt = request.prompt.trim();
        if prompt.is_empty() {
            return Err(AgentError::new("agents.emptyPrompt", "Describe what the agent should do."));
        }
        if prompt.chars().count() > MAX_PROMPT_CHARS {
            return Err(AgentError::new("agents.promptTooLong", "The request is too long."));
        }
        let files = LocalProjectFiles::open(Path::new(request.workspace_root.trim()))
            .map_err(|e| AgentError::new("agents.invalidWorkspace", e.to_string()))?;
        crate::workspace::check_root(self.inner.workspace.as_deref(), files.root())
            .map_err(|e| AgentError::new("agents.workspaceNotOpen", e))?;
        let budgets = request.budgets.unwrap_or_default();
        budgets.validate().map_err(|e| AgentError::new("agents.invalidBudgets", e))?;
        if let TaskScope::Files { paths } = &request.scope
            && paths.is_empty()
        {
            return Err(AgentError::new("agents.invalidScope", "Select at least one file for the task scope."));
        }
        let settings = self.inner.settings()?;
        let consent = request.consent.unwrap_or(settings.consent);
        let selection_text = match &request.scope {
            TaskScope::Selection { path, range, revision } => Some(self.inner.selection_text(path, *range, *revision)?),
            _ => None,
        };
        let spec = TaskSpec {
            prompt: prompt.to_owned(),
            workspace_root: files.display_root(),
            scope: request.scope,
            permission: request.permission,
            execution: request.execution,
            consent,
            budgets,
            provider: ProviderChoice { provider: settings.provider, model: settings.model, effort: settings.effort },
        };
        let id = TaskId::new();
        let provider = self.inner.make_provider(&spec.provider, &id)?;
        let now = now_ms();
        let row = TaskRow {
            id: id.clone(),
            title: prompt::title_for(prompt),
            created_at_ms: now,
            updated_at_ms: now,
            spec: spec.clone(),
            status: TaskStatus::Queued,
            usage: TaskUsage::default(),
        };
        self.inner.store.insert_task(&row)?;
        let selection = selection_text.filter(|_| consent == ContextConsent::ScopeContent);
        let context =
            prompt::context_block(&spec, &self.inner.documents, self.inner.services.as_ref(), selection.as_deref());
        self.inner.store.append_message(
            &id,
            "user",
            &[json!({"type": "text", "text": context}), json!({"type": "text", "text": spec.prompt})],
        )?;
        self.spawn(id.clone(), provider);
        let summary = self.inner.summary(&id)?;
        self.inner.emit(AgentEvent::TaskChanged { task: Box::new(summary.clone()) });
        Ok(summary)
    }

    /// Continue a stopped task with a follow-up request (optionally raising budgets).
    pub fn continue_task(&self, id: &TaskId, prompt: &str, budgets: Option<Budgets>) -> Result<TaskSummary> {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Err(AgentError::new("agents.emptyPrompt", "Describe what the agent should do next."));
        }
        let mut row = self.inner.store.task(id)?.ok_or_else(|| not_found(id))?;
        if row.status.is_active() || lock(&self.inner.running).contains_key(id) {
            return Err(AgentError::new("agents.taskRunning", "The task is still running."));
        }
        if let Some(budgets) = budgets {
            budgets.validate().map_err(|e| AgentError::new("agents.invalidBudgets", e))?;
            row.spec.budgets = budgets;
            self.inner.store.update_task_spec(id, &row.spec)?;
        }
        crate::workspace::check_root(self.inner.workspace.as_deref(), Path::new(&row.spec.workspace_root))
            .map_err(|e| AgentError::new("agents.workspaceNotOpen", e))?;
        let provider = self.inner.make_provider(&row.spec.provider, id)?;
        let mut content = self.inner.dangling_tool_results(id)?;
        content.push(json!({"type": "text", "text": prompt}));
        self.inner.store.append_message(id, "user", &content)?;
        self.inner.store.update_task(id, &TaskStatus::Queued, &row.usage)?;
        self.spawn(id.clone(), provider);
        let summary = self.inner.summary(id)?;
        self.inner.emit(AgentEvent::TaskChanged { task: Box::new(summary.clone()) });
        self.inner.emit(AgentEvent::TranscriptChanged { task_id: id.clone() });
        Ok(summary)
    }

    /// Cancel a running task: aborts the stream or the running tool.
    pub fn cancel_task(&self, id: &TaskId) -> Result<TaskSummary> {
        if let Some(running) = lock(&self.inner.running).get(id) {
            running.cancel.cancel();
        }
        self.inner.summary(id)
    }

    pub fn resolve_approval(&self, id: &TaskId, approval_id: &str, approved: bool) -> Result<()> {
        let sender = lock(&self.inner.running).get_mut(id).and_then(|r| r.approvals.remove(approval_id));
        match sender {
            Some((_, tx)) => {
                let _ = tx.send(approved);
                Ok(())
            }
            None => Err(AgentError::new("agents.noApproval", "That approval request is no longer pending.")),
        }
    }

    /// Wait for a task's current run to finish (tests and shutdown).
    pub async fn join(&self, id: &TaskId) {
        let handle = lock(&self.inner.running).get_mut(id).and_then(|r| r.join.take());
        if let Some(handle) = handle {
            let _ = handle.await;
        }
    }

    pub fn is_running(&self, id: &TaskId) -> bool {
        lock(&self.inner.running).contains_key(id)
    }

    fn spawn(&self, id: TaskId, provider: Arc<dyn Provider>) {
        let cancel = CancellationToken::new();
        lock(&self.inner.running)
            .insert(id.clone(), Running { cancel: cancel.clone(), approvals: HashMap::new(), join: None });
        let inner = Arc::clone(&self.inner);
        let task_id = id.clone();
        let handle = self.inner.runtime.spawn(async move {
            Inner::run(inner, task_id, provider, cancel).await;
        });
        if let Some(running) = lock(&self.inner.running).get_mut(&id) {
            running.join = Some(handle);
        }
    }

    // ---- review

    pub fn review(&self, id: &TaskId) -> Result<TaskReview> {
        self.inner.store.task(id)?.ok_or_else(|| not_found(id))?;
        let files = self.inner.store.task_files(id)?;
        Ok(TaskReview {
            task_id: id.clone(),
            checkpoint: self.inner.store.checkpoint(id)?,
            files: files.iter().map(|row| review::file_review(row, &self.inner.documents)).collect(),
        })
    }

    pub fn file_diff(&self, id: &TaskId, path: &WorkspacePath) -> Result<FileDiff> {
        let row = self
            .inner
            .store
            .task_file(id, path)?
            .ok_or_else(|| AgentError::new("agents.notFound", format!("The task did not change {path}.")))?;
        Ok(review::file_diff(&row))
    }

    /// Revert one file (or every file) of a task. Each file is independent;
    /// refusals leave that file untouched and are reported.
    pub fn revert(&self, id: &TaskId, path: Option<&WorkspacePath>) -> Result<Vec<ReviewActionResult>> {
        let row = self.inner.store.task(id)?.ok_or_else(|| not_found(id))?;
        if row.status.is_active() || self.is_running(id) {
            return Err(AgentError::new("agents.taskRunning", "Cancel the task before reverting its changes."));
        }
        let files = LocalProjectFiles::open(Path::new(&row.spec.workspace_root))
            .map_err(|e| AgentError::new("agents.invalidWorkspace", e.to_string()))?;
        crate::workspace::check_root(self.inner.workspace.as_deref(), files.root())
            .map_err(|e| AgentError::new("agents.workspaceNotOpen", e))?;
        let mut results = Vec::new();
        for file in self.inner.store.task_files(id)? {
            if path.is_some_and(|p| p != &file.path) {
                continue;
            }
            let workspace = self.inner.workspace.as_deref();
            results.push(review::revert_file(&self.inner.store, id, &self.inner.documents, workspace, &files, file)?);
        }
        if results.is_empty() && path.is_some() {
            return Err(AgentError::new("agents.notFound", "The task did not change that file."));
        }
        self.inner.review_changed(id);
        Ok(results)
    }

    pub fn accept(&self, id: &TaskId, path: Option<&WorkspacePath>) -> Result<Vec<ReviewActionResult>> {
        self.inner.store.task(id)?.ok_or_else(|| not_found(id))?;
        let mut results = Vec::new();
        for file in self.inner.store.task_files(id)? {
            if path.is_some_and(|p| p != &file.path) {
                continue;
            }
            if file.state == ReviewState::Pending || path.is_some() {
                results.push(review::accept_file(&self.inner.store, id, file)?);
            }
        }
        self.inner.review_changed(id);
        Ok(results)
    }
}

fn not_found(id: &TaskId) -> AgentError {
    AgentError::new("agents.notFound", format!("Unknown task {id}."))
}

enum Interrupt {
    Canceled,
    WallTime,
}

enum ApprovalOutcome {
    Approved,
    Declined,
    Canceled,
}

impl Inner {
    fn emit(&self, event: AgentEvent) {
        let _ = self.events.send(event);
    }

    fn settings(&self) -> Result<ProviderSettings> {
        let mut settings: ProviderSettings = match self.store.setting(SETTINGS_KEY)? {
            Some(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            None => ProviderSettings::default(),
        };
        if settings.provider == ProviderKind::Scripted && !self.allow_scripted {
            settings.provider = ProviderKind::Anthropic;
        }
        Ok(settings)
    }

    fn make_provider(&self, choice: &ProviderChoice, task: &TaskId) -> Result<Arc<dyn Provider>> {
        match choice.provider {
            ProviderKind::Anthropic => {
                let key = self
                    .credentials
                    .get()
                    .map_err(|e| AgentError::new("agents.credentials", e.to_string()))?
                    .ok_or_else(|| {
                        AgentError::new(
                            "agents.notConfigured",
                            "No API key is configured. Add your Anthropic API key in the agent's provider settings.",
                        )
                    })?;
                let provider = AnthropicProvider::with_base_url(
                    &self.anthropic_base_url,
                    key,
                    choice.model.clone(),
                    choice.effort,
                    self.retry,
                )
                .map_err(|e| AgentError::new("agents.provider", e.to_string()))?;
                Ok(Arc::new(provider))
            }
            ProviderKind::Scripted => self.scripted_provider(task),
        }
    }

    #[cfg(debug_assertions)]
    fn scripted_provider(&self, task: &TaskId) -> Result<Arc<dyn Provider>> {
        if !self.allow_scripted {
            return Err(AgentError::new("agents.scriptedUnavailable", "The scripted provider is disabled."));
        }
        let mut scripts = lock(&self.scripts);
        let (script, requests) = match scripts.by_task.get(task) {
            Some(entry) => entry.clone(),
            None => {
                let script = scripts.queue.pop_front().ok_or_else(|| {
                    AgentError::new("agents.noScript", "No development script is loaded for the scripted provider.")
                })?;
                let entry = (script, Arc::new(std::sync::atomic::AtomicUsize::new(0)));
                scripts.by_task.insert(task.clone(), entry.clone());
                entry
            }
        };
        Ok(Arc::new(crate::provider::scripted::ScriptedProvider::new(script, requests)))
    }

    #[cfg(not(debug_assertions))]
    fn scripted_provider(&self, _task: &TaskId) -> Result<Arc<dyn Provider>> {
        Err(AgentError::new("agents.scriptedUnavailable", "The scripted provider is not part of release builds."))
    }

    fn selection_text(&self, path: &WorkspacePath, range: exposition_core::TextRange, revision: u64) -> Result<String> {
        let id = self.documents.find_by_path(path).ok_or_else(|| {
            AgentError::new("agents.invalidScope", format!("{path} is not open, so the selection cannot be used."))
        })?;
        let snapshot =
            self.documents.snapshot(&id).map_err(|e| AgentError::new("agents.invalidScope", e.to_string()))?;
        if snapshot.revision != revision {
            return Err(AgentError::new("agents.staleSelection", "The document changed since the selection was made."));
        }
        let bytes = utf16_range_to_bytes(&snapshot.text, range)
            .map_err(|e| AgentError::new("agents.invalidScope", e.to_string()))?;
        Ok(snapshot.text[bytes].to_owned())
    }

    fn summary(&self, id: &TaskId) -> Result<TaskSummary> {
        let row = self.store.task(id)?.ok_or_else(|| not_found(id))?;
        self.summary_of(&row)
    }

    fn summary_of(&self, row: &TaskRow) -> Result<TaskSummary> {
        let files = self.store.task_files(&row.id)?;
        Ok(TaskSummary {
            id: row.id.clone(),
            title: row.title.clone(),
            created_at_ms: row.created_at_ms,
            updated_at_ms: row.updated_at_ms,
            status: row.status.clone(),
            spec: row.spec.clone(),
            usage: row.usage.clone(),
            pending_review: files.iter().filter(|f| f.state == ReviewState::Pending).count() as u32,
            changed_files: files.len() as u32,
        })
    }

    fn review_changed(&self, id: &TaskId) {
        self.emit(AgentEvent::ReviewChanged { task_id: id.clone() });
        if let Ok(summary) = self.summary(id) {
            self.emit(AgentEvent::TaskChanged { task: Box::new(summary) });
        }
    }

    fn transcript(&self, id: &TaskId) -> Result<Vec<TranscriptItem>> {
        let calls: HashMap<String, ToolCallRow> =
            self.store.tool_calls(id)?.into_iter().map(|c| (c.tool_use_id.clone(), c)).collect();
        let mut items = Vec::new();
        for row in self.store.transcript(id)? {
            let seq = row.seq;
            match row.entry {
                TranscriptEntry::Notice { level, message } => {
                    items.push(TranscriptItem::Notice { seq, level, message })
                }
                TranscriptEntry::Message { role, content } => {
                    for block in content {
                        let text = block.get("text").and_then(Value::as_str).unwrap_or_default();
                        match (role.as_str(), block_type(&block)) {
                            ("user", "text") if !text.starts_with("<task_context>") => {
                                items.push(TranscriptItem::UserText { seq, text: text.to_owned() })
                            }
                            ("assistant", "text") if !text.is_empty() => {
                                items.push(TranscriptItem::AssistantText { seq, text: text.to_owned() })
                            }
                            ("assistant", "thinking") => {
                                let thinking = block.get("thinking").and_then(Value::as_str).unwrap_or_default();
                                if !thinking.trim().is_empty() {
                                    items.push(TranscriptItem::Thinking { seq, text: thinking.to_owned() });
                                }
                            }
                            ("assistant", "tool_use") => {
                                let tool_use_id =
                                    block.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
                                let call = calls.get(&tool_use_id);
                                items.push(TranscriptItem::ToolCall {
                                    seq,
                                    name: block.get("name").and_then(Value::as_str).unwrap_or_default().to_owned(),
                                    input: block.get("input").cloned().unwrap_or(Value::Null),
                                    status: call.map(|c| c.status).unwrap_or(ToolCallStatus::NotRun),
                                    result: call
                                        .and_then(|c| c.result.as_deref())
                                        .map(|r| bounded(r, TRANSCRIPT_RESULT_LIMIT)),
                                    is_error: call.is_some_and(|c| c.is_error),
                                    tool_use_id,
                                });
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        Ok(items)
    }

    /// Results for tool calls left unanswered by an interrupted run.
    fn dangling_tool_results(&self, id: &TaskId) -> Result<Vec<Value>> {
        let messages = self.store.messages(id)?;
        let Some((role, content)) = messages.last() else { return Ok(Vec::new()) };
        if role != "assistant" {
            return Ok(Vec::new());
        }
        let calls: HashMap<String, ToolCallRow> =
            self.store.tool_calls(id)?.into_iter().map(|c| (c.tool_use_id.clone(), c)).collect();
        let mut results = Vec::new();
        for block in content.iter().filter(|b| block_type(b) == "tool_use") {
            let tool_use_id = block.get("id").and_then(Value::as_str).unwrap_or_default();
            let (text, is_error) = match calls.get(tool_use_id) {
                Some(call) if matches!(call.status, ToolCallStatus::Succeeded | ToolCallStatus::Failed) => {
                    (call.result.clone().unwrap_or_default(), call.is_error)
                }
                _ => (ToolOutput::error("Not executed: the previous run stopped before this tool ran.").text(), true),
            };
            results.push(tool_result(tool_use_id, &text, is_error));
        }
        Ok(results)
    }

    fn checkpoint(&self, id: &TaskId) -> Result<()> {
        if self.store.checkpoint(id)?.is_some() {
            return Ok(());
        }
        let documents = self
            .documents
            .snapshots()
            .into_iter()
            .map(|s| {
                let unsaved = self.documents.info(&s.id).map(|i| i.saved_revision != Some(i.revision)).unwrap_or(false);
                CheckpointDocument {
                    path: s.path.clone(),
                    document: s.id.0.clone(),
                    revision: s.revision,
                    content_hash: content_hash(&s.text),
                    unsaved,
                }
            })
            .collect();
        self.store.insert_checkpoint(id, &Checkpoint { created_at_ms: now_ms(), documents })?;
        Ok(())
    }

    async fn run(inner: Arc<Inner>, id: TaskId, provider: Arc<dyn Provider>, cancel: CancellationToken) {
        let status = match inner.store.task(&id) {
            Ok(Some(row)) => match LocalProjectFiles::open(Path::new(&row.spec.workspace_root)) {
                Ok(files) => {
                    let mut run = Run {
                        inner: &inner,
                        base_elapsed: row.usage.elapsed_ms,
                        id: id.clone(),
                        spec: row.spec,
                        usage: row.usage,
                        cancel: cancel.clone(),
                        started: Instant::now(),
                        paused: Duration::ZERO,
                        seen: SeenTexts::default(),
                        files,
                    };
                    let status = run.drive(provider.as_ref()).await;
                    run.usage.elapsed_ms = run.elapsed_ms();
                    Some((status, run.usage))
                }
                Err(e) => row_failure(&row.usage, format!("The project folder is not available: {e}")),
            },
            Ok(None) => None,
            Err(e) => row_failure(&TaskUsage::default(), e.to_string()),
        };
        if let Some((status, usage)) = status {
            inner.finish(&id, &status, &usage);
        }
        lock(&inner.running).remove(&id);
        if let Ok(summary) = inner.summary(&id) {
            inner.emit(AgentEvent::TaskChanged { task: Box::new(summary) });
        }
    }

    fn finish(&self, id: &TaskId, status: &TaskStatus, usage: &TaskUsage) {
        let notice = match status {
            TaskStatus::Canceled => {
                Some((NoticeLevel::Info, "Canceled. Changes made so far remain reviewable and revertible.".to_owned()))
            }
            TaskStatus::BudgetExceeded { detail, .. } => Some((NoticeLevel::Warning, format!("Stopped: {detail}"))),
            TaskStatus::Failed { message } => Some((NoticeLevel::Error, message.clone())),
            TaskStatus::Refused { category, explanation } => Some((
                NoticeLevel::Warning,
                format!(
                    "The model provider declined this request{}{}. Nothing from the declined response was applied.",
                    category.as_ref().map(|c| format!(" (category: {c})")).unwrap_or_default(),
                    explanation.as_ref().map(|e| format!(": {e}")).unwrap_or_default()
                ),
            )),
            _ => None,
        };
        if let Some((level, message)) = notice
            && let Err(e) = self.store.append_notice(id, level, &message)
        {
            tracing::error!("failed to record task notice: {e}");
        }
        if let Err(e) = self.store.update_task(id, status, usage) {
            tracing::error!("failed to record task status: {e}");
        }
        self.emit(AgentEvent::TranscriptChanged { task_id: id.clone() });
        self.emit(AgentEvent::ReviewChanged { task_id: id.clone() });
    }
}

fn row_failure(usage: &TaskUsage, message: String) -> Option<(TaskStatus, TaskUsage)> {
    Some((TaskStatus::Failed { message }, usage.clone()))
}

fn tool_result(tool_use_id: &str, text: &str, is_error: bool) -> Value {
    let mut block = json!({"type": "tool_result", "tool_use_id": tool_use_id, "content": text});
    if is_error {
        block["is_error"] = json!(true);
    }
    block
}

fn bounded(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

fn budget_exceeded(budget: BudgetKind, detail: impl Into<String>) -> TaskStatus {
    TaskStatus::BudgetExceeded { budget, detail: detail.into() }
}

/// One run of the task loop.
struct Run<'a> {
    inner: &'a Arc<Inner>,
    id: TaskId,
    spec: TaskSpec,
    usage: TaskUsage,
    cancel: CancellationToken,
    started: Instant,
    /// Time spent waiting for approvals (excluded from the wall-time budget).
    paused: Duration,
    base_elapsed: u64,
    seen: SeenTexts,
    files: LocalProjectFiles,
}

impl Run<'_> {
    fn elapsed_ms(&self) -> u64 {
        self.base_elapsed + self.started.elapsed().saturating_sub(self.paused).as_millis() as u64
    }

    fn remaining_wall(&self) -> Duration {
        Duration::from_millis(self.spec.budgets.max_wall_ms.saturating_sub(self.elapsed_ms()))
    }

    fn set_status(&mut self, status: TaskStatus) {
        self.usage.elapsed_ms = self.elapsed_ms();
        if let Err(e) = self.inner.store.update_task(&self.id, &status, &self.usage) {
            tracing::error!("failed to persist task status: {e}");
        }
        if let Ok(summary) = self.inner.summary(&self.id) {
            self.inner.emit(AgentEvent::TaskChanged { task: Box::new(summary) });
        }
    }

    fn notice(&self, level: NoticeLevel, message: &str) {
        if let Err(e) = self.inner.store.append_notice(&self.id, level, message) {
            tracing::error!("failed to record notice: {e}");
        }
        self.inner.emit(AgentEvent::TranscriptChanged { task_id: self.id.clone() });
    }

    fn budget_before_request(&self) -> Option<TaskStatus> {
        let b = &self.spec.budgets;
        if self.usage.turns >= b.max_turns {
            return Some(budget_exceeded(
                BudgetKind::Turns,
                format!("the limit of {} model turns was reached.", b.max_turns),
            ));
        }
        if self.usage.total_input() >= b.max_input_tokens {
            return Some(budget_exceeded(
                BudgetKind::InputTokens,
                format!("the input-token budget ({}) was used up.", b.max_input_tokens),
            ));
        }
        if b.max_output_tokens.saturating_sub(self.usage.output_tokens) < 1024 {
            return Some(budget_exceeded(
                BudgetKind::OutputTokens,
                format!("the output-token budget ({}) was used up.", b.max_output_tokens),
            ));
        }
        if self.remaining_wall().is_zero() {
            return Some(budget_exceeded(BudgetKind::WallTime, "the time budget was used up."));
        }
        None
    }

    async fn drive(&mut self, provider: &dyn Provider) -> TaskStatus {
        self.set_status(TaskStatus::Running);
        if let Err(e) = self.inner.checkpoint(&self.id) {
            return TaskStatus::Failed { message: format!("Could not record the checkpoint: {}", e.message) };
        }
        loop {
            if self.cancel.is_cancelled() {
                return TaskStatus::Canceled;
            }
            if let Some(status) = self.budget_before_request() {
                return status;
            }
            let messages = match self.inner.store.messages(&self.id) {
                Ok(m) => m.into_iter().map(|(role, content)| ProviderMessage { role, content }).collect(),
                Err(e) => return TaskStatus::Failed { message: e.to_string() },
            };
            let remaining_output = self.spec.budgets.max_output_tokens.saturating_sub(self.usage.output_tokens);
            let max_tokens = remaining_output.min(u64::from(MAX_TOKENS_PER_TURN)) as u32;
            let limited_by_budget = u64::from(max_tokens) < u64::from(MAX_TOKENS_PER_TURN);
            let request = ProviderRequest {
                system: prompt::SYSTEM_PROMPT.to_owned(),
                tools: tools::definitions(),
                messages,
                max_tokens,
            };
            let turn = self.usage.turns + 1;
            self.inner.emit(AgentEvent::StreamStarted { task_id: self.id.clone(), turn });
            let events = self.inner.events.clone();
            let task_id = self.id.clone();
            let sink = move |event: StreamEvent| {
                let event = match event {
                    StreamEvent::TextDelta(text) => AgentEvent::TextDelta { task_id: task_id.clone(), turn, text },
                    StreamEvent::ThinkingDelta(text) => {
                        AgentEvent::ThinkingDelta { task_id: task_id.clone(), turn, text }
                    }
                    StreamEvent::Reset { reason } => AgentEvent::StreamReset { task_id: task_id.clone(), turn, reason },
                    StreamEvent::Fallback { from, to } => AgentEvent::Fallback { task_id: task_id.clone(), from, to },
                    StreamEvent::MessageStart { .. } | StreamEvent::ToolUseStart { .. } => return,
                };
                let _ = events.send(event);
            };
            let result = tokio::select! {
                r = provider.stream(&request, &sink, &self.cancel) => Ok(r),
                _ = self.cancel.cancelled() => Err(Interrupt::Canceled),
                _ = tokio::time::sleep(self.remaining_wall()) => Err(Interrupt::WallTime),
            };
            let response = match result {
                Err(Interrupt::Canceled) | Ok(Err(ProviderError::Canceled)) => return TaskStatus::Canceled,
                Err(Interrupt::WallTime) => {
                    return budget_exceeded(
                        BudgetKind::WallTime,
                        "the time budget ran out while the model was responding.",
                    );
                }
                Ok(Err(e)) => return TaskStatus::Failed { message: e.to_string() },
                Ok(Ok(response)) => response,
            };
            self.usage.turns += 1;
            self.usage.input_tokens += response.usage.input_tokens;
            self.usage.output_tokens += response.usage.output_tokens;
            self.usage.cache_read_tokens += response.usage.cache_read_tokens;
            self.usage.cache_write_tokens += response.usage.cache_write_tokens;
            self.usage.served_by = Some(response.model.clone());
            self.set_status(TaskStatus::Running);

            for block in response.content.iter().filter(|b| block_type(b) == "fallback") {
                let from = block.pointer("/from/model").and_then(Value::as_str).unwrap_or("the requested model");
                let to = block.pointer("/to/model").and_then(Value::as_str).unwrap_or("a fallback model");
                self.notice(
                    NoticeLevel::Info,
                    &format!("{from} declined part of this turn; {to} continued it (server-side fallback)."),
                );
            }
            if response.stop_reason == "refusal" {
                // Never execute or keep the declined (possibly partial) output.
                let details = response.stop_details.unwrap_or(Value::Null);
                return TaskStatus::Refused {
                    category: details.get("category").and_then(Value::as_str).map(str::to_owned),
                    explanation: details.get("explanation").and_then(Value::as_str).map(str::to_owned),
                };
            }
            let content = sanitize_after_fallback(response.content);
            if !content.is_empty() {
                if let Err(e) = self.inner.store.append_message(&self.id, "assistant", &content) {
                    return TaskStatus::Failed { message: e.to_string() };
                }
                self.inner.emit(AgentEvent::TranscriptChanged { task_id: self.id.clone() });
            }
            match response.stop_reason.as_str() {
                "tool_use" => {
                    if !content.iter().any(|b| block_type(b) == "tool_use") {
                        return TaskStatus::Completed;
                    }
                    if let Some(status) = self.execute_tools(&content, &response.invalid_tool_inputs, turn).await {
                        return status;
                    }
                }
                "max_tokens" => {
                    return if limited_by_budget {
                        budget_exceeded(
                            BudgetKind::OutputTokens,
                            format!(
                                "the output-token budget ({}) ran out mid-response.",
                                self.spec.budgets.max_output_tokens
                            ),
                        )
                    } else {
                        TaskStatus::Failed {
                            message: format!(
                                "The response reached the per-turn output limit ({MAX_TOKENS_PER_TURN} tokens); its \
                                 tool calls were not run. Continue the task to resume."
                            ),
                        }
                    };
                }
                "pause_turn" => {}
                _ => return TaskStatus::Completed,
            }
        }
    }

    fn record_call(
        &self,
        tool_use_id: &str,
        turn: u32,
        name: &str,
        input: &Value,
        status: ToolCallStatus,
        result: Option<&ToolOutput>,
    ) {
        let row = ToolCallRow {
            tool_use_id: tool_use_id.to_owned(),
            turn,
            name: name.to_owned(),
            input: input.clone(),
            status,
            result: result.map(ToolOutput::text),
            is_error: result.is_some_and(|r| r.is_error),
        };
        if let Err(e) = self.inner.store.upsert_tool_call(&self.id, &row) {
            tracing::error!("failed to record tool call: {e}");
        }
    }

    /// Execute every tool call of one response, in order, and append all
    /// results as one user message. Returns a final status when the run must stop.
    async fn execute_tools(
        &mut self,
        content: &[Value],
        invalid: &HashMap<String, String>,
        turn: u32,
    ) -> Option<TaskStatus> {
        let calls: Vec<(String, String, Value)> = content
            .iter()
            .filter(|b| block_type(b) == "tool_use")
            .map(|b| {
                (
                    b.get("id").and_then(Value::as_str).unwrap_or_default().to_owned(),
                    b.get("name").and_then(Value::as_str).unwrap_or_default().to_owned(),
                    b.get("input").cloned().unwrap_or(Value::Null),
                )
            })
            .collect();
        let mut results = Vec::with_capacity(calls.len());
        let mut budget_hit: Option<TaskStatus> = None;
        for (index, (tool_use_id, name, input)) in calls.iter().enumerate() {
            self.record_call(tool_use_id, turn, name, input, ToolCallStatus::Running, None);
            self.inner.emit(AgentEvent::ToolCallStarted {
                task_id: self.id.clone(),
                tool_use_id: tool_use_id.clone(),
                name: name.clone(),
            });
            let output = match self.execute_one(tool_use_id, name, input, invalid, &mut budget_hit).await {
                Ok(output) => output,
                Err((interrupt, partial)) => {
                    let stopped = ToolOutput::error("Not executed: the task stopped.");
                    let mut rest = &calls[index..];
                    if let Some(output) = partial {
                        // The tool had started; record what it reported.
                        let status = if output.is_error { ToolCallStatus::Failed } else { ToolCallStatus::Succeeded };
                        self.record_call(tool_use_id, turn, name, input, status, Some(&output));
                        rest = &calls[index + 1..];
                    }
                    for (id, n, i) in rest {
                        self.record_call(id, turn, n, i, ToolCallStatus::NotRun, Some(&stopped));
                    }
                    self.inner.emit(AgentEvent::TranscriptChanged { task_id: self.id.clone() });
                    return Some(match interrupt {
                        Interrupt::Canceled => TaskStatus::Canceled,
                        Interrupt::WallTime => {
                            budget_exceeded(BudgetKind::WallTime, "the time budget ran out while a tool was running.")
                        }
                    });
                }
            };
            let status = if output.is_error { ToolCallStatus::Failed } else { ToolCallStatus::Succeeded };
            self.record_call(tool_use_id, turn, name, input, status, Some(&output));
            self.inner.emit(AgentEvent::ToolCallFinished {
                task_id: self.id.clone(),
                tool_use_id: tool_use_id.clone(),
                is_error: output.is_error,
            });
            results.push(tool_result(tool_use_id, &output.text(), output.is_error));
        }
        if let Err(e) = self.inner.store.append_message(&self.id, "user", &results) {
            return Some(TaskStatus::Failed { message: e.to_string() });
        }
        self.set_status(TaskStatus::Running);
        self.inner.emit(AgentEvent::TranscriptChanged { task_id: self.id.clone() });
        self.inner.emit(AgentEvent::ReviewChanged { task_id: self.id.clone() });
        budget_hit
    }

    async fn execute_one(
        &mut self,
        tool_use_id: &str,
        name: &str,
        input: &Value,
        invalid: &HashMap<String, String>,
        budget_hit: &mut Option<TaskStatus>,
    ) -> Result<ToolOutput, (Interrupt, Option<ToolOutput>)> {
        if let Some(raw) = invalid.get(tool_use_id) {
            return Ok(ToolOutput::error_with(
                "The tool input was not valid JSON, so the tool was not run. Send the call again with valid input.",
                json!({"INVALID_JSON": raw}),
            ));
        }
        if budget_hit.is_some() {
            return Ok(ToolOutput::error("Not executed: a task budget was reached."));
        }
        let budgets = self.spec.budgets.clone();
        if self.usage.tool_calls >= budgets.max_tool_calls {
            *budget_hit = Some(budget_exceeded(
                BudgetKind::ToolCalls,
                format!("the limit of {} tool calls was reached.", budgets.max_tool_calls),
            ));
            return Ok(ToolOutput::error("Not executed: the task's tool-call budget is used up."));
        }
        self.usage.tool_calls += 1;
        let call = match ToolCall::parse(name, input) {
            Ok(call) => call,
            Err(message) => return Ok(ToolOutput::error(message)),
        };
        if call.is_process() && self.usage.commands >= budgets.max_commands {
            *budget_hit = Some(budget_exceeded(
                BudgetKind::Commands,
                format!("the limit of {} process launches was reached.", budgets.max_commands),
            ));
            return Ok(ToolOutput::error("Not executed: the task's command budget is used up."));
        }
        let gate = {
            let env = self.env();
            exec::gate(&call, &env)
        };
        let policy = match gate {
            Gate::Refused(output) => return Ok(output),
            Gate::Allowed { policy } => policy,
            Gate::NeedsApproval { title, detail, policy } => {
                match self.request_approval(tool_use_id, name, input, title, detail).await {
                    ApprovalOutcome::Approved => policy,
                    ApprovalOutcome::Declined => {
                        return Ok(ToolOutput::error("The user declined this action. Do not retry it unchanged."));
                    }
                    ApprovalOutcome::Canceled => return Err((Interrupt::Canceled, None)),
                }
            }
        };
        if self.cancel.is_cancelled() {
            return Err((Interrupt::Canceled, None));
        }
        if call.is_process() {
            self.usage.commands += 1;
        }
        let cancel = self.cancel.child_token();
        let wall = self.remaining_wall();
        let inner = Arc::clone(self.inner);
        let env = ToolEnv {
            task_id: &self.id,
            spec: &self.spec,
            files: &self.files,
            documents: &inner.documents,
            workspace: inner.workspace.as_deref(),
            services: inner.services.as_ref(),
            store: &inner.store,
        };
        // A tool that is still running when the task is canceled (or the time
        // budget ends) has its own token canceled and gets a short grace period
        // to stop its process and report; its outcome is recorded as it is.
        let tool = exec::run(call, &env, &mut self.seen, tool_use_id, policy, cancel.clone());
        tokio::pin!(tool);
        let interrupt = tokio::select! {
            biased;
            output = &mut tool => return Ok(output),
            _ = self.cancel.cancelled() => Interrupt::Canceled,
            _ = tokio::time::sleep(wall) => Interrupt::WallTime,
        };
        cancel.cancel();
        let output = tokio::time::timeout(Duration::from_secs(5), &mut tool).await.ok();
        Err((interrupt, output))
    }

    fn env(&self) -> ToolEnv<'_> {
        ToolEnv {
            task_id: &self.id,
            spec: &self.spec,
            files: &self.files,
            documents: &self.inner.documents,
            workspace: self.inner.workspace.as_deref(),
            services: self.inner.services.as_ref(),
            store: &self.inner.store,
        }
    }

    async fn request_approval(
        &mut self,
        tool_use_id: &str,
        tool: &str,
        input: &Value,
        title: String,
        detail: String,
    ) -> ApprovalOutcome {
        let approval = ApprovalRequest {
            id: format!("approval-{}", uuid::Uuid::new_v4()),
            task_id: self.id.clone(),
            tool_use_id: tool_use_id.to_owned(),
            tool: tool.to_owned(),
            title,
            detail,
        };
        let (tx, rx) = oneshot::channel();
        match lock(&self.inner.running).get_mut(&self.id) {
            Some(running) => {
                running.approvals.insert(approval.id.clone(), (approval.clone(), tx));
            }
            None => return ApprovalOutcome::Canceled,
        }
        let turn = self.usage.turns;
        self.record_call(tool_use_id, turn, tool, input, ToolCallStatus::AwaitingApproval, None);
        self.set_status(TaskStatus::AwaitingApproval);
        self.inner.emit(AgentEvent::ApprovalRequested { approval: approval.clone() });
        let waiting = Instant::now();
        let outcome = tokio::select! {
            decision = rx => if decision.unwrap_or(false) { ApprovalOutcome::Approved } else { ApprovalOutcome::Declined },
            _ = self.cancel.cancelled() => ApprovalOutcome::Canceled,
        };
        self.paused += waiting.elapsed();
        if let Some(running) = lock(&self.inner.running).get_mut(&self.id) {
            running.approvals.remove(&approval.id);
        }
        self.inner.emit(AgentEvent::ApprovalResolved {
            task_id: self.id.clone(),
            approval_id: approval.id,
            approved: matches!(outcome, ApprovalOutcome::Approved),
        });
        if !matches!(outcome, ApprovalOutcome::Canceled) {
            self.set_status(TaskStatus::Running);
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::MemoryStore;
    use crate::tools::services::UnavailableServices;

    #[test]
    fn scripted_provider_is_only_available_in_debug_builds() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let service = AgentService::new(AgentConfig {
            database: None,
            documents: Arc::new(DocumentService::new()),
            workspace: None,
            credentials: Arc::new(MemoryStore::default()),
            services: Arc::new(UnavailableServices),
            runtime: runtime.handle().clone(),
            allow_scripted: true,
            anthropic_base_url: None,
            retry: None,
        })
        .unwrap();
        assert_eq!(service.provider_status().scripted_available, cfg!(debug_assertions));
        let scripted = ProviderSettings { provider: ProviderKind::Scripted, ..ProviderSettings::default() };
        assert_eq!(service.update_settings(scripted).is_ok(), cfg!(debug_assertions));
        // A stored scripted setting is never honored by a release build.
        if !cfg!(debug_assertions) {
            assert_eq!(service.settings().unwrap().provider, ProviderKind::Anthropic);
        }
    }
}
