//! Agent process tools on the shared execution service.
//!
//! `run_command` and `run_python` launch jobs through
//! [`exposition_execution::ExecutionService`] — the same service every other
//! feature uses — with an explicit program and argument array, a clean
//! environment (provider credentials can never reach the child), bounded and
//! redacted output, and process-tree cancellation.
//!
//! Policy is decided by the task loop, never by the model:
//! - `Restricted { backend: "docker" }` for unattended execution. The service
//!   refuses it when no image is configured or Docker is unavailable; there is
//!   no fallback to native execution. The project is mounted read-only at
//!   `/workspace`; files written to `/out` are kept in the job directory.
//! - `TrustedNative` only after the user approved this exact launch, and only
//!   in a workspace the user has trusted (checked before asking).
//!
//! Builds and Lean checks are not wired yet and report that they are
//! unavailable.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use exposition_core::job::{EnvironmentSpec, ExecutionPolicy, JobOutcome, JobSpec, OutputStream};
use exposition_execution::ExecutionService;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::services::{
    BuildRequest, CommandRequest, LeanRequest, PythonRequest, ServiceError, ServiceKind, ToolServices,
};

/// Default wall-clock limit for one agent-launched process.
const JOB_TIMEOUT_MS: u64 = 10 * 60 * 1000;
/// Retained output per stream.
const LOG_LIMIT_BYTES: u64 = 256 * 1024;
/// Output returned to the model per stream (the tail is kept).
const RESULT_TAIL_BYTES: usize = 16 * 1024;

pub struct ExecutionToolServices {
    execution: ExecutionService,
}

impl ExecutionToolServices {
    pub fn new(execution: ExecutionService) -> Self {
        Self { execution }
    }

    async fn run_job(&self, spec: JobSpec, root: &Path, cancel: CancellationToken) -> Result<Value, ServiceError> {
        // The task's project folder must be a registered workspace root for jobs
        // to run there (registration does not grant trust).
        self.execution
            .register_workspace_root(root)
            .map_err(|e| ServiceError::Failed(format!("the project folder cannot host jobs: {e}")))?;
        let policy = spec.policy.clone();
        let id = self.execution.launch(spec);
        let result = tokio::select! {
            result = self.execution.wait(&id) => result,
            _ = cancel.cancelled() => {
                self.execution.cancel(&id);
                let _ = self.execution.wait(&id).await;
                return Err(ServiceError::Canceled);
            }
        };
        let result = result.ok_or_else(|| ServiceError::Failed("the job record was lost".into()))?;
        let stdout = self.execution.output_text(&id, OutputStream::Stdout).unwrap_or_default();
        let stderr = self.execution.output_text(&id, OutputStream::Stderr).unwrap_or_default();
        let (status, exit_code, reason) = match &result.outcome {
            JobOutcome::Exited { code } => ("exited", Some(*code), None),
            JobOutcome::Terminated { description } => ("terminated", None, Some(description.clone())),
            JobOutcome::Canceled => return Err(ServiceError::Canceled),
            JobOutcome::TimedOut => ("timedOut", None, Some(format!("killed after {JOB_TIMEOUT_MS} ms"))),
            JobOutcome::FailedToStart { reason } => {
                return Err(ServiceError::Failed(format!("the process was not started: {reason}")));
            }
        };
        let mut value = json!({
            "status": status,
            "exitCode": exit_code,
            "stdout": tail(&stdout),
            "stderr": tail(&stderr),
            "durationMs": result.duration_ms,
            "outputTruncated": result.output_truncated || stdout.len() > RESULT_TAIL_BYTES || stderr.len() > RESULT_TAIL_BYTES,
            "policy": match &policy {
                ExecutionPolicy::TrustedNative => "trustedNative".to_owned(),
                ExecutionPolicy::Restricted { backend } => format!("restricted:{backend}"),
            },
            "job": id.0,
        });
        if let Some(reason) = reason {
            value["reason"] = json!(reason);
        }
        if matches!(policy, ExecutionPolicy::Restricted { .. }) {
            value["note"] = json!(
                "Ran in the restricted container: the project is mounted read-only at /workspace; files written to /out \
                 are kept in the job directory, not in the project."
            );
        }
        Ok(value)
    }

    fn spec(
        label: String,
        program: String,
        args: Vec<String>,
        cwd: PathBuf,
        policy: ExecutionPolicy,
        stdin: Option<String>,
    ) -> JobSpec {
        JobSpec {
            label,
            program,
            args,
            // Built from empty: nothing is inherited, so no credential can leak.
            environment: EnvironmentSpec::default(),
            working_directory: cwd.display().to_string(),
            policy,
            timeout_ms: Some(JOB_TIMEOUT_MS),
            log_limit_bytes: Some(LOG_LIMIT_BYTES),
            stdin,
        }
    }
}

fn tail(text: &str) -> String {
    if text.len() <= RESULT_TAIL_BYTES {
        return text.to_owned();
    }
    let mut start = text.len() - RESULT_TAIL_BYTES;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("… ({start} earlier bytes omitted)\n{}", &text[start..])
}

#[async_trait]
impl ToolServices for ExecutionToolServices {
    fn available(&self, kind: ServiceKind) -> bool {
        matches!(kind, ServiceKind::Execution | ServiceKind::Research)
    }

    fn restricted_backend(&self) -> Option<String> {
        // The service still refuses at launch if Docker or the image is missing.
        self.execution.settings().docker.image.as_ref().map(|_| exposition_execution::docker::BACKEND.to_owned())
    }

    fn native_refusal(&self, root: &Path) -> Option<String> {
        match self.execution.trust_status(&root.display().to_string()) {
            Ok(status) if status.trusted => None,
            Ok(_) => Some(
                "This project folder is not trusted for native execution, so commands cannot run with your account's \
                 privileges. Trust the folder in Exposition first, or run the task with the restricted sandbox."
                    .to_owned(),
            ),
            Err(e) => Some(format!("Native execution is not possible here: {e}")),
        }
    }

    async fn build_document(&self, _: BuildRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Err(ServiceError::NotAvailable(ServiceKind::Typesetting))
    }

    async fn run_python(&self, request: PythonRequest, cancel: CancellationToken) -> Result<Value, ServiceError> {
        let spec = Self::spec(
            format!("Agent Python ({})", request.session),
            "python".into(),
            vec!["-".into()],
            request.working_directory.clone(),
            request.policy,
            Some(request.code),
        );
        self.run_job(spec, &request.root, cancel).await
    }

    async fn check_lean(&self, _: LeanRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Err(ServiceError::NotAvailable(ServiceKind::Lean))
    }

    async fn run_command(&self, request: CommandRequest, cancel: CancellationToken) -> Result<Value, ServiceError> {
        let label = format!("Agent: {} {}", request.program, request.args.join(" "));
        let label: String = label.chars().take(120).collect();
        let spec = Self::spec(label, request.program, request.args, request.working_directory, request.policy, None);
        self.run_job(spec, &request.root, cancel).await
    }
}
