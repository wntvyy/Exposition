//! Agent task contracts shared with the frontend (exported to `src/bindings`).
//!
//! A task is one user request run by the in-app task loop. Its scope,
//! permission profile, execution mode, context consent, and budgets are fixed
//! when it is created and enforced by the application, never by model prose.

use exposition_core::path::WorkspacePath;
use exposition_core::text::TextRange;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TaskId(pub String);

impl TaskId {
    pub fn new() -> Self {
        Self(format!("task-{}", uuid::Uuid::new_v4()))
    }
}

impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What the agent may read and change. Tools refuse paths outside the scope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum TaskScope {
    /// Every file under the project root.
    Workspace,
    /// The listed files, and everything under listed directories.
    Files { paths: Vec<WorkspacePath> },
    /// One document (typically the active editor's).
    Document { path: WorkspacePath },
    /// A selection in one open document. The agent may read and edit that
    /// document; the selected text is the focus of the request.
    Selection { path: WorkspacePath, range: TextRange, revision: u64 },
}

impl TaskScope {
    /// Whether `path` lies inside the scope.
    pub fn allows(&self, path: &WorkspacePath) -> bool {
        match self {
            TaskScope::Workspace => true,
            TaskScope::Files { paths } => paths.iter().any(|allowed| is_same_or_under(path, allowed)),
            TaskScope::Document { path: allowed } | TaskScope::Selection { path: allowed, .. } => path == allowed,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            TaskScope::Workspace => "the whole project".into(),
            TaskScope::Files { paths } => {
                let list: Vec<&str> = paths.iter().map(WorkspacePath::as_str).collect();
                format!("only these files/directories: {}", list.join(", "))
            }
            TaskScope::Document { path } => format!("only the document {path}"),
            TaskScope::Selection { path, range, revision } => format!(
                "only the document {path}, focused on the selection at UTF-16 offsets {}..{} (revision {revision})",
                range.start, range.end
            ),
        }
    }
}

fn is_same_or_under(path: &WorkspacePath, allowed: &WorkspacePath) -> bool {
    let p = path.as_str();
    let a = allowed.as_str();
    p == a || (p.len() > a.len() && p.starts_with(a) && p.as_bytes()[a.len()] == b'/')
}

/// How the agent may change documents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum PermissionProfile {
    /// Reading and listing only; every change is refused.
    ReadOnly,
    /// Each document change waits for the user's approval before it is applied.
    ReviewEdits,
    /// Document changes apply immediately to open buffers (unsaved) and stay
    /// reviewable/revertible afterwards.
    AutoApply,
}

/// How agent-requested processes (commands, Python, Lean, builds) may run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum ExecutionMode {
    /// No process execution.
    Disabled,
    /// Trusted native execution; every launch needs the user's explicit approval.
    ApproveEach,
    /// Unattended execution, only inside the execution service's tested
    /// restricted (container) backend. Refused when that backend is unavailable;
    /// never downgraded to native execution.
    Restricted,
}

/// What document content may be sent to the model provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum ContextConsent {
    /// Only the request, file names, and metadata; reading file content is refused.
    MetadataOnly,
    /// Content of files inside the task scope may be sent when the agent reads them.
    ScopeContent,
}

/// Hard limits enforced by the task loop.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Budgets {
    /// Model requests per run.
    pub max_turns: u32,
    /// Output tokens (thinking included) across the run.
    pub max_output_tokens: u64,
    /// Input tokens (cached reads and writes included) across the run.
    pub max_input_tokens: u64,
    /// Wall-clock time, excluding time spent waiting for the user's approval.
    pub max_wall_ms: u64,
    /// Tool calls executed.
    pub max_tool_calls: u32,
    /// Process launches (commands, Python, Lean, builds).
    pub max_commands: u32,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            max_turns: 40,
            max_output_tokens: 400_000,
            max_input_tokens: 4_000_000,
            max_wall_ms: 30 * 60 * 1000,
            max_tool_calls: 150,
            max_commands: 20,
        }
    }
}

impl Budgets {
    /// Upper bounds accepted from the webview.
    pub fn validate(&self) -> Result<(), String> {
        let checks: [(&str, u64, u64, u64); 6] = [
            ("maxTurns", self.max_turns.into(), 1, 500),
            ("maxOutputTokens", self.max_output_tokens, 1_024, 20_000_000),
            ("maxInputTokens", self.max_input_tokens, 1_024, 200_000_000),
            ("maxWallMs", self.max_wall_ms, 1_000, 24 * 60 * 60 * 1000),
            ("maxToolCalls", self.max_tool_calls.into(), 0, 5_000),
            ("maxCommands", self.max_commands.into(), 0, 1_000),
        ];
        for (name, value, min, max) in checks {
            if value < min || value > max {
                return Err(format!("{name} must be between {min} and {max}"));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum BudgetKind {
    Turns,
    OutputTokens,
    InputTokens,
    WallTime,
    ToolCalls,
    Commands,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum TaskStatus {
    Queued,
    Running,
    AwaitingApproval,
    Completed,
    Canceled,
    BudgetExceeded {
        budget: BudgetKind,
        detail: String,
    },
    /// The provider declined the request (`stop_reason: "refusal"`).
    Refused {
        category: Option<String>,
        explanation: Option<String>,
    },
    Failed {
        message: String,
    },
    /// The application stopped while the task was running.
    Interrupted,
}

impl TaskStatus {
    pub fn is_active(&self) -> bool {
        matches!(self, TaskStatus::Queued | TaskStatus::Running | TaskStatus::AwaitingApproval)
    }
}

/// Counters for budget meters and evidence.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TaskUsage {
    pub turns: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub tool_calls: u32,
    pub commands: u32,
    /// Wall time spent running (approval waits excluded), across runs.
    pub elapsed_ms: u64,
    /// The model that served the latest turn (differs from the configured model
    /// after a server-side fallback).
    pub served_by: Option<String>,
}

impl TaskUsage {
    /// Input tokens processed, including cache reads and writes.
    pub fn total_input(&self) -> u64 {
        self.input_tokens + self.cache_read_tokens + self.cache_write_tokens
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum ProviderKind {
    /// Anthropic Claude Messages API.
    Anthropic,
    /// Deterministic scripted provider. Development builds only; never
    /// selectable in release builds.
    Scripted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Xhigh => "xhigh",
            Effort::Max => "max",
        }
    }
}

/// Provider configuration chosen for a task (recorded with the task).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ProviderChoice {
    pub provider: ProviderKind,
    pub model: String,
    pub effort: Effort,
}

/// Persisted provider settings (never includes the API key).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSettings {
    pub provider: ProviderKind,
    pub model: String,
    pub effort: Effort,
    /// Default consent for new tasks.
    pub consent: ContextConsent,
}

pub const DEFAULT_MODEL: &str = "claude-opus-5-5";

impl Default for ProviderSettings {
    fn default() -> Self {
        Self {
            provider: ProviderKind::Anthropic,
            model: DEFAULT_MODEL.into(),
            effort: Effort::Medium,
            consent: ContextConsent::ScopeContent,
        }
    }
}

/// What the webview may learn about the provider: never the key itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ProviderStatus {
    pub settings: ProviderSettings,
    /// An API key is stored in OS credential storage.
    pub configured: bool,
    /// Human-readable name of the credential store.
    pub credential_store: String,
    /// Credential storage could not be read (message is safe to show).
    pub credential_error: Option<String>,
    /// The development-only scripted provider can be selected (debug builds).
    pub scripted_available: bool,
}

/// Everything fixed at task creation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TaskSpec {
    pub prompt: String,
    /// Canonical absolute project root shown to the user.
    pub workspace_root: String,
    pub scope: TaskScope,
    pub permission: PermissionProfile,
    pub execution: ExecutionMode,
    pub consent: ContextConsent,
    pub budgets: Budgets,
    pub provider: ProviderChoice,
}

/// Request from the composer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct CreateTaskRequest {
    pub prompt: String,
    pub workspace_root: String,
    pub scope: TaskScope,
    pub permission: PermissionProfile,
    pub execution: ExecutionMode,
    /// `None` uses the provider settings default.
    pub consent: Option<ContextConsent>,
    /// `None` uses the default budgets.
    pub budgets: Option<Budgets>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TaskSummary {
    pub id: TaskId,
    pub title: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub status: TaskStatus,
    pub spec: TaskSpec,
    pub usage: TaskUsage,
    /// Files with agent changes still awaiting accept/revert.
    pub pending_review: u32,
    /// Files with any recorded agent change.
    pub changed_files: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum ToolCallStatus {
    Running,
    AwaitingApproval,
    Succeeded,
    Failed,
    /// Not executed (canceled, budget exhausted, or an earlier stop).
    NotRun,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum NoticeLevel {
    Info,
    Warning,
    Error,
}

/// One rendered transcript entry, in order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum TranscriptItem {
    UserText {
        seq: u64,
        text: String,
    },
    AssistantText {
        seq: u64,
        text: String,
    },
    /// Summarized reasoning or progress notes returned by the provider.
    Thinking {
        seq: u64,
        text: String,
    },
    ToolCall {
        seq: u64,
        tool_use_id: String,
        name: String,
        #[ts(type = "unknown")]
        input: serde_json::Value,
        status: ToolCallStatus,
        /// Result text (bounded).
        result: Option<String>,
        is_error: bool,
    },
    Notice {
        seq: u64,
        level: NoticeLevel,
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequest {
    pub id: String,
    pub task_id: TaskId,
    pub tool_use_id: String,
    pub tool: String,
    pub title: String,
    /// Plain-text details (proposed edits, program and arguments).
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TaskDetail {
    pub summary: TaskSummary,
    pub transcript: Vec<TranscriptItem>,
    pub approvals: Vec<ApprovalRequest>,
}

/// Events on the `agents:event` channel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum AgentEvent {
    /// Status, usage, or review counters changed.
    TaskChanged {
        task: TaskSummary,
    },
    /// Persisted transcript changed; re-read the task detail.
    TranscriptChanged {
        task_id: TaskId,
    },
    /// A model request started streaming.
    StreamStarted {
        task_id: TaskId,
        turn: u32,
    },
    TextDelta {
        task_id: TaskId,
        turn: u32,
        text: String,
    },
    ThinkingDelta {
        task_id: TaskId,
        turn: u32,
        text: String,
    },
    /// A retry discarded partially streamed output.
    StreamReset {
        task_id: TaskId,
        turn: u32,
        reason: String,
    },
    ToolCallStarted {
        task_id: TaskId,
        tool_use_id: String,
        name: String,
    },
    ToolCallFinished {
        task_id: TaskId,
        tool_use_id: String,
        is_error: bool,
    },
    ApprovalRequested {
        approval: ApprovalRequest,
    },
    ApprovalResolved {
        task_id: TaskId,
        approval_id: String,
        approved: bool,
    },
    /// Server-side fallback switched models for this turn.
    Fallback {
        task_id: TaskId,
        from: String,
        to: String,
    },
    ReviewChanged {
        task_id: TaskId,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum FileChangeKind {
    Edited,
    Created,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum ReviewState {
    Pending,
    Accepted,
    Reverted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct FileReview {
    pub path: WorkspacePath,
    pub kind: FileChangeKind,
    pub state: ReviewState,
    pub change_count: u32,
    /// Concurrent edits overlapped the agent's changes while the task ran, so
    /// the agent's net change can no longer be separated automatically.
    pub entangled: bool,
    /// Revision after the task's latest change to this file.
    pub tip_revision: Option<u64>,
    /// Current revision of the open document, if open.
    pub current_revision: Option<u64>,
    /// Outcome of the latest review action (e.g. why a revert was refused).
    pub message: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointDocument {
    pub path: Option<WorkspacePath>,
    pub document: String,
    pub revision: u64,
    pub content_hash: String,
    pub unsaved: bool,
}

/// Recorded before the task's first model request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Checkpoint {
    pub created_at_ms: u64,
    pub documents: Vec<CheckpointDocument>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TaskReview {
    pub task_id: TaskId,
    pub checkpoint: Option<Checkpoint>,
    pub files: Vec<FileReview>,
}

/// Texts for a per-file diff view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct FileDiff {
    pub path: WorkspacePath,
    pub language: String,
    /// The file without this task's changes (concurrent edits included).
    pub original: String,
    /// The file after this task's latest change.
    pub modified: String,
    /// The text before the task first changed the file (evidence).
    pub checkpoint: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ReviewActionResult {
    pub path: WorkspacePath,
    pub ok: bool,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_allows_only_listed_paths() {
        let p = |s: &str| WorkspacePath::parse(s).unwrap();
        let files = TaskScope::Files { paths: vec![p("chapters"), p("main.typ")] };
        assert!(files.allows(&p("main.typ")));
        assert!(files.allows(&p("chapters/intro.typ")));
        assert!(!files.allows(&p("chapters-old/intro.typ")));
        assert!(!files.allows(&p("refs.bib")));
        let doc = TaskScope::Document { path: p("main.typ") };
        assert!(doc.allows(&p("main.typ")));
        assert!(!doc.allows(&p("other.typ")));
        assert!(TaskScope::Workspace.allows(&p("anything/at/all.txt")));
    }

    #[test]
    fn budgets_are_bounded() {
        assert!(Budgets::default().validate().is_ok());
        let bad = Budgets { max_turns: 0, ..Budgets::default() };
        assert!(bad.validate().is_err());
    }
}
