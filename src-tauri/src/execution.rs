//! Execution commands and job event forwarding.
//!
//! The webview can observe and cancel jobs, read toolchain information, and
//! manage workspace trust. There is deliberately NO command that runs an
//! arbitrary program: launches come from application services in Rust.
//!
//! Every trust grant or revoke, whichever surface made it, is announced as
//! [`TRUST_CHANGED`] so views that show trust re-check it; Lean and research
//! follow it in their services, and watched LaTeX entries are rebuilt here.

use exposition_core::job::JobId;
use exposition_execution::{JobSummary, OutputPage, ToolchainInfo, TrustChanged, TrustError, TrustStatus};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::broadcast::error::RecvError;

use crate::CommandError;
use crate::state::AppState;

/// Event carrying every [`exposition_core::job::JobEvent`], in order.
pub const JOB_EVENT: &str = "jobs:event";
/// Emitted if the frontend fell behind and must re-read the job list.
pub const JOB_RESYNC: &str = "jobs:resync";
/// Event carrying every trust grant or revoke ([`TrustChanged`]); `null` when
/// changes were missed (re-check everything).
pub const TRUST_CHANGED: &str = "trust:changed";

const MAX_PAGE_BYTES: u32 = 1024 * 1024;
const DEFAULT_PAGE_BYTES: u32 = 64 * 1024;

fn validate_job_id(job: &JobId) -> Result<(), CommandError> {
    let valid = job.0.len() <= 64 && job.0.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if valid { Ok(()) } else { Err(CommandError::new("job.invalidId", "invalid job id")) }
}

fn unknown_job() -> CommandError {
    CommandError::new("job.unknown", "no such job (it may have been evicted from the job history)")
}

fn trust_error(error: TrustError) -> CommandError {
    let code = match error {
        TrustError::Path(_) => "trust.invalidPath",
        TrustError::TooBroad(_) => "trust.tooBroad",
        TrustError::Io(_) => "trust.io",
    };
    CommandError::new(code, error.to_string())
}

fn validate_root(root: &str) -> Result<(), CommandError> {
    if root.is_empty() || root.len() > 4096 || root.contains('\0') {
        return Err(CommandError::new("trust.invalidPath", "invalid directory path"));
    }
    Ok(())
}

#[tauri::command]
pub async fn job_list(state: State<'_, AppState>) -> Result<Vec<JobSummary>, CommandError> {
    Ok(state.execution.list())
}

#[tauri::command]
pub async fn job_output(
    state: State<'_, AppState>,
    job: JobId,
    after: Option<u64>,
    max_bytes: Option<u32>,
) -> Result<OutputPage, CommandError> {
    validate_job_id(&job)?;
    let max = max_bytes.unwrap_or(DEFAULT_PAGE_BYTES).clamp(1, MAX_PAGE_BYTES) as usize;
    state.execution.output(&job, after, max).ok_or_else(unknown_job)
}

/// Request cancellation; `false` when the job already finished.
#[tauri::command]
pub async fn job_cancel(state: State<'_, AppState>, job: JobId) -> Result<bool, CommandError> {
    validate_job_id(&job)?;
    if state.execution.summary(&job).is_none() {
        return Err(unknown_job());
    }
    Ok(state.execution.cancel(&job))
}

#[tauri::command]
pub async fn toolchains_list(state: State<'_, AppState>) -> Result<Vec<ToolchainInfo>, CommandError> {
    Ok(state.execution.toolchains().await)
}

#[tauri::command]
pub async fn toolchains_refresh(state: State<'_, AppState>) -> Result<Vec<ToolchainInfo>, CommandError> {
    Ok(state.execution.refresh_toolchains().await)
}

#[tauri::command]
pub async fn trust_status(state: State<'_, AppState>, root: String) -> Result<TrustStatus, CommandError> {
    validate_root(&root)?;
    state.execution.trust_status(&root).map_err(trust_error)
}

/// Grant trust. The frontend calls this only from the explicit trust dialog.
#[tauri::command]
pub async fn trust_grant(state: State<'_, AppState>, root: String) -> Result<TrustStatus, CommandError> {
    validate_root(&root)?;
    state.execution.grant_trust(&root).map_err(trust_error)
}

#[tauri::command]
pub async fn trust_revoke(state: State<'_, AppState>, root: String) -> Result<TrustStatus, CommandError> {
    validate_root(&root)?;
    state.execution.revoke_trust(&root).map_err(trust_error)
}

/// DEV-ONLY (debug builds): start a fixed process tree — a Python parent that
/// spawns a sleeping grandchild and prints output — for cancellation evidence.
/// Not compiled into release builds, not listed in release `COMMANDS`, and its
/// permission is granted only by a runtime capability added in debug builds.
#[cfg(debug_assertions)]
#[tauri::command]
pub async fn dev_start_probe_job(state: State<'_, AppState>) -> Result<JobId, CommandError> {
    use exposition_core::job::{EnvironmentSpec, ExecutionPolicy, JobSpec};
    const SCRIPT: &str = r#"
import os, subprocess, sys, time
child = subprocess.Popen([sys.executable, "-c", "import time\nwhile True: time.sleep(1)"])
for i in range(400):
    print(f"burst line {i:04d} " + "x" * 100)
sys.stdout.flush()
n = 0
while True:
    print(f"tick {n} parent={os.getpid()} grandchild={child.pid}", flush=True)
    sys.stderr.write(f"stderr tick {n}\n")
    sys.stderr.flush()
    n += 1
    time.sleep(0.5)
"#;
    let spec = JobSpec {
        label: "Dev probe: process tree".into(),
        program: "python".into(),
        args: vec!["-E".into(), "-u".into(), "-c".into(), SCRIPT.into()],
        environment: EnvironmentSpec::default(),
        working_directory: state.execution.probe_dir().display().to_string(),
        policy: ExecutionPolicy::TrustedNative,
        timeout_ms: Some(10 * 60 * 1000),
        log_limit_bytes: Some(16 * 1024),
        stdin: None,
    };
    Ok(state.execution.launch_application(spec))
}

/// DEV-ONLY (debug builds): register `root` as an open workspace (a stand-in
/// for project opening) and launch a fixed native job there that writes
/// `exposition-dev-ran.txt`. Used to demonstrate that an untrusted workspace
/// never executes until the user grants trust in the dialog. With
/// `hold_seconds` (at most 600) the job then keeps a two-process tree running
/// that long and prints `parent=<pid> child=<pid>` (revocation evidence).
#[cfg(debug_assertions)]
#[tauri::command]
pub async fn dev_start_workspace_job(
    state: State<'_, AppState>,
    root: String,
    hold_seconds: Option<u32>,
) -> Result<JobId, CommandError> {
    use exposition_core::job::{EnvironmentSpec, ExecutionPolicy, JobSpec};
    validate_root(&root)?;
    let canonical = state
        .execution
        .register_workspace_root(std::path::Path::new(&root))
        .map_err(|e| CommandError::new("workspace.invalidRoot", e.to_string()))?;
    let mut script = "open('exposition-dev-ran.txt', 'w').write('ran'); print('workspace job ran')".to_owned();
    let hold = hold_seconds.map(|s| s.min(600));
    if let Some(seconds) = hold {
        script.push_str(&format!(
            "\nimport os, subprocess, sys, time\n\
             child = subprocess.Popen([sys.executable, '-E', '-c', 'import time; time.sleep({seconds})'])\n\
             print(f'parent={{os.getpid()}} child={{child.pid}}', flush=True)\n\
             time.sleep({seconds})"
        ));
    }
    let spec = JobSpec {
        label: if hold.is_some() { "Dev workspace job (long)".into() } else { "Dev workspace job".into() },
        program: "python".into(),
        args: vec!["-E".into(), "-u".into(), "-c".into(), script],
        environment: EnvironmentSpec::default(),
        working_directory: canonical.display().to_string(),
        policy: ExecutionPolicy::TrustedNative,
        timeout_ms: Some(60_000 + u64::from(hold.unwrap_or(0)) * 1000),
        log_limit_bytes: None,
        stdin: None,
    };
    Ok(state.execution.launch(spec))
}

/// Forward job events to the webview in order.
pub fn forward_events(app: AppHandle) {
    let mut rx = app.state::<AppState>().execution.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let _ = app.emit(JOB_EVENT, &event);
                }
                Err(RecvError::Lagged(_)) => {
                    let _ = app.emit(JOB_RESYNC, ());
                }
                Err(RecvError::Closed) => break,
            }
        }
    });
}

/// Announce trust grants and revokes to the webview, and rebuild the watched
/// LaTeX entries of the open project they concern: after a grant the pages
/// refused before are built; after a revoke a running build is canceled (its
/// next pass would be refused anyway) and the refused build tells every page
/// view and status item that the folder is untrusted again.
pub fn forward_trust(app: AppHandle) {
    let mut rx = app.state::<AppState>().execution.subscribe_trust();
    tauri::async_runtime::spawn(async move {
        loop {
            let change = match rx.recv().await {
                Ok(change) => Some(change),
                Err(RecvError::Lagged(_)) => None,
                Err(RecvError::Closed) => break,
            };
            let state = app.state::<AppState>();
            if change.as_ref().is_none_or(|c| concerns_open_workspace(&state, c)) {
                state.tex.rebuild_watched();
            }
            let _ = app.emit(TRUST_CHANGED, change);
        }
    });
}

/// The change covers the open folder, or a folder inside it.
fn concerns_open_workspace(state: &AppState, change: &TrustChanged) -> bool {
    use exposition_execution::paths::{canonicalize, is_within};
    use std::path::Path;
    let Some(open) = state.workspace.current() else { return false };
    let open = canonicalize(Path::new(&open.root)).unwrap_or_else(|_| open.root.clone().into());
    let changed = Path::new(&change.root);
    is_within(&open, changed) || is_within(changed, &open)
}

/// Debug builds only: grant the dev probe command to the main window.
#[cfg(debug_assertions)]
pub fn add_dev_capability(app: &AppHandle) -> tauri::Result<()> {
    use tauri::ipc::CapabilityBuilder;
    app.add_capability(
        CapabilityBuilder::new("dev-execution-probe")
            .window("main")
            .permission("allow-dev-start-probe-job")
            .permission("allow-dev-start-workspace-job"),
    )
}
