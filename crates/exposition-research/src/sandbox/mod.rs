//! The research sandbox: workflow-owned Python batches ([`BatchRunner`]).
//!
//! Wiring: [`batch_runner`] takes the research service (current project,
//! runtime discovery, data directory) and the shared execution service
//! (restricted Docker backend, trust, limits, redaction).
//!
//! One batch:
//! 1. The project must be research's open workspace; the requested mode must
//!    be available now (restricted: configured image present locally and the
//!    engine reachable, checked without pulling; trusted native: the folder is
//!    trusted and a Python interpreter is discovered). Otherwise the batch is
//!    refused before anything is staged: never a fallback to another mode.
//! 2. Staging ([`stage`]): a service-owned working copy under
//!    `<data>/research/batches/<run>/work` receives only the declared inputs
//!    and the code, verified against their SHA-256 while copying.
//! 3. The run manifest (`.exposition/runs/<run>.json`, status running,
//!    owner `workflow`, actual execution mode) is written, the job launched
//!    through the execution service, and `on_dispatched` called.
//!    - Restricted: the working copy is the registered root the Docker
//!      backend mounts read-only at `/workspace` (the project itself is never
//!      mounted); `python3 <entry>` runs there with `EXPOSITION_OUTPUT_DIR=/out`,
//!      the backend's writable output area; no network, non-root, read-only
//!      root file system, limits, constructed environment.
//!    - Trusted native: the discovered interpreter runs `<entry>` in the
//!      working copy (registered as an application directory bound to the
//!      project, so the trust gate applies) with `EXPOSITION_OUTPUT_DIR` set
//!      to a per-batch host folder. A fresh process for every batch: never
//!      the user's or an agent's kernel; user-account privileges.
//! 4. Cancellation and the time limit end the job (the backend removes the
//!    container). After exit (also on failure) the output area is validated
//!    and accepted files are registered ([`outputs`]); the manifest is
//!    finalized and the staging folders removed.

mod fsx;
pub mod outputs;
pub mod stage;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use exposition_core::artifact::{Producer, RunId, RunRecord, RunStatus, ToolVersion};
use exposition_core::job::{EnvironmentSpec, ExecutionPolicy, JobId, JobOutcome, JobResult, JobSpec, OutputStream};
use exposition_execution::{ExecutionService, RefusalCode, RestrictedTarget};

use crate::batch::{BatchControl, BatchMode, BatchRequest, BatchResult, BatchRunner};
use crate::error::ResearchError;
use crate::service::{ResearchService, display_path};
use crate::store::{MANIFEST_FORMAT, new_run_id, write_manifest, write_transcript};
use crate::types::{
    Availability, InterpreterProvenance, KernelDescriptor, OutputEntry, OutputItem, OutputSummary, RunErrorInfo,
    RunExecution, RunManifest, RuntimeEnvironment, RuntimeLanguage, SessionOwner, SessionProvenance, StreamName,
};

pub(crate) use outputs::is_batch_artifact_name;

/// Environment variable naming the output area inside the batch.
pub const OUTPUT_DIR_VAR: &str = "EXPOSITION_OUTPUT_DIR";
/// Where the restricted backend mounts the output area.
const CONTAINER_OUT: &str = "/out";
/// Interpreter inside the container image.
const CONTAINER_PYTHON: &str = "python3";
/// Retained job output per stream.
const JOB_LOG_LIMIT: u64 = 1024 * 1024;
/// Characters of each stream kept in the batch log.
const LOG_TAIL: usize = 8 * 1024;
/// How often the runner looks at the job and the cancellation flag.
const POLL: Duration = Duration::from_millis(50);

/// Create the batch runner. `research` provides the open project, runtime
/// discovery, and the data directory (`<data>/research/batches`);
/// `execution` the shared execution service (Docker backend, trust, limits,
/// redaction). Wire one instance into the application state and hand it to
/// the workflow runner as `Arc<dyn BatchRunner>`.
pub fn batch_runner(research: Arc<ResearchService>, execution: ExecutionService) -> Arc<dyn BatchRunner> {
    Arc::new(ScopedBatchRunner::new(research, execution))
}

/// Remove batch staging folders an earlier session left behind (the
/// application was killed while a batch ran; its manifest then reads as
/// failed). Call once at application start, before any batch runs.
/// Returns how many were removed.
pub fn remove_stale_batches(research: &ResearchService) -> usize {
    let dir = research.data_dir().join("research").join("batches");
    let Ok(entries) = std::fs::read_dir(&dir) else { return 0 };
    entries
        .flatten()
        .filter(|e| e.file_name().to_str().is_some_and(crate::store::valid_run_id))
        .filter(|e| std::fs::remove_dir_all(e.path()).is_ok())
        .count()
}

/// The [`BatchRunner`] implementation (see the module docs).
pub struct ScopedBatchRunner {
    research: Arc<ResearchService>,
    execution: ExecutionService,
}

/// What a mode runs with, resolved before staging.
enum Target {
    Restricted(RestrictedTarget),
    Native(RuntimeEnvironment),
}

/// Removes the batch's staging folder and unregisters its root on drop.
struct Staging<'a> {
    execution: &'a ExecutionService,
    dir: PathBuf,
    registered: Option<PathBuf>,
}

impl Drop for Staging<'_> {
    fn drop(&mut self) {
        if let Some(root) = self.registered.take() {
            self.execution.unregister_root(&root);
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// The last `max` characters of `text` (on a character boundary).
fn tail(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// `Name: message` of the last Python exception line in `stderr`, if any.
fn python_error(stderr: &str) -> Option<RunErrorInfo> {
    let line = stderr.lines().rev().map(str::trim).find(|l| !l.is_empty())?;
    let (name, message) = line.split_once(": ").map_or((line, ""), |(n, m)| (n, m));
    let valid = !name.is_empty()
        && name.len() <= 100
        && name.split('.').all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && (name.ends_with("Error")
            || name.ends_with("Exception")
            || name.ends_with("Interrupt")
            || name == "SystemExit");
    valid.then(|| RunErrorInfo { ename: name.to_owned(), evalue: message.to_owned() })
}

/// A port name: realized by `<port>.<ext>`, so it must be a safe file stem.
fn valid_port(port: &str) -> bool {
    !port.is_empty()
        && port.len() <= 64
        && !port.starts_with('.')
        && port.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

impl ScopedBatchRunner {
    pub fn new(research: Arc<ResearchService>, execution: ExecutionService) -> Self {
        Self { research, execution }
    }

    /// The canonical project root, which must be research's open workspace.
    fn current_project(&self, root: &Path) -> Result<PathBuf, ResearchError> {
        let canonical = std::fs::canonicalize(root)
            .map_err(|e| ResearchError::InvalidWorkspace(format!("the project folder cannot be opened: {e}")))?;
        match self.research.workspace_root() {
            Some(current) if current == canonical => Ok(canonical),
            Some(_) => Err(ResearchError::coded(
                "research.projectNotCurrent",
                "The workflow's project is no longer the open project; the batch was not run.",
            )),
            None => Err(ResearchError::NoWorkspace),
        }
    }

    fn restricted_target(&self) -> Result<RestrictedTarget, ResearchError> {
        self.execution.restricted_preflight().map_err(|reason| ResearchError::EnvironmentUnavailable {
            reason: format!("Restricted execution is unavailable: {reason}"),
            guidance: "Nothing was run, and nothing runs natively instead. Configure a local image under Settings → Execution and start Docker."
                .into(),
        })
    }

    fn native_target(&self, root: &Path) -> Result<RuntimeEnvironment, ResearchError> {
        let trusted = self.execution.trust_status(&root.display().to_string()).map(|s| s.trusted).unwrap_or(false);
        if !trusted {
            return Err(ResearchError::EnvironmentUnavailable {
                reason: "Trusted native execution needs a trusted project folder.".into(),
                guidance: "Trust the folder to run batches natively with your account's privileges, or use restricted execution."
                    .into(),
            });
        }
        let report = self.research.environments_blocking()?;
        let pythons = report
            .runtimes
            .into_iter()
            .filter(|r| r.language == RuntimeLanguage::Python && r.executable.is_some())
            .collect::<Vec<_>>();
        pythons
            .iter()
            .find(|r| r.availability == Availability::Available)
            .or_else(|| pythons.first())
            .cloned()
            .ok_or_else(|| ResearchError::EnvironmentUnavailable {
                reason: "No Python interpreter was found for trusted native batches.".into(),
                guidance: "Install Python 3 (or create a project .venv) and refresh research environments.".into(),
            })
    }

    fn target(&self, root: &Path, mode: BatchMode) -> Result<Target, ResearchError> {
        match mode {
            BatchMode::Restricted => self.restricted_target().map(Target::Restricted),
            BatchMode::TrustedNative => self.native_target(root).map(Target::Native),
        }
    }

    fn validate_request(request: &BatchRequest) -> Result<(), ResearchError> {
        if request.time_limit.is_zero() {
            return Err(ResearchError::InvalidRequest("a batch needs a time limit".into()));
        }
        if request.owner.run.is_empty() || request.owner.step.is_empty() {
            return Err(ResearchError::InvalidRequest("a batch must name its workflow run and step".into()));
        }
        for port in &request.outputs {
            if !valid_port(&port.port) {
                return Err(ResearchError::InvalidRequest(format!(
                    "output port `{}` cannot name a file (letters, digits, '_' and '-')",
                    port.port
                )));
            }
            for ext in &port.extensions {
                let ext = ext.trim_start_matches('.').to_ascii_lowercase();
                if !outputs::EXPORTED_EXTENSIONS.contains(&ext.as_str()) {
                    return Err(ResearchError::InvalidRequest(format!(
                        "output port `{}` accepts .{ext}, which batches do not export (supported: {})",
                        port.port,
                        outputs::EXPORTED_EXTENSIONS.join(", ")
                    )));
                }
            }
        }
        Ok(())
    }

    fn job_spec(
        &self,
        request: &BatchRequest,
        target: &Target,
        work: &Path,
        entry: &str,
        out_host: Option<&Path>,
        run_id: &str,
    ) -> JobSpec {
        let timeout_ms = u64::try_from(request.time_limit.as_millis()).unwrap_or(u64::MAX).max(1);
        match target {
            Target::Restricted(_) => {
                let mut set = BTreeMap::new();
                set.insert(OUTPUT_DIR_VAR.to_owned(), CONTAINER_OUT.to_owned());
                set.insert("PYTHONDONTWRITEBYTECODE".to_owned(), "1".to_owned());
                set.insert("PYTHONUNBUFFERED".to_owned(), "1".to_owned());
                set.insert("PYTHONIOENCODING".to_owned(), "utf-8".to_owned());
                set.insert("MPLBACKEND".to_owned(), "Agg".to_owned());
                set.insert("MPLCONFIGDIR".to_owned(), "/tmp/matplotlib".to_owned());
                JobSpec {
                    label: format!("Workflow batch {run_id} (restricted container)"),
                    program: CONTAINER_PYTHON.into(),
                    args: vec![entry.to_owned()],
                    environment: EnvironmentSpec { inherit: Vec::new(), set },
                    working_directory: work.display().to_string(),
                    policy: ExecutionPolicy::Restricted { backend: exposition_execution::docker::BACKEND.into() },
                    timeout_ms: Some(timeout_ms),
                    log_limit_bytes: Some(JOB_LOG_LIMIT),
                    stdin: None,
                }
            }
            Target::Native(env) => {
                let mut extra = BTreeMap::new();
                if let Some(out) = out_host {
                    extra.insert(OUTPUT_DIR_VAR.to_owned(), out.display().to_string());
                }
                extra.insert("MPLBACKEND".to_owned(), "Agg".to_owned());
                let script = entry.split('/').fold(work.to_path_buf(), |p, c| p.join(c));
                JobSpec {
                    label: format!("Workflow batch {run_id} (trusted native)"),
                    program: env.executable.clone().unwrap_or_default(),
                    args: vec![script.display().to_string()],
                    environment: crate::launcher::research_environment(extra),
                    working_directory: work.display().to_string(),
                    policy: ExecutionPolicy::TrustedNative,
                    timeout_ms: Some(timeout_ms),
                    log_limit_bytes: Some(JOB_LOG_LIMIT),
                    stdin: None,
                }
            }
        }
    }

    /// Wait for the job, canceling it when the workflow asks.
    fn wait(&self, job: &JobId, control: &BatchControl<'_>) -> JobResult {
        let mut canceled = false;
        loop {
            if let Some(result) = self.execution.result(job) {
                return result;
            }
            if !canceled && (control.canceled)() {
                canceled = true;
                self.execution.cancel(job);
            }
            std::thread::sleep(POLL);
        }
    }

    fn execution_record(target: &Target, request: &BatchRequest) -> RunExecution {
        match target {
            Target::Restricted(t) => RunExecution::RestrictedContainer {
                backend: t.backend.clone(),
                image: t.image.clone(),
                image_id: t.image_id.clone(),
                network: "none".into(),
                user: t.docker.user.clone(),
                memory: t.docker.memory.clone(),
                cpus: t.docker.cpus.clone(),
                pids_limit: t.docker.pids_limit,
                time_limit_ms: u64::try_from(request.time_limit.as_millis()).unwrap_or(u64::MAX),
            },
            Target::Native(env) => RunExecution::TrustedNative {
                interpreter: env.executable.as_deref().map(|p| display_path(Path::new(p))).unwrap_or_default(),
            },
        }
    }

    fn environment_pairs(target: &Target, request: &BatchRequest) -> Vec<(String, String)> {
        let limit = format!("{} s", request.time_limit.as_secs_f64());
        match target {
            Target::Restricted(t) => {
                let mut pairs = vec![
                    ("mode".to_owned(), "restricted container".to_owned()),
                    ("backend".to_owned(), t.backend.clone()),
                    ("image".to_owned(), t.image.clone()),
                ];
                if let Some(id) = &t.image_id {
                    pairs.push(("imageId".into(), id.clone()));
                }
                pairs.extend([
                    ("interpreter".to_owned(), CONTAINER_PYTHON.to_owned()),
                    ("network".to_owned(), "none".to_owned()),
                    ("user".to_owned(), t.docker.user.clone()),
                    ("memory".to_owned(), t.docker.memory.clone()),
                    ("cpus".to_owned(), t.docker.cpus.clone()),
                    ("pidsLimit".to_owned(), t.docker.pids_limit.to_string()),
                    ("timeLimit".to_owned(), limit),
                    ("inputs".to_owned(), "declared files only, read-only at /workspace".to_owned()),
                    ("outputArea".to_owned(), CONTAINER_OUT.to_owned()),
                ]);
                pairs
            }
            Target::Native(env) => vec![
                ("mode".to_owned(), crate::types::TRUSTED_NATIVE_LABEL.to_owned()),
                (
                    "interpreter".to_owned(),
                    env.executable.as_deref().map(|p| display_path(Path::new(p))).unwrap_or_default(),
                ),
                ("pythonVersion".to_owned(), env.version.clone().unwrap_or_default()),
                ("timeLimit".to_owned(), limit),
            ],
        }
    }

    fn run_batch(
        &self,
        root: &Path,
        request: &BatchRequest,
        control: &BatchControl<'_>,
    ) -> Result<BatchResult, ResearchError> {
        let root = self.current_project(root)?;
        Self::validate_request(request)?;
        let target = self.target(&root, request.mode)?;
        if (control.canceled)() {
            return Err(ResearchError::coded("research.batchCanceled", "The batch was canceled before it started."));
        }
        let run_id = new_run_id();
        let batch_dir = self.research.data_dir().join("research").join("batches").join(&run_id);
        let mut staging = Staging { execution: &self.execution, dir: batch_dir.clone(), registered: None };
        let staged = stage::stage(&root, &batch_dir.join("work"), request)?;
        let out_host = match target {
            Target::Native(_) => {
                let out = batch_dir.join("out");
                std::fs::create_dir_all(&out).map_err(|e| ResearchError::io("creating the output area", e))?;
                Some(out)
            }
            Target::Restricted(_) => None,
        };
        let registered = self
            .execution
            .register_app_dir(&staged.work, Some(&root))
            .map_err(|e| ResearchError::InvalidWorkspace(format!("the working copy cannot be used: {e}")))?;
        staging.registered = Some(registered.clone());

        // The manifest exists (running) before anything runs.
        let execution = Self::execution_record(&target, request);
        let mut record_env = Vec::new();
        let (environment_id, interpreter) = match &target {
            Target::Restricted(t) => {
                record_env.push(ToolVersion { name: "image".into(), version: t.image.clone() });
                if let Some(id) = &t.image_id {
                    record_env.push(ToolVersion { name: "image id".into(), version: id.clone() });
                }
                (
                    format!("docker:{}", t.image),
                    InterpreterProvenance {
                        implementation: String::new(),
                        version: String::new(),
                        location: format!("{CONTAINER_PYTHON} in container image {}", t.image),
                    },
                )
            }
            Target::Native(env) => {
                let location = env.executable.as_deref().map(|p| display_path(Path::new(p))).unwrap_or_default();
                if let Some(version) = &env.version {
                    record_env.push(ToolVersion { name: "python".into(), version: version.clone() });
                }
                (
                    env.id.clone(),
                    InterpreterProvenance {
                        implementation: "python".into(),
                        version: env.version.clone().unwrap_or_default(),
                        location,
                    },
                )
            }
        };
        let owner = SessionOwner::Workflow { run: request.owner.run.clone(), step: request.owner.step.clone() };
        let started = now_ms();
        let spec = self.job_spec(request, &target, &registered, &staged.entry, out_host.as_deref(), &run_id);
        let mut manifest = RunManifest {
            format: MANIFEST_FORMAT.into(),
            record: RunRecord {
                id: RunId(run_id.clone()),
                producer: Producer::Job { job_id: String::new() },
                inputs: staged.inputs.clone(),
                environment: record_env,
                outputs: Vec::new(),
                status: RunStatus::Running,
                started_at_ms: started,
                finished_at_ms: None,
            },
            session: SessionProvenance {
                session_id: String::new(),
                owner,
                environment_id,
                generation: 1,
                interpreter,
                kernel: KernelDescriptor::default(),
                helper_version: String::new(),
                protocol: 0,
            },
            code: staged.code.clone(),
            execution_count: None,
            error: None,
            output: OutputSummary::default(),
            unsaved_inputs: Vec::new(),
            execution: Some(execution),
        };
        write_manifest(&root, &manifest)?;

        let job = self.execution.launch(spec);
        manifest.record.producer = Producer::Job { job_id: job.0.clone() };
        manifest.session.session_id = job.0.clone();
        // Best effort: the running manifest names its job (it is rewritten at the end).
        let _ = write_manifest(&root, &manifest);
        (control.on_dispatched)(&run_id);
        let result = self.wait(&job, control);

        // Output area: the backend's /out folder, or the native batch folder.
        let job_dir = self.execution.job_dir(&job);
        let out_dir = match &out_host {
            Some(dir) => dir.clone(),
            None => exposition_execution::docker::out_dir(&job_dir),
        };
        let (validated, mut rejected) = outputs::validate(&out_dir, request.max_output_bytes);
        if out_host.is_none() {
            // The restricted job's folder holds only what the container wrote.
            let _ = std::fs::remove_dir_all(&job_dir);
        }
        let redact = |text: &str| self.execution.redact(text);
        let registered_outputs = match outputs::register(&root, &run_id, validated, &redact) {
            Ok((records, failed)) => {
                rejected.extend(failed);
                records
            }
            Err(e) => {
                manifest.record.status = RunStatus::Failed { message: format!("outputs could not be stored: {e}") };
                manifest.record.finished_at_ms = Some(now_ms());
                let _ = write_manifest(&root, &manifest);
                return Err(e);
            }
        };

        let stdout = self.execution.output_text(&job, OutputStream::Stdout).unwrap_or_default();
        let stderr = self.execution.output_text(&job, OutputStream::Stderr).unwrap_or_default();
        let refusal = self.execution.summary(&job).and_then(|s| s.refusal);
        let (status, exit_code) = match &result.outcome {
            JobOutcome::Exited { code: 0 } => (RunStatus::Succeeded, Some(0)),
            JobOutcome::Exited { code } => {
                let detail = python_error(&stderr).map(|e| format!(": {}: {}", e.ename, e.evalue)).unwrap_or_default();
                (RunStatus::Failed { message: format!("the batch exited with code {code}{detail}") }, Some(*code))
            }
            JobOutcome::TimedOut => (
                RunStatus::Failed {
                    message: format!(
                        "the batch reached its time limit ({} s) and was stopped",
                        request.time_limit.as_secs_f64()
                    ),
                },
                None,
            ),
            JobOutcome::Canceled => (RunStatus::Canceled, None),
            JobOutcome::Terminated { description } => {
                (RunStatus::Failed { message: format!("the batch was terminated: {description}") }, None)
            }
            JobOutcome::FailedToStart { reason } => {
                (RunStatus::Failed { message: format!("the batch could not start: {reason}") }, None)
            }
        };
        manifest.record.status = status.clone();
        manifest.record.finished_at_ms = Some(now_ms());
        manifest.record.outputs = registered_outputs.clone();
        manifest.error = match (&result.outcome, &status) {
            (_, RunStatus::Succeeded) => None,
            (JobOutcome::Exited { code }, _) => python_error(&stderr)
                .or_else(|| Some(RunErrorInfo { ename: "ExitCode".into(), evalue: code.to_string() })),
            (JobOutcome::TimedOut, _) => Some(RunErrorInfo { ename: "TimeLimit".into(), evalue: String::new() }),
            _ => None,
        };

        // Transcript (the Research page shows it) and the bounded log.
        let mut entries = Vec::new();
        let mut push = |item: OutputItem| {
            let seq = entries.len() as u32 + 1;
            entries.push(OutputEntry { seq, item });
        };
        let mut log = String::new();
        for (name, text) in [(StreamName::Stdout, &stdout), (StreamName::Stderr, &stderr)] {
            if text.is_empty() {
                continue;
            }
            let kept = tail(text, LOG_TAIL);
            log.push_str(match name {
                StreamName::Stdout => "[stdout]\n",
                StreamName::Stderr => "[stderr]\n",
            });
            log.push_str(kept);
            if !kept.ends_with('\n') {
                log.push('\n');
            }
            push(OutputItem::Stream { name, text: kept.to_owned() });
        }
        for r in &rejected {
            push(OutputItem::Notice { message: format!("Output {} was not exported: {}", r.name, r.reason) });
        }
        if let RunStatus::Failed { message } = &status {
            log.push_str(&format!("[batch] {message}\n"));
            push(OutputItem::Notice { message: message.clone() });
        }
        manifest.output = OutputSummary {
            entries: entries.len() as u32,
            stream_chars: (stdout.chars().count() + stderr.chars().count()) as u64,
            dropped_stream_chars: 0,
            dropped_files: rejected.len() as u32,
            truncated: result.output_truncated || stdout.len() > LOG_TAIL || stderr.len() > LOG_TAIL,
        };
        let _ = write_transcript(&root, &run_id, &entries);
        write_manifest(&root, &manifest)?;
        drop(staging);

        if let JobOutcome::FailedToStart { reason } = &result.outcome {
            return Err(match refusal.map(|r| r.code) {
                Some(RefusalCode::RestrictedBackendUnavailable) => ResearchError::EnvironmentUnavailable {
                    reason: reason.clone(),
                    guidance: "Nothing was run, and nothing runs natively instead.".into(),
                },
                Some(RefusalCode::UntrustedWorkspace) => ResearchError::NotTrusted,
                _ => ResearchError::Launch(reason.clone()),
            });
        }
        let (ports, extra) = outputs::map_ports(&request.outputs, registered_outputs);
        Ok(BatchResult {
            research_run: run_id,
            status,
            exit_code,
            ports,
            extra,
            rejected,
            log,
            environment: Self::environment_pairs(&target, request),
        })
    }
}

impl BatchRunner for ScopedBatchRunner {
    fn availability(&self, root: &Path, mode: BatchMode) -> Result<(), String> {
        let root = self.current_project(root).map_err(|e| e.to_string())?;
        self.target(&root, mode).map(|_| ()).map_err(|e| e.to_string())
    }

    fn run(
        &self,
        root: &Path,
        request: &BatchRequest,
        control: &BatchControl<'_>,
    ) -> Result<BatchResult, ResearchError> {
        self.run_batch(root, request, control)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_errors_are_recognised() {
        let stderr = "Traceback (most recent call last):\n  File \"x\", line 1\nModuleNotFoundError: No module named 'helpers'\n";
        let e = python_error(stderr).unwrap();
        assert_eq!(e.ename, "ModuleNotFoundError");
        assert_eq!(e.evalue, "No module named 'helpers'");
        assert!(python_error("plain output\n").is_none());
        assert!(python_error("").is_none());
        assert_eq!(tail("abcdef", 3), "def");
        assert_eq!(tail("ééé", 3), "é");
    }

    #[test]
    fn ports_must_name_files() {
        assert!(valid_port("table"));
        assert!(valid_port("figure_1-a"));
        for bad in ["", ".x", "a/b", "a.b", "../x", "a b"] {
            assert!(!valid_port(bad), "{bad}");
        }
    }
}
