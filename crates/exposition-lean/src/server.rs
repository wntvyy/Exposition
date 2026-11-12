//! Launching a Lean language server through the execution service.
//!
//! Lake projects run `<toolchain>/bin/lake serve` in the project root (Lake
//! elaborates `lakefile.lean`, and the server elaborates files that may run
//! code at compile time), so the launch is `TrustedNative` and the execution
//! service refuses it for untrusted workspaces. Standalone files run
//! `<toolchain>/bin/lean --server`. The toolchain's own executables are used,
//! never elan's shims, so nothing is downloaded implicitly.

use std::sync::Arc;
use std::time::Duration;

use exposition_core::job::{EnvironmentSpec, ExecutionPolicy, JobId, JobSpec, OutputStream};
use exposition_execution::{ExecutionService, InteractiveOptions, ProtocolLog, RefusalCode};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::lsp::{Incoming, LspClient};
use crate::project::{LeanProject, ProjectKind};
use crate::toolchain::ResolvedToolchain;
use crate::uri::path_to_uri;

/// Lake may elaborate `lakefile.lean` before answering `initialize`.
const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(180);
const SERVER_LOG_LIMIT: u64 = 1024 * 1024;

pub(crate) struct StartedServer {
    pub client: Arc<LspClient>,
    pub incoming: mpsc::UnboundedReceiver<Incoming>,
    pub job: JobId,
    pub pid: Option<u32>,
    /// Semantic token type index of `leanSorryLike`, if the server has one.
    pub sorry_token_type: Option<u64>,
}

#[derive(Debug, Clone)]
pub(crate) enum StartError {
    Untrusted { workspace_root: String },
    Failed { message: String },
}

pub(crate) fn server_spec(project: &LeanProject, toolchain: &ResolvedToolchain) -> JobSpec {
    let name = project.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let (program, args, label) = match &project.kind {
        ProjectKind::Lake { .. } => {
            (&toolchain.lake, vec!["serve".to_owned()], format!("Lean server: {name} (lake serve)"))
        }
        ProjectKind::Standalone => {
            (&toolchain.lean, vec!["--server".to_owned()], format!("Lean server: {name} (standalone files)"))
        }
    };
    JobSpec {
        label,
        program: program.display().to_string(),
        args,
        // The clean base environment plus a PATH with the toolchain's own bin
        // directory first; nothing else is needed or passed.
        environment: EnvironmentSpec::default(),
        working_directory: project.root.display().to_string(),
        policy: ExecutionPolicy::TrustedNative,
        timeout_ms: None,
        log_limit_bytes: Some(SERVER_LOG_LIMIT),
        stdin: None,
    }
}

/// Last lines of a job's stderr (redacted by the execution service).
pub(crate) fn stderr_tail(exec: &ExecutionService, job: &JobId) -> Option<String> {
    let text = exec.output_text(job, OutputStream::Stderr)?;
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let tail: Vec<&str> = text.lines().rev().take(8).collect();
    let mut out = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
    if out.len() > 1500 {
        let mut cut = out.len() - 1500;
        while !out.is_char_boundary(cut) {
            cut += 1;
        }
        out = out[cut..].to_owned();
    }
    Some(out)
}

pub(crate) async fn start(
    exec: &ExecutionService,
    project: &LeanProject,
    toolchain: &ResolvedToolchain,
) -> Result<StartedServer, StartError> {
    let spec = server_spec(project, toolchain);
    let job = match exec.launch_interactive(spec, InteractiveOptions { stdout_log: ProtocolLog::Summary }).await {
        Ok(job) => job,
        Err(result) => {
            let refusal = exec.summary(&result.id).and_then(|s| s.refusal);
            return Err(match refusal {
                Some(r) if r.code == RefusalCode::UntrustedWorkspace => StartError::Untrusted {
                    workspace_root: r.workspace_root.unwrap_or_else(|| project.root.display().to_string()),
                },
                Some(r) => StartError::Failed { message: r.message },
                None => StartError::Failed { message: format!("{:?}", result.outcome) },
            });
        }
    };
    let job_id = job.id.clone();
    let pid = job.pid;
    let (client, incoming) = LspClient::start(job.input, job.output);
    let root_uri = path_to_uri(&project.root);
    let params = json!({
        "processId": std::process::id(),
        "clientInfo": { "name": "Exposition" },
        "rootUri": root_uri,
        "workspaceFolders": [{ "uri": root_uri, "name": project.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default() }],
        "capabilities": {
            "textDocument": {
                "synchronization": { "didSave": true, "dynamicRegistration": false },
                "publishDiagnostics": { "versionSupport": true, "relatedInformation": false },
                "hover": { "contentFormat": ["markdown", "plaintext"] },
                "definition": { "linkSupport": true },
                "semanticTokens": {
                    "requests": { "full": true },
                    "tokenTypes": [], "tokenModifiers": [], "formats": ["relative"]
                }
            },
            "window": { "workDoneProgress": false },
            "workspace": { "didChangeWatchedFiles": { "dynamicRegistration": true } }
        },
        // No infoview widgets: Exposition renders goals as plain text only.
        "initializationOptions": { "hasWidgets": false }
    });
    let init = client.call("initialize", params, INITIALIZE_TIMEOUT).await;
    let init = match init {
        Ok(v) => v,
        Err(e) => {
            exec.cancel(&job_id);
            let _ = exec.wait(&job_id).await;
            let tail = stderr_tail(exec, &job_id).map(|t| format!("\n{t}")).unwrap_or_default();
            return Err(StartError::Failed { message: format!("The Lean server did not initialize: {e}.{tail}") });
        }
    };
    let sorry_token_type = sorry_token_type(&init);
    if client.notify("initialized", json!({})).is_err() {
        return Err(StartError::Failed { message: "The Lean server exited during startup.".into() });
    }
    Ok(StartedServer { client, incoming, job: job_id, pid, sorry_token_type })
}

fn sorry_token_type(init: &Value) -> Option<u64> {
    init.pointer("/capabilities/semanticTokensProvider/legend/tokenTypes")?
        .as_array()?
        .iter()
        .position(|t| t.as_str() == Some("leanSorryLike"))
        .map(|i| i as u64)
}

/// Ask the server to shut down; cancel the job if it does not exit promptly.
pub(crate) async fn stop(exec: &ExecutionService, client: &Arc<LspClient>, job: &JobId) {
    let _ = client.call("shutdown", Value::Null, Duration::from_secs(3)).await;
    let _ = client.notify("exit", Value::Null);
    if tokio::time::timeout(Duration::from_secs(3), exec.wait(job)).await.is_err() {
        exec.cancel(job);
        let _ = tokio::time::timeout(Duration::from_secs(5), exec.wait(job)).await;
    }
}

/// `lean --version` of a toolchain, as an application probe (a fixed version
/// query in the application-owned probe directory: no project content).
pub(crate) async fn probe_version(exec: &ExecutionService, toolchain: &ResolvedToolchain) -> Option<String> {
    let spec = JobSpec {
        label: format!("Probe Lean {}", toolchain.name),
        program: toolchain.lean.display().to_string(),
        args: vec!["--version".into()],
        environment: EnvironmentSpec::default(),
        working_directory: exec.probe_dir().display().to_string(),
        policy: ExecutionPolicy::TrustedNative,
        timeout_ms: Some(60_000),
        log_limit_bytes: Some(16 * 1024),
        stdin: None,
    };
    let id = exec.launch_application(spec);
    let result = exec.wait(&id).await?;
    if !matches!(result.outcome, exposition_core::job::JobOutcome::Exited { code: 0 }) {
        return None;
    }
    let out = exec.output_text(&id, OutputStream::Stdout)?;
    out.lines().map(str::trim).find(|l| !l.is_empty()).map(|l| l.chars().take(300).collect())
}
