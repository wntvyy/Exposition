//! Process launching for the kernel host helper and discovery probes.
//!
//! Research never spawns processes itself: everything goes through a
//! [`KernelProcessLauncher`] given an [`exposition_core::job::JobSpec`]
//! (executable + argument array, deliberately constructed environment,
//! validated working directory, execution policy). The application uses
//! [`ExecutionLauncher`], an adapter over the shared
//! [`exposition_execution::ExecutionService`]: interactive jobs for the helper
//! (job-object containment, clean environment, trust enforcement, bounded and
//! redacted stderr log) and application probes for discovery.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use exposition_core::job::{EnvironmentSpec, JobId, JobOutcome, JobSpec, OutputStream};
use exposition_execution::credentials::is_credential_name;
use exposition_execution::{ExecutionService, InteractiveOptions, RefusalCode};
use tokio::sync::mpsc;

/// Never placed in a research child's environment.
pub fn looks_secret(name: &str) -> bool {
    is_credential_name(name)
}

/// Variables research children get on top of the execution service's base
/// environment. Nothing is inherited from the host beyond that base.
pub fn research_environment(extra: BTreeMap<String, String>) -> EnvironmentSpec {
    let mut set = BTreeMap::new();
    set.insert("PYTHONUTF8".to_owned(), "1".to_owned());
    set.insert("PYTHONIOENCODING".to_owned(), "utf-8".to_owned());
    set.insert("PYTHONUNBUFFERED".to_owned(), "1".to_owned());
    set.insert("PYTHONDONTWRITEBYTECODE".to_owned(), "1".to_owned());
    set.insert("PYDEVD_DISABLE_FILE_VALIDATION".to_owned(), "1".to_owned());
    set.extend(extra.into_iter().filter(|(name, _)| !looks_secret(name)));
    EnvironmentSpec { inherit: Vec::new(), set }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LaunchError {
    /// The workspace is not trusted for native execution.
    #[error("{0}")]
    Untrusted(String),
    #[error("{0}")]
    Refused(String),
    #[error("{0}")]
    InvalidSpec(String),
    #[error("could not start {program}: {message}")]
    Spawn { program: String, message: String },
    #[error("timed out after {0} ms")]
    TimedOut(u64),
}

/// How a process ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitInfo {
    pub code: Option<i32>,
}

/// Control over a launched process tree.
pub trait ProcessControl: Send + Sync {
    /// Terminate the process and its descendants.
    fn kill_tree(&self);
    /// Exit status, if the process has exited.
    fn try_wait(&self) -> Option<ExitInfo>;
}

/// The child's stdin is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the process input is closed")]
pub struct InputClosed;

/// Writes protocol lines to a child's stdin without blocking the caller.
pub trait LineSink: Send + Sync {
    /// Queue `line` (a newline is appended). Fails once stdin is closed.
    fn send_line(&self, line: &str) -> Result<(), InputClosed>;
}

/// A long-lived child with open stdin and line-oriented stdout.
pub struct InteractiveProcess {
    pub pid: Option<u32>,
    pub input: Arc<dyn LineSink>,
    /// Stdout lines in order; the channel closes at end of stream.
    pub lines: mpsc::UnboundedReceiver<String>,
    /// Stderr lines when the launcher hands them to the owner (otherwise they
    /// stay in the launcher's own bounded, redacted log).
    pub diagnostics: Option<mpsc::UnboundedReceiver<String>>,
    pub control: Arc<dyn ProcessControl>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeOutput {
    pub exit: ExitInfo,
    pub stdout: String,
    pub stderr: String,
}

pub type LaunchFuture<'a> = Pin<Box<dyn Future<Output = Result<InteractiveProcess, LaunchError>> + Send + 'a>>;

/// How a project job ([`KernelProcessLauncher::start_project_job`]) ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectJobEnd {
    Exited(i32),
    Canceled,
    TimedOut,
    /// Refused or failed to start; `untrusted` when the folder is not trusted.
    NotStarted {
        message: String,
        untrusted: bool,
    },
    Terminated(String),
}

/// New output of a project job since the last poll, and its end once it ended.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectJobProgress {
    /// Redacted output chunks in order (stdout and stderr interleaved).
    pub output: Vec<String>,
    /// Pass back to the next poll.
    pub cursor: Option<u64>,
    /// Set once every chunk has been returned and the job has ended.
    pub end: Option<ProjectJobEnd>,
}

pub trait KernelProcessLauncher: Send + Sync {
    /// Launch a long-lived interactive process (from an async context).
    fn launch_interactive(&self, spec: JobSpec) -> LaunchFuture<'_>;
    /// Run a short discovery probe of a system tool to completion (blocking;
    /// call off the async runtime). Application probes touch no project content.
    fn run_probe(&self, spec: &JobSpec) -> Result<ProbeOutput, LaunchError>;
    /// Run a short probe of a project-provided tool (e.g. a project `.venv`
    /// interpreter) as a trusted project job: refused unless the project is trusted.
    fn run_project_probe(&self, spec: &JobSpec) -> Result<ProbeOutput, LaunchError>;
    /// Application-owned directory probes must run in, if the launcher has one.
    fn probe_dir(&self) -> Option<PathBuf> {
        None
    }
    /// Make `root` an allowed working directory for project jobs.
    fn register_workspace(&self, _root: &Path) -> Result<(), String> {
        Ok(())
    }
    /// The launcher's trust decision for `root`, if it keeps one.
    fn workspace_trusted(&self, _root: &Path) -> Option<bool> {
        None
    }
    /// Redact secrets from child output before it is shown, stored, or logged.
    fn redact(&self, text: &str) -> String {
        text.to_owned()
    }
    /// Start a short project command (environment setup) as a trusted
    /// project batch job: refused unless the project is trusted, never
    /// downgraded. Returns the job id for [`Self::poll_project_job`].
    fn start_project_job(&self, _spec: &JobSpec) -> Result<String, LaunchError> {
        Err(LaunchError::Refused("this launcher cannot run project commands".into()))
    }
    /// New output after `cursor` and, once it ended, how the job ended.
    fn poll_project_job(&self, _job: &str, _cursor: Option<u64>) -> ProjectJobProgress {
        ProjectJobProgress {
            end: Some(ProjectJobEnd::NotStarted { message: "unknown job".into(), untrusted: false }),
            ..Default::default()
        }
    }
    /// Stop a project job and its process tree.
    fn cancel_project_job(&self, _job: &str) {}
    /// Make `dir` (an empty, application-owned folder) the working directory
    /// of project commands for `owner`: they still need `owner`'s trust and
    /// stop when it is revoked. Returns the canonical folder.
    fn register_project_workdir(&self, dir: &Path, _owner: &Path) -> Result<PathBuf, String> {
        Ok(dir.to_path_buf())
    }
    /// Forget a folder registered with [`Self::register_project_workdir`].
    fn unregister_project_workdir(&self, _dir: &Path) {}
}

/// [`KernelProcessLauncher`] over the shared execution service.
#[derive(Clone)]
pub struct ExecutionLauncher {
    execution: ExecutionService,
}

impl ExecutionLauncher {
    pub fn new(execution: ExecutionService) -> Self {
        Self { execution }
    }

    fn refusal(&self, id: &JobId, outcome: &JobOutcome) -> LaunchError {
        let summary = self.execution.summary(id);
        let refusal = summary.and_then(|s| s.refusal);
        match (refusal, outcome) {
            (Some(refusal), _) if refusal.code == RefusalCode::UntrustedWorkspace => {
                LaunchError::Untrusted(refusal.message)
            }
            (Some(refusal), _) => LaunchError::Refused(refusal.message),
            (None, JobOutcome::FailedToStart { reason }) => LaunchError::Refused(reason.clone()),
            (None, other) => LaunchError::Refused(format!("the process ended before it started: {other:?}")),
        }
    }
}

struct ExecutionControl {
    execution: ExecutionService,
    id: JobId,
}

impl ProcessControl for ExecutionControl {
    fn kill_tree(&self) {
        self.execution.cancel(&self.id);
    }

    fn try_wait(&self) -> Option<ExitInfo> {
        self.execution.result(&self.id).map(|result| ExitInfo {
            code: match result.outcome {
                JobOutcome::Exited { code } => Some(code),
                _ => None,
            },
        })
    }
}

struct ChannelSink {
    tx: mpsc::UnboundedSender<String>,
}

impl LineSink for ChannelSink {
    fn send_line(&self, line: &str) -> Result<(), InputClosed> {
        self.tx.send(line.to_owned()).map_err(|_| InputClosed)
    }
}

impl KernelProcessLauncher for ExecutionLauncher {
    fn launch_interactive(&self, spec: JobSpec) -> LaunchFuture<'_> {
        Box::pin(async move {
            let job = match self.execution.launch_interactive(spec, InteractiveOptions::default()).await {
                Ok(job) => job,
                Err(result) => return Err(self.refusal(&result.id, &result.outcome)),
            };
            let (line_tx, lines) = mpsc::unbounded_channel();
            let mut output = job.output;
            tokio::spawn(async move {
                while let Some(line) = output.next_line().await {
                    if line_tx.send(line).is_err() {
                        break;
                    }
                }
            });
            let (input_tx, mut input_rx) = mpsc::unbounded_channel::<String>();
            let input = job.input;
            tokio::spawn(async move {
                while let Some(line) = input_rx.recv().await {
                    if input.write_line(&line).await.is_err() {
                        break;
                    }
                }
                // Dropping `input` closes the helper's stdin (it then shuts down).
            });
            Ok(InteractiveProcess {
                pid: job.pid,
                input: Arc::new(ChannelSink { tx: input_tx }),
                lines,
                diagnostics: None,
                control: Arc::new(ExecutionControl { execution: self.execution.clone(), id: job.id }),
            })
        })
    }

    fn run_probe(&self, spec: &JobSpec) -> Result<ProbeOutput, LaunchError> {
        self.wait_probe(spec, self.execution.launch_application(spec.clone()))
    }

    fn run_project_probe(&self, spec: &JobSpec) -> Result<ProbeOutput, LaunchError> {
        self.wait_probe(spec, self.execution.launch(spec.clone()))
    }

    fn probe_dir(&self) -> Option<PathBuf> {
        Some(self.execution.probe_dir().to_path_buf())
    }

    fn register_workspace(&self, root: &Path) -> Result<(), String> {
        self.execution.register_workspace_root(root).map(|_| ()).map_err(|e| e.to_string())
    }

    fn workspace_trusted(&self, root: &Path) -> Option<bool> {
        Some(self.execution.trust_status(&root.display().to_string()).map(|s| s.trusted).unwrap_or(false))
    }

    fn redact(&self, text: &str) -> String {
        self.execution.redact(text)
    }

    fn start_project_job(&self, spec: &JobSpec) -> Result<String, LaunchError> {
        if spec.policy != exposition_core::job::ExecutionPolicy::TrustedNative {
            return Err(LaunchError::InvalidSpec("project commands run as trusted native jobs".into()));
        }
        Ok(self.execution.launch(spec.clone()).0)
    }

    fn poll_project_job(&self, job: &str, cursor: Option<u64>) -> ProjectJobProgress {
        let id = JobId(job.to_owned());
        let Some(page) = self.execution.output(&id, cursor, 256 * 1024) else {
            return ProjectJobProgress {
                end: Some(ProjectJobEnd::NotStarted { message: "the job is no longer known".into(), untrusted: false }),
                ..Default::default()
            };
        };
        let next = page.chunks.last().map(|c| c.seq).or(cursor);
        let output = page.chunks.into_iter().map(|c| c.text).collect();
        // `complete` means the result exists and every retained chunk is in
        // the log: once no page remains, the end is final.
        let end = if page.complete && !page.has_more {
            self.execution.result(&id).map(|result| match result.outcome {
                JobOutcome::Exited { code } => ProjectJobEnd::Exited(code),
                JobOutcome::Canceled => ProjectJobEnd::Canceled,
                JobOutcome::TimedOut => ProjectJobEnd::TimedOut,
                JobOutcome::Terminated { description } => ProjectJobEnd::Terminated(description),
                JobOutcome::FailedToStart { reason } => {
                    let refusal = self.execution.summary(&id).and_then(|s| s.refusal);
                    let untrusted = refusal.as_ref().is_some_and(|r| r.code == RefusalCode::UntrustedWorkspace);
                    ProjectJobEnd::NotStarted { message: refusal.map(|r| r.message).unwrap_or(reason), untrusted }
                }
            })
        } else {
            None
        };
        ProjectJobProgress { output, cursor: next, end }
    }

    fn cancel_project_job(&self, job: &str) {
        self.execution.cancel(&JobId(job.to_owned()));
    }

    fn register_project_workdir(&self, dir: &Path, owner: &Path) -> Result<PathBuf, String> {
        self.execution.register_app_dir(dir, Some(owner)).map_err(|e| e.to_string())
    }

    fn unregister_project_workdir(&self, dir: &Path) {
        self.execution.unregister_root(dir);
    }
}

impl ExecutionLauncher {
    fn wait_probe(&self, spec: &JobSpec, id: JobId) -> Result<ProbeOutput, LaunchError> {
        let timeout = Duration::from_millis(spec.timeout_ms.unwrap_or(30_000) + 5_000);
        let started = Instant::now();
        let result = loop {
            if let Some(result) = self.execution.result(&id) {
                break result;
            }
            if started.elapsed() > timeout {
                self.execution.cancel(&id);
                return Err(LaunchError::TimedOut(timeout.as_millis() as u64));
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let code = match &result.outcome {
            JobOutcome::Exited { code } => Some(*code),
            JobOutcome::FailedToStart { .. } => return Err(self.refusal(&id, &result.outcome)),
            JobOutcome::TimedOut => return Err(LaunchError::TimedOut(spec.timeout_ms.unwrap_or(0))),
            _ => None,
        };
        Ok(ProbeOutput {
            exit: ExitInfo { code },
            stdout: self.execution.output_text(&id, OutputStream::Stdout).unwrap_or_default(),
            stderr: self.execution.output_text(&id, OutputStream::Stderr).unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn research_environment_never_carries_credentials() {
        let mut extra = BTreeMap::new();
        extra.insert("ANTHROPIC_API_KEY".to_owned(), "x".to_owned());
        extra.insert("GITHUB_TOKEN".to_owned(), "x".to_owned());
        extra.insert("R_LIBS_USER".to_owned(), "C:/lib".to_owned());
        let env = research_environment(extra);
        assert!(env.inherit.is_empty());
        assert!(env.set.keys().all(|name| !looks_secret(name)));
        assert_eq!(env.set.get("R_LIBS_USER").map(String::as_str), Some("C:/lib"));
        assert_eq!(env.set.get("PYTHONUTF8").map(String::as_str), Some("1"));
    }
}
