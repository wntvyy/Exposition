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
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::attachments::{self as att, AttachmentId, AttachmentInfo, AttachmentKind, ContentStore, IngestOutcome};
use crate::catalog::{self, AgentSupport, CatalogState, ModelCatalog, Support};
use crate::credentials::{SecretStore, normalize_api_key};
use crate::graph::{GraphEnv, TaskGraphInfo};
use crate::model::*;
use crate::permissions::{
    PermissionChoice, PermissionError, PermissionStore, PermissionsChange, ProjectPermissions, ProjectPermissionsEntry,
    ProjectPermissionsState, same_folder, within_folder,
};
use crate::provider::anthropic::{API_BASE, AnthropicProvider, MAX_TOKENS_PER_TURN, RetryPolicy};
use crate::provider::openai::{self as openai_provider, OpenAiProvider, REASONING_BLOCK};
use crate::provider::{
    Provider, ProviderError, ProviderMessage, ProviderRequest, StreamEvent, block_type, sanitize_after_fallback,
};
use crate::questions::{self, QuestionAnswer, QuestionRequest, QuestionRequestState};
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
    /// The OpenAI connection's credential store and endpoint (configured by
    /// the application after construction; absent in most tests).
    openai: Mutex<OpenAiSetup>,
    /// Attachment snapshots (beside the database; in memory without one).
    attachments: ContentStore,
    /// Cached model catalogs by provider and connection fingerprint.
    catalogs: Mutex<HashMap<String, (u64, Vec<catalog::ModelDescriptor>)>>,
    /// Attachments being sent (validated, not yet stored in a message):
    /// a concurrent discard leaves them alone.
    reserved: Mutex<std::collections::HashSet<String>>,
    /// Per connection (Anthropic, OpenAI): bumped on every key save/removal.
    credential_versions: [std::sync::atomic::AtomicU64; 2],
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

struct OpenAiSetup {
    credentials: Option<Arc<dyn SecretStore>>,
    base_url: String,
}

/// How long a fetched model catalog counts as fresh.
const CATALOG_FRESH_MS: u64 = 24 * 60 * 60 * 1000;
/// Draft attachments never sent are forgotten after this long.
const DRAFT_ATTACHMENT_MS: u64 = 24 * 60 * 60 * 1000;

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
    /// Open question requests the worker is blocked on, by request id.
    questions: HashMap<String, QuestionSlot>,
    /// Why the run is being stopped, when not by the user's Cancel (open
    /// questions are superseded with it).
    stop_reason: Option<String>,
    join: Option<JoinHandle<()>>,
    /// The task loop has ended and its terminal status is stored; the worker
    /// may still be releasing services (e.g. stopping research kernels).
    finished: bool,
}

impl Running {
    fn new() -> Self {
        Self {
            cancel: CancellationToken::new(),
            approvals: HashMap::new(),
            questions: HashMap::new(),
            stop_reason: None,
            join: None,
            finished: false,
        }
    }
}

/// A task's worker slot reserved for a run that has not started yet; given
/// back when dropped unless the worker took it over (`keep`).
struct RunSlot<'a> {
    running: &'a Mutex<HashMap<TaskId, Running>>,
    id: TaskId,
    kept: bool,
}

impl RunSlot<'_> {
    fn keep(&mut self) {
        self.kept = true;
    }
}

impl Drop for RunSlot<'_> {
    fn drop(&mut self) {
        if !self.kept {
            lock(self.running).remove(&self.id);
        }
    }
}

/// A question request the worker waits on. Its state is changed only under
/// the `running` lock (answers, discussions, revisions), and stored at once.
struct QuestionSlot {
    request: QuestionRequest,
    /// Wakes the worker.
    tx: mpsc::UnboundedSender<QuestionInput>,
    /// Ends a running discussion turn early (an answer arrived).
    discussion: Option<CancellationToken>,
}

/// What wakes a worker blocked on a question.
enum QuestionInput {
    /// The request was answered or superseded (its state says which).
    Resolved,
    /// The user started a discussion (state `Discussing`).
    Discuss,
}

/// Model requests one discussion turn may make (each counted in the budgets).
const DISCUSSION_REQUESTS: usize = 3;
/// Why a question's task can no longer act on its answer.
const PROJECT_CLOSED: &str = "The project was closed or switched";
/// Image media types a figure reference may have.
const FIGURE_MEDIA_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/svg+xml"];

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// The request text of an attachment-only message (not shown as the user's).
const ATTACHMENTS_ONLY: &str = "Use the attached files.";
/// Start of the context block that carries a question's discussion and final
/// options into the history with its answer (not shown as the user's text).
const QUESTION_CONTEXT: &str = "<question_discussion";

/// Bytes of an attachment once base64-encoded in a request.
fn encoded_size(bytes: u64) -> u64 {
    bytes.div_ceil(3) * 4
}

fn rejected(name: &str, code: &str, message: impl Into<String>) -> IngestOutcome {
    IngestOutcome::Rejected { name: name.to_owned(), code: code.to_owned(), message: message.into() }
}

/// A model identifier as typed or chosen (exact provider ID, trimmed).
fn valid_model_id(raw: &str) -> Result<String> {
    let model = raw.trim();
    let valid = !model.is_empty()
        && model.len() <= 100
        && model.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '@' | '/'));
    if valid {
        Ok(model.to_owned())
    } else {
        Err(AgentError::new("agents.invalidModel", "The model identifier is not valid."))
    }
}

/// An optional OpenAI organization/project identifier (sent as a header).
fn context_value(raw: Option<String>, prefix: &str, what: &str) -> Result<Option<String>> {
    let Some(value) = raw.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty()) else { return Ok(None) };
    let valid = value.len() <= 64
        && value.starts_with(prefix)
        && value.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
    if valid {
        Ok(Some(value))
    } else {
        Err(AgentError::new("agents.invalidConnection", format!("The OpenAI {what} ID should look like {prefix}…")))
    }
}

fn provider_key(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Anthropic => "anthropic:",
        ProviderKind::OpenAi => "openai:",
        ProviderKind::Scripted => "scripted:",
    }
}

/// A non-secret identity of a connection (provider, a digest of the key, and
/// the request context) for caching catalogs per account.
fn connection_fingerprint(
    provider: ProviderKind,
    key: &crate::credentials::Secret,
    connection: &OpenAiConnection,
) -> String {
    let digest = att::sha256_hex(key.expose().as_bytes());
    format!(
        "{}{}:{}:{}",
        provider_key(provider),
        &digest[..16],
        connection.organization.as_deref().unwrap_or(""),
        connection.project.as_deref().unwrap_or("")
    )
}

fn catalog_client() -> std::result::Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("Exposition/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("cannot create HTTP client: {e}"))
}

fn catalog_error(status: u16, body: &str, key: &str) -> String {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.pointer("/error/message").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| format!("HTTP {status}"))
        .replace(key, "<redacted>");
    let message = crate::provider::sse::bounded(&message);
    match status {
        401 | 403 => format!("The provider rejected the API key ({message})."),
        _ => format!("The model list could not be loaded ({message})."),
    }
}

async fn fetch_openai_models(
    base: &str,
    key: &crate::credentials::Secret,
    connection: &OpenAiConnection,
) -> std::result::Result<Vec<catalog::ModelDescriptor>, String> {
    let client = catalog_client()?;
    let mut request = client.get(format!("{}/v1/models", base.trim_end_matches('/'))).bearer_auth(key.expose());
    if let Some(org) = connection.organization.as_deref() {
        request = request.header("OpenAI-Organization", org);
    }
    if let Some(project) = connection.project.as_deref() {
        request = request.header("OpenAI-Project", project);
    }
    let response = request.send().await.map_err(|_| "Could not reach OpenAI to list models.".to_owned())?;
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(catalog_error(status, &body, key.expose()));
    }
    let json: Value =
        serde_json::from_str(&body).map_err(|_| "OpenAI returned an unreadable model list.".to_owned())?;
    catalog::parse_openai(&json).map_err(|e| format!("OpenAI returned an unexpected model list: {e}"))
}

async fn fetch_anthropic_models(
    base: &str,
    key: &crate::credentials::Secret,
) -> std::result::Result<Vec<catalog::ModelDescriptor>, String> {
    let client = catalog_client()?;
    let mut models = Vec::new();
    let mut after: Option<String> = None;
    // Bounded pagination (1000 per page).
    for _ in 0..10 {
        let mut url = format!("{}/v1/models?limit=1000", base.trim_end_matches('/'));
        if let Some(cursor) = &after {
            url.push_str("&after_id=");
            url.push_str(
                &cursor
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                    .collect::<String>(),
            );
        }
        let response = client
            .get(url)
            .header("x-api-key", key.expose())
            .header("anthropic-version", crate::provider::anthropic::API_VERSION)
            .send()
            .await
            .map_err(|_| "Could not reach Anthropic to list models.".to_owned())?;
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        if !(200..300).contains(&status) {
            return Err(catalog_error(status, &body, key.expose()));
        }
        let json: Value =
            serde_json::from_str(&body).map_err(|_| "Anthropic returned an unreadable model list.".to_owned())?;
        let (page, next) = catalog::parse_anthropic_page(&json)
            .map_err(|e| format!("Anthropic returned an unexpected model list: {e}"))?;
        models.extend(page);
        match next {
            Some(cursor) if Some(&cursor) != after.as_ref() => after = Some(cursor),
            _ => break,
        }
    }
    Ok(models)
}

/// What a model choice resolves to for a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resolved {
    /// Effort to send (`None`: the model takes no effort/reasoning parameters).
    pub effort: Option<Effort>,
    /// The model's output limit when known.
    pub max_output_tokens: Option<u64>,
}

/// Whether a model choice can run the agent with the given attachments
/// (the new message's plus the history's). `known` is the connection's
/// catalog entry for the model when one was fetched: its evidence comes
/// before the maintained rules. Unknown support is not proof of support:
/// such attachments are refused with an explanation instead of being dropped.
pub fn preflight(
    choice: &ProviderChoice,
    attachments: &[AttachmentInfo],
    known: Option<&catalog::ModelDescriptor>,
) -> std::result::Result<Resolved, AgentError> {
    let kinds: Vec<AttachmentKind> = attachments.iter().map(|a| a.kind).collect();
    let (agent, caps) = match (choice.provider, known) {
        (ProviderKind::Scripted, _) => {
            return Ok(Resolved { effort: Some(choice.effort), max_output_tokens: None });
        }
        (_, Some(descriptor)) => (descriptor.agent.clone(), descriptor.capabilities.clone()),
        (ProviderKind::OpenAi, None) => catalog::openai_rules(&choice.model),
        (ProviderKind::Anthropic, None) => catalog::anthropic_rules(&choice.model),
    };
    let images: Vec<&AttachmentInfo> = attachments.iter().filter(|a| a.kind == AttachmentKind::Image).collect();
    if choice.provider == ProviderKind::Anthropic
        && images.len() > att::MANY_IMAGES
        && images.iter().any(|a| a.width.max(a.height).unwrap_or(0) > att::MANY_IMAGES_MAX_SIDE)
    {
        return Err(AgentError::new(
            "attachments.tooManyLargeImages",
            format!(
                "With more than {} images in a conversation, Anthropic accepts images of at most {} pixels on a side; \
                 start a new task or attach smaller images.",
                att::MANY_IMAGES,
                att::MANY_IMAGES_MAX_SIDE
            ),
        ));
    }
    let max_output_tokens = caps.max_output_tokens;
    let kinds = kinds.as_slice();
    if let AgentSupport::Unsupported { reason } = agent {
        return Err(AgentError::new(
            "agents.modelUnsupported",
            format!("{} cannot run Exposition's agent: {reason}", choice.model),
        ));
    }
    let label = choice.provider.label();
    for (kind, support, what) in
        [(AttachmentKind::Image, caps.images, "images"), (AttachmentKind::Pdf, caps.pdf, "PDF documents")]
    {
        if kinds.contains(&kind) && support != Support::Yes {
            let why = if support == Support::No { "does not accept" } else { "is not known to accept" };
            return Err(AgentError::new(
                "agents.attachmentUnsupported",
                format!(
                    "{label} model {} {why} {what}. Choose a model that reads them, or remove those attachments.",
                    choice.model
                ),
            ));
        }
    }
    // Effort: only where the model takes it; an unsupported level is refused.
    if caps.efforts.is_empty() {
        // OpenAI: no reasoning parameters for a model not known to take them.
        // Anthropic requests always carry the effort (unverified models are
        // the user's explicit choice).
        let effort = (choice.provider != ProviderKind::OpenAi).then_some(choice.effort);
        return Ok(Resolved { effort, max_output_tokens });
    }
    if !caps.efforts.contains(&choice.effort) {
        return Err(AgentError::new(
            "agents.effortUnsupported",
            format!("{} does not accept the {} effort level.", choice.model, choice.effort.as_str()),
        ));
    }
    Ok(Resolved { effort: Some(choice.effort), max_output_tokens })
}

/// Releases attachment reservations when a send finishes (or fails).
struct Reservation<'a> {
    set: &'a Mutex<std::collections::HashSet<String>>,
    ids: Vec<String>,
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        let mut set = lock(self.set);
        for id in &self.ids {
            set.remove(id);
        }
    }
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
        // No worker of a previous process waits for an answer any more.
        store.supersede_open_questions(None, "Exposition restarted")?;
        // Nor can anyone approve a graph proposal a previous process showed
        // (approvals live in memory): none is left "waiting for approval".
        store.close_waiting_proposals(None, &unapproved_proposal("Exposition restarted"))?;
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
        // Attachment snapshots live beside the database; drafts never sent
        // are forgotten after a day, and snapshots no message refers to go.
        let attachments = match config.database.as_deref().and_then(Path::parent) {
            Some(data) => ContentStore::on_disk(data.join("agents").join("attachments")),
            None => ContentStore::in_memory(),
        };
        match store.collect_attachments(now_ms().saturating_sub(DRAFT_ATTACHMENT_MS)) {
            Ok(keep) => attachments.retain(&keep),
            Err(e) => tracing::warn!("attachment cleanup skipped: {e}"),
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
                openai: Mutex::new(OpenAiSetup { credentials: None, base_url: openai_provider::API_BASE.to_owned() }),
                attachments,
                catalogs: Mutex::new(HashMap::new()),
                reserved: Mutex::new(std::collections::HashSet::new()),
                credential_versions: Default::default(),
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

    /// Cost counters of the task store (measurement only).
    pub fn store_stats(&self) -> crate::store::StoreStats {
        self.inner.store.stats()
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

    /// Enable the OpenAI connection with its credential store (the
    /// application's OS credential entry; tests use a memory store).
    /// `base_url` is the official endpoint unless a test overrides it.
    pub fn configure_openai(&self, credentials: Arc<dyn SecretStore>, base_url: Option<String>) {
        let mut setup = lock(&self.inner.openai);
        setup.credentials = Some(credentials);
        if let Some(url) = base_url {
            setup.base_url = url;
        }
    }

    pub fn provider_status(&self) -> ProviderStatus {
        let settings = self.inner.settings().unwrap_or_default();
        let (anthropic_configured, anthropic_error) = match self.inner.credentials.get() {
            Ok(secret) => (secret.is_some(), None),
            Err(e) => (false, Some(e.to_string())),
        };
        let mut connections = vec![ConnectionStatus {
            provider: ProviderKind::Anthropic,
            auth: AuthMode::ApiKey,
            configured: anthropic_configured,
            credential_error: anthropic_error.clone(),
            context: None,
            version: self.inner.credential_version(ProviderKind::Anthropic),
        }];
        let openai_store = lock(&self.inner.openai).credentials.clone();
        let (openai_configured, openai_error) = match &openai_store {
            Some(store) => match store.get() {
                Ok(secret) => (secret.is_some(), None),
                Err(e) => (false, Some(e.to_string())),
            },
            None => (false, Some("OpenAI connections are not available in this build.".to_owned())),
        };
        let openai_context = settings.openai.as_ref().and_then(|c| {
            let parts: Vec<String> = [("Organization", &c.organization), ("Project", &c.project)]
                .into_iter()
                .filter_map(|(label, v)| v.as_deref().filter(|s| !s.is_empty()).map(|v| format!("{label} {v}")))
                .collect();
            (!parts.is_empty()).then(|| parts.join(", "))
        });
        connections.push(ConnectionStatus {
            provider: ProviderKind::OpenAi,
            auth: AuthMode::ApiKey,
            configured: openai_configured,
            credential_error: openai_error.clone(),
            context: openai_context,
            version: self.inner.credential_version(ProviderKind::OpenAi),
        });
        if self.inner.allow_scripted {
            connections.push(ConnectionStatus {
                provider: ProviderKind::Scripted,
                auth: AuthMode::ApiKey,
                configured: true,
                credential_error: None,
                context: None,
                version: 0,
            });
        }
        let (configured, credential_error) = match settings.provider {
            ProviderKind::Anthropic => (anthropic_configured, anthropic_error),
            ProviderKind::OpenAi => (openai_configured, openai_error),
            ProviderKind::Scripted => (self.inner.allow_scripted, None),
        };
        ProviderStatus {
            settings,
            configured,
            connections,
            credential_store: self.inner.credentials.describe(),
            credential_error,
            scripted_available: self.inner.allow_scripted,
        }
    }

    pub fn update_settings(&self, settings: ProviderSettings) -> Result<ProviderStatus> {
        let model = valid_model_id(&settings.model)?;
        if settings.provider == ProviderKind::Scripted && !self.inner.allow_scripted {
            return Err(AgentError::new(
                "agents.scriptedUnavailable",
                "The scripted provider is only available in development builds.",
            ));
        }
        let openai = match settings.openai {
            Some(c) => Some(OpenAiConnection {
                organization: context_value(c.organization, "org-", "organization")?,
                project: context_value(c.project, "proj_", "project")?,
            }),
            None => None,
        };
        let settings = ProviderSettings { model, openai, ..settings };
        let raw = serde_json::to_string(&settings).map_err(|e| AgentError::new("agents.storage", e.to_string()))?;
        self.inner.store.set_setting(SETTINGS_KEY, &raw)?;
        Ok(self.provider_changed())
    }

    /// Store a provider's API key in OS credential storage (never returned).
    pub fn set_provider_key(&self, provider: ProviderKind, raw: &str) -> Result<ProviderStatus> {
        let store = self.inner.credential_store(provider)?;
        let key = normalize_api_key(raw).map_err(|e| AgentError::new("agents.invalidKey", e.to_string()))?;
        store.set(&key).map_err(|e| AgentError::new("agents.credentials", e.to_string()))?;
        lock(&self.inner.catalogs).retain(|k, _| !k.starts_with(provider_key(provider)));
        self.inner.bump_credential_version(provider);
        Ok(self.provider_changed())
    }

    pub fn clear_provider_key(&self, provider: ProviderKind) -> Result<ProviderStatus> {
        let store = self.inner.credential_store(provider)?;
        store.delete().map_err(|e| AgentError::new("agents.credentials", e.to_string()))?;
        lock(&self.inner.catalogs).retain(|k, _| !k.starts_with(provider_key(provider)));
        self.inner.bump_credential_version(provider);
        Ok(self.provider_changed())
    }

    /// The models a connection offers, with Exposition's support information.
    /// `refresh` fetches now; otherwise a catalog fetched within a day is
    /// reused. A failed refresh returns the cached list as stale (with the
    /// reason), and the saved default model is always included as itself.
    pub async fn models(&self, provider: ProviderKind, refresh: bool) -> Result<ModelCatalog> {
        let settings = self.inner.settings()?;
        let saved: Vec<String> = if settings.provider == provider { vec![settings.model.clone()] } else { Vec::new() };
        let done = |state: CatalogState,
                    fetched: Option<u64>,
                    error: Option<String>,
                    models: Vec<catalog::ModelDescriptor>| {
            let (models, suggested, suggested_reason) = catalog::finish(provider, models, &saved);
            ModelCatalog {
                provider,
                state,
                fetched_at_ms: fetched,
                error,
                models,
                suggested,
                suggested_reason,
                metadata_checked: catalog::METADATA_CHECKED.to_owned(),
            }
        };
        if provider == ProviderKind::Scripted {
            if !self.inner.allow_scripted {
                return Err(AgentError::new("agents.scriptedUnavailable", "The scripted provider is disabled."));
            }
            return Ok(done(
                CatalogState::Fresh,
                Some(now_ms()),
                None,
                vec![catalog::saved_descriptor(provider, "scripted")],
            ));
        }
        let (key, connection) = match self.inner.connection_key(provider) {
            Ok(found) => found,
            Err(e) => return Ok(done(CatalogState::Unavailable, None, Some(e.message), Vec::new())),
        };
        let fingerprint = connection_fingerprint(provider, &key, &connection);
        let cached = lock(&self.inner.catalogs).get(&fingerprint).cloned();
        if !refresh
            && let Some((at, models)) = &cached
            && now_ms().saturating_sub(*at) < CATALOG_FRESH_MS
        {
            return Ok(done(CatalogState::Fresh, Some(*at), None, models.clone()));
        }
        let fetched = match provider {
            ProviderKind::OpenAi => {
                let base = lock(&self.inner.openai).base_url.clone();
                fetch_openai_models(&base, &key, &connection).await
            }
            _ => fetch_anthropic_models(&self.inner.anthropic_base_url, &key).await,
        };
        // A connection changed while fetching: its result is not this one's.
        let current = self.inner.connection_key(provider).ok().map(|(k, c)| connection_fingerprint(provider, &k, &c));
        if current.as_deref() != Some(fingerprint.as_str()) {
            return Ok(done(
                CatalogState::Unavailable,
                None,
                Some("The connection changed while its models were loading; refresh again.".into()),
                Vec::new(),
            ));
        }
        match fetched {
            Ok(models) => {
                let at = now_ms();
                lock(&self.inner.catalogs).insert(fingerprint, (at, models.clone()));
                Ok(done(CatalogState::Fresh, Some(at), None, models))
            }
            Err(error) => match cached {
                Some((at, models)) => Ok(done(CatalogState::Stale, Some(at), Some(error), models)),
                None => Ok(done(CatalogState::Unavailable, None, Some(error), Vec::new())),
            },
        }
    }

    // ---- attachments

    /// Ingest files the user chose (paths come from the native file dialog
    /// or from a native drop the desktop shell recorded; never from the
    /// webview alone). A folder is rejected, never read recursively.
    pub fn add_attachment_files(&self, paths: &[PathBuf]) -> Vec<IngestOutcome> {
        paths
            .iter()
            .map(|path| {
                let name = att::safe_name(&path.to_string_lossy(), "file");
                let limit = att::MAX_PDF_BYTES.max(att::MAX_IMAGE_BYTES).max(att::MAX_TEXT_BYTES);
                match std::fs::metadata(path) {
                    Ok(meta) if meta.is_dir() => rejected(
                        &name,
                        "attachments.folder",
                        "Folders cannot be attached. Attach the files inside it instead.",
                    ),
                    Ok(meta) if !meta.is_file() => rejected(&name, "attachments.notAFile", "This is not a file."),
                    Ok(meta) if meta.len() > limit => rejected(
                        &name,
                        "attachments.tooLarge",
                        format!(
                            "The file is {}; attachments can be at most {}.",
                            att::human_size(meta.len()),
                            att::human_size(limit)
                        ),
                    ),
                    Ok(_) => match std::fs::read(path) {
                        Ok(bytes) => self.ingest(&name, None, &bytes),
                        Err(e) => rejected(&name, "attachments.unreadable", format!("The file could not be read: {e}")),
                    },
                    Err(e) => rejected(&name, "attachments.unreadable", format!("The file could not be read: {e}")),
                }
            })
            .collect()
    }

    /// Ingest pasted bytes (base64, as the webview read them from the clipboard).
    pub fn add_attachment_bytes(
        &self,
        name: Option<&str>,
        media_type: Option<&str>,
        data_base64: &str,
    ) -> IngestOutcome {
        let fallback = match media_type {
            Some(t) if t.starts_with("image/") => {
                format!("Pasted image.{}", t.trim_start_matches("image/").replace("jpeg", "jpg"))
            }
            _ => "Pasted file".to_owned(),
        };
        let name = att::safe_name(name.unwrap_or(""), &fallback);
        if data_base64.len() as u64 > att::MAX_PDF_BYTES / 3 * 4 + 64 {
            return rejected(&name, "attachments.tooLarge", "The pasted data is too large to attach.");
        }
        match att::base64_decode(data_base64) {
            Some(bytes) => self.ingest(&name, media_type, &bytes),
            None => rejected(&name, "attachments.invalidData", "The pasted data could not be read."),
        }
    }

    fn ingest(&self, name: &str, declared: Option<&str>, bytes: &[u8]) -> IngestOutcome {
        let checked = match att::check(bytes, name, declared) {
            Ok(c) => c,
            Err(r) => return rejected(name, r.code, r.message),
        };
        let sha256 = match self.inner.attachments.put(bytes) {
            Ok(digest) => digest,
            Err(e) => return rejected(name, "attachments.storage", format!("The attachment could not be stored: {e}")),
        };
        let info = AttachmentInfo {
            id: AttachmentId::new(),
            name: name.to_owned(),
            media_type: checked.media_type,
            kind: checked.kind,
            size_bytes: bytes.len() as u64,
            width: checked.width,
            height: checked.height,
            sha256,
            missing: false,
        };
        match self.inner.store.insert_attachment(&info) {
            Ok(()) => IngestOutcome::Added { attachment: info },
            Err(e) => rejected(name, "attachments.storage", e.to_string()),
        }
    }

    /// An image attachment as a data URL for its thumbnail (`None` otherwise).
    pub fn attachment_preview(&self, id: &AttachmentId) -> Result<Option<String>> {
        let info = self.inner.attachment(id)?;
        if info.kind != AttachmentKind::Image {
            return Ok(None);
        }
        Ok(self
            .inner
            .attachments
            .get(&info.sha256)
            .map(|bytes| format!("data:{};base64,{}", info.media_type, att::base64_encode(&bytes))))
    }

    /// Forget draft attachments the user removed before sending.
    pub fn discard_attachments(&self, ids: &[AttachmentId]) -> Result<()> {
        // Attachments of a send in progress are kept (the message is about
        // to refer to them).
        let reserved = lock(&self.inner.reserved).clone();
        let ids: Vec<AttachmentId> = ids.iter().filter(|id| !reserved.contains(&id.0)).cloned().collect();
        Ok(self.inner.store.discard_attachments(&ids)?)
    }

    /// The current provider status, announced to every view.
    fn provider_changed(&self) -> ProviderStatus {
        let status = self.provider_status();
        self.inner.emit(AgentEvent::ProviderChanged { status: Box::new(status.clone()) });
        status
    }

    /// Store the Anthropic API key in OS credential storage (never returned).
    pub fn set_api_key(&self, raw: &str) -> Result<ProviderStatus> {
        self.set_provider_key(ProviderKind::Anthropic, raw)
    }

    pub fn clear_api_key(&self) -> Result<ProviderStatus> {
        self.clear_provider_key(ProviderKind::Anthropic)
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
        self.task_detail_from(id, None)
    }

    /// The task's detail; with `from`, a partial read for a reader that
    /// holds an earlier one: only the transcript items with that sequence
    /// number or a later one, and no graph context (see
    /// [`TaskDetail::transcript_from`]). Everything else is complete and
    /// current either way. Stored rows before the latest assistant message
    /// never change, so a reader that asks from there (or from its earliest
    /// unfinished tool call) misses nothing.
    pub fn task_detail_from(&self, id: &TaskId, from: Option<u64>) -> Result<TaskDetail> {
        let summary = self.inner.summary(id)?;
        let transcript = self.inner.transcript(id, from.unwrap_or(0))?;
        let approvals = lock(&self.inner.running)
            .get(id)
            .map(|r| r.approvals.values().map(|(a, _)| a.clone()).collect())
            .unwrap_or_default();
        let graph = self.inner.graph_info(id, from.is_none())?;
        let allowances = self.inner.task_allowances(id)?;
        let questions = self.inner.store.questions(id)?;
        Ok(TaskDetail { summary, transcript, approvals, graph, allowances, questions, transcript_from: from })
    }

    pub fn create_task(&self, request: CreateTaskRequest) -> Result<TaskSummary> {
        self.create(request, false)
    }

    /// Create a task that runs without a user in the loop (a workflow's
    /// review step): like [`Self::create_task`], but it is never offered
    /// structured questions (`ask_user`), so it cannot wait for an answer.
    pub fn create_unattended_task(&self, request: CreateTaskRequest) -> Result<TaskSummary> {
        self.create(request, true)
    }

    fn create(&self, request: CreateTaskRequest, unattended: bool) -> Result<TaskSummary> {
        let attachments = self.inner.message_attachments(request.attachments.as_deref().unwrap_or_default())?;
        let prompt = request.prompt.trim();
        if prompt.is_empty() && attachments.is_empty() {
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
        // Resolved now and recorded with the task: later changes to the
        // defaults affect later tasks only.
        let choice = match request.provider {
            Some(choice) => {
                if choice.provider == ProviderKind::Scripted && !self.inner.allow_scripted {
                    return Err(AgentError::new("agents.scriptedUnavailable", "The scripted provider is disabled."));
                }
                ProviderChoice { model: valid_model_id(&choice.model)?, ..choice }
            }
            None => ProviderChoice { provider: settings.provider, model: settings.model, effort: settings.effort },
        };
        preflight(&choice, &attachments, self.inner.known_model(&choice).as_ref())?;
        let _reserved = self.inner.reserve(&attachments);
        let bytes: u64 = attachments.iter().map(|a| encoded_size(a.size_bytes)).sum();
        if bytes > att::MAX_REQUEST_BYTES {
            return Err(AgentError::new(
                "attachments.requestTooLarge",
                format!("The attachments total more than {} per request.", att::human_size(att::MAX_REQUEST_BYTES)),
            ));
        }
        let prompt = if prompt.is_empty() { ATTACHMENTS_ONLY } else { prompt };
        let spec = TaskSpec {
            prompt: prompt.to_owned(),
            workspace_root: files.display_root(),
            scope: request.scope,
            permission: request.permission,
            execution: request.execution,
            consent,
            budgets,
            provider: choice,
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
        if unattended {
            self.inner.store.set_task_unattended(&id)?;
        }
        if spec.provider.provider == ProviderKind::OpenAi {
            // The billing context this task resolved at start (kept for continuations).
            let current = self.inner.settings()?.openai.unwrap_or_default();
            self.inner.store.set_task_connection(&id, &serde_json::to_value(&current).unwrap_or(Value::Null))?;
        }
        let selection = selection_text.filter(|_| consent == ContextConsent::ScopeContent);
        // What the gate would decide now (a new task has no allowances yet).
        let authority = Authority { project: grant, ..Authority::default() };
        let outlook = self.inner.outlook(&id, &spec, &files, authority.clone());
        let context = prompt::context_block(&spec, &self.inner.documents, selection.as_deref(), &authority, &outlook);
        let mut content = vec![json!({"type": "text", "text": context})];
        if let Some(graph_context) = &graph_context {
            self.inner.store.insert_graph_context(&id, graph_context)?;
            content.push(json!({"type": "text", "text": graph::context::block(graph_context)}));
        }
        content.push(json!({"type": "text", "text": spec.prompt}));
        content.extend(attachments.iter().map(att::reference_block));
        self.inner.store.append_message(&id, "user", &content)?;
        self.spawn(id.clone(), provider);
        let summary = self.inner.summary(&id)?;
        self.inner.emit(AgentEvent::TaskChanged { task: Box::new(summary.clone()) });
        Ok(summary)
    }

    /// Continue a stopped task with a follow-up request (optionally raising budgets).
    pub fn continue_task(&self, id: &TaskId, prompt: &str, budgets: Option<Budgets>) -> Result<TaskSummary> {
        self.continue_task_with(
            id,
            ContinueTaskRequest { prompt: prompt.to_owned(), budgets, attachments: Vec::new(), provider: None },
        )
    }

    /// Continue a stopped task: a follow-up with optional attachments, raised
    /// budgets, or an explicit switch of provider/model. A switch keeps the
    /// task (identity, instructions, permissions, graph record, changes) and
    /// its canonical history; it is refused with an explanation when the new
    /// model cannot read what the history contains, and recorded as a notice.
    pub fn continue_task_with(&self, id: &TaskId, request: ContinueTaskRequest) -> Result<TaskSummary> {
        let attachments = self.inner.message_attachments(&request.attachments)?;
        let prompt = request.prompt.trim();
        if prompt.is_empty() && attachments.is_empty() {
            return Err(AgentError::new("agents.emptyPrompt", "Describe what the agent should do next."));
        }
        let budgets = request.budgets;
        if let Some(budgets) = &budgets {
            budgets.validate().map_err(|e| AgentError::new("agents.invalidBudgets", e))?;
        }
        // One run per task: the task's worker slot is taken here, before
        // anything is read or decided, and given back if the request is
        // refused. A second Continue arriving meanwhile finds it taken.
        let mut slot = self.reserve_run(id)?;
        let mut row = self.inner.store.task(id)?.ok_or_else(|| not_found(id))?;
        if row.status.is_active() {
            return Err(AgentError::new("agents.taskRunning", "The task is still running."));
        }
        let switched = match request.provider {
            Some(choice) if choice != row.spec.provider => {
                if choice.provider == ProviderKind::Scripted && !self.inner.allow_scripted {
                    return Err(AgentError::new("agents.scriptedUnavailable", "The scripted provider is disabled."));
                }
                Some(ProviderChoice { model: valid_model_id(&choice.model)?, ..choice })
            }
            _ => None,
        };
        let target = switched.clone().unwrap_or_else(|| row.spec.provider.clone());
        let _reserved = self.inner.reserve(&attachments);
        let (all, bytes) = self.inner.history_attachments(id, &attachments)?;
        preflight(&target, &all, self.inner.known_model(&target).as_ref()).map_err(|e| match &switched {
            Some(_) => AgentError::new(
                &e.code,
                format!("This conversation cannot continue on {}: {}", target.model, e.message),
            ),
            None => e,
        })?;
        if bytes > att::MAX_REQUEST_BYTES {
            return Err(AgentError::new(
                "attachments.requestTooLarge",
                format!(
                    "With these attachments the conversation would exceed {} per request; start a new task instead.",
                    att::human_size(att::MAX_REQUEST_BYTES)
                ),
            ));
        }
        // Nothing is stored until the request can no longer be refused: a
        // refused Continue leaves the task exactly as it was.
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
        // A switch resolves the connection context again (the current
        // settings); it is recorded only once the provider could be made.
        let connection = match &switched {
            Some(_) if target.provider == ProviderKind::OpenAi => {
                Some(self.inner.settings()?.openai.unwrap_or_default())
            }
            _ => None,
        };
        let provider = self.inner.make_provider_with(&target, id, connection.clone())?;
        if let Some(connection) = &connection {
            self.inner.store.set_task_connection(id, &serde_json::to_value(connection).unwrap_or(Value::Null))?;
        }
        if let Some(budgets) = budgets {
            row.spec.budgets = budgets;
            self.inner.store.update_task_spec(id, &row.spec)?;
        }
        if let Some(choice) = &switched {
            let previous = row.spec.provider.clone();
            row.spec.provider = choice.clone();
            self.inner.store.update_task_spec(id, &row.spec)?;
            self.inner.store.append_notice(
                id,
                NoticeLevel::Info,
                &format!(
                    "Model changed from {} {} to {} {} for this and later turns. Instructions, permissions, and the \
                     conversation stay the same; another provider's private reasoning is not carried over.",
                    previous.provider.label(),
                    previous.model,
                    choice.provider.label(),
                    choice.model
                ),
            )?;
        }
        let mut content = self.inner.dangling_tool_results(id)?;
        // The approvals and the files' saved and review state as they are
        // now (the user may have approved, revoked, saved, kept, or reverted
        // since the last run): the model must not infer them from the graph.
        let authority = self.inner.authority(id, &row.spec, files.root(), recorded.as_ref());
        let outlook = self.inner.outlook(id, &row.spec, &files, authority.clone());
        let changed = prompt::changed_files(&self.inner.store, id, &self.inner.documents, &files);
        content.push(json!({
            "type": "text",
            "text": prompt::context_update(&row.spec, &self.inner.documents, &authority, &outlook, &changed),
        }));
        if let Some(graph_context) = &graph_context {
            self.inner.store.insert_graph_context(id, graph_context)?;
            content.push(json!({"type": "text", "text": graph::context::block(graph_context)}));
        }
        content.push(json!({"type": "text", "text": if prompt.is_empty() { ATTACHMENTS_ONLY } else { prompt }}));
        content.extend(attachments.iter().map(att::reference_block));
        self.inner.store.append_message(id, "user", &content)?;
        self.inner.store.update_task(id, &TaskStatus::Queued, &row.usage)?;
        // The worker takes over the reserved slot.
        slot.keep();
        self.spawn(id.clone(), provider);
        let summary = self.inner.summary(id)?;
        self.inner.emit(AgentEvent::TaskChanged { task: Box::new(summary.clone()) });
        self.inner.emit(AgentEvent::TranscriptChanged { task_id: id.clone() });
        Ok(summary)
    }

    /// Take the task's worker slot for a new run of an existing task, or
    /// refuse: while its loop runs (`agents.taskRunning`), or while a finished
    /// worker still releases the task's services, e.g. research sessions
    /// (`agents.taskFinishing`; a new run must wait until it has exited). The
    /// check and the reservation are one step under the lock, so two requests
    /// can never both start a worker.
    fn reserve_run(&self, id: &TaskId) -> Result<RunSlot<'_>> {
        let mut running = lock(&self.inner.running);
        match running.get(id) {
            Some(r) if !r.finished => Err(AgentError::new("agents.taskRunning", "The task is still running.")),
            Some(_) => Err(AgentError::new("agents.taskFinishing", "The task is finishing up; try again in a moment.")),
            None => {
                running.insert(id.clone(), Running::new());
                Ok(RunSlot { running: &self.inner.running, id: id.clone(), kept: false })
            }
        }
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

    /// Answer a structured question (the user's answer card). Validates every
    /// answer against the request's current revision; a second identical
    /// answer is accepted and changes nothing. The answer grants nothing: the
    /// central gate still decides every later action.
    pub fn answer_question(
        &self,
        id: &TaskId,
        request_id: &str,
        revision: u32,
        answers: Vec<QuestionAnswer>,
    ) -> Result<()> {
        let answers = questions::normalize(&answers);
        let mut running = lock(&self.inner.running);
        let Some(slot) = running.get_mut(id).and_then(|r| r.questions.get_mut(request_id)) else {
            drop(running);
            // No worker waits for it: answered, superseded, or unknown.
            let stored = self.inner.stored_question(id, request_id)?;
            return settled_answer(&stored.state, &answers).unwrap_or_else(|| Err(no_longer_waiting()));
        };
        if let Some(settled) = settled_answer(&slot.request.state, &answers) {
            return settled;
        }
        if revision != slot.request.revision {
            return Err(stale_revision(slot.request.revision));
        }
        questions::validate_answers(&slot.request.questions, &answers)
            .map_err(|message| AgentError::new(questions::codes::INVALID_ANSWER, message))?;
        // Like an approval, an answer is acted on only in the task's own open
        // project; otherwise the question is superseded and the held calls
        // meet the gate's own checks.
        let closed = self.inner.project_closed(id);
        slot.request.state = if closed {
            QuestionRequestState::Superseded { reason: PROJECT_CLOSED.to_owned() }
        } else {
            QuestionRequestState::Answered { answers }
        };
        self.inner.store.update_question(&slot.request)?;
        if let Some(abort) = &slot.discussion {
            abort.cancel();
        }
        let _ = slot.tx.send(QuestionInput::Resolved);
        drop(running);
        self.inner.emit(AgentEvent::QuestionResolved { task_id: id.clone(), request_id: request_id.to_owned() });
        if closed {
            return Err(AgentError::new(
                questions::codes::RESOLVED,
                format!("This question can no longer be answered: {}.", PROJECT_CLOSED.to_lowercase()),
            ));
        }
        Ok(())
    }

    /// "Chat about it": one bounded discussion turn about a pending question,
    /// in the task's own worker and budgets; the question's tool batch stays held.
    pub fn discuss_question(&self, id: &TaskId, request_id: &str, revision: u32, text: &str) -> Result<()> {
        let text = text.trim();
        if text.is_empty() {
            return Err(AgentError::new("agents.emptyPrompt", "Write what you want to ask about the question."));
        }
        if text.chars().count() > questions::MAX_DISCUSSION_CHARS {
            return Err(AgentError::new(
                "agents.promptTooLong",
                format!("A message about a question can have at most {} characters.", questions::MAX_DISCUSSION_CHARS),
            ));
        }
        let mut running = lock(&self.inner.running);
        let Some(slot) = running.get_mut(id).and_then(|r| r.questions.get_mut(request_id)) else {
            drop(running);
            let stored = self.inner.stored_question(id, request_id)?;
            return Err(settled_error(&stored.state).unwrap_or_else(no_longer_waiting));
        };
        match &slot.request.state {
            QuestionRequestState::Discussing => {
                return Err(AgentError::new(
                    questions::codes::BUSY,
                    "The agent is still replying about this question; wait for its reply.",
                ));
            }
            QuestionRequestState::Pending => {}
            settled => return Err(settled_error(settled).unwrap_or_else(no_longer_waiting)),
        }
        if revision != slot.request.revision {
            return Err(stale_revision(slot.request.revision));
        }
        if self.inner.project_closed(id) {
            slot.request.state = QuestionRequestState::Superseded { reason: PROJECT_CLOSED.to_owned() };
            self.inner.store.update_question(&slot.request)?;
            let _ = slot.tx.send(QuestionInput::Resolved);
            drop(running);
            self.inner.emit(AgentEvent::QuestionResolved { task_id: id.clone(), request_id: request_id.to_owned() });
            return Err(AgentError::new(
                questions::codes::RESOLVED,
                format!("This question can no longer be discussed: {}.", PROJECT_CLOSED.to_lowercase()),
            ));
        }
        slot.request.state = QuestionRequestState::Discussing;
        self.inner.store.update_question(&slot.request)?;
        self.inner.store.append_discussion(id, request_id, "user", &[json!({"type": "text", "text": text})])?;
        let _ = slot.tx.send(QuestionInput::Discuss);
        drop(running);
        self.inner.emit(AgentEvent::QuestionChanged { task_id: id.clone(), request_id: request_id.to_owned() });
        self.inner.emit(AgentEvent::TranscriptChanged { task_id: id.clone() });
        Ok(())
    }

    /// The bytes of a figure one of this task's tool calls recorded, read
    /// through the research artifact store (hash and size verified).
    pub fn figure(&self, id: &TaskId, run_id: &str, artifact_id: &str) -> Result<Vec<u8>> {
        let unavailable = |reason: &str| {
            AgentError::new("agents.figureUnavailable", format!("The figure is not available: {reason}."))
        };
        let row = self.inner.store.task(id)?.ok_or_else(|| not_found(id))?;
        let recorded = self
            .inner
            .store
            .tool_calls(id)?
            .into_iter()
            .flat_map(|call| call.figures)
            .find(|f| f.run_id == run_id && f.artifact_id == artifact_id);
        let Some(figure) = recorded else {
            return Err(unavailable("it is not this task's (no tool call of this task recorded it)"));
        };
        let files =
            self.inner.content_files(&row).ok_or_else(|| unavailable("missing: the task's project is not open"))?;
        let (record, bytes) = match self.inner.services.artifact_bytes(files.root(), run_id, artifact_id) {
            Ok(found) => found,
            Err(tools::services::ServiceError::Outcome(value))
                if value.get("code").and_then(Value::as_str) == Some("research.unknownRun") =>
            {
                return Err(unavailable("purged: the record of the run that made it was removed"));
            }
            Err(tools::services::ServiceError::Outcome(value)) => {
                let reason = value.get("error").and_then(Value::as_str).unwrap_or("it cannot be read").to_owned();
                return Err(unavailable(&format!("missing: {reason}")));
            }
            Err(e) => return Err(unavailable(&format!("missing: {e}"))),
        };
        let digest = format!("sha256:{}", att::sha256_hex(&bytes));
        let intact = record.kind == exposition_core::artifact::ArtifactKind::Figure
            && record.media_type == figure.media_type
            && record.size_bytes == bytes.len() as u64
            && record.content_hash.eq_ignore_ascii_case(&digest);
        if !intact {
            return Err(unavailable("it changed after the run recorded it (its size or hash no longer match)"));
        }
        Ok(bytes)
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
        // A continued task's slot was reserved by `reserve_run` (a cancel
        // that arrived since then is kept); a new task takes its slot here.
        let cancel = lock(&self.inner.running).entry(id.clone()).or_insert_with(Running::new).cancel.clone();
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
            provider: None,
            attachments: None,
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
            if let Some(running) = lock(&self.inner.running).get_mut(&row.id) {
                running.stop_reason = Some(format!("The task stopped: project permissions changed ({what})"));
            }
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

    /// Compact the task database after removals; (before, after) sizes.
    pub fn compact_tasks(&self) -> Result<(Option<u64>, Option<u64>)> {
        Ok(self.inner.store.compact()?)
    }
}

fn not_found(id: &TaskId) -> AgentError {
    AgentError::new("agents.notFound", format!("Unknown task {id}."))
}

/// Answering a request that is no longer open: the identical answer again is
/// accepted (a duplicate delivery); anything else is refused. `None` while
/// the request is open.
fn settled_answer(state: &QuestionRequestState, answers: &[QuestionAnswer]) -> Option<Result<()>> {
    match state {
        QuestionRequestState::Answered { answers: given } if given.as_slice() == answers => Some(Ok(())),
        open if open.is_open() => None,
        settled => settled_error(settled).map(Err),
    }
}

/// Why a resolved request takes no more input.
fn settled_error(state: &QuestionRequestState) -> Option<AgentError> {
    match state {
        QuestionRequestState::Answered { .. } => {
            Some(AgentError::new(questions::codes::RESOLVED, "This question was already answered."))
        }
        QuestionRequestState::Superseded { reason } => Some(AgentError::new(
            questions::codes::RESOLVED,
            format!("This question can no longer be answered ({reason})."),
        )),
        QuestionRequestState::Pending | QuestionRequestState::Discussing => None,
    }
}

fn no_longer_waiting() -> AgentError {
    AgentError::new(questions::codes::RESOLVED, "The task is no longer waiting for this answer.")
}

fn stale_revision(current: u32) -> AgentError {
    AgentError::new(
        questions::codes::STALE,
        format!("The options changed since you read them (now revision {current}); answer the current question."),
    )
}

enum Interrupt {
    Canceled,
    WallTime,
    /// The run ends with this status (a budget reached while discussing a
    /// question).
    Stop(TaskStatus),
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

    /// A task's stored question request (no worker waits for it).
    fn stored_question(&self, id: &TaskId, request_id: &str) -> Result<QuestionRequest> {
        self.store.task(id)?.ok_or_else(|| not_found(id))?;
        self.store
            .question(id, request_id)?
            .ok_or_else(|| AgentError::new(questions::codes::NOT_FOUND, "This task has no such question."))
    }

    /// Record on a finished native `run_python` call the figures of the run
    /// it created, from that run's manifest (never from the result text):
    /// artifacts of kind figure with an image media type.
    fn record_figures(&self, id: &TaskId, root: &Path, tool_use_id: &str, output: &ToolOutput) {
        let Some(run_id) = output.content.get("run").and_then(Value::as_str) else { return };
        let artifacts = match self.services.run_artifacts(root, run_id) {
            Ok(artifacts) => artifacts,
            Err(e) => {
                tracing::warn!("the figures of run {run_id} could not be read: {e}");
                return;
            }
        };
        let figures: Vec<FigureRef> = artifacts
            .into_iter()
            .filter(|a| {
                a.kind == exposition_core::artifact::ArtifactKind::Figure
                    && FIGURE_MEDIA_TYPES.contains(&a.media_type.as_str())
            })
            .map(|a| {
                let name = a
                    .path
                    .as_ref()
                    .map(|p| p.as_str())
                    .or(a.scratch_path.as_deref())
                    .and_then(|p| p.rsplit(['/', '\\']).next())
                    .filter(|n| !n.is_empty())
                    .unwrap_or(a.id.0.as_str())
                    .to_owned();
                FigureRef {
                    run_id: run_id.to_owned(),
                    artifact_id: a.id.0,
                    media_type: a.media_type,
                    name,
                    caption: None,
                }
            })
            .collect();
        if figures.is_empty() {
            return;
        }
        if let Err(e) = self.store.set_tool_call_figures(id, tool_use_id, &figures) {
            tracing::error!("failed to record figures: {e}");
        }
    }

    /// The task's project folder is no longer the open project.
    fn project_closed(&self, id: &TaskId) -> bool {
        match self.store.task(id) {
            Ok(Some(row)) => {
                crate::workspace::check_root(self.workspace.as_deref(), Path::new(&row.spec.workspace_root)).is_err()
            }
            _ => true,
        }
    }

    /// Once a run ended (cancel, a budget stop, a failure, a permission
    /// change): supersede the task's open questions and drop their slots, and
    /// close a graph proposal that still waits for an approval (its approval
    /// request ended with the run, so it can never be approved).
    fn close_pending(&self, id: &TaskId, status: &TaskStatus) {
        let reason = {
            let mut running = lock(&self.running);
            let entry = running.get_mut(id);
            let reason = entry.as_ref().and_then(|r| r.stop_reason.clone());
            if let Some(r) = entry {
                r.questions.clear();
            }
            reason
        };
        let reason = reason.unwrap_or_else(|| match status {
            TaskStatus::Canceled => "The task was canceled".to_owned(),
            TaskStatus::BudgetExceeded { detail, .. } => format!("The task stopped: {}", detail.trim_end_matches('.')),
            TaskStatus::Failed { message } => format!("The task stopped: {}", message.trim_end_matches('.')),
            _ => "The task stopped".to_owned(),
        });
        if let Err(e) = self.store.close_waiting_proposals(Some(id), &unapproved_proposal(&reason)) {
            tracing::error!("failed to close a waiting graph proposal: {e}");
        }
        match self.store.supersede_open_questions(Some(id), &reason) {
            Ok(changed) => {
                for (task_id, request_id) in changed {
                    self.emit(AgentEvent::QuestionResolved { task_id, request_id });
                }
            }
            Err(e) => tracing::error!("failed to supersede open questions: {e}"),
        }
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

    /// What the gate would decide now for each kind of action of a task
    /// (for the context the model is given; it authorizes nothing).
    fn outlook(&self, id: &TaskId, spec: &TaskSpec, files: &LocalProjectFiles, authority: Authority) -> exec::Outlook {
        exec::outlook(&ToolEnv {
            task_id: id,
            spec,
            files,
            documents: &self.documents,
            workspace: self.workspace.as_deref(),
            services: self.services.as_ref(),
            store: &self.store,
            graph: None,
            authority,
        })
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

    /// The task's graph record; without `context`, only the number of
    /// contexts attached is read (a partial detail read).
    fn graph_info(&self, id: &TaskId, context: bool) -> Result<TaskGraphInfo> {
        let (context, contexts) =
            if context { self.store.graph_context(id)? } else { (None, self.store.graph_context_count(id)?) };
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

    fn credential_store(&self, provider: ProviderKind) -> Result<Arc<dyn SecretStore>> {
        match provider {
            ProviderKind::Anthropic => Ok(Arc::clone(&self.credentials)),
            ProviderKind::OpenAi => lock(&self.openai).credentials.clone().ok_or_else(|| {
                AgentError::new("agents.providerUnavailable", "OpenAI connections are not available in this build.")
            }),
            ProviderKind::Scripted => {
                Err(AgentError::new("agents.noCredential", "The scripted provider has no credential."))
            }
        }
    }

    /// The stored key of a connection and its request context.
    fn connection_key(&self, provider: ProviderKind) -> Result<(crate::credentials::Secret, OpenAiConnection)> {
        let key = self
            .credential_store(provider)?
            .get()
            .map_err(|e| AgentError::new("agents.credentials", e.to_string()))?
            .ok_or_else(|| {
                AgentError::new(
                    "agents.notConfigured",
                    format!("No {} API key is configured. Add it in the agent's provider settings.", provider.label()),
                )
            })?;
        let connection = match provider {
            ProviderKind::OpenAi => self.settings()?.openai.unwrap_or_default(),
            _ => OpenAiConnection::default(),
        };
        Ok((key, connection))
    }

    fn credential_version(&self, provider: ProviderKind) -> u64 {
        match provider {
            ProviderKind::Anthropic => self.credential_versions[0].load(std::sync::atomic::Ordering::SeqCst),
            ProviderKind::OpenAi => self.credential_versions[1].load(std::sync::atomic::Ordering::SeqCst),
            ProviderKind::Scripted => 0,
        }
    }

    fn bump_credential_version(&self, provider: ProviderKind) {
        let index = match provider {
            ProviderKind::Anthropic => 0,
            ProviderKind::OpenAi => 1,
            ProviderKind::Scripted => return,
        };
        self.credential_versions[index].fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    /// The current connection's catalog entry for a model, when its catalog
    /// was fetched (the provider's own evidence comes before the rules).
    fn known_model(&self, choice: &ProviderChoice) -> Option<catalog::ModelDescriptor> {
        if choice.provider == ProviderKind::Scripted {
            return None;
        }
        let (key, connection) = self.connection_key(choice.provider).ok()?;
        let fingerprint = connection_fingerprint(choice.provider, &key, &connection);
        let catalogs = lock(&self.catalogs);
        let (_, models) = catalogs.get(&fingerprint)?;
        models.iter().find(|m| m.id == choice.model).cloned()
    }

    /// Keep these attachments from being discarded until the guard drops
    /// (after the message referring to them is stored, or the send failed).
    fn reserve(&self, attachments: &[AttachmentInfo]) -> Reservation<'_> {
        let ids: Vec<String> = attachments.iter().map(|a| a.id.0.clone()).collect();
        lock(&self.reserved).extend(ids.iter().cloned());
        Reservation { set: &self.reserved, ids }
    }

    /// An attachment's record, marked missing when its snapshot is gone.
    fn attachment(&self, id: &AttachmentId) -> Result<AttachmentInfo> {
        if !id.is_valid() {
            return Err(AgentError::new("attachments.invalid", "invalid attachment identifier"));
        }
        let mut info = self.store.attachment(id)?.ok_or_else(|| {
            AgentError::new("attachments.notFound", "The attachment no longer exists; attach it again.")
        })?;
        info.missing = !self.attachments.exists(&info.sha256);
        Ok(info)
    }

    /// Validated attachments for a new message (order kept, duplicates removed).
    fn message_attachments(&self, ids: &[AttachmentId]) -> Result<Vec<AttachmentInfo>> {
        if ids.len() > att::MAX_PER_MESSAGE {
            return Err(AgentError::new(
                "attachments.tooMany",
                format!("A message can carry at most {} attachments.", att::MAX_PER_MESSAGE),
            ));
        }
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for id in ids.iter().filter(|id| seen.insert((*id).clone())) {
            let info = self.attachment(id)?;
            if info.missing {
                return Err(AgentError::new(
                    "attachments.missing",
                    format!("“{}” is no longer available; remove it and attach it again.", info.name),
                ));
            }
            out.push(info);
        }
        Ok(out)
    }

    /// Every attachment in a task's history plus `extra`, and their encoded size.
    fn history_attachments(&self, id: &TaskId, extra: &[AttachmentInfo]) -> Result<(Vec<AttachmentInfo>, u64)> {
        let mut all: Vec<AttachmentInfo> = extra.to_vec();
        for (_, content) in self.store.messages(id)? {
            for block in content.iter().filter(|b| block_type(b) == "exposition_attachment") {
                if let Some(aid) = block.get("attachment_id").and_then(Value::as_str)
                    && let Ok(Some(info)) = self.store.attachment(&AttachmentId(aid.to_owned()))
                {
                    all.push(info);
                }
            }
        }
        let bytes = all.iter().map(|a| encoded_size(a.size_bytes)).sum();
        Ok((all, bytes))
    }

    /// The history as a provider receives it: attachment references become
    /// their content (text, image, or document blocks read from the
    /// snapshot), and another provider's private state is dropped (OpenAI
    /// reasoning items are only replayed to OpenAI; the OpenAI adapter drops
    /// Anthropic thinking itself). A missing snapshot fails the request with
    /// an explanation; nothing is silently omitted. `total` is the encoded
    /// size of the attachments of the conversation so far (the per-request
    /// limit counts all of them); it grows by what `messages` carry.
    fn provider_messages(
        &self,
        kind: ProviderKind,
        messages: Vec<(String, Vec<Value>)>,
        total: &mut u64,
    ) -> std::result::Result<Vec<ProviderMessage>, String> {
        let mut out = Vec::with_capacity(messages.len());
        for (role, content) in messages {
            let mut blocks = Vec::with_capacity(content.len());
            for block in content {
                match block_type(&block) {
                    "exposition_attachment" => {
                        let id =
                            AttachmentId(block.get("attachment_id").and_then(Value::as_str).unwrap_or("").to_owned());
                        let name = block.get("name").and_then(Value::as_str).unwrap_or("attachment").to_owned();
                        let info = self
                            .store
                            .attachment(&id)
                            .map_err(|e| e.to_string())?
                            .ok_or_else(|| format!("The attachment “{name}” is no longer stored, so this conversation cannot be sent again."))?;
                        let bytes = self
                            .attachments
                            .get(&info.sha256)
                            .ok_or_else(|| format!("The attachment “{name}” is missing from storage, so this conversation cannot be sent again."))?;
                        *total += encoded_size(bytes.len() as u64);
                        if *total > att::MAX_REQUEST_BYTES {
                            return Err(format!(
                                "The conversation's attachments exceed {} per request; start a new task to continue.",
                                att::human_size(att::MAX_REQUEST_BYTES)
                            ));
                        }
                        blocks.push(match info.kind {
                            AttachmentKind::Text => {
                                let text = String::from_utf8_lossy(bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes)).into_owned();
                                json!({"type": "text", "text": format!("<attachment name=\"{}\" media_type=\"{}\">\n{text}\n</attachment>", info.name.replace('"', "'"), info.media_type)})
                            }
                            AttachmentKind::Image => json!({"type": "image", "source": {"type": "base64", "media_type": info.media_type, "data": att::base64_encode(&bytes)}}),
                            AttachmentKind::Pdf => json!({"type": "document", "title": info.name, "source": {"type": "base64", "media_type": "application/pdf", "data": att::base64_encode(&bytes)}}),
                        });
                    }
                    t if t == REASONING_BLOCK && kind != ProviderKind::OpenAi => {}
                    _ => blocks.push(block),
                }
            }
            if !blocks.is_empty() {
                out.push(ProviderMessage { role, content: blocks });
            }
        }
        Ok(out)
    }

    fn make_provider(&self, choice: &ProviderChoice, task: &TaskId) -> Result<Arc<dyn Provider>> {
        self.make_provider_with(choice, task, None)
    }

    /// `make_provider` with the OpenAI connection context to use instead of
    /// the one the task recorded (a model switch resolves it again; the
    /// caller records it once the switch is accepted).
    fn make_provider_with(
        &self,
        choice: &ProviderChoice,
        task: &TaskId,
        resolved: Option<OpenAiConnection>,
    ) -> Result<Arc<dyn Provider>> {
        match choice.provider {
            ProviderKind::OpenAi => {
                let (key, current) = self.connection_key(ProviderKind::OpenAi)?;
                // The context the task resolved when it started (or switched)
                // stays its billing context; a new task records the current one.
                let connection = match (resolved, self.store.task_connection(task)?) {
                    (Some(resolved), _) => resolved,
                    (None, Some(recorded)) => serde_json::from_value(recorded).unwrap_or(current),
                    (None, None) => {
                        let value = serde_json::to_value(&current).unwrap_or(Value::Null);
                        if self.store.task(task)?.is_some() {
                            self.store.set_task_connection(task, &value)?;
                        }
                        current
                    }
                };
                let known = self.known_model(choice);
                let resolved = preflight(choice, &[], known.as_ref())?;
                let base_url = lock(&self.openai).base_url.clone();
                let provider = OpenAiProvider::with_base_url(
                    &base_url,
                    key,
                    choice.model.clone(),
                    resolved.effort,
                    connection,
                    self.retry,
                )
                .map_err(|e| AgentError::new("agents.provider", e.to_string()))?
                .with_output_cap(resolved.max_output_tokens);
                Ok(Arc::new(provider))
            }
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
    ///
    /// The stored texts stay in the database: a file at the task's tip or
    /// back at its baseline is decided by comparing there, and only a file
    /// that has moved on from both has its texts read and diffed.
    fn pending_review(&self, row: &TaskRow, files: &[crate::store::TaskFileState]) -> Result<u32> {
        let project = if row.status.is_active() { None } else { self.content_files(row) };
        let Some(project) = project else {
            return Ok(files.iter().filter(|f| f.state == ReviewState::Pending).count() as u32);
        };
        let mut pending = 0;
        for file in files.iter().filter(|f| matches!(f.state, ReviewState::Pending | ReviewState::Reverted)) {
            let current = review::current(&file.path, &self.documents, &project);
            let compared = match current.text() {
                Some(text) if file.kind == ReviewChangeKind::Edited && !file.entangled => {
                    Some(self.net_change_in(&row.id, &file.path, text)?)
                }
                _ => None,
            };
            // `judge` consults the comparison exactly when it was made above.
            let judged = review::judge(file.kind, file.state, file.entangled, &current, |_| {
                compared.unwrap_or((review::Presence::Present, true))
            });
            if judged.map_or(file.state, |(state, _)| state) == ReviewState::Pending {
                pending += 1;
            }
        }
        Ok(pending)
    }

    /// Whether the task's net change to an edited file is in `text`, and
    /// whether `text` is the file without that change (its stored baseline).
    fn net_change_in(&self, id: &TaskId, path: &WorkspacePath, text: &str) -> Result<(review::Presence, bool)> {
        if let Some(compared) = self.store.task_file_match(id, path, text)?
            && let Some(presence) = review::presence_of(compared)
        {
            return Ok((presence, compared.at_baseline));
        }
        // The file has moved on from both stored texts: diff them.
        Ok(match self.store.task_file(id, path)? {
            Some(row) => (review::presence(&row.baseline_text, &row.tip_text, text), text == row.baseline_text),
            // The record is gone: the stored state stands.
            None => (review::Presence::Present, true),
        })
    }

    fn summary_of(&self, row: &TaskRow) -> Result<TaskSummary> {
        let files = self.store.task_file_states(&row.id)?;
        Ok(TaskSummary {
            id: row.id.clone(),
            title: row.title.clone(),
            created_at_ms: row.created_at_ms,
            updated_at_ms: row.updated_at_ms,
            status: row.status.clone(),
            spec: row.spec.clone(),
            usage: row.usage.clone(),
            pending_review: self.pending_review(row, &files)?,
            changed_files: files.len() as u32,
        })
    }

    fn review_changed(&self, id: &TaskId) {
        self.emit(AgentEvent::ReviewChanged { task_id: id.clone() });
        if let Ok(summary) = self.summary(id) {
            self.emit(AgentEvent::TaskChanged { task: Box::new(summary) });
        }
    }

    /// The rendered transcript from sequence number `from` on (0: all of it).
    fn transcript(&self, id: &TaskId, from: u64) -> Result<Vec<TranscriptItem>> {
        // Results are shown cut to `TRANSCRIPT_RESULT_LIMIT` bytes: read no
        // more of each than that can need (one more character than the
        // limit has bytes, so a longer result is still seen to be longer).
        let calls: HashMap<String, ToolCallRow> = self
            .store
            .tool_call_previews(id, TRANSCRIPT_RESULT_LIMIT + 1)?
            .into_iter()
            .map(|c| (c.tool_use_id.clone(), c))
            .collect();
        let mut items = Vec::new();
        for row in self.store.transcript_from(id, from)? {
            let seq = row.seq;
            // A discussion entry renders like a message; its user text is
            // marked with the question it is about.
            let (role, content, question) = match row.entry {
                TranscriptEntry::Notice { level, message } => {
                    items.push(TranscriptItem::Notice { seq, level, message });
                    continue;
                }
                TranscriptEntry::Message { role, content } => (role, content, None),
                TranscriptEntry::Discussion { question, role, content } => (role, content, Some(question)),
            };
            // Attachments render with the message's text (or alone).
            let attached: Vec<AttachmentInfo> = content
                .iter()
                .filter(|b| role == "user" && block_type(b) == "exposition_attachment")
                .map(|b| {
                    let id = AttachmentId(b.get("attachment_id").and_then(Value::as_str).unwrap_or("").to_owned());
                    self.attachment(&id).unwrap_or_else(|_| AttachmentInfo {
                        id,
                        name: b.get("name").and_then(Value::as_str).unwrap_or("attachment").to_owned(),
                        media_type: b.get("media_type").and_then(Value::as_str).unwrap_or("").to_owned(),
                        kind: b
                            .get("kind")
                            .and_then(Value::as_str)
                            .and_then(AttachmentKind::parse)
                            .unwrap_or(AttachmentKind::Text),
                        size_bytes: 0,
                        width: None,
                        height: None,
                        sha256: String::new(),
                        missing: true,
                    })
                })
                .collect();
            let first_user_item = items.len();
            for block in content {
                let text = block.get("text").and_then(Value::as_str).unwrap_or_default();
                match (role.as_str(), block_type(&block)) {
                    ("user", "text")
                        if !text.starts_with("<task_context>")
                            && !text.starts_with("<graph_context>")
                            && !text.starts_with(QUESTION_CONTEXT) =>
                    {
                        let shown = if text == ATTACHMENTS_ONLY && !attached.is_empty() { "" } else { text };
                        items.push(TranscriptItem::UserText {
                            seq,
                            text: shown.to_owned(),
                            attachments: Vec::new(),
                            question: question.clone(),
                        })
                    }
                    ("assistant", t) if t == REASONING_BLOCK => {
                        let summary: Vec<&str> = block
                            .get("summary")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .filter_map(|s| s.get("text").and_then(Value::as_str))
                            .collect();
                        if !summary.is_empty() {
                            items.push(TranscriptItem::Thinking { seq, text: summary.join("\n\n") });
                        }
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
                        let tool_use_id = block.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
                        let call = calls.get(&tool_use_id);
                        items.push(TranscriptItem::ToolCall {
                            seq,
                            name: block.get("name").and_then(Value::as_str).unwrap_or_default().to_owned(),
                            input: block.get("input").cloned().unwrap_or(Value::Null),
                            status: call.map(|c| c.status).unwrap_or(ToolCallStatus::NotRun),
                            result: call.and_then(|c| c.result.as_deref()).map(|r| bounded(r, TRANSCRIPT_RESULT_LIMIT)),
                            is_error: call.is_some_and(|c| c.is_error),
                            authorized_by: call.and_then(|c| c.authorized_by.clone()),
                            figures: call.map(|c| c.figures.clone()).unwrap_or_default(),
                            tool_use_id,
                        });
                    }
                    _ => {}
                }
            }
            if !attached.is_empty() {
                let user_text = items[first_user_item..].iter_mut().rev().find_map(|item| match item {
                    TranscriptItem::UserText { attachments, .. } => Some(attachments),
                    _ => None,
                });
                match user_text {
                    Some(attachments) => *attachments = attached,
                    None => items.push(TranscriptItem::UserText {
                        seq,
                        text: String::new(),
                        attachments: attached,
                        question: None,
                    }),
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
                    // Questions need a user in the loop: never for a task
                    // created without one (an unreadable flag counts as
                    // without), nor for a restricted-only (unattended) run.
                    let interactive = !inner.store.task_unattended(&id).unwrap_or(true)
                        && row.spec.execution != ExecutionMode::Restricted;
                    let mut run = Run {
                        interactive,
                        context: Vec::new(),
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
                        history: None,
                        attachment_bytes: 0,
                        authorized: HashMap::new(),
                        granted: HashMap::new(),
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
        if let Some((status, usage)) = &status {
            inner.finish(&id, status, usage);
        }
        if let Some(running) = lock(&inner.running).get_mut(&id) {
            running.finished = true;
        }
        // Release what the services hold for this run (agent research
        // sessions). This can take seconds; the finished task was announced
        // in `finish`. Exactly one terminal `TaskChanged` is emitted per
        // run: listeners take it as "this run ended", and a second one would
        // be read as the end of the task's next run.
        inner.services.task_finished(&id.0).await;
        lock(&inner.running).remove(&id);
        if status.is_some() {
            return;
        }
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
        // What waited for the user ended with the run.
        self.close_pending(id, status);
        // The terminal status is stored: review actions may proceed while the
        // worker still releases services, and the finished task is announced
        // now rather than after that release (lists would show it running
        // meanwhile). The summary goes before the transcript event, so a view
        // that re-reads the conversation has already seen the task finish.
        if let Some(running) = lock(&self.running).get_mut(id) {
            running.finished = true;
        }
        match self.summary(id) {
            Ok(summary) => self.emit(AgentEvent::TaskChanged { task: Box::new(summary) }),
            Err(e) => tracing::error!("failed to read the finished task: {}", e.message),
        }
        self.emit(AgentEvent::TranscriptChanged { task_id: id.clone() });
        self.emit(AgentEvent::ReviewChanged { task_id: id.clone() });
    }
}

/// The record of a graph proposal whose approval can no longer be given.
fn unapproved_proposal(reason: &str) -> String {
    format!("{reason} before this proposal was answered: it was not approved, and nothing was applied.")
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

/// Output tokens one more model request needs: below this remainder of the
/// output-token budget the task stops before sending another request.
const MIN_OUTPUT_TOKENS_PER_REQUEST: u64 = 1024;

/// Why the output-token budget stops the task before another request. A
/// remainder above zero is not "used up": the meter still shows it, so say it
/// is less than a request needs (XR-F-02: "used up" beside 51 of 1024).
fn output_budget_detail(max_output_tokens: u64, remaining: u64) -> String {
    if remaining == 0 {
        format!("the output-token budget ({max_output_tokens}) was used up.")
    } else {
        format!(
            "{remaining} output tokens of the budget ({max_output_tokens}) remain, fewer than one more model \
             request needs ({MIN_OUTPUT_TOKENS_PER_REQUEST})."
        )
    }
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

/// The most output tokens one request asks for: the per-turn limit, or the
/// model's own limit when the provider applies a smaller one.
fn turn_output_limit(provider: &dyn Provider) -> u32 {
    provider.output_cap().map_or(MAX_TOKENS_PER_TURN, |cap| cap.min(MAX_TOKENS_PER_TURN))
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
fn stop_message(stop_reason: &str, unrun_calls: usize, output_limit: u32) -> String {
    let calls = match unrun_calls {
        0 => String::new(),
        1 => " Its tool call was not run.".to_owned(),
        n => format!(" Its {n} tool calls were not run."),
    };
    match stop_reason {
        "max_tokens" => format!(
            "The response reached the per-turn output limit ({output_limit} tokens) and was cut off.{calls} \
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

/// How long streamed text is held to be sent with what follows it. A model
/// streams many small deltas per second; each event sent to the webview
/// costs an IPC message and a render there.
const DELTA_INTERVAL: Duration = Duration::from_millis(25);
/// Held text is sent at this size whatever the time.
const DELTA_MAX_BYTES: usize = 16 * 1024;

/// The stream of one model request, forwarded to the task's events with
/// consecutive text (or thinking) deltas joined: the first delta after a
/// pause goes out at once; what arrives within [`DELTA_INTERVAL`] of the last
/// event is held and sent together. Nothing is dropped or reordered: a delta
/// of the other kind, a reset, or a fallback sends the held text first, a
/// timer sends it while the stream is quiet, and the end of the stream sends
/// it before any other event of the task.
struct StreamDeltas {
    events: broadcast::Sender<AgentEvent>,
    task_id: TaskId,
    turn: u32,
    held: Mutex<HeldDelta>,
}

struct HeldDelta {
    thinking: bool,
    text: String,
    /// When this stream last sent a delta event.
    sent: Option<Instant>,
}

impl StreamDeltas {
    fn push(&self, event: StreamEvent) {
        let event = match event {
            StreamEvent::TextDelta(text) => return self.delta(false, &text),
            StreamEvent::ThinkingDelta(text) => return self.delta(true, &text),
            StreamEvent::Reset { reason } => {
                AgentEvent::StreamReset { task_id: self.task_id.clone(), turn: self.turn, reason }
            }
            StreamEvent::Fallback { from, to } => AgentEvent::Fallback { task_id: self.task_id.clone(), from, to },
            StreamEvent::MessageStart { .. } | StreamEvent::ToolUseStart { .. } => return,
        };
        self.flush();
        let _ = self.events.send(event);
    }

    fn delta(&self, thinking: bool, text: &str) {
        let mut held = lock(&self.held);
        if held.thinking != thinking {
            self.send(&mut held);
            held.thinking = thinking;
        }
        held.text.push_str(text);
        let due = held.sent.is_none_or(|sent| sent.elapsed() >= DELTA_INTERVAL);
        if due || held.text.len() >= DELTA_MAX_BYTES {
            self.send(&mut held);
        }
    }

    fn send(&self, held: &mut HeldDelta) {
        if held.text.is_empty() {
            return;
        }
        let text = std::mem::take(&mut held.text);
        let (task_id, turn) = (self.task_id.clone(), self.turn);
        let event = if held.thinking {
            AgentEvent::ThinkingDelta { task_id, turn, text }
        } else {
            AgentEvent::TextDelta { task_id, turn, text }
        };
        held.sent = Some(Instant::now());
        let _ = self.events.send(event);
    }

    /// Send what is held.
    fn flush(&self) {
        self.send(&mut lock(&self.held));
    }

    /// Run the provider's stream to its end, sending held text while the
    /// stream is quiet (so the tail of a burst never waits for the next
    /// delta) and when it ends.
    async fn drive<T>(&self, stream: impl Future<Output = T>) -> T {
        tokio::pin!(stream);
        let mut tick = tokio::time::interval(DELTA_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                out = &mut stream => {
                    self.flush();
                    return out;
                }
                _ = tick.tick() => self.flush(),
            }
        }
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
    /// The conversation as the provider receives it, kept between the
    /// requests of this run: read and converted once (attachments encoded
    /// once), then extended by what each turn stores. `None` until the first
    /// request, and whenever it must be read again. Only this run appends
    /// messages while it is active, so it always equals the stored history.
    history: Option<Vec<ProviderMessage>>,
    /// Encoded size of the attachments in `history`.
    attachment_bytes: u64,
    /// How each call of this run that is about to run, or ran, was authorized.
    authorized: HashMap<String, AuthorizedBy>,
    /// What the approval of a call granted for the rest of the task.
    granted: HashMap<String, Vec<AllowanceKind>>,
    files: LocalProjectFiles,
    /// The project graph the task is bound to.
    graph_project: Option<ProjectId>,
    /// A user is in the loop: `ask_user` is offered.
    interactive: bool,
    /// Text blocks that go with the current batch's results (a question's
    /// discussion and final options, beside its answer).
    context: Vec<Value>,
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
        let remaining_output = b.max_output_tokens.saturating_sub(self.usage.output_tokens);
        if remaining_output < MIN_OUTPUT_TOKENS_PER_REQUEST {
            return Some(budget_exceeded(
                BudgetKind::OutputTokens,
                output_budget_detail(b.max_output_tokens, remaining_output),
            ));
        }
        if self.remaining_wall().is_zero() {
            return Some(budget_exceeded(BudgetKind::WallTime, "the time budget was used up."));
        }
        None
    }

    /// The conversation for the next request: the run's own copy, or (first
    /// request of the run) the stored history, converted for the provider.
    fn take_history(&mut self, kind: ProviderKind) -> Result<Vec<ProviderMessage>, TaskStatus> {
        if let Some(history) = self.history.take() {
            return Ok(history);
        }
        let stored =
            self.inner.store.messages(&self.id).map_err(|e| TaskStatus::Failed { message: e.to_string() })?;
        self.attachment_bytes = 0;
        self.inner
            .provider_messages(kind, stored, &mut self.attachment_bytes)
            .map_err(|message| TaskStatus::Failed { message })
    }

    /// Add a message this run just stored to its copy of the conversation,
    /// converted exactly as a stored message is.
    fn remember(&mut self, kind: ProviderKind, role: &str, content: &[Value]) {
        let Some(history) = self.history.as_mut() else { return };
        let message = vec![(role.to_owned(), content.to_vec())];
        match self.inner.provider_messages(kind, message, &mut self.attachment_bytes) {
            Ok(added) => history.extend(added),
            // The next request reads the stored history again (and reports why it cannot).
            Err(_) => self.history = None,
        }
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
            let messages = match self.take_history(provider.kind()) {
                Ok(messages) => messages,
                Err(status) => return status,
            };
            // One request asks for the remaining output budget, at most the
            // per-turn limit or the model's own smaller limit. Only a request
            // that asked for less than that limit was limited by the budget.
            let remaining_output = self.spec.budgets.max_output_tokens.saturating_sub(self.usage.output_tokens);
            let output_limit = turn_output_limit(provider);
            let max_tokens = remaining_output.min(u64::from(output_limit)) as u32;
            let limited_by_budget = max_tokens < output_limit;
            let request = ProviderRequest {
                system: prompt::SYSTEM_PROMPT.to_owned(),
                tools: tools::task_definitions(self.interactive),
                messages,
                max_tokens,
            };
            let turn = self.usage.turns + 1;
            self.inner.emit(AgentEvent::StreamStarted { task_id: self.id.clone(), turn });
            let deltas = self.deltas(turn);
            let sink = |event: StreamEvent| deltas.push(event);
            let result = tokio::select! {
                r = deltas.drive(provider.stream(&request, &sink, &self.cancel)) => Ok(r),
                _ = self.cancel.cancelled() => Err(Interrupt::Canceled),
                _ = tokio::time::sleep(self.remaining_wall()) => Err(Interrupt::WallTime),
            };
            // Whatever the stream delivered is sent before anything else is.
            deltas.flush();
            // The conversation goes back to the run for the next request.
            self.history = Some(request.messages);
            let response = match result {
                Err(Interrupt::Stop(status)) => return status,
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
                self.remember(provider.kind(), "assistant", &content);
                self.inner.emit(AgentEvent::TranscriptChanged { task_id: self.id.clone() });
            }
            // Only a response that ended normally completes the task, and only
            // a `tool_use` stop runs tools. Every other stop reason ends the run
            // with its own status; the response's tool calls (possibly cut off
            // mid-input) are recorded as not run, never executed.
            let calls = content.iter().filter(|b| block_type(b) == "tool_use").count();
            match response.stop_reason.as_str() {
                "tool_use" if calls > 0 => {
                    if let Some(status) =
                        self.execute_tools(&content, &response.invalid_tool_inputs, turn, provider).await
                    {
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
                    return TaskStatus::Stopped {
                        message: stop_message(&stop_reason, calls, output_limit),
                        stop_reason,
                    };
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
            figures: Vec::new(),
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
        provider: &dyn Provider,
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
            let output = match self.execute_one(tool_use_id, name, input, invalid, &mut budget_hit, provider).await {
                Ok(output) => output,
                Err((interrupt, partial)) => {
                    self.context.clear();
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
                        Interrupt::Stop(status) => status,
                    });
                }
            };
            // A tool that saw the task's cancellation can return before the
            // loop notices it; its error then means "stopped", not "failed".
            let status = match finished_status(&output) {
                ToolCallStatus::Failed if self.cancel.is_cancelled() => ToolCallStatus::Canceled,
                status => status,
            };
            let output = self.with_authorization(tool_use_id, output);
            self.record_call(tool_use_id, turn, name, input, status, Some(&output));
            self.inner.emit(AgentEvent::ToolCallFinished {
                task_id: self.id.clone(),
                tool_use_id: tool_use_id.clone(),
                is_error: output.is_error,
            });
            results.push(tool_result(tool_use_id, &output.text(), output.is_error));
        }
        // After the results: what goes with them (a question's discussion).
        results.append(&mut self.context);
        if let Err(e) = self.inner.store.append_message(&self.id, "user", &results) {
            return Some(TaskStatus::Failed { message: e.to_string() });
        }
        self.remember(provider.kind(), "user", &results);
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
        provider: &dyn Provider,
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
        // Questions are handled by the loop itself: they read and change
        // nothing, so the gate has nothing to decide.
        match name {
            tools::ASK_USER => return self.ask_user(tool_use_id, input, provider).await,
            tools::REVISE_QUESTION => {
                return Ok(ToolOutput::error(
                    "revise_question is only available while the user discusses a question with you.",
                ));
            }
            _ => {}
        }
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
                                if granted.is_some() {
                                    self.granted.insert(tool_use_id.to_owned(), for_task.clone());
                                }
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
        // A native Python run creates a research run whose manifest lists
        // its figures (restricted runs create none).
        let records_figures = matches!(call, ToolCall::RunPython(_))
            && authorization.policy() == Some(&exposition_core::job::ExecutionPolicy::TrustedNative);
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
                if records_figures {
                    inner.record_figures(&self.id, self.files.root(), tool_use_id, &output);
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

    /// Forwards a model request's stream to the task's events.
    fn deltas(&self, turn: u32) -> StreamDeltas {
        StreamDeltas {
            events: self.inner.events.clone(),
            task_id: self.id.clone(),
            turn,
            held: Mutex::new(HeldDelta { thinking: false, text: String::new(), sent: None }),
        }
    }

    /// `ask_user`: record the request and block, like an approval wait,
    /// until the user answers (the rest of the batch stays held). A
    /// discussion runs inside this wait, in this worker.
    async fn ask_user(
        &mut self,
        tool_use_id: &str,
        input: &Value,
        provider: &dyn Provider,
    ) -> Result<ToolOutput, (Interrupt, Option<ToolOutput>)> {
        if !self.interactive {
            return Ok(ToolOutput::error(
                "ask_user is not available in this task: it runs without a user to answer (a workflow review or a \
                 restricted-only run). Decide with what you have, or report what you need from the user.",
            ));
        }
        let asked = match questions::parse_ask_user(input) {
            Ok(asked) => asked,
            Err(message) => return Ok(ToolOutput::error(message)),
        };
        let request = QuestionRequest::new(self.id.clone(), tool_use_id, asked, now_ms() as i64);
        let request_id = request.id.clone();
        let (tx, mut rx) = mpsc::unbounded_channel();
        // The worker's slot exists before the request can be seen anywhere.
        match lock(&self.inner.running).get_mut(&self.id) {
            Some(running) => {
                running
                    .questions
                    .insert(request_id.clone(), QuestionSlot { request: request.clone(), tx, discussion: None });
            }
            None => return Err((Interrupt::Canceled, None)),
        }
        if let Err(e) = self.inner.store.insert_question(&request) {
            self.remove_question(&request_id);
            return Ok(ToolOutput::error(format!("The question could not be recorded: {e}")));
        }
        self.set_status(TaskStatus::AwaitingAnswer);
        self.inner.emit(AgentEvent::QuestionRequested { request });
        self.inner.emit(AgentEvent::TranscriptChanged { task_id: self.id.clone() });
        loop {
            // Passive waiting is not work time.
            let waiting = Instant::now();
            let input = tokio::select! {
                input = rx.recv() => input,
                _ = self.cancel.cancelled() => None,
            };
            self.paused += waiting.elapsed();
            match input {
                // Canceled (or stopped): superseded when the run ends.
                None => return Err((Interrupt::Canceled, None)),
                Some(QuestionInput::Resolved) => break,
                Some(QuestionInput::Discuss) => {
                    if let Err(status) = self.discuss(&request_id, tool_use_id, provider).await {
                        return Err((Interrupt::Stop(status), None));
                    }
                    // Pending again, unless the user answered meanwhile.
                    let reopened = {
                        let mut running = lock(&self.inner.running);
                        match running.get_mut(&self.id).and_then(|r| r.questions.get_mut(&request_id)) {
                            Some(slot) if slot.request.state == QuestionRequestState::Discussing => {
                                slot.request.state = QuestionRequestState::Pending;
                                if let Err(e) = self.inner.store.update_question(&slot.request) {
                                    tracing::error!("failed to record the question state: {e}");
                                }
                                true
                            }
                            _ => false,
                        }
                    };
                    if reopened {
                        self.inner.emit(AgentEvent::QuestionChanged {
                            task_id: self.id.clone(),
                            request_id: request_id.clone(),
                        });
                    }
                    self.set_status(TaskStatus::AwaitingAnswer);
                }
            }
        }
        let resolved = self.remove_question(&request_id);
        self.set_status(TaskStatus::Running);
        match resolved.map(|slot| slot.request) {
            Some(QuestionRequest {
                state: QuestionRequestState::Answered { answers },
                revision,
                questions: final_questions,
                ..
            }) => {
                if let Some(block) = self.question_context(&request_id, revision, &final_questions) {
                    self.context.push(block);
                }
                Ok(ToolOutput::ok(questions::answer_result(&answers)))
            }
            Some(QuestionRequest { state: QuestionRequestState::Superseded { reason }, .. }) => Ok(ToolOutput::error(
                format!("The user did not answer: the question was superseded ({reason}). Nothing was decided."),
            )),
            _ => Ok(ToolOutput::error("The user did not answer the question.")),
        }
    }

    fn remove_question(&self, request_id: &str) -> Option<QuestionSlot> {
        lock(&self.inner.running).get_mut(&self.id).and_then(|r| r.questions.remove(request_id))
    }

    /// The discussion and the final questions, as a text block that goes with
    /// the answer into the history (the discussion itself is not part of
    /// it). `None` when there was neither a discussion nor a revision.
    fn question_context(
        &self,
        request_id: &str,
        revision: u32,
        final_questions: &[questions::Question],
    ) -> Option<Value> {
        let discussion = self.inner.store.discussion(&self.id, request_id).unwrap_or_default();
        let mut lines = Vec::new();
        for (role, content) in &discussion {
            for block in content.iter().filter(|b| block_type(b) == "text") {
                let text = block.get("text").and_then(Value::as_str).unwrap_or_default().trim();
                if !text.is_empty() {
                    lines.push(format!("{}: {text}", if role == "user" { "User" } else { "You" }));
                }
            }
        }
        if lines.is_empty() && revision <= 1 {
            return None;
        }
        let questions = serde_json::to_string(final_questions).unwrap_or_default();
        let text = format!(
            "{QUESTION_CONTEXT} request=\"{request_id}\" revision=\"{revision}\">\nThe user discussed this question \
             before answering; the answer above refers to these questions (revision {revision}):\n{}\nDiscussion:\n{}\n\
             </question_discussion>",
            bounded(&questions, 8_000),
            bounded(&lines.join("\n"), 16_000)
        );
        Some(json!({"type": "text", "text": text}))
    }

    /// One "Chat about it" turn: at most [`DISCUSSION_REQUESTS`] model
    /// requests, counted in the budgets, with read-only tools and
    /// `revise_question`. `Err` ends the run with that status.
    async fn discuss(&mut self, request_id: &str, ask_id: &str, provider: &dyn Provider) -> Result<(), TaskStatus> {
        let abort = self.cancel.child_token();
        if let Some(slot) = lock(&self.inner.running).get_mut(&self.id).and_then(|r| r.questions.get_mut(request_id)) {
            slot.discussion = Some(abort.clone());
        }
        let outcome = self.discussion_turn(request_id, ask_id, provider, &abort).await;
        if let Some(slot) = lock(&self.inner.running).get_mut(&self.id).and_then(|r| r.questions.get_mut(request_id)) {
            slot.discussion = None;
        }
        outcome
    }

    async fn discussion_turn(
        &mut self,
        request_id: &str,
        ask_id: &str,
        provider: &dyn Provider,
        abort: &CancellationToken,
    ) -> Result<(), TaskStatus> {
        for _ in 0..DISCUSSION_REQUESTS {
            if self.cancel.is_cancelled() {
                return Err(TaskStatus::Canceled);
            }
            if abort.is_cancelled() {
                return Ok(());
            }
            if let Some(status) = self.budget_before_request() {
                return Err(status);
            }
            let messages = match self.discussion_messages(request_id, ask_id, provider.kind()) {
                Ok(messages) => messages,
                Err(message) => {
                    self.notice(
                        NoticeLevel::Warning,
                        &format!("The message about the question was not sent: {message}"),
                    );
                    return Ok(());
                }
            };
            let remaining_output = self.spec.budgets.max_output_tokens.saturating_sub(self.usage.output_tokens);
            let max_tokens = remaining_output.min(u64::from(turn_output_limit(provider))) as u32;
            let request = ProviderRequest {
                system: prompt::SYSTEM_PROMPT.to_owned(),
                tools: tools::discussion_definitions(),
                messages,
                max_tokens,
            };
            let turn = self.usage.turns + 1;
            self.inner.emit(AgentEvent::StreamStarted { task_id: self.id.clone(), turn });
            let deltas = self.deltas(turn);
            let sink = |event: StreamEvent| deltas.push(event);
            // `abort` is a child of the task's token: Cancel ends it too.
            let result = tokio::select! {
                r = deltas.drive(provider.stream(&request, &sink, abort)) => Some(r),
                _ = tokio::time::sleep(self.remaining_wall()) => None,
            };
            deltas.flush();
            let response = match result {
                None => {
                    return Err(budget_exceeded(
                        BudgetKind::WallTime,
                        "the time budget ran out while the model was replying about a question.",
                    ));
                }
                Some(Err(ProviderError::Canceled)) if self.cancel.is_cancelled() => return Err(TaskStatus::Canceled),
                // The user answered meanwhile.
                Some(Err(ProviderError::Canceled)) => return Ok(()),
                Some(Err(e)) => {
                    self.notice(
                        NoticeLevel::Warning,
                        &format!("The reply about the question failed: {e} The question still waits for your answer."),
                    );
                    return Ok(());
                }
                Some(Ok(response)) => response,
            };
            self.usage.turns += 1;
            self.usage.input_tokens += response.usage.input_tokens;
            self.usage.output_tokens += response.usage.output_tokens;
            self.usage.cache_read_tokens += response.usage.cache_read_tokens;
            self.usage.cache_write_tokens += response.usage.cache_write_tokens;
            self.usage.served_by = Some(response.model.clone());
            self.set_status(TaskStatus::AwaitingAnswer);
            if response.stop_reason == "refusal" {
                self.notice(
                    NoticeLevel::Warning,
                    "The model provider declined to reply about the question. It still waits for your answer.",
                );
                return Ok(());
            }
            let content = sanitize_after_fallback(response.content);
            if content.is_empty() {
                return Ok(());
            }
            if let Err(e) = self.inner.store.append_discussion(&self.id, request_id, "assistant", &content) {
                return Err(TaskStatus::Failed { message: e.to_string() });
            }
            self.inner.emit(AgentEvent::TranscriptChanged { task_id: self.id.clone() });
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
            if calls.is_empty() {
                return Ok(());
            }
            // Every call gets a result in the discussion (run or not), so the
            // next message about the question has a complete history.
            let ran = response.stop_reason == "tool_use";
            let skipped = ToolOutput::error(format!(
                "{NOT_EXECUTED} {}",
                skip_reason(&sanitize_stop_reason(&response.stop_reason))
            ));
            let mut results = Vec::with_capacity(calls.len());
            for (tool_use_id, name, input) in &calls {
                let output = if ran {
                    self.record_call(tool_use_id, turn, name, input, ToolCallStatus::Running, None);
                    self.discussion_tool(request_id, tool_use_id, name, input, &response.invalid_tool_inputs).await
                } else {
                    skipped.clone()
                };
                let output = self.with_authorization(tool_use_id, output);
                self.record_call(tool_use_id, turn, name, input, finished_status(&output), Some(&output));
                results.push(tool_result(tool_use_id, &output.text(), output.is_error));
            }
            if let Err(e) = self.inner.store.append_discussion(&self.id, request_id, "user", &results) {
                return Err(TaskStatus::Failed { message: e.to_string() });
            }
            self.set_status(TaskStatus::AwaitingAnswer);
            self.inner.emit(AgentEvent::TranscriptChanged { task_id: self.id.clone() });
            if !ran {
                return Ok(());
            }
        }
        Ok(())
    }

    /// The history a discussion request sends: the task's history (ending
    /// with the response that asked), held results for that response's calls
    /// (the question's own says it waits and shows its current revision),
    /// then the discussion so far.
    fn discussion_messages(
        &self,
        request_id: &str,
        ask_id: &str,
        kind: ProviderKind,
    ) -> std::result::Result<Vec<ProviderMessage>, String> {
        let mut messages = self.inner.store.messages(&self.id).map_err(|e| e.to_string())?;
        let request = lock(&self.inner.running)
            .get(&self.id)
            .and_then(|r| r.questions.get(request_id))
            .map(|slot| slot.request.clone())
            .ok_or("the question is no longer open")?;
        let batch: Vec<String> = match messages.last() {
            Some((role, content)) if role == "assistant" => content
                .iter()
                .filter(|b| block_type(b) == "tool_use")
                .map(|b| b.get("id").and_then(Value::as_str).unwrap_or_default().to_owned())
                .collect(),
            _ => return Err("the response that asked the question is not the latest one".into()),
        };
        let held: Vec<Value> = batch
            .iter()
            .map(|id| {
                if id == ask_id {
                    let waiting = json!({
                        "status": "awaitingAnswer",
                        "request": request.id,
                        "revision": request.revision,
                        "questions": request.questions,
                        "note": "The user has not answered yet. They want to discuss this question with you first; \
                                 their messages follow. Reply to them: explain, compare the options, clarify. You may \
                                 read project files and the graph (list_files, read_file, graph_query) and call \
                                 revise_question to change the options, keeping the ids of options you keep. You \
                                 cannot answer the question yourself, change anything, or run anything until the \
                                 user answers; the other calls of your previous response stay held until then.",
                    });
                    tool_result(id, &waiting.to_string(), false)
                } else {
                    tool_result(id, "Held: not run yet. It runs only after the user answers the question.", false)
                }
            })
            .collect();
        messages.push(("user".to_owned(), held));
        for (role, content) in self.inner.store.discussion(&self.id, request_id).map_err(|e| e.to_string())? {
            match messages.last_mut() {
                Some((last, blocks)) if *last == role => blocks.extend(content),
                _ => messages.push((role, content)),
            }
        }
        self.inner.provider_messages(kind, messages, &mut 0)
    }

    /// A tool call of a discussion turn: read-only tools under the gate, and
    /// `revise_question`; everything else is refused (the batch stays held).
    async fn discussion_tool(
        &mut self,
        request_id: &str,
        tool_use_id: &str,
        name: &str,
        input: &Value,
        invalid: &HashMap<String, String>,
    ) -> ToolOutput {
        if invalid.contains_key(tool_use_id) {
            return ToolOutput::error("The tool input was not valid JSON, so the tool was not run.");
        }
        if self.usage.tool_calls >= self.spec.budgets.max_tool_calls {
            return ToolOutput::error(format!("{NOT_EXECUTED} the task's tool-call budget is used up."));
        }
        self.usage.tool_calls += 1;
        if name == tools::REVISE_QUESTION {
            return self.revise_question(request_id, input);
        }
        if !matches!(name, tools::LIST_FILES | tools::READ_FILE | tools::GRAPH_QUERY) {
            return ToolOutput::error(format!(
                "{NOT_EXECUTED} {name} is not available while the user discusses a question: until they answer, only \
                 list_files, read_file, graph_query, and revise_question can be used, and the other calls of the \
                 response that asked stay held."
            ));
        }
        let call = match ToolCall::parse(name, input) {
            Ok(call) => call,
            Err(message) => return ToolOutput::error(message),
        };
        self.settle_graph(&call).await;
        let gate = {
            let env = self.env();
            exec::gate(&call, &env)
        };
        let authorization = match gate {
            Gate::Allowed(authorization) => authorization,
            Gate::Refused(output) => return output,
            Gate::NeedsApproval { .. } => {
                return ToolOutput::error(format!(
                    "{NOT_EXECUTED} it would need the user's approval, which is not asked while they discuss a \
                     question."
                ));
            }
        };
        self.authorized.insert(tool_use_id.to_owned(), authorization.authorized_by().clone());
        let inner = Arc::clone(self.inner);
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
        exec::run(call, &env, &mut self.seen, tool_use_id, authorization, self.cancel.child_token()).await
    }

    /// `revise_question`: new questions for the request being discussed;
    /// the revision moves on so stale answers are refused.
    fn revise_question(&self, request_id: &str, input: &Value) -> ToolOutput {
        let mut running = lock(&self.inner.running);
        let Some(slot) = running.get_mut(&self.id).and_then(|r| r.questions.get_mut(request_id)) else {
            return ToolOutput::error("The question is no longer open.");
        };
        let allow_custom = slot.request.questions.first().is_none_or(|q| q.allow_custom);
        let (named, revised) = match questions::parse_revise(input, allow_custom) {
            Ok(parsed) => parsed,
            Err(message) => return ToolOutput::error(message),
        };
        if named != request_id {
            return ToolOutput::error(format!(
                "revise_question can only revise the question being discussed (request {request_id})."
            ));
        }
        if slot.request.state != QuestionRequestState::Discussing {
            return ToolOutput::error("The user already answered; the question can no longer be revised.");
        }
        let mut next = slot.request.clone();
        next.questions = revised;
        next.revision += 1;
        if let Err(e) = self.inner.store.update_question(&next) {
            return ToolOutput::error(format!("The revision could not be recorded: {e}"));
        }
        slot.request = next;
        let revision = slot.request.revision;
        drop(running);
        self.inner.emit(AgentEvent::QuestionChanged { task_id: self.id.clone(), request_id: request_id.to_owned() });
        ToolOutput::ok(json!({
            "revised": true,
            "request": request_id,
            "revision": revision,
            "note": "The user now sees the revised question and answers when ready; you cannot answer it yourself.",
        }))
    }

    /// Add how a call that ran was authorized to its result, so the model
    /// describes approvals as they were (a call that did not run has none).
    fn with_authorization(&self, tool_use_id: &str, mut output: ToolOutput) -> ToolOutput {
        let granted = self.granted.get(tool_use_id).map(Vec::as_slice).unwrap_or_default();
        if let Some(by) = self.authorized.get(tool_use_id)
            && let Some(note) = prompt::authorization_note(by, granted)
            && let Some(obj) = output.content.as_object_mut()
        {
            obj.insert("authorization".into(), json!(note));
        }
        output
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
    fn output_budget_stop_says_used_up_only_when_nothing_remains() {
        assert_eq!(output_budget_detail(1024, 0), "the output-token budget (1024) was used up.");
        assert_eq!(
            output_budget_detail(1024, 973),
            "973 output tokens of the budget (1024) remain, fewer than one more model request needs (1024)."
        );
    }

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

    fn image(id: &str, side: u32) -> AttachmentInfo {
        AttachmentInfo {
            id: AttachmentId(id.into()),
            name: format!("{id}.png"),
            media_type: "image/png".into(),
            kind: AttachmentKind::Image,
            size_bytes: 10,
            width: Some(side),
            height: Some(side),
            sha256: String::new(),
            missing: false,
        }
    }

    fn choice(provider: ProviderKind, model: &str, effort: Effort) -> ProviderChoice {
        ProviderChoice { provider, model: model.into(), effort }
    }

    #[test]
    fn preflight_uses_catalog_evidence_and_anthropic_image_rules() {
        let claude = choice(ProviderKind::Anthropic, "claude-opus-5-5", Effort::High);
        let pdf = AttachmentInfo { kind: AttachmentKind::Pdf, ..image("p", 0) };
        assert!(preflight(&claude, std::slice::from_ref(&pdf), None).is_ok());
        // The connection's catalog says this model reads no PDFs: refused.
        let mut known = catalog::saved_descriptor(ProviderKind::Anthropic, "claude-opus-5-5");
        known.capabilities.pdf = Support::No;
        assert_eq!(preflight(&claude, &[pdf], Some(&known)).unwrap_err().code, "agents.attachmentUnsupported");
        // More than 20 images: each at most 2000 px.
        let small: Vec<AttachmentInfo> = (0..21).map(|i| image(&format!("s{i}"), 1500)).collect();
        assert!(preflight(&claude, &small, None).is_ok());
        let mut large = small.clone();
        large[3] = image("big", 2400);
        assert_eq!(preflight(&claude, &large, None).unwrap_err().code, "attachments.tooManyLargeImages");
        assert!(preflight(&claude, &large[..20], None).is_ok(), "20 images may be larger");
        // Earlier non-reasoning OpenAI models: no effort, smaller output limit.
        let gpt41 = preflight(&choice(ProviderKind::OpenAi, "gpt-4.1", Effort::High), &[], None).unwrap();
        assert_eq!(gpt41, Resolved { effort: None, max_output_tokens: Some(16_384) });
    }

    #[test]
    fn attachment_limits_fit_one_request() {
        // A file the ingestion accepts always fits the request limit once encoded.
        for limit in [att::MAX_PDF_BYTES, att::MAX_IMAGE_BYTES, att::MAX_TEXT_BYTES] {
            assert!(encoded_size(limit) <= att::MAX_REQUEST_BYTES, "{limit}");
        }
        assert!(encoded_size(att::MAX_IMAGE_BYTES) <= 5 * 1024 * 1024, "an image's base64 stays within 5 MiB");
    }

    #[test]
    fn attachments_in_a_send_survive_a_concurrent_discard() {
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
        let IngestOutcome::Added { attachment } =
            service.add_attachment_bytes(Some("a.txt"), None, &att::base64_encode(b"hello\n"))
        else {
            panic!("not added")
        };
        {
            let _sending = service.inner.reserve(std::slice::from_ref(&attachment));
            service.discard_attachments(std::slice::from_ref(&attachment.id)).unwrap();
            assert!(service.inner.attachment(&attachment.id).is_ok(), "kept while its send is in progress");
        }
        service.discard_attachments(std::slice::from_ref(&attachment.id)).unwrap();
        assert!(service.inner.attachment(&attachment.id).is_err(), "a draft removed afterwards is forgotten");
    }
}
