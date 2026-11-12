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

/// How incomplete material was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum SorrySource {
    /// Lean warned "declaration uses `sorry`".
    Warning,
    /// The axiom audit found `sorryAx` among the declaration's axioms: the
    /// `sorry` is imported, reached through another declaration, or its warning
    /// was suppressed (`#guard_msgs`, `warn.sorry false`).
    AxiomAudit,
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
    pub source: SorrySource,
}

/// A declaration of the checked file and the axioms beyond Lean's standard
/// ones (`propext`, `Classical.choice`, `Quot.sound`) that it depends on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct AxiomUse {
    pub declaration: String,
    /// Range of the declaration name in the checked text, when known.
    pub range: Option<TextRange>,
    pub line: Option<u32>,
    pub axioms: Vec<String>,
}

/// A declaration of a checked file as the audit verified it: re-checked by
/// Lean's kernel in the audit's own elaboration of the text, its type printed
/// by a process that runs none of the file's code (the toolchain's notation,
/// none of the project's).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct CheckedDeclaration {
    pub name: String,
    /// `theorem`, `def`, `axiom`, `opaque`, `inductive`, `constructor`, `recursor`.
    pub kind: String,
    /// The declaration's type (its statement, for a theorem).
    pub statement: String,
    /// Range of the declaration name in the checked text; `None` for
    /// declarations Lean generated (equation lemmas, recursors, ...).
    pub range: Option<TextRange>,
    pub line: Option<u32>,
    /// The (standard) axioms it depends on.
    pub axioms: Vec<String>,
}

/// At most this many declarations are listed in [`LeanStatus::Checked`].
pub const CHECKED_DECLARATIONS_SHOWN: usize = 300;

/// What the axiom audit says about one declaration, from the axioms that
/// declaration itself depends on (the audit's per-declaration granularity).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum DeclarationVerdict {
    /// Kernel-checked in the audit's elaboration; only Lean's standard axioms.
    Checked,
    /// Depends on axioms beyond the standard ones.
    CheckedWithAxioms,
    /// Depends on `sorry`.
    Incomplete,
    /// Trusts compiled code instead of the kernel.
    ReliesOnCompiledCode,
}

/// A declaration as the axiom audit reported it, with its own verdict.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct AuditedDeclaration {
    pub name: String,
    pub kind: String,
    /// The type the audit printed (the toolchain's notation only).
    pub statement: String,
    pub range: Option<TextRange>,
    pub line: Option<u32>,
    /// Every axiom it depends on.
    pub axioms: Vec<String>,
    pub verdict: DeclarationVerdict,
}

/// At most this many declarations are kept in a [`LeanAuditRecord`].
pub const AUDIT_RECORD_LIMIT: usize = 2000;

/// The axiom audit of one finished revision of a document, kept by the Lean
/// service so formal evidence is recorded from what the audit actually
/// reported (never from data supplied by a view). Kept after the document
/// changes: it describes exactly `revision` / `content_hash`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct LeanAuditRecord {
    pub document: DocumentId,
    pub path: WorkspacePath,
    /// The revision audited.
    pub revision: u64,
    /// SHA-256 (hex) of the exact UTF-8 text audited.
    pub content_hash: String,
    /// Exact toolchain, e.g. `leanprover/lean4:v4.35.0`.
    pub toolchain: String,
    /// `lean --version` of that toolchain, when probed.
    pub toolchain_version: Option<String>,
    /// The final status Lean and the audit gave `revision`.
    pub status: LeanStatus,
    /// Declarations the audit reported (written ones first, in text order).
    pub declarations: Vec<AuditedDeclaration>,
    /// Declarations the audit reported in total.
    pub declaration_count: u32,
    /// More declarations were audited than are kept here; a declaration not
    /// listed has no evidence of its own.
    pub truncated: bool,
    pub finished_at_ms: u64,
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
    /// still being elaborated (UTF-16, over that revision's text). `auditing`:
    /// Lean reported no errors and the axiom audit is running.
    Checking { revision: u64, toolchain: String, starting: bool, auditing: bool, processing: Vec<TextRange> },
    /// A checked proof: Lean elaborated all of `revision` with no errors, no
    /// `sorry` (also not through imports or suppressed warnings), the kernel
    /// checked it, and its declarations use only the standard axioms listed.
    /// `declarations` is what was verified (those written in the text first;
    /// at most [`CHECKED_DECLARATIONS_SHOWN`]), out of `declaration_count`.
    Checked {
        revision: u64,
        toolchain: String,
        warnings: u32,
        axioms: Vec<String>,
        declarations: Vec<CheckedDeclaration>,
        declaration_count: u32,
    },
    /// Incomplete: no errors, but `count` declarations depend on `sorry`
    /// (directly, through an import, or with the warning suppressed).
    CheckedWithSorry { revision: u64, toolchain: String, count: u32, locations: Vec<SorryLocation>, warnings: u32 },
    /// No errors, but declarations depend on axioms beyond the standard ones
    /// (e.g. a user `axiom`). Not a checked proof of the stated results alone.
    CheckedWithAxioms {
        revision: u64,
        toolchain: String,
        axioms: Vec<String>,
        declarations: Vec<AxiomUse>,
        warnings: u32,
    },
    /// No errors, but declarations trust compiled code instead of the kernel
    /// (`native_decide`, `Lean.ofReduceBool`, `Lean.trustCompiler`).
    ReliesOnCompiledCode {
        revision: u64,
        toolchain: String,
        axioms: Vec<String>,
        declarations: Vec<AxiomUse>,
        warnings: u32,
    },
    /// Elaboration stopped before the end of the file (e.g. `#exit`); the rest
    /// was never checked. `line`: zero-based line where it stopped, if known.
    PartiallyChecked { revision: u64, toolchain: String, line: Option<u32>, reason: String },
    /// Lean reported no errors, but the result cannot be trusted as checked:
    /// kernel checking was disabled, or the axiom audit could not run.
    Unverified { revision: u64, toolchain: String, reason: String },
    /// Lean finished `revision` and reported errors.
    Errors { revision: u64, toolchain: String, count: u32, sorry_count: u32 },
    /// The language server could not be started (e.g. too many interactive
    /// sessions are running); it is retried automatically and on Restart.
    ServerUnavailable { reason: String },
    /// The language server stopped unexpectedly.
    ServerCrashed { message: String, restarting: bool, attempts: u32 },
}

impl LeanStatus {
    /// True for statuses that describe a finished check of a revision.
    pub fn finished_revision(&self) -> Option<u64> {
        match self {
            LeanStatus::Checked { revision, .. }
            | LeanStatus::CheckedWithSorry { revision, .. }
            | LeanStatus::CheckedWithAxioms { revision, .. }
            | LeanStatus::ReliesOnCompiledCode { revision, .. }
            | LeanStatus::PartiallyChecked { revision, .. }
            | LeanStatus::Unverified { revision, .. }
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
    Running {
        job: JobId,
        pid: Option<u32>,
    },
    Crashed {
        message: String,
        attempts: u32,
        restarting: bool,
    },
    Untrusted {
        workspace_root: String,
    },
    /// Could not start (e.g. too many interactive sessions); retried.
    Unavailable {
        reason: String,
    },
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
    /// True only for a checked proof (`LeanStatus::Checked`): Lean elaborated
    /// exactly this text completely, with no errors, no `sorry` (including
    /// imported or suppressed), kernel checking enabled, and only the standard
    /// axioms.
    pub checked: bool,
    pub diagnostics: Vec<Diagnostic>,
    pub sorries: Vec<SorryLocation>,
    /// Every axiom the file's declarations depend on (from the axiom audit).
    pub axioms: Vec<String>,
    /// Declarations depending on anything beyond the standard axioms.
    pub axiom_uses: Vec<AxiomUse>,
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
