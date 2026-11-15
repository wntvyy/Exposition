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
use exposition_graph::{GraphService, ProjectId};
use exposition_workspace::{DocumentService, WorkspaceService};
use serde_json::{Value, json};
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::credentials::{SecretStore, normalize_api_key};
use crate::graph::{GraphEnv, TaskGraphInfo};
use crate::model::*;
use crate::permissions::{
    PermissionChoice, PermissionError, PermissionStore, PermissionsChange, ProjectPermissions, ProjectPermissionsEntry,
    ProjectPermissionsState, same_folder, within_folder,
};
use crate::provider::anthropic::{API_BASE, AnthropicProvider, MAX_TOKENS_PER_TURN, RetryPolicy};
use crate::provider::{
    Provider, ProviderError, ProviderMessage, ProviderRequest, StreamEvent, block_type, sanitize_after_fallback,
};
use crate::store::{Store, StoreError, TaskRow, ToolCallRow, TranscriptEntry, now_ms};
use crate::tools::exec::{self, Allowances, Authority, Gate, SeenTexts, ToolEnv, ToolOutput, codes};
use crate::tools::services::ToolServices;
use crate::tools::{self, ToolCall};
use crate::workspace::{LocalProjectFiles, ProjectFiles, content_hash};
use crate::{graph, prompt, review};

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
    /// The project graph (one per application, attached to the open project).
    /// Required whenever `workspace` is present: every project task then
    /// receives a scoped graph context and every tool call resolves through
    /// the graph. `None` only for embeddings without a project graph (unit
    /// tests of unrelated behavior).
    pub graph: Option<Arc<GraphService>>,
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

impl AgentError {
    /// A refusal (tool output with a stable code) as an error of a service call.
    fn from_refusal(output: &ToolOutput) -> Self {
        let code = output.content.get("code").and_then(Value::as_str).unwrap_or("agents.graphUnavailable");
        let message =
            output.content.get("error").and_then(Value::as_str).unwrap_or("The project graph cannot be used.");
        let message = message.strip_prefix("Not executed: ").unwrap_or(message);
        let mut chars = message.chars();
        let message = match chars.next() {
            Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            None => String::new(),
        };
        AgentError::new(code, message)
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
    graph: Option<Arc<GraphService>>,
    /// How long a call waits for reconciliation of its targets before pausing.
    graph_settle: Mutex<Duration>,
    /// Project permissions (Auto): `<data>/execution/project-permissions.json`
    /// beside the task database's data directory; in memory without one.
    permissions: PermissionStore,
    #[cfg(debug_assertions)]
    scripts: Mutex<ScriptState>,
}

/// The user's answer to an approval request.
#[derive(Clone, Copy, Debug)]
struct ApprovalAnswer {
    approved: bool,
    scope: ApprovalScope,
}

#[cfg(debug_assertions)]
#[derive(Default)]
struct ScriptState {
    queue: VecDeque<crate::provider::scripted::Script>,
    by_task: HashMap<TaskId, (crate::provider::scripted::Script, Arc<std::sync::atomic::AtomicUsize>)>,
}

struct Running {
    cancel: CancellationToken,
    approvals: HashMap<String, (ApprovalRequest, oneshot::Sender<ApprovalAnswer>)>,
    join: Option<JoinHandle<()>>,
    /// The task loop has ended and its terminal status is stored; the worker
    /// may still be releasing services (e.g. stopping research kernels).
    finished: bool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl AgentService {
    pub fn new(config: AgentConfig) -> Result<Self> {
        if config.workspace.is_some() && config.graph.is_none() {
            return Err(AgentError::new(
                "agents.graphRequired",
                "Project agents need the project graph: the agent service was configured without it.",
            ));
        }
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
        // Project permissions live in the application's data directory (the
        // task database's folder), under execution/ beside trust.json.
        let permissions = match config.database.as_deref().and_then(Path::parent) {
            Some(data) => PermissionStore::load(data.join("execution").join("project-permissions.json")),
            None => PermissionStore::in_memory(),
        };
        if let Some(warning) = permissions.warning() {
            tracing::warn!("{warning}");
        }
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
                graph: config.graph,
                graph_settle: Mutex::new(graph::targets::DEFAULT_SETTLE),
                permissions,
                #[cfg(debug_assertions)]
                scripts: Mutex::new(ScriptState::default()),
            }),
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.inner.events.subscribe()
    }

    /// How long a tool call waits for the graph to reconcile its targets
    /// before it is paused (tests shorten it).
    pub fn set_graph_settle(&self, settle: Duration) {
        *lock(&self.inner.graph_settle) = settle;
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
        Ok(self.provider_changed())
    }

    /// The current provider status, announced to every view.
    fn provider_changed(&self) -> ProviderStatus {
        let status = self.provider_status();
        self.inner.emit(AgentEvent::ProviderChanged { status: Box::new(status.clone()) });
        status
    }

    /// Store the API key in OS credential storage. The key is never returned.
    pub fn set_api_key(&self, raw: &str) -> Result<ProviderStatus> {
        let key = normalize_api_key(raw).map_err(|e| AgentError::new("agents.invalidKey", e.to_string()))?;
        self.inner.credentials.set(&key).map_err(|e| AgentError::new("agents.credentials", e.to_string()))?;
        Ok(self.provider_changed())
    }

    pub fn clear_api_key(&self) -> Result<ProviderStatus> {
        self.inner.credentials.delete().map_err(|e| AgentError::new("agents.credentials", e.to_string()))?;
        Ok(self.provider_changed())
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
        let graph = self.inner.graph_info(id)?;
        let allowances = self.inner.task_allowances(id)?;
        Ok(TaskDetail { summary, transcript, approvals, graph, allowances })
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
        let approval_mode = request.approval_mode.unwrap_or_default();
        // Auto acts under the project's permissions: a project without a
        // record (new, imported, copied, moved) has none until the user
        // chooses them.
        let grant = match approval_mode {
            ApprovalMode::Ask => None,
            ApprovalMode::Auto => Some(self.inner.require_permissions(files.root())?),
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
            approval_mode,
        };
        let id = TaskId::new();
        // Mandatory scoped graph context (refused when the graph cannot be used).
        let graph_context = self.inner.graph_context(files.root(), &spec, None)?;
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
        let context = prompt::context_block(
            &spec,
            &self.inner.documents,
            self.inner.services.as_ref(),
            selection.as_deref(),
            grant.as_ref(),
        );
        let mut content = vec![json!({"type": "text", "text": context})];
        if let Some(graph_context) = &graph_context {
            self.inner.store.insert_graph_context(&id, graph_context)?;
            content.push(json!({"type": "text", "text": graph::context::block(graph_context)}));
        }
        content.push(json!({"type": "text", "text": spec.prompt}));
        self.inner.store.append_message(&id, "user", &content)?;
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
        // A finished worker may still be releasing this task's services
        // (research sessions); a new run must wait until it has exited.
        if row.status.is_active() || self.is_running(id) {
            return Err(AgentError::new("agents.taskRunning", "The task is still running."));
        }
        if lock(&self.inner.running).contains_key(id) {
            return Err(AgentError::new("agents.taskFinishing", "The task is finishing up; try again in a moment."));
        }
        if let Some(budgets) = budgets {
            budgets.validate().map_err(|e| AgentError::new("agents.invalidBudgets", e))?;
            row.spec.budgets = budgets;
            self.inner.store.update_task_spec(id, &row.spec)?;
        }
        crate::workspace::check_root(self.inner.workspace.as_deref(), Path::new(&row.spec.workspace_root))
            .map_err(|e| AgentError::new("agents.workspaceNotOpen", e))?;
        // A fresh graph context for this run, from the graph the task is bound to.
        let files = LocalProjectFiles::open(Path::new(&row.spec.workspace_root))
            .map_err(|e| AgentError::new("agents.invalidWorkspace", e.to_string()))?;
        // An Auto task continues only while the project has permissions
        // (they may have been revoked since it stopped).
        if row.spec.approval_mode == ApprovalMode::Auto {
            self.inner.require_permissions(files.root())?;
        }
        let recorded = self.inner.store.graph_project(id)?;
        let graph_context = self.inner.graph_context(files.root(), &row.spec, recorded.as_ref())?;
        let provider = self.inner.make_provider(&row.spec.provider, id)?;
        let mut content = self.inner.dangling_tool_results(id)?;
        if let Some(graph_context) = &graph_context {
            self.inner.store.insert_graph_context(id, graph_context)?;
            content.push(json!({"type": "text", "text": graph::context::block(graph_context)}));
        }
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

    /// Answer an approval request for this call only.
    pub fn resolve_approval(&self, id: &TaskId, approval_id: &str, approved: bool) -> Result<()> {
        self.resolve_approval_scoped(id, approval_id, approved, ApprovalScope::Once)
    }

    /// Answer an approval request; `ApprovalScope::Task` also allows later
    /// calls of the request's repeatable kinds for the rest of the task. Only
    /// the user's approval card calls this (through the webview); a request
    /// that can only be approved once refuses `Task` and stays pending.
    pub fn resolve_approval_scoped(
        &self,
        id: &TaskId,
        approval_id: &str,
        approved: bool,
        scope: ApprovalScope,
    ) -> Result<()> {
        let mut running = lock(&self.inner.running);
        let pending = running.get_mut(id).and_then(|r| r.approvals.get(approval_id).map(|(a, _)| a.clone()));
        let Some(request) = pending else {
            return Err(AgentError::new("agents.noApproval", "That approval request is no longer pending."));
        };
        if approved && scope == ApprovalScope::Task && request.for_task.is_empty() {
            return Err(AgentError::new(
                codes::NOT_REPEATABLE,
                "This action can only be approved once: it has consequences you decide each time.",
            ));
        }
        let sender = running.get_mut(id).and_then(|r| r.approvals.remove(approval_id));
        drop(running);
        if let Some((_, tx)) = sender {
            let _ = tx.send(ApprovalAnswer { approved, scope: if approved { scope } else { ApprovalScope::Once } });
        }
        Ok(())
    }

    /// Wait for a task's current run to finish (tests and shutdown).
    pub async fn join(&self, id: &TaskId) {
        let handle = lock(&self.inner.running).get_mut(id).and_then(|r| r.join.take());
        if let Some(handle) = handle {
            let _ = handle.await;
        }
    }

    /// True while the task loop is running (not merely while its worker
    /// releases services after a terminal status was stored).
    pub fn is_running(&self, id: &TaskId) -> bool {
        lock(&self.inner.running).get(id).is_some_and(|r| !r.finished)
    }

    fn spawn(&self, id: TaskId, provider: Arc<dyn Provider>) {
        let cancel = CancellationToken::new();
        lock(&self.inner.running).insert(
            id.clone(),
            Running { cancel: cancel.clone(), approvals: HashMap::new(), join: None, finished: false },
        );
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
        let task = self.inner.store.task(id)?.ok_or_else(|| not_found(id))?;
        let files = self.inner.store.task_files(id)?;
        let project = self.inner.content_files(&task);
        Ok(TaskReview {
            task_id: id.clone(),
            checkpoint: self.inner.store.checkpoint(id)?,
            files: files
                .iter()
                .map(|row| {
                    let current = project.as_ref().map(|p| review::current(&row.path, &self.inner.documents, p));
                    review::file_review(row, &self.inner.documents, current.as_ref())
                })
                .collect(),
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
            let ops = self.inner.services.project_operations();
            results.push(review::revert_file(
                &self.inner.store,
                id,
                &self.inner.documents,
                workspace,
                &files,
                ops.as_deref(),
                file,
            )?);
        }
        if results.is_empty() && path.is_some() {
            return Err(AgentError::new("agents.notFound", "The task did not change that file."));
        }
        self.inner.review_changed(id);
        Ok(results)
    }

    pub fn accept(&self, id: &TaskId, path: Option<&WorkspacePath>) -> Result<Vec<ReviewActionResult>> {
        let task = self.inner.store.task(id)?.ok_or_else(|| not_found(id))?;
        // A running task may still change the files under review.
        if task.status.is_active() || self.is_running(id) {
            return Err(AgentError::new("agents.taskRunning", "Cancel the task before accepting its changes."));
        }
        let project = self.inner.content_files(&task);
        let mut results = Vec::new();
        for file in self.inner.store.task_files(id)? {
            if path.is_some_and(|p| p != &file.path) {
                continue;
            }
            let current = match &project {
                Some(p) => review::current(&file.path, &self.inner.documents, p),
                None => review::Current::Unknown,
            };
            if path.is_some() || review::effective_state(&file, &current).0 == ReviewState::Pending {
                results.push(review::accept_file(&self.inner.store, id, file, &current)?);
            }
        }
        self.inner.review_changed(id);
        Ok(results)
    }

    // ---- project graph

    /// Keep a stopped task as an investigation: the user's deliberate
    /// admission of an `Investigation` entity into the project graph that
    /// references the task (its id, the files it acted on) and its run as
    /// evidence. An ordinary task stays an operational record until this.
    /// Keeping the same task again returns the same entity.
    pub fn keep_as_investigation(&self, id: &TaskId) -> Result<exposition_graph::EntityId> {
        use exposition_graph::{
            ApplyOutcome, ChangeRequest, EntityKind, EntityRef, Evidence, EvidenceId, EvidenceKind, EvidenceOutcome,
            GraphOperation, Lifecycle, OperationRef, Origin, RelationKind, VersionedInput,
        };
        let row = self.inner.store.task(id)?.ok_or_else(|| not_found(id))?;
        if row.status.is_active() || self.is_running(id) {
            return Err(AgentError::new("agents.taskRunning", "Wait until the task has stopped before keeping it."));
        }
        if let Some(existing) = self.inner.store.investigation(id)? {
            return Ok(existing);
        }
        let service = self.inner.graph.as_deref().ok_or_else(|| {
            AgentError::new("agents.graphUnavailable", "The project graph is not available to agent tasks.")
        })?;
        let files = LocalProjectFiles::open(Path::new(&row.spec.workspace_root))
            .map_err(|e| AgentError::new("agents.invalidWorkspace", e.to_string()))?;
        crate::workspace::check_root(self.inner.workspace.as_deref(), files.root())
            .map_err(|e| AgentError::new("agents.workspaceNotOpen", e))?;
        let recorded = self.inner.store.graph_project(id)?;
        let env = GraphEnv { service, project: recorded.as_ref() };
        let status = graph::bound_status(env, files.root()).map_err(|r| AgentError::from_refusal(&r))?;
        let targets = self.inner.store.targets(id)?;
        let referenced: Vec<exposition_graph::EntityId> = service
            .with_store(|store| {
                let mut out = Vec::new();
                for t in &targets {
                    if let Some(e) = &t.entity
                        && let Some(entity) = store.entity(e)?
                        && entity.lifecycle != Lifecycle::Tombstoned
                        && !out.contains(e)
                    {
                        out.push(e.clone());
                    }
                }
                Ok(out)
            })
            .map_err(|e| AgentError::new("agents.graphUnavailable", e.message))?;
        let status_kind = serde_json::to_value(&row.status)
            .ok()
            .and_then(|v| v.get("kind").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_default();
        let mut attributes = std::collections::BTreeMap::new();
        attributes.insert("agentTask".to_owned(), id.0.clone());
        attributes.insert("taskStatus".to_owned(), status_kind.clone());
        let label = row.title.trim();
        let label = if label.is_empty() { "Agent investigation" } else { label };
        let mut operations = vec![GraphOperation::CreateEntity {
            temp_id: "investigation".into(),
            kind: EntityKind::Investigation,
            roles: Vec::new(),
            label: label.chars().take(200).collect(),
            key: Some(format!("agentTask:{}", id.0)),
            attributes,
            binding: None,
        }];
        for entity in referenced.iter().take(50) {
            operations.push(GraphOperation::AddRelation {
                kind: RelationKind::References,
                from: EntityRef::Temp { temp_id: "investigation".into() },
                to: EntityRef::Id { id: entity.clone() },
                evidence_kind: None,
                evidence: Vec::new(),
                note: None,
            });
        }
        let request = ChangeRequest {
            base_revision: status.revision,
            operations,
            summary: Some(format!("Keep agent task “{label}” as an investigation")),
        };
        // The person's own action (through the task's Keep command).
        let (_, outcome) =
            service.propose_and_apply(Origin::User, &request).map_err(|e| AgentError::new(&e.code, e.message))?;
        let entity = match outcome {
            ApplyOutcome::Applied { created, .. } => created
                .into_iter()
                .find(|c| c.temp_id == "investigation")
                .map(|c| c.id)
                .ok_or_else(|| AgentError::new("agents.graphInvalid", "The investigation was not created."))?,
            ApplyOutcome::Conflict { diagnostics, .. } | ApplyOutcome::Invalid { diagnostics, .. } => {
                let message = diagnostics.iter().map(|d| d.message.clone()).collect::<Vec<_>>().join("; ");
                return Err(AgentError::new(
                    "agents.graphInvalid",
                    format!("The investigation was not created: {message}"),
                ));
            }
        };
        // The task run as evidence of the investigation (a report, not a
        // verification; input versions are not recorded, so freshness is unknown).
        let evidence = Evidence {
            id: EvidenceId::mint(),
            kind: EvidenceKind::Review,
            subject: Some(entity.clone()),
            operation: OperationRef { kind: "agentTask".into(), id: id.0.clone() },
            inputs: targets
                .iter()
                .take(200)
                .map(|t| VersionedInput { path: t.path.clone(), content_hash: None, revision: None, unsaved: false })
                .collect(),
            outcome: EvidenceOutcome::Reported { summary: format!("Agent task “{label}” ({status_kind})") },
            environment: vec![("model".into(), row.spec.provider.model.clone())],
            outputs: Vec::new(),
            formal: None,
            recorded_by: Origin::Service { service: "agents".into() },
            recorded_at_ms: now_ms(),
        };
        if let Err(e) = service.register_evidence(&Origin::Service { service: "agents".into() }, evidence) {
            tracing::error!("could not record the investigation's task evidence: {e}");
        }
        self.inner.store.set_investigation(id, &entity)?;
        self.inner.store.append_notice(
            id,
            NoticeLevel::Info,
            "Kept as an investigation in the project graph, referencing this task and the files it worked on.",
        )?;
        self.inner.emit(AgentEvent::TranscriptChanged { task_id: id.clone() });
        if let Ok(summary) = self.inner.summary(id) {
            self.inner.emit(AgentEvent::TaskChanged { task: Box::new(summary) });
        }
        Ok(entity)
    }

    /// Start the project-generation task of a newly created project: an
    /// ordinary agent task (whole-project scope, execution disabled unless
    /// requested) whose request is the objective with the generation brief.
    /// It receives the (near-empty) graph context like any task and creates
    /// files, research structure, and graph associations through the same
    /// validated tools; it has no access outside the project folder.
    pub fn create_generation_task(&self, request: GenerationTaskRequest) -> Result<TaskSummary> {
        let objective = request.objective.trim();
        if objective.is_empty() {
            return Err(AgentError::new("agents.emptyPrompt", "Describe the project's objective."));
        }
        self.create_task(CreateTaskRequest {
            prompt: prompt::project_generation(objective),
            workspace_root: request.workspace_root,
            scope: TaskScope::Workspace,
            permission: request.permission,
            execution: request.execution.unwrap_or(ExecutionMode::Disabled),
            consent: request.consent,
            budgets: request.budgets,
            approval_mode: request.approval_mode,
        })
    }
}

// ---- A1: project permissions (Auto) and task allowances

fn permission_error(e: PermissionError) -> AgentError {
    match e {
        PermissionError::Invalid(message) => AgentError::new(codes::INVALID_PERMISSIONS, message),
        PermissionError::Io(message) => AgentError::new("agents.storage", message),
    }
}

/// How long changing permissions waits for an affected task to stop.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

impl AgentService {
    /// The open project's permissions, with the facts the composer states
    /// about the execution environment. `root` must be the open project.
    pub fn project_permissions(&self, root: &str) -> Result<ProjectPermissionsState> {
        let files = self.inner.open_project(root)?;
        let project = self.inner.graph_project_of(files.root());
        let restricted_available = self.inner.services.restricted_backend().is_some();
        Ok(ProjectPermissionsState {
            root: files.display_root(),
            permissions: self.inner.permissions.get(files.root(), project.as_deref()),
            project,
            defaults: PermissionChoice::defaults(restricted_available),
            restricted_available,
            trusted: self.inner.services.native_refusal(files.root()).is_none(),
        })
    }

    /// Every project-permission record (Settings › Agents).
    pub fn list_project_permissions(&self) -> Vec<ProjectPermissionsEntry> {
        let open = self.inner.workspace.as_deref().and_then(|w| w.current()).map(|c| PathBuf::from(c.root));
        let open_project = open.as_deref().and_then(|root| self.inner.graph_project_of(root));
        self.inner
            .permissions
            .list()
            .into_iter()
            .map(|(root, project, permissions)| ProjectPermissionsEntry {
                exists: Path::new(&root).is_dir(),
                open: open.as_deref().is_some_and(|o| same_folder(o, Path::new(&root))) && open_project == project,
                root,
                project,
                permissions,
            })
            .collect()
    }

    /// Grant or change the open project's permissions (the user's choice in
    /// the composer or Settings). A narrowing clears the allowances of the
    /// project's Auto tasks and stops the active ones (it waits, bounded,
    /// until they stopped; call it off the async runtime). Ask tasks and
    /// other projects are not affected.
    pub fn set_project_permissions(&self, root: &str, choice: PermissionChoice) -> Result<PermissionsChange> {
        let files = self.inner.open_project(root)?;
        let project = self.inner.graph_project_of(files.root());
        if self.inner.graph.is_some() && project.is_none() {
            return Err(AgentError::new(
                "agents.graphUnavailable",
                "The project graph is not attached, so the project cannot be identified for its permissions.",
            ));
        }
        let (before, after) =
            self.inner.permissions.set(files.root(), project.as_deref(), choice).map_err(permission_error)?;
        Ok(self.permissions_changed(files.display_root(), project, before.as_ref(), Some(after)))
    }

    /// Revoke a project's permissions (Settings, composer). The project need
    /// not be open; its active Auto tasks stop and its Auto tasks' allowances
    /// are cleared.
    pub fn revoke_project_permissions(&self, root: &str, project: Option<&str>) -> Result<PermissionsChange> {
        if root.is_empty() || root.len() > 4096 || root.contains('\0') || !Path::new(root).is_absolute() {
            return Err(AgentError::new("agents.invalidWorkspace", "invalid project folder"));
        }
        let before = self.inner.permissions.revoke(Path::new(root), project).map_err(permission_error)?;
        let Some(before) = before else {
            return Err(AgentError::new("agents.notFound", "This project has no permissions to revoke."));
        };
        Ok(self.permissions_changed(root.to_owned(), project.map(str::to_owned), Some(&before), None))
    }

    fn permissions_changed(
        &self,
        root: String,
        project: Option<String>,
        before: Option<&ProjectPermissions>,
        after: Option<ProjectPermissions>,
    ) -> PermissionsChange {
        let narrowed = crate::permissions::narrowed(before, after.as_ref());
        let (stopped, problems) = if narrowed.is_empty() {
            (Vec::new(), Vec::new())
        } else {
            // Cleared first: a call an allowance authorized and not yet
            // started is refused at launch (`agents.permissionsChanged`).
            let mut problems = self.clear_auto_allowances(Path::new(&root), project.as_deref(), &narrowed);
            let (stopped, more) = self.stop_auto_tasks(Path::new(&root), project.as_deref(), &narrowed);
            problems.extend(more);
            (stopped, problems)
        };
        let change = PermissionsChange { root, project, permissions: after, narrowed, stopped, problems };
        self.inner.emit(AgentEvent::PermissionsChanged { change: Box::new(change.clone()) });
        change
    }

    /// Stop the project's active Auto tasks after a narrowing: each gets a
    /// notice, is canceled (its stream, approval wait, or running tool, with
    /// the tool's job or research run), and its agent research sessions are
    /// shut down by the worker. Returns the stopped tasks and what could not
    /// be confirmed.
    fn stop_auto_tasks(&self, root: &Path, project: Option<&str>, narrowed: &[String]) -> (Vec<TaskId>, Vec<String>) {
        let rows = self.inner.store.tasks().unwrap_or_default();
        let affected: Vec<TaskRow> = rows
            .into_iter()
            .filter(|row| row.status.is_active() || self.is_running(&row.id))
            .filter(|row| self.is_auto_task_of(row, root, project))
            .collect();
        let what = narrowed.join(", ");
        for row in &affected {
            let _ = self.inner.store.append_notice(
                &row.id,
                NoticeLevel::Warning,
                &format!(
                    "Project permissions changed ({what}), so this Auto task was stopped. What it did before \
                     stopping was not undone and stays reviewable."
                ),
            );
            self.inner.emit(AgentEvent::TranscriptChanged { task_id: row.id.clone() });
            let _ = self.cancel_task(&row.id);
        }
        let mut problems = Vec::new();
        let deadline = Instant::now() + STOP_TIMEOUT;
        for row in &affected {
            // Stopped, and the worker released the task's services.
            while lock(&self.inner.running).contains_key(&row.id) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            if lock(&self.inner.running).contains_key(&row.id) {
                problems.push(format!("“{}” is still stopping.", row.title));
                continue;
            }
            for leftover in self.inner.services.task_leftovers(&row.id.0) {
                problems.push(format!("“{}”: {leftover}", row.title));
            }
        }
        (affected.into_iter().map(|row| row.id).collect(), problems)
    }

    /// Whether `row` is an Auto task of the project at `root` (bound to
    /// `project` when both are known).
    fn is_auto_task_of(&self, row: &TaskRow, root: &Path, project: Option<&str>) -> bool {
        if row.spec.approval_mode != ApprovalMode::Auto || !same_folder(Path::new(&row.spec.workspace_root), root) {
            return false;
        }
        let bound = self.inner.store.graph_project(&row.id).ok().flatten();
        match (bound, project) {
            (Some(bound), Some(project)) => bound.as_str() == project,
            _ => true,
        }
    }

    /// After a narrowing or revocation, clear the allowances of every Auto
    /// task of the project, active or not (the version bump refuses calls
    /// they authorized and that have not started), so a later Continue under
    /// the narrower permissions cannot inherit what was approved under the
    /// broader grant. Ask tasks keep theirs: they do not depend on the
    /// project's permissions. Returns what could not be cleared.
    fn clear_auto_allowances(&self, root: &Path, project: Option<&str>, narrowed: &[String]) -> Vec<String> {
        let ids = match self.inner.store.tasks_with_allowances() {
            Ok(ids) => ids,
            Err(e) => {
                return vec![format!("What you approved for this project's Auto tasks could not be cleared ({e}).")];
            }
        };
        let what = narrowed.join(", ");
        let mut problems = Vec::new();
        for id in ids {
            let row = match self.inner.store.task_any(&id) {
                Ok(Some((row, _))) => row,
                Ok(None) => continue,
                Err(e) => {
                    problems.push(format!("What you approved for task {} could not be checked ({e}).", id.0));
                    continue;
                }
            };
            if !self.is_auto_task_of(&row, root, project) {
                continue;
            }
            match self.inner.store.revoke_allowances(&id, None) {
                Ok((_, true)) => {
                    let _ = self.inner.store.append_notice(
                        &id,
                        NoticeLevel::Info,
                        &format!(
                            "Project permissions changed ({what}), so what you approved for the rest of this Auto \
                             task was cleared."
                        ),
                    );
                    self.inner.emit(AgentEvent::TranscriptChanged { task_id: id.clone() });
                }
                Ok((_, false)) => {}
                Err(e) => problems.push(format!(
                    "“{}”: what you approved for the rest of the task could not be cleared ({e}).",
                    row.title
                )),
            }
        }
        problems
    }

    /// Revoke one of a task's allowances (`Some(kind)`) or all of them (the
    /// user, from the task view). Calls already authorized by them and not yet
    /// started are refused (`agents.permissionsChanged`).
    pub fn revoke_allowance(&self, id: &TaskId, kind: Option<AllowanceKind>) -> Result<()> {
        self.inner.store.task(id)?.ok_or_else(|| not_found(id))?;
        self.inner.store.revoke_allowances(id, kind)?;
        self.inner.emit(AgentEvent::TranscriptChanged { task_id: id.clone() });
        Ok(())
    }

    /// Folder trust was revoked for `root`: the allowances of tasks in it are
    /// cleared (a re-grant of trust does not bring them back).
    pub fn trust_revoked(&self, root: &str) {
        let Ok(ids) = self.inner.store.tasks_with_allowances() else { return };
        for id in ids {
            let Ok(Some((row, _))) = self.inner.store.task_any(&id) else { continue };
            if !within_folder(Path::new(&row.spec.workspace_root), Path::new(root)) {
                continue;
            }
            if let Ok((_, true)) = self.inner.store.revoke_allowances(&id, None) {
                let _ = self.inner.store.append_notice(
                    &id,
                    NoticeLevel::Info,
                    "Folder trust was revoked, so what you approved for the rest of this task was cleared.",
                );
                self.inner.emit(AgentEvent::TranscriptChanged { task_id: id.clone() });
            }
        }
    }
}

// ---- P1 project operations: task records

/// A task record as Storage & History lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskRecord {
    pub id: TaskId,
    pub title: String,
    pub created_at_ms: u64,
    /// The project folder the task worked in.
    pub workspace_root: String,
    /// Stored size of everything recorded for the task.
    pub bytes: u64,
    /// The task loop runs now (it is stopped before anything is removed).
    pub active: bool,
    /// Files whose changes still await accept/revert.
    pub pending_review: u32,
    /// When it was recoverably deleted.
    pub deleted_at_ms: Option<u64>,
}

impl AgentService {
    fn record_of(&self, row: &TaskRow, deleted_at_ms: Option<u64>) -> Result<TaskRecord> {
        let summary = self.inner.summary_of(row)?;
        Ok(TaskRecord {
            id: row.id.clone(),
            title: row.title.clone(),
            created_at_ms: row.created_at_ms,
            workspace_root: row.spec.workspace_root.clone(),
            bytes: self.inner.store.task_bytes(&row.id)?,
            active: self.is_running(&row.id) || row.status.is_active(),
            pending_review: summary.pending_review,
            deleted_at_ms,
        })
    }

    /// Task records (not deleted), newest first.
    pub fn task_records(&self) -> Result<Vec<TaskRecord>> {
        let rows = self.inner.store.tasks()?;
        rows.iter().map(|row| self.record_of(row, None)).collect()
    }

    /// Recoverably deleted tasks, newest deletion first.
    pub fn deleted_task_records(&self) -> Result<Vec<TaskRecord>> {
        let rows = self.inner.store.deleted_tasks()?;
        rows.iter().map(|(row, at)| self.record_of(row, Some(*at))).collect()
    }

    /// A task record, deleted or not.
    pub fn task_record(&self, id: &TaskId) -> Result<TaskRecord> {
        let (row, deleted) = self.inner.store.task_any(id)?.ok_or_else(|| not_found(id))?;
        self.record_of(&row, deleted)
    }

    /// The project files a task may write or has touched: its explicit scope
    /// paths, the files it changed, and the targets it resolved. A
    /// whole-project scope is not listed (only what it actually touched).
    pub fn task_paths(&self, id: &TaskId) -> Result<Vec<WorkspacePath>> {
        let (row, _) = self.inner.store.task_any(id)?.ok_or_else(|| not_found(id))?;
        let mut out = match &row.spec.scope {
            TaskScope::Workspace => Vec::new(),
            TaskScope::Files { paths } => paths.clone(),
            TaskScope::Document { path } | TaskScope::Selection { path, .. } => vec![path.clone()],
        };
        for p in self.inner.store.task_paths(id)? {
            if !out.contains(&p) {
                out.push(p);
            }
        }
        Ok(out)
    }

    /// Delete a task's record: recoverably (hidden from the task list,
    /// restorable) or `permanent`ly (with its transcript, tool calls and
    /// change records — its changes can then no longer be reverted from the
    /// task). An active task is refused: stop it first.
    pub fn delete_task(&self, id: &TaskId, permanent: bool) -> Result<()> {
        let (row, deleted) = self.inner.store.task_any(id)?.ok_or_else(|| not_found(id))?;
        if self.is_running(id) || row.status.is_active() {
            return Err(AgentError::new("agents.taskActive", "The task is still running; stop it first."));
        }
        if permanent {
            self.inner.store.purge_task(id)?;
        } else if deleted.is_some() {
            return Err(AgentError::new("agents.alreadyDeleted", "The task is already deleted."));
        } else {
            self.inner.store.set_task_deleted(id, Some(now_ms()))?;
        }
        Ok(())
    }

    /// Bring a recoverably deleted task back. Nothing is re-run.
    pub fn restore_task(&self, id: &TaskId) -> Result<TaskSummary> {
        let (_, deleted) = self.inner.store.task_any(id)?.ok_or_else(|| not_found(id))?;
        if deleted.is_none() {
            return Err(AgentError::new("agents.notDeleted", "The task is not deleted."));
        }
        self.inner.store.set_task_deleted(id, None)?;
        self.inner.summary(id)
    }

    /// Cancel a running task and wait (bounded) until its loop has ended.
    pub fn stop_and_wait(&self, id: &TaskId, timeout: Duration) -> bool {
        if !self.is_running(id) {
            return true;
        }
        let _ = self.cancel_task(id);
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if !self.is_running(id) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        !self.is_running(id)
    }

    /// Bytes the task database occupies on disk (with its log), if on disk.
    pub fn task_history_bytes(&self) -> Option<u64> {
        self.inner.store.file_bytes()
    }

    /// Compact the task database after removals; (before, after) sizes.
    pub fn compact_tasks(&self) -> Result<(Option<u64>, Option<u64>)> {
        Ok(self.inner.store.compact()?)
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
    Approved(ApprovalScope),
    Declined,
    Canceled,
}

impl Inner {
    fn emit(&self, event: AgentEvent) {
        let _ = self.events.send(event);
    }

    /// `root` as the open project (absolute, existing, the open folder).
    fn open_project(&self, root: &str) -> Result<LocalProjectFiles> {
        if root.len() > 4096 || root.contains('\0') {
            return Err(AgentError::new("agents.invalidWorkspace", "invalid project folder"));
        }
        let files = LocalProjectFiles::open(Path::new(root.trim()))
            .map_err(|e| AgentError::new("agents.invalidWorkspace", e.to_string()))?;
        crate::workspace::check_root(self.workspace.as_deref(), files.root())
            .map_err(|e| AgentError::new("agents.workspaceNotOpen", e))?;
        Ok(files)
    }

    /// The identity of the project graph attached to `root` (`None` without
    /// a graph service, or when the attached graph is another folder's).
    fn graph_project_of(&self, root: &Path) -> Option<String> {
        let graph = self.graph.as_deref()?;
        let graph_root = graph.root()?;
        if !same_folder(&graph_root, root) {
            return None;
        }
        graph.status().project.map(|p| p.as_str().to_owned())
    }

    /// The project's permissions, required for Auto.
    fn require_permissions(&self, root: &Path) -> Result<ProjectPermissions> {
        let project = self.graph_project_of(root);
        self.permissions.get(root, project.as_deref()).ok_or_else(|| {
            AgentError::new(
                codes::PERMISSIONS_REQUIRED,
                "This project has no Auto permissions yet. Choose them (Use these permissions) before starting an \
                 Auto task, or start it in Ask mode.",
            )
        })
    }

    /// The live authority of a task's calls: the project's permissions (Auto
    /// tasks only) and the task's allowances that are bound to this project.
    fn authority(&self, id: &TaskId, spec: &TaskSpec, root: &Path, project: Option<&ProjectId>) -> Authority {
        let project = project.map(|p| p.as_str().to_owned());
        let grant = match spec.approval_mode {
            ApprovalMode::Ask => None,
            ApprovalMode::Auto => self.permissions.get(root, project.as_deref()),
        };
        let allowances = match self.store.allowances(id) {
            Ok((version, rows)) => Allowances {
                version,
                kinds: rows
                    .into_iter()
                    .filter(|row| same_folder(Path::new(&row.root), root) && row.project == project)
                    .map(|row| row.kind)
                    .collect(),
            },
            // Unreadable allowances allow nothing (and change the basis).
            Err(_) => Allowances { version: u64::MAX, kinds: Vec::new() },
        };
        Authority { project: grant, allowances }
    }

    fn task_allowances(&self, id: &TaskId) -> Result<Vec<TaskAllowance>> {
        let (_, rows) = self.store.allowances(id)?;
        Ok(rows
            .into_iter()
            .map(|row| TaskAllowance { kind: row.kind, granted_at_ms: row.granted_at_ms, approved: row.approved })
            .collect())
    }

    /// The scoped graph context for a run of a task with `spec` in `root`
    /// (`None` only without a project graph). Refused with the graph codes
    /// when the graph cannot be used, or is not the task's own.
    fn graph_context(
        &self,
        root: &Path,
        spec: &TaskSpec,
        project: Option<&ProjectId>,
    ) -> Result<Option<graph::GraphContext>> {
        let Some(service) = self.graph.as_deref() else { return Ok(None) };
        let env = GraphEnv { service, project };
        graph::context::build(env, root, spec, self.services.as_ref())
            .map(Some)
            .map_err(|refusal| AgentError::from_refusal(&refusal))
    }

    fn graph_info(&self, id: &TaskId) -> Result<TaskGraphInfo> {
        let (context, contexts) = self.store.graph_context(id)?;
        Ok(TaskGraphInfo {
            project: self.store.graph_project(id)?,
            context,
            contexts,
            targets: self.store.targets(id)?,
            proposals: self.store.proposals(id)?,
            investigation: self.store.investigation(id)?,
        })
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

    /// The task's project files, for judging review state from content; `None`
    /// when the task's folder is not the open one (a same-named file there is
    /// not this task's file) or is unavailable.
    fn content_files(&self, task: &TaskRow) -> Option<LocalProjectFiles> {
        let files = LocalProjectFiles::open(Path::new(&task.spec.workspace_root)).ok()?;
        crate::workspace::check_root(self.workspace.as_deref(), files.root()).ok()?;
        Some(files)
    }

    /// Files whose changes still await review, judged from content once the
    /// task has stopped (a change the user undid is not pending).
    fn pending_review(&self, row: &TaskRow, files: &[crate::store::TaskFileRow]) -> u32 {
        let stored = |f: &crate::store::TaskFileRow| f.state == ReviewState::Pending;
        let project = if row.status.is_active() { None } else { self.content_files(row) };
        let Some(project) = project else { return files.iter().filter(|f| stored(f)).count() as u32 };
        files
            .iter()
            .filter(|f| matches!(f.state, ReviewState::Pending | ReviewState::Reverted))
            .filter(|f| {
                let current = review::current(&f.path, &self.documents, &project);
                review::effective_state(f, &current).0 == ReviewState::Pending
            })
            .count() as u32
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
            pending_review: self.pending_review(row, &files),
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
                            ("user", "text")
                                if !text.starts_with("<task_context>") && !text.starts_with("<graph_context>") =>
                            {
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
                                    authorized_by: call.and_then(|c| c.authorized_by.clone()),
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
                Some(call)
                    if matches!(
                        call.status,
                        ToolCallStatus::Succeeded
                            | ToolCallStatus::Failed
                            | ToolCallStatus::Declined
                            | ToolCallStatus::Canceled
                    ) =>
                {
                    (call.result.clone().unwrap_or_default(), call.is_error)
                }
                // Skipped with a recorded reason (e.g. the response was cut off).
                Some(ToolCallRow { status: ToolCallStatus::NotRun, result: Some(result), .. }) => {
                    (result.clone(), true)
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
                    let graph_project = inner.store.graph_project(&id).ok().flatten();
                    let mut run = Run {
                        inner: &inner,
                        graph_project,
                        base_elapsed: row.usage.elapsed_ms,
                        id: id.clone(),
                        spec: row.spec,
                        usage: row.usage,
                        cancel: cancel.clone(),
                        started: Instant::now(),
                        paused: Duration::ZERO,
                        seen: SeenTexts::default(),
                        authorized: HashMap::new(),
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
        // The terminal status is stored: review actions may proceed while the
        // worker releases services below.
        if let Some(running) = lock(&inner.running).get_mut(&id) {
            running.finished = true;
        }
        // Release what the services hold for this run (agent research sessions).
        inner.services.task_finished(&id.0).await;
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
            TaskStatus::Stopped { message, .. } => Some((NoticeLevel::Warning, format!("Stopped: {message}"))),
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

/// Result text of a call the user declined in its approval request.
const DECLINED: &str = "The user declined this action. Do not retry it unchanged.";
/// Prefix of every result for a call that was never executed.
const NOT_EXECUTED: &str = "Not executed:";

/// The status a call that returned `output` is recorded with: what happened
/// to it, not merely whether its result is an error.
fn finished_status(output: &ToolOutput) -> ToolCallStatus {
    if !output.is_error {
        return ToolCallStatus::Succeeded;
    }
    match output.content.get("error").and_then(Value::as_str).unwrap_or_default() {
        e if e.starts_with(NOT_EXECUTED) => ToolCallStatus::NotRun,
        e if e == DECLINED => ToolCallStatus::Declined,
        _ => ToolCallStatus::Failed,
    }
}

/// The provider's stop reason, reduced to a short identifier safe to show.
fn sanitize_stop_reason(reason: &str) -> String {
    let clean: String = reason.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').take(64).collect();
    if clean.is_empty() { "unknown".to_owned() } else { clean }
}

/// Why a tool call of a stopped response did not run (sent to the model on continuation).
fn skip_reason(stop_reason: &str) -> String {
    match stop_reason {
        "max_tokens" => "your response was cut off at the per-turn output limit before this call was complete. Send \
                         it again; split very large content across several calls."
            .to_owned(),
        "model_context_window_exceeded" => {
            "your response was cut off because the conversation filled the model's context window.".to_owned()
        }
        "pause_turn" => "the response was paused before this call ran.".to_owned(),
        other => format!("the response ended (stop reason \"{other}\") without asking to run tools."),
    }
}

/// The user-facing explanation of a response that stopped without finishing its turn.
fn stop_message(stop_reason: &str, unrun_calls: usize) -> String {
    let calls = match unrun_calls {
        0 => String::new(),
        1 => " Its tool call was not run.".to_owned(),
        n => format!(" Its {n} tool calls were not run."),
    };
    match stop_reason {
        "max_tokens" => format!(
            "The response reached the per-turn output limit ({MAX_TOKENS_PER_TURN} tokens) and was cut off.{calls} \
             Continue the task to resume."
        ),
        "model_context_window_exceeded" => format!(
            "The conversation filled the model's context window, so the response was cut off.{calls} Start a new \
             task with a narrower scope, or continue with a short follow-up."
        ),
        "pause_turn" => {
            format!("The model provider paused the response before it finished.{calls} Continue the task to resume.")
        }
        other => format!(
            "The model provider ended the response in a way Exposition does not handle (stop reason \"{other}\").\
             {calls} Continue the task to retry."
        ),
    }
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
    /// How each call of this run that is about to run, or ran, was authorized.
    authorized: HashMap<String, AuthorizedBy>,
    files: LocalProjectFiles,
    /// The project graph the task is bound to.
    graph_project: Option<ProjectId>,
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
            // Only a response that ended normally completes the task, and only
            // a `tool_use` stop runs tools. Every other stop reason ends the run
            // with its own status; the response's tool calls (possibly cut off
            // mid-input) are recorded as not run, never executed.
            let calls = content.iter().filter(|b| block_type(b) == "tool_use").count();
            match response.stop_reason.as_str() {
                "tool_use" if calls > 0 => {
                    if let Some(status) = self.execute_tools(&content, &response.invalid_tool_inputs, turn).await {
                        return status;
                    }
                }
                "end_turn" | "stop_sequence" if calls == 0 => return TaskStatus::Completed,
                "max_tokens" if limited_by_budget => {
                    self.skip_calls(&content, turn, "the task's output-token budget ran out mid-response.");
                    return budget_exceeded(
                        BudgetKind::OutputTokens,
                        format!(
                            "the output-token budget ({}) ran out mid-response.",
                            self.spec.budgets.max_output_tokens
                        ),
                    );
                }
                reason => {
                    let stop_reason = sanitize_stop_reason(reason);
                    self.skip_calls(&content, turn, &skip_reason(&stop_reason));
                    return TaskStatus::Stopped { message: stop_message(&stop_reason, calls), stop_reason };
                }
            }
        }
    }

    /// Record the tool calls of a response that ended without running them,
    /// so the transcript (and a later continuation) says why.
    fn skip_calls(&self, content: &[Value], turn: u32, reason: &str) {
        let output = ToolOutput::error(format!("{NOT_EXECUTED} {reason}"));
        let mut skipped = false;
        for block in content.iter().filter(|b| block_type(b) == "tool_use") {
            let id = block.get("id").and_then(Value::as_str).unwrap_or_default();
            let name = block.get("name").and_then(Value::as_str).unwrap_or_default();
            let input = block.get("input").cloned().unwrap_or(Value::Null);
            self.record_call(id, turn, name, &input, ToolCallStatus::NotRun, Some(&output));
            skipped = true;
        }
        if skipped {
            self.inner.emit(AgentEvent::TranscriptChanged { task_id: self.id.clone() });
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
        // Recorded once the call was authorized and started (never for a call
        // that was refused, declined, or not run).
        let authorized_by = match status {
            ToolCallStatus::Running | ToolCallStatus::Succeeded | ToolCallStatus::Failed | ToolCallStatus::Canceled => {
                self.authorized.get(tool_use_id).cloned()
            }
            _ => None,
        };
        let row = ToolCallRow {
            tool_use_id: tool_use_id.to_owned(),
            turn,
            name: name.to_owned(),
            input: input.clone(),
            status,
            result: result.map(ToolOutput::text),
            is_error: result.is_some_and(|r| r.is_error),
            authorized_by,
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
        // Calls run one at a time; the ones behind the running call are queued.
        for (tool_use_id, name, input) in &calls {
            self.record_call(tool_use_id, turn, name, input, ToolCallStatus::Pending, None);
        }
        self.inner.emit(AgentEvent::TranscriptChanged { task_id: self.id.clone() });
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
                    let stopped = ToolOutput::error(format!("{NOT_EXECUTED} the task stopped."));
                    let mut rest = &calls[index..];
                    if let Some(output) = partial {
                        // The tool had started and was stopped: record what it
                        // reported (a tool that still finished cleanly succeeded).
                        let status = if output.is_error { ToolCallStatus::Canceled } else { ToolCallStatus::Succeeded };
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
            // A tool that saw the task's cancellation can return before the
            // loop notices it; its error then means "stopped", not "failed".
            let status = match finished_status(&output) {
                ToolCallStatus::Failed if self.cancel.is_cancelled() => ToolCallStatus::Canceled,
                status => status,
            };
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
        let mut call = match ToolCall::parse(name, input) {
            Ok(call) => call,
            Err(message) => return Ok(ToolOutput::error(message)),
        };
        // A package setup is planned by the application (it runs nothing)
        // before the gate decides, so the approval shows the exact plan; a
        // call the task's contract refuses is not planned at all.
        if let ToolCall::InstallPackages(setup) = &call {
            let packages = setup.packages.clone();
            if let Err(refused) = exec::precheck(&call, &self.env()) {
                return Ok(refused);
            }
            let planned = self.inner.services.setup_plan(self.files.root(), &packages).await;
            match (planned, &mut call) {
                (Ok(plan), ToolCall::InstallPackages(setup)) => setup.plan = Some(Box::new(plan)),
                (Err(tools::services::ServiceError::Outcome(value)), _) => {
                    return Ok(ToolOutput { content: value, is_error: true });
                }
                (Err(e), _) => return Ok(ToolOutput::error(e.to_string())),
                (Ok(_), _) => {}
            }
        }
        // Targets pending reconciliation get a short, bounded wait before the
        // gate pauses the call.
        self.settle_graph(&call).await;
        let gate = {
            let env = self.env();
            exec::gate(&call, &env)
        };
        // A call the gate refuses (e.g. a process in a task with execution
        // disabled) gets that refusal and the task continues; only a call
        // that could launch a process counts against the command budget.
        let gate = match gate {
            Gate::Refused(output) => return Ok(output),
            other => other,
        };
        if call.is_process() && self.usage.commands >= budgets.max_commands {
            *budget_hit = Some(budget_exceeded(
                BudgetKind::Commands,
                format!("the limit of {} process launches was reached.", budgets.max_commands),
            ));
            return Ok(ToolOutput::error("Not executed: the task's command budget is used up."));
        }
        let authorization = match gate {
            Gate::Refused(output) => return Ok(output),
            Gate::Allowed(authorization) => authorization,
            Gate::NeedsApproval { title, detail, reason, for_task, authorization } => {
                if let Some(prepared) = authorization.proposal() {
                    graph::propose::record(
                        &self.inner.store,
                        &self.id,
                        tool_use_id,
                        prepared,
                        graph::ProposalState::AwaitingApproval,
                        None,
                        None,
                    );
                }
                let shown = authorization.proposal().cloned();
                let approved_title = title.clone();
                match self.request_approval(tool_use_id, name, input, title, detail, reason, for_task.clone()).await {
                    // The approval binds the call to exactly what was shown, in
                    // the task's own project, under the same authority: checked
                    // again now. "Approve for this task" grants its kinds only
                    // once the call is confirmed.
                    ApprovalOutcome::Approved(scope) => {
                        self.settle_graph(&call).await;
                        match exec::confirm(&call, &self.env(), authorization) {
                            Ok(mut authorization) => {
                                let granted = match scope {
                                    ApprovalScope::Task if !for_task.is_empty() => {
                                        match self.grant_allowances(&for_task, &approved_title) {
                                            Ok(version) => Some(version),
                                            Err(e) => {
                                                // Approved once; the allowance could not be stored.
                                                self.notice(
                                                    NoticeLevel::Warning,
                                                    &format!(
                                                        "Approved once: the approval for the rest of the task could \
                                                         not be saved ({e})."
                                                    ),
                                                );
                                                None
                                            }
                                        }
                                    }
                                    _ => None,
                                };
                                authorization.approve(granted);
                                authorization
                            }
                            Err(refusal) => {
                                if let Some(prepared) = &shown {
                                    let message =
                                        refusal.content.get("error").and_then(Value::as_str).map(str::to_owned);
                                    graph::propose::record(
                                        &self.inner.store,
                                        &self.id,
                                        tool_use_id,
                                        prepared,
                                        graph::ProposalState::Failed,
                                        None,
                                        message,
                                    );
                                }
                                return Ok(refusal);
                            }
                        }
                    }
                    ApprovalOutcome::Declined => {
                        if let Some(prepared) = &shown {
                            graph::propose::record(
                                &self.inner.store,
                                &self.id,
                                tool_use_id,
                                prepared,
                                graph::ProposalState::Declined,
                                None,
                                None,
                            );
                        }
                        return Ok(ToolOutput::error(DECLINED));
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
        self.authorized.insert(tool_use_id.to_owned(), authorization.authorized_by().clone());
        let cancel = self.cancel.child_token();
        let wall = self.remaining_wall();
        let inner = Arc::clone(self.inner);
        // The authority read again now: `run` refuses the call when the
        // project's permissions or the task's allowances changed since it was
        // authorized (the check immediately before it runs).
        let authority = inner.authority(&self.id, &self.spec, self.files.root(), self.graph_project.as_ref());
        let env = ToolEnv {
            task_id: &self.id,
            spec: &self.spec,
            files: &self.files,
            documents: &inner.documents,
            workspace: inner.workspace.as_deref(),
            services: inner.services.as_ref(),
            store: &inner.store,
            graph: inner.graph.as_deref().map(|service| GraphEnv { service, project: self.graph_project.as_ref() }),
            authority,
        };
        // A tool that is still running when the task is canceled (or the time
        // budget ends) has its own token canceled and gets a short grace period
        // to stop its process and report; its outcome is recorded as it is.
        let tool = exec::run(call, &env, &mut self.seen, tool_use_id, authorization, cancel.clone());
        tokio::pin!(tool);
        let interrupt = tokio::select! {
            biased;
            output = &mut tool => {
                // Refused immediately before running: it did not run.
                let code = output.content.get("code").and_then(Value::as_str);
                if matches!(code, Some(codes::PERMISSIONS_CHANGED | codes::PROJECT_CHANGED)) {
                    self.authorized.remove(tool_use_id);
                }
                return Ok(output);
            }
            _ = self.cancel.cancelled() => Interrupt::Canceled,
            _ = tokio::time::sleep(wall) => Interrupt::WallTime,
        };
        cancel.cancel();
        // The tool started, so it is recorded as stopped (canceled) even when
        // it does not report within the grace period.
        let output = tokio::time::timeout(Duration::from_secs(5), &mut tool)
            .await
            .unwrap_or_else(|_| ToolOutput::error("Stopped while running; the tool did not report how far it got."));
        Err((interrupt, Some(output)))
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
            graph: self
                .inner
                .graph
                .as_deref()
                .map(|service| GraphEnv { service, project: self.graph_project.as_ref() }),
            authority: self.inner.authority(&self.id, &self.spec, self.files.root(), self.graph_project.as_ref()),
        }
    }

    /// Store the kinds the user approved for the rest of this task, bound to
    /// its project folder and graph identity; the new allowance version.
    fn grant_allowances(&self, kinds: &[AllowanceKind], approved: &str) -> Result<u64, StoreError> {
        let root = crate::workspace::display_path(self.files.root());
        let project = self.graph_project.as_ref().map(|p| p.as_str().to_owned());
        let version = self.inner.store.grant_allowances(&self.id, kinds, &root, project.as_deref(), approved)?;
        self.inner.emit(AgentEvent::TranscriptChanged { task_id: self.id.clone() });
        Ok(version)
    }

    /// Wait (bounded) while the call's targets are pending reconciliation.
    async fn settle_graph(&self, call: &ToolCall) {
        let Some(service) = self.inner.graph.as_deref() else { return };
        let max = *lock(&self.inner.graph_settle);
        let env = GraphEnv { service, project: self.graph_project.as_ref() };
        graph::targets::settle(env, &call.effects(), max, &self.cancel).await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn request_approval(
        &mut self,
        tool_use_id: &str,
        tool: &str,
        input: &Value,
        title: String,
        detail: String,
        reason: Option<String>,
        for_task: Vec<AllowanceKind>,
    ) -> ApprovalOutcome {
        let approval = ApprovalRequest {
            id: format!("approval-{}", uuid::Uuid::new_v4()),
            task_id: self.id.clone(),
            tool_use_id: tool_use_id.to_owned(),
            tool: tool.to_owned(),
            title,
            detail,
            reason,
            for_task,
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
            decision = rx => match decision {
                Ok(ApprovalAnswer { approved: true, scope }) => ApprovalOutcome::Approved(scope),
                _ => ApprovalOutcome::Declined,
            },
            _ = self.cancel.cancelled() => ApprovalOutcome::Canceled,
        };
        self.paused += waiting.elapsed();
        if let Some(running) = lock(&self.inner.running).get_mut(&self.id) {
            running.approvals.remove(&approval.id);
        }
        if matches!(outcome, ApprovalOutcome::Approved(_)) {
            // Approved: it runs now (shown before the resolution is announced).
            self.record_call(tool_use_id, turn, tool, input, ToolCallStatus::Running, None);
        }
        self.inner.emit(AgentEvent::ApprovalResolved {
            task_id: self.id.clone(),
            approval_id: approval.id,
            approved: matches!(outcome, ApprovalOutcome::Approved(_)),
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
            graph: None,
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
