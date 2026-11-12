//! Lean IPC and agent-facing types (exported to TypeScript by ts-rs).
//!
//! Checking status is always tied to the exact document revision and Lean
//! toolchain it was computed for. A checked proof, an incomplete proof
//! (`sorry`/`admit`), and a proof with errors are distinct statuses; none of
//! them is ever inferred from a build success or a generated draft.

use exposition_core::build::Diagnostic;
use exposition_core::document::DocumentId;
use exposition_core::job::JobId;
use exposition_core::path::WorkspacePath;
use exposition_core::text::TextRange;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The exact toolchain a project is checked with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct LeanToolchainInfo {
    /// As requested by `lean-toolchain` (or elan's default), e.g. `leanprover/lean4:v4.35.0`.
    pub requested: String,
    /// Exact toolchain used.
    pub name: String,
    /// `lean --version` output of that toolchain, once probed.
    pub version: Option<String>,
    /// Where the request came from: a `lean-toolchain` path or "elan default".
    pub source: String,
    /// Why `name` differs from `requested` (channel resolution), if it does.
    pub note: Option<String>,
}

/// Where a declaration is incomplete.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SorryLocation {
    /// Range Lean reports for the declaration (usually its name).
    pub range: TextRange,
    /// Zero-based line of `range.start`.
    pub line: u32,
    /// Declaration name, when the range covers one.
    pub declaration: Option<String>,
    /// The `sorry`/`admit` tokens inside the declaration, when Lean reported them.
    pub tokens: Vec<TextRange>,
}

/// Checking status of one Lean document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum LeanStatus {
    /// Tracked, but checking has not begun.
    NotStarted,
    /// Checking is not possible here (e.g. not inside the open workspace).
    Unavailable { reason: String },
    /// The project's toolchain is not installed. `install` is the toolchain an
    /// explicit Install action would install (nothing is installed implicitly).
    ToolchainMissing { toolchain: String, install: Option<String>, guidance: String },
    /// Checking runs project code (lakefile, `#eval`, macros); the workspace is not trusted.
    Untrusted { workspace_root: String },
    /// The server is processing `revision`. `processing` lists the ranges
    /// still being elaborated (UTF-16, over that revision's text).
    Checking { revision: u64, toolchain: String, starting: bool, processing: Vec<TextRange> },
    /// Lean finished `revision` with no errors and no `sorry`.
    Checked { revision: u64, toolchain: String, warnings: u32 },
    /// Lean finished `revision` with no errors, but `count` declarations use
    /// `sorry` (incomplete material).
    CheckedWithSorry { revision: u64, toolchain: String, count: u32, locations: Vec<SorryLocation>, warnings: u32 },
    /// Lean finished `revision` and reported errors.
    Errors { revision: u64, toolchain: String, count: u32, sorry_count: u32 },
    /// The language server stopped unexpectedly.
    ServerCrashed { message: String, restarting: bool, attempts: u32 },
}

impl LeanStatus {
    /// True for statuses that describe a finished check of a revision.
    pub fn finished_revision(&self) -> Option<u64> {
        match self {
            LeanStatus::Checked { revision, .. }
            | LeanStatus::CheckedWithSorry { revision, .. }
            | LeanStatus::Errors { revision, .. } => Some(*revision),
            _ => None,
        }
    }

    /// True when no further progress happens without a change or user action.
    pub fn is_settled(&self) -> bool {
        !matches!(self, LeanStatus::Checking { .. } | LeanStatus::NotStarted)
            && !matches!(self, LeanStatus::ServerCrashed { restarting: true, .. })
    }
}

/// Everything the workbench shows for one open Lean document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct LeanDocumentState {
    pub document: DocumentId,
    pub path: WorkspacePath,
    /// Current revision of the document.
    pub revision: u64,
    /// Absolute root of the Lake project (or workspace, for standalone files).
    pub project_root: Option<String>,
    pub toolchain: Option<LeanToolchainInfo>,
    pub status: LeanStatus,
    /// Diagnostics reported for `diagnostics_revision` (partial while checking).
    pub diagnostics: Vec<Diagnostic>,
    pub diagnostics_revision: Option<u64>,
    /// The language-server job serving this document.
    pub job: Option<JobId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum LeanProjectKind {
    Lake { lakefile: String },
    Standalone,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum LeanToolchainState {
    Installed { info: LeanToolchainInfo },
    Missing { requested: String, install: String, guidance: String },
    Invalid { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum LeanServerState {
    Stopped,
    Starting,
    Running { job: JobId, pid: Option<u32> },
    Crashed { message: String, attempts: u32, restarting: bool },
    Untrusted { workspace_root: String },
}

/// A Lean project found in the workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct LeanProjectInfo {
    /// Absolute project root.
    pub root: String,
    /// Root relative to the workspace (`None` for the workspace root itself).
    pub path: Option<WorkspacePath>,
    pub kind: LeanProjectKind,
    pub toolchain: LeanToolchainState,
    pub server: LeanServerState,
}

/// Expected type of the term at a position (`$/lean/plainTermGoal`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct LeanTermGoal {
    pub goal: String,
    pub range: Option<TextRange>,
}

/// Goals at a position, as plain text (hypotheses, then `⊢ target`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct LeanGoals {
    pub document: DocumentId,
    pub revision: u64,
    pub offset: u32,
    /// Tactic goals: `None` outside a tactic proof, empty when no goals remain.
    pub goals: Option<Vec<String>>,
    pub term_goal: Option<LeanTermGoal>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct LeanHover {
    /// Markdown from the server. Render as untrusted text (no HTML, no commands).
    pub markdown: String,
    pub range: Option<TextRange>,
}

/// A definition target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct LeanLocation {
    /// Workspace path when the target is inside the workspace.
    pub path: Option<WorkspacePath>,
    /// Absolute file path (e.g. toolchain sources for library definitions).
    pub file: String,
    /// Zero-based line and UTF-16 column of the target start.
    pub line: u32,
    pub character: u32,
    /// UTF-16 range over the target's current text, when it could be read.
    pub range: Option<TextRange>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum LeanCheckTarget {
    /// An open document (its unsaved buffer is what is checked).
    Document { document: DocumentId },
    /// A workspace file: the open buffer if the file is open, otherwise the file on disk.
    Path { path: WorkspacePath },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct LeanCheckRequest {
    pub target: LeanCheckTarget,
    /// UTF-16 offsets at which to report goals after checking.
    #[serde(default)]
    pub goals_at: Vec<u32>,
    /// How long to wait for Lean (default 120 s, at most 600 s).
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct LeanCheckGoals {
    pub offset: u32,
    pub goals: Option<Vec<String>>,
    pub term_goal: Option<LeanTermGoal>,
    pub error: Option<String>,
}

/// Result of [`crate::LeanService::check`]: the agent `check_lean` seam.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct LeanCheckReport {
    pub path: WorkspacePath,
    /// The open document checked, if the text came from a buffer.
    pub document: Option<DocumentId>,
    /// Document revision checked (`None` when the file was read from disk).
    pub revision: Option<u64>,
    /// SHA-256 of the exact UTF-8 text sent to Lean.
    pub content_hash: String,
    pub project_root: Option<String>,
    pub toolchain: Option<LeanToolchainInfo>,
    pub status: LeanStatus,
    /// True only if Lean finished exactly this text without errors and without `sorry`.
    pub checked: bool,
    pub diagnostics: Vec<Diagnostic>,
    pub sorries: Vec<SorryLocation>,
    pub goals: Vec<LeanCheckGoals>,
    pub duration_ms: u64,
}

/// Lean events, forwarded to the webview as `lean:event`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum LeanEvent {
    /// A document's status or diagnostics changed.
    Document { state: Box<LeanDocumentState> },
    /// A document is no longer tracked (closed or no longer a Lean file).
    Removed { document: DocumentId },
}

/// Error for Lean requests: a stable code and a message safe to show.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS, thiserror::Error)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
#[error("{code}: {message}")]
pub struct LeanError {
    pub code: String,
    pub message: String,
    /// The document's current revision, for `lean.stale`.
    pub current_revision: Option<u64>,
}

impl LeanError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self { code: code.to_owned(), message: message.into(), current_revision: None }
    }
}
