//! The execution service: validated launch, queueing, supervision, events.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use exposition_core::job::{
    EnvironmentSpec, ExecutionPolicy, JobEvent, JobId, JobInfo, JobOutcome, JobResult, JobSpec, OutputStream,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use ts_rs::TS;

use crate::credentials::Redactor;
use crate::docker;
use crate::env::{EnvContext, Environment, HostEnv, build_container_environment, build_environment};
use crate::log::{DEFAULT_LOG_LIMIT, OutputPage, RingLog, StreamCapture, page};
use crate::paths::{PathError, RootKind, Roots, canonical_directory, canonicalize};
use crate::process::{Containment, outcome_from_status, spawn_contained};
use crate::settings::{ExecutionSettings, SettingsStore};
use crate::toolchains::{
    Located, Locator, TOOLS, ToolStatus, ToolchainInfo, interpret_probe, is_executable_file, tool,
};
use crate::trust::{TrustError, TrustStatus, TrustStore};

/// Default wall-clock limit for batch jobs without `timeout_ms`.
pub const DEFAULT_BATCH_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Limit for application probes (version queries).
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
const PROBE_LOG_LIMIT: u64 = 64 * 1024;
/// Finished jobs retained for listing/output reads.
const MAX_FINISHED_JOBS: usize = 500;
/// Concurrent interactive sessions.
const MAX_INTERACTIVE: usize = 16;
/// Supervisor poll interval; output is flushed at most every `FLUSH_INTERVAL`
/// (so at most 10 output events per stream per second, 20 per job).
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const FLUSH_INTERVAL: Duration = Duration::from_millis(100);
/// How long to wait for output readers after the process tree is gone.
const READER_GRACE: Duration = Duration::from_secs(3);

/// What kind of job this is (affects trust rules and UI grouping).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum JobKind {
    /// A build/run submitted by an application service.
    Batch,
    /// A long-lived protocol session (kernel helper, language server).
    Interactive,
    /// A fixed toolchain version query (no workspace trust needed).
    Probe,
    /// A fixed application command with no project content (no workspace trust needed).
    Application,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum JobState {
    Queued,
    Running,
    Canceling,
    Finished,
}

/// Machine-readable reason a job was refused before starting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum RefusalCode {
    InvalidWorkingDirectory,
    OutsideAllowedRoots,
    UntrustedWorkspace,
    ApplicationDirectoryOnly,
    ProgramNotFound,
    ProgramRefused,
    EnvironmentRefused,
    InvalidArgument,
    UnsupportedBackend,
    RestrictedBackendUnavailable,
    SpawnFailed,
    TooManySessions,
    ShuttingDown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct JobRefusal {
    pub code: RefusalCode,
    pub message: String,
    /// The workspace whose trust is required (for `untrustedWorkspace`).
    pub workspace_root: Option<String>,
}

impl JobRefusal {
    fn new(code: RefusalCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), workspace_root: None }
    }
}

/// A job listing entry for the UI.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct JobSummary {
    pub info: JobInfo,
    pub kind: JobKind,
    pub state: JobState,
    pub working_directory: String,
    pub refusal: Option<JobRefusal>,
    pub pid: Option<u32>,
}

/// How the stdout of an interactive job is recorded in the job log. The owner
/// always receives the exact, unredacted byte stream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProtocolLog {
    /// Log only a note and the byte count (default for protocol streams).
    #[default]
    Summary,
    /// Also keep a bounded, redacted copy in the log.
    RedactedCopy,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct InteractiveOptions {
    pub stdout_log: ProtocolLog,
}

/// Configuration for [`ExecutionService::with_config`].
pub struct ExecutionConfig {
    /// Application data directory; the service uses `<data_dir>/execution`.
    pub data_dir: PathBuf,
    /// Host environment snapshot (defaults to the current process environment).
    pub host_env: HostEnv,
    /// In-memory settings instead of `<data>/execution/settings.json`.
    pub settings: Option<ExecutionSettings>,
    /// Directories that may never be trusted wholesale (home, system dirs).
    pub too_broad_to_trust: Option<Vec<PathBuf>>,
}

impl ExecutionConfig {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self { data_dir: data_dir.into(), host_env: HostEnv::capture(), settings: None, too_broad_to_trust: None }
    }
}

#[derive(Debug)]
enum StdinMode {
    Null,
    Data(String),
    Interactive,
}

/// Container side of a restricted launch. The host side (`Prepared.program`,
/// `Prepared.env`) is the docker client and its clean environment.
struct DockerLaunch {
    image: String,
    container_env: Environment,
    /// Program and arguments inside the container.
    program: String,
    args: Vec<String>,
    /// Registered root mounted read-only at `/workspace`.
    mount_root: PathBuf,
    workdir_components: Vec<String>,
}

/// A validated launch plan.
struct Prepared {
    /// Native executable (for restricted jobs: the docker client).
    program: PathBuf,
    /// Native arguments (for restricted jobs: built at spawn time).
    args: Vec<String>,
    env: Environment,
    cwd: PathBuf,
    timeout: Option<Duration>,
    stdin: StdinMode,
    docker: Option<DockerLaunch>,
}

struct Control {
    cancel: Mutex<bool>,
    wake: Condvar,
}

impl Control {
    fn request(&self) {
        *self.cancel.lock().expect("cancel lock") = true;
        self.wake.notify_all();
    }

    fn requested(&self) -> bool {
        *self.cancel.lock().expect("cancel lock")
    }

    /// Wait until canceled or the timeout passes; returns whether canceled.
    fn wait(&self, timeout: Duration) -> bool {
        let guard = self.cancel.lock().expect("cancel lock");
        let (guard, _) = self.wake.wait_timeout_while(guard, timeout, |c| !*c).expect("cancel lock");
        *guard
    }
}

struct Mutable {
    state: JobState,
    started_at_ms: Option<u64>,
    started: Option<Instant>,
    result: Option<JobResult>,
    refusal: Option<JobRefusal>,
    pid: Option<u32>,
    program_display: String,
}

struct Output {
    stdout: RingLog,
    stderr: RingLog,
    cap_out: StreamCapture,
    cap_err: StreamCapture,
    next_seq: u64,
    /// Data arrived since the last flush (partial lines are held while busy).
    dirty: bool,
    protocol_bytes: u64,
}

struct Job {
    id: JobId,
    kind: JobKind,
    label: String,
    args: Vec<String>,
    policy: ExecutionPolicy,
    working_directory: String,
    state: Mutex<Mutable>,
    output: Mutex<Output>,
    control: Control,
    done: watch::Sender<Option<JobResult>>,
}

impl Job {
    fn summary(&self) -> JobSummary {
        let m = self.state.lock().expect("job state");
        JobSummary {
            info: JobInfo {
                id: self.id.clone(),
                label: self.label.clone(),
                program: m.program_display.clone(),
                args: self.args.clone(),
                policy: self.policy.clone(),
                started_at_ms: m.started_at_ms,
                result: m.result.clone(),
            },
            kind: self.kind,
            state: m.state,
            working_directory: self.working_directory.clone(),
            refusal: m.refusal.clone(),
            pid: m.pid,
        }
    }
}

#[derive(Default)]
struct Table {
    jobs: HashMap<JobId, Arc<Job>>,
    order: VecDeque<JobId>,
    queue: VecDeque<(Arc<Job>, Prepared)>,
    running: usize,
}

struct Inner {
    exec_dir: PathBuf,
    probe_dir: PathBuf,
    host: HostEnv,
    redactor: Arc<Redactor>,
    settings: SettingsStore,
    trust: TrustStore,
    roots: Mutex<Roots>,
    table: Mutex<Table>,
    events: broadcast::Sender<JobEvent>,
    instance: String,
    docker_cleaned: Mutex<bool>,
    toolchains: tokio::sync::Mutex<Option<Vec<ToolchainInfo>>>,
    shutting_down: AtomicBool,
    interactive: AtomicUsize,
    /// Workers (batch and interactive) currently supervising a process.
    active_workers: AtomicUsize,
}

/// The execution service. Cheap to clone; all clones share state.
#[derive(Clone)]
pub struct ExecutionService {
    inner: Arc<Inner>,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn new_job_id() -> JobId {
    JobId(format!("job-{}", uuid::Uuid::new_v4().simple()))
}

impl ExecutionService {
    /// Create the service for an application data directory, reading settings
    /// and trust from `<data_dir>/execution` and capturing the host environment.
    pub fn new(data_dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        Self::with_config(ExecutionConfig::new(data_dir))
    }

    pub fn with_config(config: ExecutionConfig) -> std::io::Result<Self> {
        let exec_dir = config.data_dir.join("execution");
        let probe_dir = exec_dir.join("probe");
        std::fs::create_dir_all(&probe_dir)?;
        std::fs::create_dir_all(exec_dir.join("jobs"))?;
        let exec_dir = canonicalize(&exec_dir)?;
        let probe_dir = canonicalize(&probe_dir)?;
        let host = config.host_env;
        let redactor = Arc::new(Redactor::from_host_env(host.iter()));
        let settings = match config.settings {
            Some(s) => SettingsStore::in_memory(s),
            None => SettingsStore::load(exec_dir.join("settings.json")),
        };
        let too_broad = config.too_broad_to_trust.unwrap_or_else(|| default_too_broad(&host));
        let trust = TrustStore::load(exec_dir.join("trust.json"), too_broad);
        let mut roots = Roots::default();
        roots
            .register(&probe_dir, RootKind::AppOwned { owner: None })
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let (events, _) = broadcast::channel(4096);
        let instance = docker::instance_label(&exec_dir);
        Ok(Self {
            inner: Arc::new(Inner {
                exec_dir,
                probe_dir,
                host,
                redactor,
                settings,
                trust,
                roots: Mutex::new(roots),
                table: Mutex::new(Table::default()),
                events,
                instance,
                docker_cleaned: Mutex::new(false),
                toolchains: tokio::sync::Mutex::new(None),
                shutting_down: AtomicBool::new(false),
                interactive: AtomicUsize::new(0),
                active_workers: AtomicUsize::new(0),
            }),
        })
    }

    // ----- roots, trust, settings -------------------------------------------------

    /// Register an opened project directory; returns its canonical path. Jobs
    /// may run inside it; native jobs additionally need it to be trusted.
    pub fn register_workspace_root(&self, path: &Path) -> Result<PathBuf, PathError> {
        self.inner.roots.lock().expect("roots").register(path, RootKind::Workspace)
    }

    pub fn unregister_root(&self, path: &Path) {
        self.inner.roots.lock().expect("roots").unregister(path);
    }

    /// Register an application-owned directory (build output, scratch). Pass the
    /// workspace it serves so native jobs there follow that workspace's trust;
    /// `None` makes it usable only by application probes.
    pub fn register_app_dir(&self, path: &Path, owner: Option<&Path>) -> Result<PathBuf, PathError> {
        let owner = owner.map(|o| canonicalize(o).unwrap_or_else(|_| o.to_path_buf()));
        self.inner.roots.lock().expect("roots").register(path, RootKind::AppOwned { owner })
    }

    /// The application-owned directory used for version probes.
    pub fn probe_dir(&self) -> &Path {
        &self.inner.probe_dir
    }

    /// Per-job directory (`out/` is mounted at `/out` for container jobs).
    pub fn job_dir(&self, id: &JobId) -> PathBuf {
        self.inner.exec_dir.join("jobs").join(&id.0)
    }

    pub fn trust_status(&self, path: &str) -> Result<TrustStatus, TrustError> {
        self.inner.trust.status(path)
    }

    /// Grant trust. Call only from an explicit user action (trust dialog).
    pub fn grant_trust(&self, path: &str) -> Result<TrustStatus, TrustError> {
        self.inner.trust.grant(path)
    }

    pub fn revoke_trust(&self, path: &str) -> Result<TrustStatus, TrustError> {
        self.inner.trust.revoke(path)
    }

    pub fn settings(&self) -> ExecutionSettings {
        self.inner.settings.get()
    }

    pub fn update_settings(&self, change: impl FnOnce(&mut ExecutionSettings)) -> std::io::Result<ExecutionSettings> {
        self.inner.settings.update(change)
    }

    /// Redact secrets from text (for owners forwarding protocol data to logs/UI).
    pub fn redact(&self, text: &str) -> String {
        self.inner.redactor.redact(text)
    }

    // ----- launching ---------------------------------------------------------------

    /// Submit a batch job. Validation failures produce a job that finishes
    /// immediately with `FailedToStart`; the id is always returned.
    pub fn launch(&self, spec: JobSpec) -> JobId {
        self.submit(spec, JobKind::Batch)
    }

    /// Submit a fixed application command that touches no project content
    /// (version probes, diagnostics). Allowed without workspace trust, but only
    /// with a working directory in an application-owned directory that is not
    /// bound to a workspace. Never use this for user/agent/project commands.
    pub fn launch_application(&self, spec: JobSpec) -> JobId {
        self.submit(spec, JobKind::Application)
    }

    /// Launch and wait for the result.
    pub async fn run(&self, spec: JobSpec) -> JobResult {
        let id = self.launch(spec);
        self.wait(&id).await.expect("job just submitted")
    }

    /// Launch a long-lived interactive job (stdin kept open; stdout delivered
    /// to the caller as an ordered byte stream). Not queued: it starts at once.
    /// Resolves after the process has started, or with the `FailedToStart`
    /// result if validation or spawning failed. `spec.stdin` must be `None`;
    /// `spec.timeout_ms = None` means no wall-clock limit.
    pub async fn launch_interactive(
        &self,
        spec: JobSpec,
        options: InteractiveOptions,
    ) -> Result<InteractiveJob, JobResult> {
        let (job, prepared) = match self.register(&spec, JobKind::Interactive) {
            Ok(v) => v,
            Err(result) => return Err(result),
        };
        let mut prepared = prepared;
        prepared.stdin = StdinMode::Interactive;
        if self.inner.interactive.fetch_add(1, Ordering::SeqCst) >= MAX_INTERACTIVE {
            self.inner.interactive.fetch_sub(1, Ordering::SeqCst);
            let refusal = JobRefusal::new(
                RefusalCode::TooManySessions,
                format!("at most {MAX_INTERACTIVE} interactive sessions may run at once"),
            );
            return Err(self.inner.finish_refused(&job, refusal));
        }
        let (input_tx, input_rx) = mpsc::channel::<Vec<u8>>(256);
        let (output_tx, output_rx) = mpsc::channel::<Vec<u8>>(256);
        let (started_tx, started_rx) = oneshot::channel::<Result<Option<u32>, JobResult>>();
        let inner = self.inner.clone();
        let worker_job = job.clone();
        inner.active_workers.fetch_add(1, Ordering::SeqCst);
        std::thread::Builder::new()
            .name(format!("exec-{}", job.id.0))
            .spawn(move || {
                let session = Session {
                    stdin_rx: Some(input_rx),
                    stdout_tx: Some(output_tx),
                    log_mode: options.stdout_log,
                    started: Some(started_tx),
                };
                inner.run_job(&worker_job, prepared, session);
                inner.interactive.fetch_sub(1, Ordering::SeqCst);
                inner.active_workers.fetch_sub(1, Ordering::SeqCst);
            })
            .expect("spawn job thread");
        match started_rx.await {
            Ok(Ok(pid)) => Ok(InteractiveJob {
                id: job.id.clone(),
                pid,
                input: InteractiveInput { tx: input_tx },
                output: InteractiveOutput { rx: output_rx, buffer: Vec::new() },
            }),
            Ok(Err(result)) => Err(result),
            Err(_) => Err(self.wait(&job.id).await.expect("job registered")),
        }
    }

    fn submit(&self, spec: JobSpec, kind: JobKind) -> JobId {
        match self.register(&spec, kind) {
            Ok((job, prepared)) => {
                let id = job.id.clone();
                {
                    let mut table = self.inner.table.lock().expect("job table");
                    table.queue.push_back((job, prepared));
                }
                Inner::schedule(&self.inner);
                id
            }
            Err(result) => result.id,
        }
    }

    /// Create the job record, emit `Queued`, and validate. On refusal the job
    /// is finished with `FailedToStart` and its result returned.
    fn register(&self, spec: &JobSpec, kind: JobKind) -> Result<(Arc<Job>, Prepared), JobResult> {
        let inner = &self.inner;
        let id = new_job_id();
        let redactor = &inner.redactor;
        let limit = spec.log_limit_bytes.map(|l| l.min(usize::MAX as u64) as usize).unwrap_or(DEFAULT_LOG_LIMIT);
        let stdout = RingLog::new(limit);
        let effective = stdout.limit();
        let (done, _) = watch::channel(None);
        let job = Arc::new(Job {
            id: id.clone(),
            kind,
            label: redactor.redact(&spec.label),
            args: spec.args.iter().map(|a| redactor.redact(a)).collect(),
            policy: spec.policy.clone(),
            working_directory: spec.working_directory.clone(),
            state: Mutex::new(Mutable {
                state: JobState::Queued,
                started_at_ms: None,
                started: None,
                result: None,
                refusal: None,
                pid: None,
                program_display: redactor.redact(&spec.program),
            }),
            output: Mutex::new(Output {
                stdout,
                stderr: RingLog::new(effective),
                cap_out: StreamCapture::new(effective),
                cap_err: StreamCapture::new(effective),
                next_seq: 1,
                dirty: false,
                protocol_bytes: 0,
            }),
            control: Control { cancel: Mutex::new(false), wake: Condvar::new() },
            done,
        });
        {
            let mut table = inner.table.lock().expect("job table");
            table.jobs.insert(id.clone(), job.clone());
            table.order.push_back(id.clone());
            evict_finished(&mut table);
        }
        let _ = inner.events.send(JobEvent::Queued { id: id.clone(), label: job.label.clone() });
        if inner.shutting_down.load(Ordering::SeqCst) {
            return Err(inner
                .finish_refused(&job, JobRefusal::new(RefusalCode::ShuttingDown, "the application is shutting down")));
        }
        match inner.prepare(spec, kind) {
            Ok(prepared) => {
                job.state.lock().expect("job state").program_display = prepared.display_program();
                Ok((job, prepared))
            }
            Err(refusal) => Err(inner.finish_refused(&job, refusal)),
        }
    }

    // ----- observing and controlling -------------------------------------------------

    pub fn subscribe(&self) -> broadcast::Receiver<JobEvent> {
        self.inner.events.subscribe()
    }

    /// Wait for a job's result. `None` for an unknown (or evicted) id.
    pub async fn wait(&self, id: &JobId) -> Option<JobResult> {
        let mut rx = {
            let table = self.inner.table.lock().expect("job table");
            table.jobs.get(id)?.done.subscribe()
        };
        rx.wait_for(|r| r.is_some()).await.ok()?.clone()
    }

    pub fn result(&self, id: &JobId) -> Option<JobResult> {
        self.job(id)?.state.lock().expect("job state").result.clone()
    }

    fn job(&self, id: &JobId) -> Option<Arc<Job>> {
        self.inner.table.lock().expect("job table").jobs.get(id).cloned()
    }

    pub fn summary(&self, id: &JobId) -> Option<JobSummary> {
        Some(self.job(id)?.summary())
    }

    /// All retained jobs, oldest first.
    pub fn list(&self) -> Vec<JobSummary> {
        let jobs: Vec<Arc<Job>> = {
            let table = self.inner.table.lock().expect("job table");
            table.order.iter().filter_map(|id| table.jobs.get(id).cloned()).collect()
        };
        jobs.iter().map(|j| j.summary()).collect()
    }

    /// A page of retained output after sequence number `after`.
    pub fn output(&self, id: &JobId, after: Option<u64>, max_bytes: usize) -> Option<OutputPage> {
        let job = self.job(id)?;
        let complete = job.state.lock().expect("job state").result.is_some();
        let out = job.output.lock().expect("job output");
        Some(page(&out.stdout, &out.stderr, after, max_bytes.max(1), complete))
    }

    /// All retained (redacted) text of one stream.
    pub fn output_text(&self, id: &JobId, stream: OutputStream) -> Option<String> {
        let job = self.job(id)?;
        let out = job.output.lock().expect("job output");
        Some(match stream {
            OutputStream::Stdout => out.stdout.text(),
            OutputStream::Stderr => out.stderr.text(),
        })
    }

    /// Request cancellation. Returns false when the job is unknown or finished.
    pub fn cancel(&self, id: &JobId) -> bool {
        let Some(job) = self.job(id) else { return false };
        let queued = {
            let mut table = self.inner.table.lock().expect("job table");
            let position = table.queue.iter().position(|(j, _)| j.id == *id);
            position.and_then(|p| table.queue.remove(p))
        };
        {
            let mut m = job.state.lock().expect("job state");
            if m.result.is_some() {
                return false;
            }
            if m.state != JobState::Queued || queued.is_none() {
                m.state = JobState::Canceling;
            }
        }
        let _ = self.inner.events.send(JobEvent::CancelRequested { id: id.clone() });
        job.control.request();
        if queued.is_some() {
            self.inner.finish(&job, JobOutcome::Canceled);
        }
        true
    }

    /// Cancel everything and wait (bounded) for process trees to be killed.
    /// Called on application exit; job-object kill-on-close covers crashes.
    pub fn shutdown(&self) {
        self.inner.shutting_down.store(true, Ordering::SeqCst);
        let jobs: Vec<Arc<Job>> = self.inner.table.lock().expect("job table").jobs.values().cloned().collect();
        for job in jobs {
            self.cancel(&job.id);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.inner.active_workers.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    // ----- toolchains --------------------------------------------------------------

    /// Locate a tool without running it.
    pub fn locate_tool(&self, id: &str) -> Located {
        let settings = self.inner.settings.get();
        Locator { host: &self.inner.host, settings: &settings }.locate(id)
    }

    /// Cached toolchain information (discovers on first use).
    pub async fn toolchains(&self) -> Vec<ToolchainInfo> {
        {
            let cache = self.inner.toolchains.lock().await;
            if let Some(list) = cache.as_ref() {
                return list.clone();
            }
        }
        self.refresh_toolchains().await
    }

    /// Re-run discovery and version probes.
    pub async fn refresh_toolchains(&self) -> Vec<ToolchainInfo> {
        let mut cache = self.inner.toolchains.lock().await;
        let settings = self.inner.settings.get();
        let locator = Locator { host: &self.inner.host, settings: &settings };
        let mut pending = Vec::new();
        let mut list = Vec::new();
        for def in TOOLS {
            let mut info = ToolchainInfo {
                id: def.id.into(),
                name: def.name.into(),
                category: def.category.into(),
                path: None,
                source: None,
                version: None,
                status: ToolStatus::Missing,
                detail: None,
                guidance: Some(def.guidance.into()),
            };
            match locator.locate(def.id) {
                Located::NotFound => {
                    if let Some(base) = def.component_of {
                        info.detail = Some(format!("requires {base}, which was not found"));
                    }
                }
                Located::BadOverride { path } => {
                    info.status = ToolStatus::Broken;
                    info.detail = Some(format!("configured path {path} is not an existing executable"));
                }
                Located::Found { path, source } => {
                    info.path = Some(path.display().to_string());
                    info.source = Some(source);
                    let spec = JobSpec {
                        label: format!("Probe {}", def.name),
                        program: path.display().to_string(),
                        args: def.probe_args.iter().map(|a| a.to_string()).collect(),
                        environment: EnvironmentSpec::default(),
                        working_directory: self.inner.probe_dir.display().to_string(),
                        policy: ExecutionPolicy::TrustedNative,
                        timeout_ms: Some(PROBE_TIMEOUT.as_millis() as u64),
                        log_limit_bytes: Some(PROBE_LOG_LIMIT),
                        stdin: None,
                    };
                    pending.push((list.len(), self.submit(spec, JobKind::Probe)));
                }
            }
            list.push(info);
        }
        for (index, id) in pending {
            let result = self.wait(&id).await;
            let stdout = self.output_text(&id, OutputStream::Stdout).unwrap_or_default();
            let stderr = self.output_text(&id, OutputStream::Stderr).unwrap_or_default();
            let def = tool(&list[index].id).expect("catalog tool");
            let outcome = result.map(|r| r.outcome).unwrap_or(JobOutcome::Canceled);
            let (status, version, detail) = interpret_probe(def, &outcome, &stdout, &stderr);
            let info = &mut list[index];
            info.status = status;
            info.version = version;
            info.detail = detail;
            if status == ToolStatus::Available {
                info.guidance = None;
            }
        }
        *cache = Some(list.clone());
        list
    }
}

impl Prepared {
    fn display_program(&self) -> String {
        match &self.docker {
            Some(d) => format!("{} (container {})", d.program, d.image),
            None => self.program.display().to_string(),
        }
    }
}

fn default_too_broad(host: &HostEnv) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for var in ["USERPROFILE", "HOME", "SystemRoot", "ProgramFiles", "ProgramFiles(x86)", "APPDATA", "LOCALAPPDATA"] {
        if let Some(v) = host.get(var)
            && let Ok(p) = canonicalize(Path::new(v))
        {
            dirs.push(p);
        }
    }
    if let Some(profile) = host.get("USERPROFILE").or_else(|| host.get("HOME")) {
        for sub in ["Desktop", "Documents", "Downloads"] {
            if let Ok(p) = canonicalize(&Path::new(profile).join(sub)) {
                dirs.push(p);
            }
        }
    }
    dirs
}

fn evict_finished(table: &mut Table) {
    while table.order.len() > MAX_FINISHED_JOBS {
        let Some(position) = table
            .order
            .iter()
            .position(|id| table.jobs.get(id).is_none_or(|j| j.state.lock().expect("job state").result.is_some()))
        else {
            break;
        };
        if let Some(id) = table.order.remove(position) {
            table.jobs.remove(&id);
        }
    }
}

/// Validate a native executable path.
fn validate_executable(raw: &str) -> Result<PathBuf, JobRefusal> {
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return Err(JobRefusal::new(
            RefusalCode::ProgramNotFound,
            format!("program must be an absolute executable path or a known tool id: {raw}"),
        ));
    }
    if path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err(JobRefusal::new(RefusalCode::ProgramRefused, format!("program path may not contain `..`: {raw}")));
    }
    #[cfg(windows)]
    {
        let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let name = path.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        if ext == "bat" || ext == "cmd" || name == "cmd.exe" {
            return Err(JobRefusal::new(
                RefusalCode::ProgramRefused,
                format!(
                    "{raw} is run by cmd.exe, which re-parses arguments as a shell command line; launch the tool's real executable instead"
                ),
            ));
        }
        if ext != "exe" {
            return Err(JobRefusal::new(
                RefusalCode::ProgramRefused,
                format!("{raw} is not a Windows executable (.exe); scripts must be run through their interpreter"),
            ));
        }
    }
    if !is_executable_file(&path) {
        return Err(JobRefusal::new(RefusalCode::ProgramNotFound, format!("program does not exist: {raw}")));
    }
    Ok(path)
}

/// Per-launch session plumbing for interactive jobs.
struct Session {
    stdin_rx: Option<mpsc::Receiver<Vec<u8>>>,
    stdout_tx: Option<mpsc::Sender<Vec<u8>>>,
    log_mode: ProtocolLog,
    started: Option<oneshot::Sender<Result<Option<u32>, JobResult>>>,
}

impl Session {
    fn batch() -> Self {
        Self { stdin_rx: None, stdout_tx: None, log_mode: ProtocolLog::RedactedCopy, started: None }
    }
}

impl Inner {
    fn locator_dirs(&self, settings: &ExecutionSettings) -> Vec<PathBuf> {
        Locator { host: &self.host, settings }.toolchain_dirs()
    }

    fn resolve_program(&self, raw: &str, settings: &ExecutionSettings) -> Result<PathBuf, JobRefusal> {
        let bare = !raw.is_empty() && !raw.contains(['/', '\\']) && !Path::new(raw).is_absolute();
        if !bare {
            return validate_executable(raw);
        }
        let Some(def) = tool(raw) else {
            return Err(JobRefusal::new(
                RefusalCode::ProgramNotFound,
                format!("unknown tool id `{raw}`; pass an absolute executable path or a discovered tool id"),
            ));
        };
        if def.component_of.is_some() {
            return Err(JobRefusal::new(
                RefusalCode::ProgramRefused,
                format!("`{raw}` is a kernel component, not a program"),
            ));
        }
        match (Locator { host: &self.host, settings }).locate(raw) {
            Located::Found { path, .. } => validate_executable(&path.display().to_string()),
            Located::BadOverride { path } => Err(JobRefusal::new(
                RefusalCode::ProgramNotFound,
                format!("configured path for `{raw}` is not an executable: {path}"),
            )),
            Located::NotFound => Err(JobRefusal::new(
                RefusalCode::ProgramNotFound,
                format!("{} was not found. {}", def.name, def.guidance),
            )),
        }
    }

    fn prepare(&self, spec: &JobSpec, kind: JobKind) -> Result<Prepared, JobRefusal> {
        let settings = self.settings.get();
        let cwd = canonical_directory(&spec.working_directory)
            .map_err(|e| JobRefusal::new(RefusalCode::InvalidWorkingDirectory, e.to_string()))?;
        let root = self.roots.lock().expect("roots").containing(&cwd).cloned().ok_or_else(|| {
            JobRefusal::new(
                RefusalCode::OutsideAllowedRoots,
                format!(
                    "working directory {} is not inside an open workspace or an application-owned directory",
                    cwd.display()
                ),
            )
        })?;
        if spec.args.iter().any(|a| a.contains('\0')) || spec.program.contains('\0') {
            return Err(JobRefusal::new(RefusalCode::InvalidArgument, "arguments may not contain NUL bytes"));
        }
        let unbound_app_dir = matches!(root.kind, RootKind::AppOwned { owner: None });
        let app_kind = matches!(kind, JobKind::Probe | JobKind::Application);
        if app_kind && !unbound_app_dir {
            return Err(JobRefusal::new(
                RefusalCode::ApplicationDirectoryOnly,
                "application commands must run in an application-owned directory without project content",
            ));
        }
        let timeout = match (spec.timeout_ms, kind) {
            (Some(ms), _) => Some(Duration::from_millis(ms)),
            (None, JobKind::Interactive) => None,
            (None, JobKind::Probe) => Some(PROBE_TIMEOUT),
            (None, _) => Some(DEFAULT_BATCH_TIMEOUT),
        };
        let stdin = match &spec.stdin {
            Some(data) if kind != JobKind::Interactive => StdinMode::Data(data.clone()),
            Some(_) => {
                return Err(JobRefusal::new(
                    RefusalCode::InvalidArgument,
                    "interactive jobs write stdin through their handle, not JobSpec.stdin",
                ));
            }
            None => StdinMode::Null,
        };
        match &spec.policy {
            ExecutionPolicy::TrustedNative => {
                if !app_kind {
                    let workspace = match &root.kind {
                        RootKind::Workspace => Some(root.path.clone()),
                        RootKind::AppOwned { owner } => owner.clone(),
                    };
                    let Some(workspace) = workspace else {
                        return Err(JobRefusal::new(
                            RefusalCode::ApplicationDirectoryOnly,
                            "this application-owned directory is not bound to a workspace; native jobs need a trusted workspace",
                        ));
                    };
                    if !self.trust.is_trusted(&workspace) {
                        return Err(JobRefusal {
                            code: RefusalCode::UntrustedWorkspace,
                            message: format!(
                                "workspace {} is not trusted. Trusted native execution runs with your account's privileges; trust the workspace to allow it, or use restricted execution",
                                workspace.display()
                            ),
                            workspace_root: Some(workspace.display().to_string()),
                        });
                    }
                }
                let program = self.resolve_program(&spec.program, &settings)?;
                let temp = PathBuf::new(); // replaced at spawn time with the job temp dir
                let mut path_dirs = Vec::new();
                if let Some(dir) = program.parent() {
                    path_dirs.push(dir.to_path_buf());
                }
                path_dirs.extend(self.locator_dirs(&settings));
                let env = build_environment(
                    &spec.environment,
                    &EnvContext { host: &self.host, redactor: &self.redactor, temp_dir: &temp, path_dirs: &path_dirs },
                )
                .map_err(|e| JobRefusal::new(RefusalCode::EnvironmentRefused, e.to_string()))?;
                Ok(Prepared { program, args: spec.args.clone(), env, cwd, timeout, stdin, docker: None })
            }
            ExecutionPolicy::Restricted { backend } => {
                if backend != docker::BACKEND {
                    return Err(JobRefusal::new(
                        RefusalCode::UnsupportedBackend,
                        format!("restricted backend `{backend}` is not supported; available: `docker`"),
                    ));
                }
                let unavailable = |message: String| {
                    JobRefusal::new(
                        RefusalCode::RestrictedBackendUnavailable,
                        format!("restricted execution is unavailable: {message} The job was not run natively."),
                    )
                };
                let docker_path = match (Locator { host: &self.host, settings: &settings }).locate("docker") {
                    Located::Found { path, .. } => path,
                    Located::BadOverride { path } => {
                        return Err(unavailable(format!("the configured Docker path {path} does not exist.")));
                    }
                    Located::NotFound => {
                        return Err(unavailable(
                            "Docker was not found. Install Docker Desktop (https://docs.docker.com/desktop/) and configure a local image."
                                .into(),
                        ));
                    }
                };
                let docker_path = validate_executable(&docker_path.display().to_string())?;
                let Some(image) = settings.docker.image.clone() else {
                    return Err(unavailable(
                        "no container image is configured. Set execution.docker.image to an image present locally (`docker image ls`)."
                            .into(),
                    ));
                };
                if !docker::valid_image_ref(&image) {
                    return Err(unavailable(format!("configured image `{image}` is not a valid image reference.")));
                }
                if !docker::valid_container_program(&spec.program) {
                    return Err(JobRefusal::new(
                        RefusalCode::ProgramRefused,
                        "container program must be a command name or absolute path inside the image",
                    ));
                }
                let container_env = build_container_environment(&spec.environment, &self.host, &self.redactor)
                    .map_err(|e| JobRefusal::new(RefusalCode::EnvironmentRefused, e.to_string()))?;
                let client_spec = EnvironmentSpec {
                    inherit: [
                        "DOCKER_HOST",
                        "DOCKER_CONTEXT",
                        "DOCKER_CONFIG",
                        "DOCKER_CERT_PATH",
                        "DOCKER_TLS_VERIFY",
                    ]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
                    set: Default::default(),
                };
                let temp = PathBuf::new();
                let client_env = build_environment(
                    &client_spec,
                    &EnvContext {
                        host: &self.host,
                        redactor: &self.redactor,
                        temp_dir: &temp,
                        path_dirs: &[docker_path.parent().map(Path::to_path_buf).unwrap_or_default()],
                    },
                )
                .map_err(|e| JobRefusal::new(RefusalCode::EnvironmentRefused, e.to_string()))?;
                // Mount the registered root; run in the matching subdirectory.
                let relative = relative_components(&cwd, &root.path);
                Ok(Prepared {
                    program: docker_path,
                    args: Vec::new(),
                    env: client_env,
                    cwd,
                    timeout,
                    stdin,
                    docker: Some(DockerLaunch {
                        image,
                        container_env,
                        program: spec.program.clone(),
                        args: spec.args.clone(),
                        mount_root: root.path.clone(),
                        workdir_components: relative,
                    }),
                })
            }
        }
    }

    fn schedule(this: &Arc<Inner>) {
        let max = {
            let settings = this.settings.get();
            settings
                .max_concurrent_jobs
                .map(|m| m.max(1) as usize)
                .unwrap_or_else(|| std::thread::available_parallelism().map(|n| (n.get() / 2).clamp(2, 8)).unwrap_or(2))
        };
        loop {
            let next = {
                let mut table = this.table.lock().expect("job table");
                if table.running >= max {
                    None
                } else {
                    let next = table.queue.pop_front();
                    if next.is_some() {
                        table.running += 1;
                    }
                    next
                }
            };
            let Some((job, prepared)) = next else { break };
            let inner = this.clone();
            inner.active_workers.fetch_add(1, Ordering::SeqCst);
            std::thread::Builder::new()
                .name(format!("exec-{}", job.id.0))
                .spawn(move || {
                    inner.run_job(&job, prepared, Session::batch());
                    inner.table.lock().expect("job table").running -= 1;
                    inner.active_workers.fetch_sub(1, Ordering::SeqCst);
                    Inner::schedule(&inner);
                })
                .expect("spawn job thread");
        }
    }

    fn finish_refused(&self, job: &Arc<Job>, refusal: JobRefusal) -> JobResult {
        let reason = refusal.message.clone();
        job.state.lock().expect("job state").refusal = Some(refusal);
        self.finish(job, JobOutcome::FailedToStart { reason })
    }

    /// Record the result, emit `Finished`, wake waiters.
    fn finish(&self, job: &Arc<Job>, outcome: JobOutcome) -> JobResult {
        let truncated = {
            let out = job.output.lock().expect("job output");
            out.stdout.truncated() || out.stderr.truncated()
        };
        let result = {
            let mut m = job.state.lock().expect("job state");
            if let Some(existing) = &m.result {
                return existing.clone();
            }
            let duration_ms = m.started.map(|s| s.elapsed().as_millis() as u64).unwrap_or(0);
            let result = JobResult { id: job.id.clone(), outcome, duration_ms, output_truncated: truncated };
            m.result = Some(result.clone());
            m.state = JobState::Finished;
            result
        };
        let _ = self.events.send(JobEvent::Finished { result: result.clone() });
        job.done.send_replace(Some(result.clone()));
        result
    }

    /// Move ready output into the ring buffers and emit it.
    fn flush(&self, job: &Job, release_idle: bool) {
        let mut events = Vec::new();
        {
            let mut out = job.output.lock().expect("job output");
            let out = &mut *out;
            let idle = !out.dirty;
            out.dirty = false;
            for stream in [OutputStream::Stdout, OutputStream::Stderr] {
                let (cap, ring) = match stream {
                    OutputStream::Stdout => (&mut out.cap_out, &mut out.stdout),
                    OutputStream::Stderr => (&mut out.cap_err, &mut out.stderr),
                };
                if release_idle && idle && cap.has_partial() {
                    cap.release_partial(&self.redactor, false);
                }
                if cap.take_dropped() {
                    ring.mark_truncated();
                }
                if let Some(text) = cap.take_ready() {
                    let seq = out.next_seq;
                    out.next_seq += 1;
                    ring.push(crate::log::OutputChunk { seq, stream, text: text.clone() });
                    events.push(JobEvent::Output { id: job.id.clone(), stream, text });
                }
            }
        }
        for event in events {
            let _ = self.events.send(event);
        }
    }

    fn final_flush(&self, job: &Job) {
        {
            let mut out = job.output.lock().expect("job output");
            out.cap_out.release_partial(&self.redactor, true);
            out.cap_err.release_partial(&self.redactor, true);
        }
        self.flush(job, false);
    }

    fn note(&self, job: &Job, stream: OutputStream, text: &str) {
        {
            let mut out = job.output.lock().expect("job output");
            let cap = match stream {
                OutputStream::Stdout => &mut out.cap_out,
                OutputStream::Stderr => &mut out.cap_err,
            };
            cap.push_bytes(text.as_bytes(), &self.redactor);
        }
    }

    /// Supervise one job from spawn to finish (runs on a dedicated thread).
    fn run_job(self: &Arc<Self>, job: &Arc<Job>, mut prepared: Prepared, mut session: Session) {
        let job_dir = self.exec_dir.join("jobs").join(&job.id.0);
        let fail = |inner: &Arc<Inner>, session: &mut Session, code: RefusalCode, message: String| {
            remove_job_dir(&inner.exec_dir.join("jobs").join(&job.id.0));
            let result = inner.finish_refused(job, JobRefusal::new(code, message));
            if let Some(tx) = session.started.take() {
                let _ = tx.send(Err(result));
            }
        };
        if job.control.requested() || self.shutting_down.load(Ordering::SeqCst) {
            let result = self.finish(job, JobOutcome::Canceled);
            if let Some(tx) = session.started.take() {
                let _ = tx.send(Err(result));
            }
            return;
        }
        // Per-job directories.
        let temp_dir = job_dir.join("tmp");
        if let Err(e) = std::fs::create_dir_all(&temp_dir) {
            return fail(self, &mut session, RefusalCode::SpawnFailed, format!("could not create job directory: {e}"));
        }
        let temp_text = temp_dir.display().to_string();
        if cfg!(windows) {
            prepared.env.insert("TEMP", temp_text.clone());
            prepared.env.insert("TMP", temp_text);
        } else {
            prepared.env.insert("TMPDIR", temp_text);
        }

        // Restricted (docker): preflight and build the real client arguments.
        let mut docker_ctx: Option<(PathBuf, Environment)> = None;
        if let Some(launch) = prepared.docker.take() {
            let docker_path = prepared.program.clone();
            let client_env = prepared.env.clone();
            {
                let mut cleaned = self.docker_cleaned.lock().expect("docker cleanup");
                if !*cleaned {
                    docker::remove_stale(&docker_path, &self.instance, &client_env, &self.probe_dir);
                    *cleaned = true;
                }
            }
            if let Err(message) = docker::preflight(&docker_path, &launch.image, &client_env, &self.probe_dir) {
                return fail(
                    self,
                    &mut session,
                    RefusalCode::RestrictedBackendUnavailable,
                    format!("restricted execution is unavailable: {message} The job was not run natively."),
                );
            }
            let out_dir = docker::out_dir(&job_dir);
            let env_file = job_dir.join("container.env");
            if let Err(e) =
                std::fs::create_dir_all(&out_dir).and_then(|_| docker::write_env_file(&env_file, &launch.container_env))
            {
                return fail(
                    self,
                    &mut session,
                    RefusalCode::SpawnFailed,
                    format!("could not prepare container files: {e}"),
                );
            }
            let stdin = !matches!(prepared.stdin, StdinMode::Null);
            prepared.args = docker::run_args(&docker::RunPlan {
                job_id: &job.id.0,
                instance: &self.instance,
                image: &launch.image,
                settings: &self.settings.get().docker,
                mount_root: &launch.mount_root,
                workdir_components: launch.workdir_components,
                out_dir: &out_dir,
                env_file: &env_file,
                program: &launch.program,
                args: &launch.args,
                stdin,
            });
            docker_ctx = Some((docker_path, client_env));
        }

        // Spawn inside the process-tree container.
        let mut command = Command::new(&prepared.program);
        command
            .args(&prepared.args)
            .env_clear()
            .envs(prepared.env.iter())
            .current_dir(if docker_ctx.is_some() { &self.probe_dir } else { &prepared.cwd })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(match prepared.stdin {
                StdinMode::Null => Stdio::null(),
                _ => Stdio::piped(),
            });
        let (mut child, containment) = match spawn_contained(&mut command) {
            Ok(v) => v,
            Err(e) => {
                return fail(
                    self,
                    &mut session,
                    RefusalCode::SpawnFailed,
                    format!("could not start {}: {e}", prepared.program.display()),
                );
            }
        };
        let pid = child.id();
        let start = Instant::now();
        {
            let mut m = job.state.lock().expect("job state");
            m.started = Some(start);
            m.started_at_ms = Some(now_ms());
            m.pid = Some(pid);
            if m.state == JobState::Queued {
                m.state = JobState::Running;
            }
        }
        let _ = self.events.send(JobEvent::Started { id: job.id.clone(), pid: Some(pid) });
        if session.stdout_tx.is_some() && session.log_mode == ProtocolLog::Summary {
            self.note(job, OutputStream::Stdout, "[stdout is a protocol stream; content is not logged]\n");
            self.flush(job, false);
        }
        if let Some(tx) = session.started.take() {
            let _ = tx.send(Ok(Some(pid)));
        }

        // stdin
        if let Some(mut stdin) = child.stdin.take() {
            match std::mem::replace(&mut prepared.stdin, StdinMode::Null) {
                StdinMode::Data(data) => {
                    std::thread::spawn(move || {
                        let _ = stdin.write_all(data.as_bytes());
                    });
                }
                StdinMode::Interactive => {
                    if let Some(mut rx) = session.stdin_rx.take() {
                        std::thread::spawn(move || {
                            while let Some(bytes) = rx.blocking_recv() {
                                if stdin.write_all(&bytes).and_then(|_| stdin.flush()).is_err() {
                                    break;
                                }
                            }
                        });
                    }
                }
                StdinMode::Null => {}
            }
        }

        // Output readers.
        let (reader_tx, reader_rx) = std::sync::mpsc::channel::<()>();
        let mut readers = 0;
        if let Some(stdout) = child.stdout.take() {
            readers += 1;
            let protocol = session.stdout_tx.take().map(|tx| (tx, session.log_mode));
            self.spawn_reader(job.clone(), stdout, OutputStream::Stdout, protocol, reader_tx.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            readers += 1;
            self.spawn_reader(job.clone(), stderr, OutputStream::Stderr, None, reader_tx.clone());
        }
        drop(reader_tx);

        // Supervise.
        let deadline = prepared.timeout.map(|t| start + t);
        let mut last_flush = Instant::now();
        let kill = |containment: &Containment| {
            containment.kill_tree();
            if let Some((docker_path, env)) = &docker_ctx {
                docker::kill_container(docker_path, &job.id.0, env, &self.probe_dir);
            }
        };
        let outcome = loop {
            if job.control.wait(POLL_INTERVAL) || self.shutting_down.load(Ordering::SeqCst) {
                kill(&containment);
                break JobOutcome::Canceled;
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                kill(&containment);
                break JobOutcome::TimedOut;
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    // The primary process exited: no descendant may outlive it.
                    containment.kill_tree();
                    break outcome_from_status(status);
                }
                Ok(None) => {}
                Err(e) => {
                    kill(&containment);
                    break JobOutcome::Terminated { description: format!("could not query process status: {e}") };
                }
            }
            if last_flush.elapsed() >= FLUSH_INTERVAL {
                self.flush(job, true);
                last_flush = Instant::now();
            }
        };
        let _ = child.wait();
        let grace_end = Instant::now() + READER_GRACE;
        let mut finished_readers = 0;
        while finished_readers < readers {
            let remaining = grace_end.saturating_duration_since(Instant::now());
            match reader_rx.recv_timeout(remaining) {
                Ok(()) => finished_readers += 1,
                Err(_) => break,
            }
        }
        if session.log_mode == ProtocolLog::Summary && job.kind == JobKind::Interactive {
            let bytes = job.output.lock().expect("job output").protocol_bytes;
            self.note(job, OutputStream::Stdout, &format!("[protocol stdout total: {bytes} bytes]\n"));
        }
        self.final_flush(job);
        drop(containment);
        remove_job_dir(&job_dir);
        self.finish(job, outcome);
    }

    fn spawn_reader<R: Read + Send + 'static>(
        self: &Arc<Self>,
        job: Arc<Job>,
        mut source: R,
        stream: OutputStream,
        protocol: Option<(mpsc::Sender<Vec<u8>>, ProtocolLog)>,
        done: std::sync::mpsc::Sender<()>,
    ) {
        let redactor = self.redactor.clone();
        std::thread::spawn(move || {
            let mut buffer = vec![0u8; 16 * 1024];
            let is_protocol = protocol.is_some();
            let log_copy = protocol.as_ref().is_none_or(|(_, mode)| *mode == ProtocolLog::RedactedCopy);
            let mut owner = protocol.map(|(tx, _)| tx);
            loop {
                let n = match source.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let bytes = &buffer[..n];
                // Deliver the exact bytes to the owner, in order. If the owner has
                // gone away, keep draining so the child never blocks on a full pipe.
                if let Some(tx) = &owner
                    && tx.blocking_send(bytes.to_vec()).is_err()
                {
                    owner = None;
                }
                let mut out = job.output.lock().expect("job output");
                out.dirty = true;
                if is_protocol {
                    out.protocol_bytes += n as u64;
                }
                if log_copy {
                    match stream {
                        OutputStream::Stdout => out.cap_out.push_bytes(bytes, &redactor),
                        OutputStream::Stderr => out.cap_err.push_bytes(bytes, &redactor),
                    }
                }
            }
            let _ = done.send(());
        });
    }
}

/// Write side of an interactive job's stdin. Clones share the pipe; the child
/// sees end-of-file once every clone is dropped.
#[derive(Clone)]
pub struct InteractiveInput {
    tx: mpsc::Sender<Vec<u8>>,
}

#[derive(Debug, thiserror::Error)]
#[error("the interactive job's stdin is closed")]
pub struct InputClosed;

impl InteractiveInput {
    pub async fn write(&self, bytes: impl Into<Vec<u8>>) -> Result<(), InputClosed> {
        self.tx.send(bytes.into()).await.map_err(|_| InputClosed)
    }

    /// Write `line` followed by `\n`.
    pub async fn write_line(&self, line: &str) -> Result<(), InputClosed> {
        let mut bytes = Vec::with_capacity(line.len() + 1);
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
        self.write(bytes).await
    }

    /// Blocking variant for non-async callers (never call inside an async task).
    pub fn blocking_write(&self, bytes: impl Into<Vec<u8>>) -> Result<(), InputClosed> {
        self.tx.blocking_send(bytes.into()).map_err(|_| InputClosed)
    }
}

/// Read side of an interactive job's stdout: the exact bytes, in order.
pub struct InteractiveOutput {
    rx: mpsc::Receiver<Vec<u8>>,
    buffer: Vec<u8>,
}

impl InteractiveOutput {
    /// Next chunk of raw bytes; `None` once stdout has closed.
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        if !self.buffer.is_empty() {
            return Some(std::mem::take(&mut self.buffer));
        }
        self.rx.recv().await
    }

    /// Next `\n`-terminated line without its terminator (`\r\n` accepted).
    /// A final unterminated line is returned at end of stream.
    pub async fn next_line(&mut self) -> Option<String> {
        loop {
            if let Some(pos) = self.buffer.iter().position(|&b| b == b'\n') {
                let mut line: Vec<u8> = self.buffer.drain(..=pos).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Some(String::from_utf8_lossy(&line).into_owned());
            }
            match self.rx.recv().await {
                Some(bytes) => self.buffer.extend_from_slice(&bytes),
                None => {
                    if self.buffer.is_empty() {
                        return None;
                    }
                    let rest = std::mem::take(&mut self.buffer);
                    return Some(String::from_utf8_lossy(&rest).into_owned());
                }
            }
        }
    }
}

/// A running interactive job.
pub struct InteractiveJob {
    pub id: JobId,
    pub pid: Option<u32>,
    pub input: InteractiveInput,
    pub output: InteractiveOutput,
}

/// Remove a job's temporary files; keep `out/` when it has content.
fn remove_job_dir(job_dir: &Path) {
    let _ = std::fs::remove_dir_all(job_dir.join("tmp"));
    let _ = std::fs::remove_file(job_dir.join("container.env"));
    let _ = std::fs::remove_dir(docker::out_dir(job_dir));
    let _ = std::fs::remove_dir(job_dir);
}

/// Components of `path` below `root` (both canonical).
fn relative_components(path: &Path, root: &Path) -> Vec<String> {
    path.components().skip(root.components().count()).map(|c| c.as_os_str().to_string_lossy().into_owned()).collect()
}
