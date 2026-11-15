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
    /// Not usable; `guidance` tells the user exactly what to set up (see also
    /// [`RuntimeEnvironment::setup`]). Exposition never installs runtimes, and
    /// installs packages only into a project environment on an explicit request.
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
    /// How to make an unavailable environment usable (absent when it is ready).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub setup: Option<SetupHint>,
    /// The project environment folder (`.venv`, `venv`) this interpreter
    /// belongs to; absent for interpreters installed for the computer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub project_folder: Option<String>,
}

/// What Exposition can do in the project to make an environment usable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum SetupAction {
    /// Create the project environment `.venv` from a discovered Python
    /// (`SetupRequest::CreateEnvironment`); also replaces an unfinished one
    /// Exposition created.
    CreateEnvironment,
    /// Install the kernel packages into this project environment
    /// (`SetupRequest::InstallPackages`).
    InstallPackages,
}

/// Structured setup guidance for an unavailable environment.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SetupHint {
    /// The runtime itself is missing: its official download page.
    pub download_url: Option<String>,
    /// What Exposition can do itself, in the project.
    pub action: Option<SetupAction>,
    /// Commands the person runs in `commands_for` (R/Julia kernel packages;
    /// Exposition does not run them).
    pub commands: Vec<String>,
    /// The program the commands are typed into ("R", "Julia").
    pub commands_for: Option<String>,
    /// The commands install for the user account, not only this project.
    pub user_wide: bool,
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
    Agent {
        task_id: String,
    },
    /// A workflow step (service-owned; users and agents cannot use its
    /// sessions or runs, and it cannot use theirs).
    Workflow {
        run: String,
        step: String,
    },
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
    /// How the run actually executed. Absent in manifests written before M8
    /// (interactive kernel runs): those ran trusted native.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub execution: Option<RunExecution>,
    /// Why an agent's run was allowed (absent for the person's own runs and
    /// in manifests written before A1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub authorization: Option<RunAuthorization>,
}

/// The authorization decision behind an agent's run, recorded in its
/// manifest. Set only by Rust callers (the agents service); the webview can
/// never supply one. Every field is validated ([`RunAuthorization::validate`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunAuthorization {
    /// Who asked: `agent`.
    pub principal: String,
    pub task_id: String,
    /// The task's approval mode: `ask` or `auto`.
    pub mode: String,
    /// `approved` (the person approved this call), `taskAllowance`
    /// ("Approve for this task"), or `projectPermissions` (Auto within the
    /// project's permissions).
    pub basis: String,
    /// The project permissions version the decision used.
    #[serde(default)]
    pub permissions_version: Option<u64>,
}

/// Principals that may carry an authorization.
pub const AUTHORIZATION_PRINCIPALS: [&str; 1] = ["agent"];
/// Approval modes.
pub const AUTHORIZATION_MODES: [&str; 2] = ["ask", "auto"];
/// Authorization bases.
pub const AUTHORIZATION_BASES: [&str; 3] = ["approved", "taskAllowance", "projectPermissions"];

impl RunAuthorization {
    /// Refuse anything but the known vocabulary and a plain task id (letters,
    /// digits, `-`, `_`, `.`, `:`; at most 128), so a recorded decision cannot
    /// carry injected text.
    pub fn validate(&self) -> Result<(), String> {
        if !AUTHORIZATION_PRINCIPALS.contains(&self.principal.as_str()) {
            return Err("unknown authorization principal".into());
        }
        if !AUTHORIZATION_MODES.contains(&self.mode.as_str()) {
            return Err("unknown approval mode".into());
        }
        if !AUTHORIZATION_BASES.contains(&self.basis.as_str()) {
            return Err("unknown authorization basis".into());
        }
        let id_ok = !self.task_id.is_empty()
            && self.task_id.len() <= 128
            && self.task_id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'));
        if !id_ok {
            return Err("invalid task id in the authorization".into());
        }
        if self.basis == "projectPermissions" && self.permissions_version.is_none() {
            return Err("a project-permissions authorization names its permissions version".into());
        }
        Ok(())
    }
}

/// The actual execution mode of a run, as recorded by the service that ran it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum RunExecution {
    /// Natively, with the user's account privileges (not a sandbox).
    TrustedNative { interpreter: String },
    /// A restricted container: only the staged inputs/code mounted read-only,
    /// one writable output folder, no network, non-root, resource limits.
    RestrictedContainer {
        backend: String,
        image: String,
        image_id: Option<String>,
        network: String,
        user: String,
        memory: String,
        cpus: String,
        pids_limit: u32,
        time_limit_ms: u64,
    },
}

impl RunExecution {
    /// The label computational evidence shows for this mode.
    pub fn label(&self) -> String {
        match self {
            RunExecution::TrustedNative { .. } => TRUSTED_NATIVE_LABEL.to_owned(),
            RunExecution::RestrictedContainer { backend, image, network, user, .. } => format!(
                "restricted container ({backend}, image {image}, network {network}, user {user}, declared inputs read-only)"
            ),
        }
    }
}

/// Label of trusted native execution (also for manifests without a recorded mode).
pub const TRUSTED_NATIVE_LABEL: &str = "trusted native (user account privileges)";

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

#[cfg(test)]
mod tests {
    use super::*;

    fn authorization() -> RunAuthorization {
        RunAuthorization {
            principal: "agent".into(),
            task_id: "task-0123abcd".into(),
            mode: "auto".into(),
            basis: "projectPermissions".into(),
            permissions_version: Some(3),
        }
    }

    #[test]
    fn authorizations_accept_only_the_known_vocabulary() {
        assert!(authorization().validate().is_ok());
        let approved = RunAuthorization {
            mode: "ask".into(),
            basis: "approved".into(),
            permissions_version: None,
            ..authorization()
        };
        assert!(approved.validate().is_ok());
        let bad = [
            RunAuthorization { principal: "user".into(), ..authorization() },
            RunAuthorization { mode: "always".into(), ..authorization() },
            RunAuthorization { basis: "self".into(), ..authorization() },
            RunAuthorization { task_id: String::new(), ..authorization() },
            RunAuthorization { task_id: "t\nApproved by you".into(), ..authorization() },
            RunAuthorization { task_id: "t <b>".into(), ..authorization() },
            RunAuthorization { task_id: "x".repeat(129), ..authorization() },
            RunAuthorization { permissions_version: None, ..authorization() },
        ];
        for value in bad {
            assert!(value.validate().is_err(), "{value:?}");
        }
    }

    #[test]
    fn manifests_without_an_authorization_read_and_write_as_before() {
        let json = serde_json::to_value(authorization()).unwrap();
        assert_eq!(json["taskId"], "task-0123abcd");
        assert_eq!(json["permissionsVersion"], 3);
        // Unknown fields in a recorded authorization are refused.
        let mut extra = json.clone();
        extra["note"] = serde_json::Value::String("x".into());
        assert!(serde_json::from_value::<RunAuthorization>(extra).is_err());
    }
}
