//! Research IPC types. Generated TypeScript bindings live in `src/bindings`.

use exposition_core::artifact::{ArtifactId, ArtifactRecord, RunId, RunRecord, RunStatus, ToolVersion};
use exposition_core::document::DocumentId;
use exposition_core::path::WorkspacePath;
use exposition_core::text::TextRange;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Runtime languages Exposition knows how to discover.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeLanguage {
    Python,
    R,
    Julia,
}

impl RuntimeLanguage {
    pub fn as_str(self) -> &'static str {
        match self {
            RuntimeLanguage::Python => "python",
            RuntimeLanguage::R => "r",
            RuntimeLanguage::Julia => "julia",
        }
    }
}

/// Which kernel a session starts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum KernelChoice {
    /// ipykernel of the host interpreter itself.
    Python,
    /// An installed Jupyter kernelspec (IRkernel, IJulia, ...).
    Kernelspec { name: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Availability {
    Available,
    /// Not usable; `guidance` tells the user exactly what to set up. Exposition
    /// never installs runtimes or packages itself.
    Unavailable {
        reason: String,
        guidance: String,
    },
}

/// One discovered (or missing) research runtime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeEnvironment {
    /// Stable identifier used to start sessions, e.g. `python:<path>` or `r:ir`.
    pub id: String,
    pub language: RuntimeLanguage,
    pub display_name: String,
    /// Runtime executable (python.exe, Rscript, julia) when found.
    pub executable: Option<String>,
    pub version: Option<String>,
    /// Kernel the session starts when available.
    pub kernel: Option<KernelChoice>,
    /// Python interpreter that runs the kernel host helper (jupyter_client).
    pub host_interpreter: Option<String>,
    /// Key package versions (Python) or kernel tooling versions.
    pub packages: Vec<ToolVersion>,
    /// Library locations the kernel needs under the cleaned research
    /// environment (e.g. `R_LIBS_USER`, `JULIA_DEPOT_PATH`), from discovery.
    pub kernel_environment: BTreeMap<String, String>,
    pub availability: Availability,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentReport {
    pub runtimes: Vec<RuntimeEnvironment>,
    pub scanned_at_ms: u64,
}

/// Who a session belongs to. Agent runs use their own sessions by default.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum SessionOwner {
    User,
    Agent { task_id: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum SessionState {
    Starting,
    Idle,
    Busy,
    Restarting,
    /// The kernel or the helper died, or the helper stopped responding. Runs
    /// in flight were marked failed; restart creates a fresh kernel.
    Crashed {
        reason: String,
    },
    /// The session's workspace was closed or switched, or its trust was
    /// revoked: the kernel was stopped and the session accepts no more code.
    /// Start a new session for the open folder.
    Detached {
        reason: String,
    },
    Stopped,
}

/// The kernel a session is running, as reported by the kernel itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct KernelDescriptor {
    pub name: String,
    pub language: String,
    pub language_version: String,
    pub implementation: String,
    pub implementation_version: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub id: String,
    pub owner: SessionOwner,
    pub environment_id: String,
    pub language: RuntimeLanguage,
    pub display_name: String,
    pub state: SessionState,
    pub kernel: Option<KernelDescriptor>,
    pub kernel_pid: Option<u32>,
    pub helper_pid: Option<u32>,
    pub helper_version: Option<String>,
    /// `curve` when kernel channels are encrypted, otherwise `none`.
    pub transport_encryption: String,
    /// How interrupts reach the kernel (`event`, `signal`, `message`).
    pub interrupt_mode: String,
    pub active_run: Option<RunId>,
    pub queued_runs: Vec<RunId>,
    pub started_at_ms: u64,
    /// Number of kernels started in this session (restarts included).
    pub generation: u32,
}

/// Where the code of a run came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum RunSource {
    /// Code typed into the Research panel.
    Input { code: String },
    /// A selection of an open document at an exact revision.
    Selection { document: DocumentId, revision: u64, range: TextRange },
    /// A whole file: the open buffer when the file is open, else the file on disk.
    File { path: WorkspacePath },
}

/// Run code in a session (Rust callers; the webview uses its own request type
/// and always runs as the user).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecuteRequest {
    pub session_id: String,
    pub source: RunSource,
    /// Who is asking. A run is refused in a session owned by someone else.
    pub requester: SessionOwner,
}

/// Recorded origin of the code (the code text itself is in [`CodeProvenance`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum CodeOrigin {
    Input,
    Selection { path: Option<WorkspacePath>, revision: u64, range: TextRange },
    File { path: WorkspacePath, revision: Option<u64> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct CodeProvenance {
    pub origin: CodeOrigin,
    /// SHA-256 of the exact UTF-8 code sent to the kernel.
    pub sha256: String,
    pub bytes: u64,
    /// The code itself, unless it exceeded the manifest limit.
    pub text: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct InterpreterProvenance {
    pub implementation: String,
    pub version: String,
    /// Interpreter location with the home directory abbreviated to `~`.
    pub location: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SessionProvenance {
    pub session_id: String,
    pub owner: SessionOwner,
    pub environment_id: String,
    pub generation: u32,
    pub interpreter: InterpreterProvenance,
    pub kernel: KernelDescriptor,
    pub helper_version: String,
    pub protocol: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct RunErrorInfo {
    pub ename: String,
    pub evalue: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct OutputSummary {
    pub entries: u32,
    pub stream_chars: u64,
    /// Characters the helper dropped after the per-run stream limit.
    pub dropped_stream_chars: u64,
    pub dropped_files: u32,
    /// True when older retained output was discarded from memory.
    pub truncated: bool,
}

/// Portable run manifest stored at `.exposition/runs/<run-id>.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct RunManifest {
    /// Manifest format identifier, currently `exposition.run/1`.
    pub format: String,
    pub record: RunRecord,
    pub session: SessionProvenance,
    pub code: CodeProvenance,
    pub execution_count: Option<u32>,
    pub error: Option<RunErrorInfo>,
    pub output: OutputSummary,
    /// Inputs that were unsaved buffers when the run was dispatched: their
    /// `content_hash` is the digest of that buffer text, not of the saved file.
    /// (The code itself is kept in `code.text` when it fits; otherwise only its
    /// digest remains.)
    #[serde(default)]
    pub unsaved_inputs: Vec<WorkspacePath>,
}

/// Lightweight run listing entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct RunSummary {
    pub id: RunId,
    pub session_id: String,
    pub owner: SessionOwner,
    pub status: RunStatus,
    /// True while waiting behind another run in the same session.
    pub queued: bool,
    pub origin: CodeOrigin,
    pub code_preview: String,
    pub execution_count: Option<u32>,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub outputs: Vec<ArtifactRecord>,
    pub error: Option<RunErrorInfo>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum StreamName {
    Stdout,
    Stderr,
}

/// One retained output item of a run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum OutputItem {
    Stream {
        name: StreamName,
        text: String,
    },
    /// A rich result. Images and tables are referenced by artifact, never inlined.
    Display {
        output_type: String,
        execution_count: Option<u32>,
        text: Option<String>,
        artifacts: Vec<ArtifactId>,
    },
    Error {
        ename: String,
        evalue: String,
        traceback: Vec<String>,
    },
    ClearOutput {
        wait: bool,
    },
    /// Supervisor notice (interrupt requested, kernel died, output dropped, ...).
    Notice {
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct OutputEntry {
    /// Increasing per run; used for paging.
    pub seq: u32,
    pub item: OutputItem,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct RunOutputPage {
    pub run_id: RunId,
    pub entries: Vec<OutputEntry>,
    /// Sequence number of the oldest entry still retained.
    pub first_retained_seq: u32,
    pub last_seq: u32,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceStatus {
    pub root: String,
    pub trusted: bool,
}

/// Events emitted to the webview as `research:event`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ResearchEvent {
    SessionChanged { session: SessionInfo },
    SessionRemoved { session_id: String },
    RunQueued { run: RunSummary },
    RunStarted { run_id: RunId, session_id: String, execution_count: Option<u32> },
    Output { run_id: RunId, entry: OutputEntry },
    RunFinished { run: RunSummary },
}

/// Status helper for listings.
pub fn status_label(status: &RunStatus) -> &'static str {
    match status {
        RunStatus::Running => "running",
        RunStatus::Succeeded => "succeeded",
        RunStatus::Failed { .. } => "failed",
        RunStatus::Canceled => "canceled",
        RunStatus::Interrupted => "interrupted",
    }
}
