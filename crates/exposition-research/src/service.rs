//! The research supervisor: workspace trust, sessions, runs, and events.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use exposition_core::artifact::{ArtifactRecord, Producer, RunId, RunInput, RunRecord, RunStatus, ToolVersion};
use exposition_core::job::{ExecutionPolicy, JobSpec};
use exposition_core::path::WorkspacePath;
use exposition_core::text::utf16_range_to_bytes;
use exposition_workspace::DocumentService;
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::discovery::{DiscoveryOptions, ProjectScope, discover, now_ms};
use crate::error::ResearchError;
use crate::helper::{HELPER_VERSION, PROTOCOL_VERSION, install_helper, sha256_hex};
use crate::launcher::{KernelProcessLauncher, LaunchError, LineSink, ProcessControl, research_environment};
use crate::protocol::{HelperEvent, HelperFailure, HelperMessage, parse_line};
use crate::runs::{
    ARTIFACT_SIZE_LIMIT, MANIFEST_CODE_LIMIT, RunState, artifact_belongs_to_run, page_of, settle_orphan,
};
use crate::store::{
    self, MANIFEST_FORMAT, artifact_dir_path, ensure_dir_within, new_run_id, read_manifest, resolve_existing,
    write_manifest, write_transcript,
};
use crate::types::{
    Availability, CodeOrigin, CodeProvenance, EnvironmentReport, ExecuteRequest, InterpreterProvenance, KernelChoice,
    KernelDescriptor, OutputItem, OutputSummary, ResearchEvent, RunErrorInfo, RunManifest, RunOutputPage, RunSource,
    RunSummary, RuntimeEnvironment, RuntimeLanguage, SessionInfo, SessionOwner, SessionProvenance, SessionState,
    StreamName, WorkspaceStatus,
};

/// Largest code accepted for one run.
const CODE_LIMIT: usize = 4 * 1024 * 1024;
/// Declared inputs pinned per run.
const MAX_DECLARED_INPUTS: usize = 32;
/// Largest declared input hashed when a run is dispatched.
const DECLARED_INPUT_LIMIT: u64 = 1024 * 1024 * 1024;

/// SHA-256 (hex) of a file, streamed.
pub fn hash_file(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}
/// Helper stderr lines kept for diagnostics.
const DIAGNOSTIC_LINES: usize = 200;

#[derive(Clone)]
pub struct ResearchConfig {
    /// Application data directory; research uses `<data_dir>/research/...` for
    /// the installed helper, Jupyter connection files, and probe scratch space.
    pub data_dir: PathBuf,
    pub launcher: Arc<dyn KernelProcessLauncher>,
    pub documents: Arc<DocumentService>,
    /// Timeout for helper requests (kernel start/restart can take a while).
    pub request_timeout: Duration,
    /// The helper is declared unresponsive after this long without any line.
    pub heartbeat_timeout: Duration,
    /// Cancel waits this long after interrupting before restarting the kernel.
    pub cancel_grace: Duration,
    /// Live research sessions allowed per owner kind. Kernels share the
    /// execution service's interactive-job slots with language servers (Lean),
    /// so research leaves headroom.
    pub max_user_sessions: usize,
    pub max_agent_sessions: usize,
    /// Reads project files the way builds see them (the workspace snapshot:
    /// open buffer preferred, exact saved bytes hashed). Without one, research
    /// reads the open buffer or the file under the workspace root itself.
    pub source_reader: Option<Arc<dyn SourceReader>>,
}

/// A project file as a run consumes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedSource {
    pub text: String,
    /// Document revision when the text came from an open buffer.
    pub revision: Option<u64>,
    /// Hex SHA-256 of the exact bytes (as saved, in the file's encoding).
    pub content_hash: String,
    /// Root of the workspace the file was read from.
    pub root: PathBuf,
}

/// Source access provided by the workspace service.
pub trait SourceReader: Send + Sync {
    fn read(&self, path: &WorkspacePath) -> Result<ResolvedSource, String>;
}

impl ResearchConfig {
    pub fn new(data_dir: PathBuf, launcher: Arc<dyn KernelProcessLauncher>, documents: Arc<DocumentService>) -> Self {
        Self {
            data_dir,
            launcher,
            documents,
            request_timeout: Duration::from_secs(120),
            heartbeat_timeout: Duration::from_secs(20),
            cancel_grace: Duration::from_secs(5),
            max_user_sessions: 8,
            max_agent_sessions: 4,
            source_reader: None,
        }
    }
}

/// Start a session (Rust callers). The application's IPC never accepts an
/// owner from the webview: webview sessions are always the user's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartSessionRequest {
    pub environment_id: String,
    pub owner: SessionOwner,
}

#[derive(Clone)]
struct Workspace {
    root: PathBuf,
    trusted: bool,
}

/// A project root and whether it was trusted when discovery ran (project
/// interpreters are offered only then).
type DiscoveredFor = (PathBuf, bool);

pub struct ResearchService {
    shared: Arc<Shared>,
}

struct Shared {
    config: ResearchConfig,
    events: broadcast::Sender<ResearchEvent>,
    workspace: Mutex<Option<Workspace>>,
    /// Discovery report and the project it was made for.
    environments: Mutex<Option<(Option<DiscoveredFor>, EnvironmentReport)>>,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    runs: Mutex<HashMap<String, RunState>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Shared {
    fn emit(&self, event: ResearchEvent) {
        let _ = self.events.send(event);
    }

    fn research_dir(&self, name: &str) -> Result<PathBuf, ResearchError> {
        let dir = self.config.data_dir.join("research").join(name);
        std::fs::create_dir_all(&dir).map_err(|e| ResearchError::io("creating research data directory", e))?;
        Ok(dir)
    }
}

impl ResearchService {
    pub fn new(config: ResearchConfig) -> Self {
        let (events, _) = broadcast::channel(8192);
        Self {
            shared: Arc::new(Shared {
                config,
                events,
                workspace: Mutex::new(None),
                environments: Mutex::new(None),
                sessions: Mutex::new(HashMap::new()),
                runs: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ResearchEvent> {
        self.shared.events.subscribe()
    }

    // ---- workspace ------------------------------------------------------

    /// Set the workspace research operates in. The application passes the
    /// workspace service's open folder; `trusted` is only a fallback for
    /// launchers without a trust store (the execution launcher decides itself).
    pub fn set_workspace(&self, root: &Path, trusted: bool) -> Result<WorkspaceStatus, ResearchError> {
        if !root.is_absolute() {
            return Err(ResearchError::InvalidWorkspace("workspace root must be an absolute path".into()));
        }
        let canonical = std::fs::canonicalize(root)
            .map_err(|e| ResearchError::InvalidWorkspace(format!("workspace root cannot be opened: {e}")))?;
        if !canonical.is_dir() {
            return Err(ResearchError::InvalidWorkspace("workspace root must be a directory".into()));
        }
        self.shared
            .config
            .launcher
            .register_workspace(&canonical)
            .map_err(|e| ResearchError::InvalidWorkspace(format!("the workspace cannot be used for execution: {e}")))?;
        let previous = lock(&self.shared.workspace).replace(Workspace { root: canonical.clone(), trusted });
        if previous.as_ref().is_none_or(|p| p.root != canonical) {
            self.detach_sessions_outside(
                Some(&canonical),
                "The project folder changed; this session's kernel was stopped.",
            );
            *lock(&self.shared.environments) = None;
        }
        let trusted = self.shared.config.launcher.workspace_trusted(&canonical).unwrap_or(trusted);
        Ok(WorkspaceStatus { root: display_path(&canonical), trusted })
    }

    /// Forget the workspace (the project was closed): every session is
    /// detached (its kernel stopped) and accepts no more code.
    pub fn clear_workspace(&self) {
        let previous = lock(&self.shared.workspace).take();
        if previous.is_some() {
            self.detach_sessions_outside(None, "The project folder was closed; this session's kernel was stopped.");
            *lock(&self.shared.environments) = None;
        }
    }

    /// Detach every live session whose workspace is not `keep`.
    fn detach_sessions_outside(&self, keep: Option<&Path>, reason: &str) {
        let sessions: Vec<Arc<Session>> = lock(&self.shared.sessions).values().cloned().collect();
        for session in sessions {
            if keep != Some(session.root.as_path()) {
                session.detach(reason);
            }
        }
    }

    /// The current workspace; trust comes from the launcher's trust store when it has one.
    pub fn workspace(&self) -> Option<WorkspaceStatus> {
        let workspace = lock(&self.shared.workspace).clone()?;
        let trusted = self.shared.config.launcher.workspace_trusted(&workspace.root).unwrap_or(workspace.trusted);
        Some(WorkspaceStatus { root: display_path(&workspace.root), trusted })
    }

    /// Canonical root of the current workspace.
    pub fn workspace_root(&self) -> Option<PathBuf> {
        lock(&self.shared.workspace).as_ref().map(|w| w.root.clone())
    }

    fn current_workspace(&self) -> Result<Workspace, ResearchError> {
        lock(&self.shared.workspace).clone().ok_or(ResearchError::NoWorkspace)
    }

    // ---- environments ---------------------------------------------------

    /// Discover research runtimes. Cached per open project and its trust (a
    /// grant or revoke discovers again); `refresh` probes again. Only
    /// discovery chooses executables: system interpreters run as application
    /// probes, and interpreters inside the open project (`.venv`) are probed
    /// only when the project is trusted, as trusted project jobs.
    pub async fn environments(&self, refresh: bool) -> Result<EnvironmentReport, ResearchError> {
        let workspace = lock(&self.shared.workspace).clone();
        let project = workspace.map(|w| ProjectScope {
            trusted: self.shared.config.launcher.workspace_trusted(&w.root).unwrap_or(w.trusted),
            root: w.root,
        });
        let key = project.as_ref().map(|p| (p.root.clone(), p.trusted));
        if !refresh
            && let Some((cached, report)) = lock(&self.shared.environments).clone()
            && cached == key
        {
            return Ok(report);
        }
        let probe_dir = match self.shared.config.launcher.probe_dir() {
            Some(dir) => dir,
            None => self.shared.research_dir("probe")?,
        };
        let launcher = self.shared.config.launcher.clone();
        let report = tokio::task::spawn_blocking(move || {
            discover(&DiscoveryOptions { launcher: launcher.as_ref(), probe_dir: &probe_dir, project })
        })
        .await
        .map_err(|e| ResearchError::Io(format!("environment discovery failed: {e}")))?;
        *lock(&self.shared.environments) = Some((key, report.clone()));
        Ok(report)
    }

    /// [`Self::environments`] for blocking callers (workflow batches run on
    /// worker threads): the same cache, discovery, and trust rules.
    pub(crate) fn environments_blocking(&self) -> Result<EnvironmentReport, ResearchError> {
        let workspace = lock(&self.shared.workspace).clone();
        let project = workspace.map(|w| ProjectScope {
            trusted: self.shared.config.launcher.workspace_trusted(&w.root).unwrap_or(w.trusted),
            root: w.root,
        });
        let key = project.as_ref().map(|p| (p.root.clone(), p.trusted));
        if let Some((cached, report)) = lock(&self.shared.environments).clone()
            && cached == key
        {
            return Ok(report);
        }
        let probe_dir = match self.shared.config.launcher.probe_dir() {
            Some(dir) => dir,
            None => self.shared.research_dir("probe")?,
        };
        let report = discover(&DiscoveryOptions {
            launcher: self.shared.config.launcher.as_ref(),
            probe_dir: &probe_dir,
            project,
        });
        *lock(&self.shared.environments) = Some((key, report.clone()));
        Ok(report)
    }

    /// The application data directory research uses.
    pub(crate) fn data_dir(&self) -> &Path {
        &self.shared.config.data_dir
    }

    fn environment(&self, id: &str) -> Result<RuntimeEnvironment, ResearchError> {
        let guard = lock(&self.shared.environments);
        let (_, report) = guard.as_ref().ok_or_else(|| ResearchError::UnknownEnvironment(id.to_owned()))?;
        report.runtimes.iter().find(|r| r.id == id).cloned().ok_or_else(|| ResearchError::UnknownEnvironment(id.into()))
    }

    // ---- sessions -------------------------------------------------------

    pub async fn start_session(&self, request: StartSessionRequest) -> Result<SessionInfo, ResearchError> {
        let workspace = self.current_workspace()?;
        let trusted = self.shared.config.launcher.workspace_trusted(&workspace.root).unwrap_or(workspace.trusted);
        if !trusted {
            return Err(ResearchError::NotTrusted);
        }
        let cached_for = lock(&self.shared.environments).as_ref().map(|(key, _)| key.clone());
        if cached_for != Some(Some((workspace.root.clone(), true))) {
            self.environments(false).await?;
        }
        let environment = self.environment(&request.environment_id)?;
        let (kind, limit) = match request.owner {
            SessionOwner::User => ("user", self.shared.config.max_user_sessions),
            SessionOwner::Agent { .. } => ("agent", self.shared.config.max_agent_sessions),
            SessionOwner::Workflow { .. } => ("workflow", self.shared.config.max_agent_sessions),
        };
        let live = lock(&self.shared.sessions)
            .values()
            .filter(|s| std::mem::discriminant(&s.owner) == std::mem::discriminant(&request.owner) && s.is_live())
            .count();
        if live >= limit {
            return Err(ResearchError::TooManySessions { owner: kind.into(), limit });
        }
        if let Availability::Unavailable { reason, guidance } = &environment.availability {
            return Err(ResearchError::EnvironmentUnavailable { reason: reason.clone(), guidance: guidance.clone() });
        }
        if environment.kernel.is_none() || environment.host_interpreter.is_none() {
            return Err(ResearchError::EnvironmentUnavailable {
                reason: format!("{} has no usable kernel.", environment.display_name),
                guidance: "Refresh environments.".into(),
            });
        }
        let session = Session::new(&self.shared, environment, request.owner, workspace.root.clone());
        lock(&self.shared.sessions).insert(session.id.clone(), session.clone());
        session.emit_changed();
        match session.launch().await {
            Ok(()) => {
                session.spawn_watchdog();
                Ok(session.info())
            }
            Err(error) => {
                session.stop_process();
                lock(&self.shared.sessions).remove(&session.id);
                self.shared.emit(ResearchEvent::SessionRemoved { session_id: session.id.clone() });
                Err(error)
            }
        }
    }

    pub fn sessions(&self) -> Vec<SessionInfo> {
        let mut infos: Vec<SessionInfo> = lock(&self.shared.sessions).values().map(|s| s.info()).collect();
        infos.sort_by_key(|s| s.started_at_ms);
        infos
    }

    fn session(&self, id: &str) -> Result<Arc<Session>, ResearchError> {
        lock(&self.shared.sessions).get(id).cloned().ok_or_else(|| ResearchError::UnknownSession(id.to_owned()))
    }

    pub async fn interrupt(&self, session_id: &str) -> Result<SessionInfo, ResearchError> {
        let session = self.session(session_id)?;
        session.interrupt().await?;
        Ok(session.info())
    }

    pub async fn restart(&self, session_id: &str) -> Result<SessionInfo, ResearchError> {
        let session = self.session(session_id)?;
        session.restart().await?;
        Ok(session.info())
    }

    pub async fn shutdown(&self, session_id: &str) -> Result<(), ResearchError> {
        let session = self.session(session_id)?;
        session.shutdown().await;
        lock(&self.shared.sessions).remove(session_id);
        self.shared.emit(ResearchEvent::SessionRemoved { session_id: session_id.to_owned() });
        Ok(())
    }

    /// Stop every session (application exit).
    pub async fn shutdown_all(&self) {
        let sessions: Vec<Arc<Session>> = lock(&self.shared.sessions).drain().map(|(_, s)| s).collect();
        for session in sessions {
            session.shutdown().await;
        }
    }

    // ---- runs -----------------------------------------------------------

    pub fn execute(&self, request: ExecuteRequest) -> Result<RunSummary, ResearchError> {
        self.execute_with_inputs(request, &[])
    }

    /// Run code and pin `declared` project files (data the code reads, e.g. an
    /// experiment's declared inputs) in the run's inputs: each is hashed as it
    /// is when the run is dispatched (the open buffer when the file is open).
    /// The kernel itself does not report which files it read; declared inputs
    /// are the person's statement, pinned by version.
    pub fn execute_with_inputs(
        &self,
        request: ExecuteRequest,
        declared: &[WorkspacePath],
    ) -> Result<RunSummary, ResearchError> {
        if declared.len() > MAX_DECLARED_INPUTS {
            return Err(ResearchError::InvalidRequest(format!("at most {MAX_DECLARED_INPUTS} declared inputs")));
        }
        let session = self.session(&request.session_id)?;
        if session.owner != request.requester {
            return Err(ResearchError::OwnerMismatch { session: session.id.clone() });
        }
        // A session runs code only for its own workspace, and only while that
        // workspace is open and trusted.
        let current = self.workspace_root();
        if current.as_deref() != Some(session.root.as_path()) {
            return Err(ResearchError::WorkspaceMismatch {
                session: session.id.clone(),
                root: display_path(&session.root),
            });
        }
        if session.trust_revoked() {
            session.detach(TRUST_REVOKED);
            return Err(ResearchError::NotTrusted);
        }
        let (code, origin, mut inputs) = self.resolve_source(&session, &request.source)?;
        for path in declared {
            if inputs.iter().any(|i| &i.path == path) {
                continue;
            }
            inputs.push(self.pin_declared_input(&session, path)?);
        }
        let unsaved = inputs.iter().filter(|i| self.is_unsaved(&i.path, i.revision)).map(|i| i.path.clone()).collect();
        session.enqueue(code, origin, inputs, unsaved)
    }

    /// Whether `path` at `revision` was an unsaved buffer.
    fn is_unsaved(&self, path: &WorkspacePath, revision: Option<u64>) -> bool {
        let Some(revision) = revision else { return false };
        let documents = &self.shared.config.documents;
        documents
            .find_by_path(path)
            .and_then(|id| documents.info(&id).ok())
            .is_some_and(|info| info.saved_revision != Some(revision))
    }

    /// Hash a declared input as it is now (open buffer first, else the file).
    fn pin_declared_input(&self, session: &Session, path: &WorkspacePath) -> Result<RunInput, ResearchError> {
        let documents = &self.shared.config.documents;
        if let Some(id) = documents.find_by_path(path) {
            let snapshot = documents.snapshot(&id).map_err(|e| ResearchError::InvalidRequest(e.to_string()))?;
            return Ok(RunInput {
                path: path.clone(),
                revision: Some(snapshot.revision),
                content_hash: Some(format!("sha256:{}", sha256_hex(snapshot.text.as_bytes()))),
            });
        }
        let file = resolve_existing(&session.root, path)?;
        let meta = std::fs::metadata(&file).map_err(|e| ResearchError::io("reading a declared input", e))?;
        if !meta.is_file() {
            return Err(ResearchError::InvalidRequest(format!("{path} is not a file")));
        }
        if meta.len() > DECLARED_INPUT_LIMIT {
            return Err(ResearchError::InvalidRequest(format!(
                "{path} is too large to pin by content ({} MiB at most)",
                DECLARED_INPUT_LIMIT / (1024 * 1024)
            )));
        }
        let hash = hash_file(&file).map_err(|e| ResearchError::io("hashing a declared input", e))?;
        Ok(RunInput { path: path.clone(), revision: None, content_hash: Some(format!("sha256:{hash}")) })
    }

    fn resolve_source(
        &self,
        session: &Session,
        source: &RunSource,
    ) -> Result<(String, CodeOrigin, Vec<RunInput>), ResearchError> {
        let documents = &self.shared.config.documents;
        let resolved = match source {
            RunSource::Input { code } => (code.clone(), CodeOrigin::Input, Vec::new()),
            RunSource::Selection { document, revision, range } => {
                let snapshot =
                    documents.snapshot(document).map_err(|e| ResearchError::InvalidRequest(e.to_string()))?;
                if snapshot.revision != *revision {
                    return Err(ResearchError::StaleSource(format!(
                        "the document changed since the selection was made (revision {} is now {}); select again",
                        revision, snapshot.revision
                    )));
                }
                let bytes = utf16_range_to_bytes(&snapshot.text, *range)
                    .map_err(|e| ResearchError::InvalidRequest(format!("invalid selection: {e}")))?;
                if let Some(path) = &snapshot.path {
                    check_language(session.env.language, path.extension())?;
                }
                let inputs = snapshot
                    .path
                    .iter()
                    .map(|path| RunInput {
                        path: path.clone(),
                        revision: Some(snapshot.revision),
                        content_hash: Some(format!("sha256:{}", sha256_hex(snapshot.text.as_bytes()))),
                    })
                    .collect();
                (
                    snapshot.text[bytes].to_owned(),
                    CodeOrigin::Selection { path: snapshot.path.clone(), revision: snapshot.revision, range: *range },
                    inputs,
                )
            }
            RunSource::File { path } if self.shared.config.source_reader.is_some() => {
                check_language(session.env.language, path.extension())?;
                let reader = self.shared.config.source_reader.as_ref().expect("checked");
                let source = reader.read(path).map_err(ResearchError::InvalidRequest)?;
                if !same_dir(&source.root, &session.root) {
                    return Err(ResearchError::WorkspaceMismatch {
                        session: session.id.clone(),
                        root: display_path(&session.root),
                    });
                }
                let input = RunInput {
                    path: path.clone(),
                    revision: source.revision,
                    content_hash: Some(format!("sha256:{}", source.content_hash)),
                };
                (source.text, CodeOrigin::File { path: path.clone(), revision: source.revision }, vec![input])
            }
            RunSource::File { path } => {
                check_language(session.env.language, path.extension())?;
                let (text, revision) = match documents.find_by_path(path) {
                    Some(id) => {
                        let snapshot =
                            documents.snapshot(&id).map_err(|e| ResearchError::InvalidRequest(e.to_string()))?;
                        (snapshot.text.to_string(), Some(snapshot.revision))
                    }
                    None => {
                        let file = resolve_existing(&session.root, path)?;
                        let bytes = std::fs::read(&file).map_err(|e| ResearchError::io("reading the file", e))?;
                        let text = String::from_utf8(bytes)
                            .map_err(|_| ResearchError::InvalidRequest(format!("{path} is not UTF-8 text")))?;
                        (text, None)
                    }
                };
                let input = RunInput {
                    path: path.clone(),
                    revision,
                    content_hash: Some(format!("sha256:{}", sha256_hex(text.as_bytes()))),
                };
                (text, CodeOrigin::File { path: path.clone(), revision }, vec![input])
            }
        };
        if resolved.0.trim().is_empty() {
            return Err(ResearchError::InvalidRequest("there is no code to run".into()));
        }
        if resolved.0.len() > CODE_LIMIT {
            return Err(ResearchError::InvalidRequest("the code is too large to run interactively".into()));
        }
        Ok(resolved)
    }

    /// Cancel a queued run, or interrupt (then restart, if needed) a running one.
    pub async fn cancel_run(&self, run_id: &str) -> Result<(), ResearchError> {
        let session_id = lock(&self.shared.runs)
            .get(run_id)
            .filter(|run| !run.finished)
            .map(|run| run.session_id.clone())
            .ok_or_else(|| ResearchError::UnknownRun(run_id.to_owned()))?;
        let session = self.session(&session_id)?;
        session.cancel(run_id).await
    }

    /// Run history for the current workspace, newest first.
    pub fn runs(&self) -> Result<Vec<RunSummary>, ResearchError> {
        let workspace = self.current_workspace()?;
        let live: HashMap<String, RunSummary> = lock(&self.shared.runs)
            .values()
            .filter(|run| run.root == workspace.root)
            .map(|run| (run.id().to_owned(), run.summary()))
            .collect();
        let mut out: Vec<RunSummary> = live.values().cloned().collect();
        for manifest in store::list_manifests(&workspace.root) {
            if !live.contains_key(&manifest.record.id.0) {
                let owner = manifest.session.owner.clone();
                out.push(crate::runs::summary_of(&settle_orphan(manifest), owner, false));
            }
        }
        out.sort_by(|a, b| b.started_at_ms.cmp(&a.started_at_ms).then_with(|| b.id.0.cmp(&a.id.0)));
        Ok(out)
    }

    pub fn run_manifest(&self, run_id: &str) -> Result<RunManifest, ResearchError> {
        if let Some(run) = lock(&self.shared.runs).get(run_id) {
            if Some(&run.root) != self.workspace_root().as_ref() {
                return Err(ResearchError::UnknownRun(run_id.to_owned()));
            }
            return Ok(run.manifest.clone());
        }
        let workspace = self.current_workspace()?;
        read_manifest(&workspace.root, run_id).map(settle_orphan)
    }

    /// Page through retained output (live runs) or the stored transcript.
    pub fn run_output(&self, run_id: &str, after: Option<u32>, limit: u32) -> Result<RunOutputPage, ResearchError> {
        if let Some(run) = lock(&self.shared.runs).get(run_id) {
            if Some(&run.root) != self.workspace_root().as_ref() {
                return Err(ResearchError::UnknownRun(run_id.to_owned()));
            }
            return Ok(run.page(after, limit));
        }
        let workspace = self.current_workspace()?;
        let manifest = read_manifest(&workspace.root, run_id)?;
        let entries = store::read_transcript(&workspace.root, run_id);
        let last = entries.last().map(|e| e.seq).unwrap_or(0);
        Ok(page_of(run_id, entries.iter(), after, limit, manifest.output.truncated, last))
    }

    /// Resolve an artifact of a run to its file inside the workspace.
    pub fn artifact(&self, run_id: &str, artifact_id: &str) -> Result<(ArtifactRecord, PathBuf), ResearchError> {
        let manifest = self.run_manifest(run_id)?;
        let record = manifest
            .record
            .outputs
            .iter()
            .find(|a| a.id.0 == artifact_id)
            .cloned()
            .ok_or_else(|| ResearchError::UnknownArtifact(artifact_id.to_owned()))?;
        let path = record.path.clone().ok_or_else(|| ResearchError::UnknownArtifact(artifact_id.to_owned()))?;
        if !artifact_belongs_to_run(&path, run_id) {
            return Err(ResearchError::UnknownArtifact(artifact_id.to_owned()));
        }
        // Only artifacts of runs in the open workspace are reachable.
        let current = self.current_workspace()?.root;
        let root = match lock(&self.shared.runs).get(run_id) {
            Some(run) if run.root != current => return Err(ResearchError::UnknownRun(run_id.to_owned())),
            Some(run) => run.root.clone(),
            None => current,
        };
        let file = resolve_existing(&root, &path)?;
        Ok((record, file))
    }

    /// Bytes of an artifact (bounded), for display in the webview.
    pub fn artifact_bytes(&self, run_id: &str, artifact_id: &str) -> Result<(ArtifactRecord, Vec<u8>), ResearchError> {
        let (record, file) = self.artifact(run_id, artifact_id)?;
        let metadata = std::fs::metadata(&file).map_err(|e| ResearchError::io("reading artifact", e))?;
        if metadata.len() > ARTIFACT_SIZE_LIMIT {
            return Err(ResearchError::InvalidRequest("artifact is too large to display".into()));
        }
        let bytes = std::fs::read(&file).map_err(|e| ResearchError::io("reading artifact", e))?;
        Ok((record, bytes))
    }
}

fn check_language(language: RuntimeLanguage, extension: Option<&str>) -> Result<(), ResearchError> {
    let extension = extension.unwrap_or("").to_ascii_lowercase();
    let ok = match language {
        RuntimeLanguage::Python => matches!(extension.as_str(), "py" | "ipy" | "pyw"),
        RuntimeLanguage::R => extension == "r",
        RuntimeLanguage::Julia => extension == "jl",
    };
    if ok {
        Ok(())
    } else {
        Err(ResearchError::InvalidRequest(format!(
            "a .{extension} file cannot run in a {} session",
            match language {
                RuntimeLanguage::Python => "Python",
                RuntimeLanguage::R => "R",
                RuntimeLanguage::Julia => "Julia",
            }
        )))
    }
}

/// Whether two directories are the same after resolving links.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// A canonical path for display (without the Windows `\\?\` verbatim prefix).
pub fn display_path(path: &Path) -> String {
    let text = path.display().to_string();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        text
    }
}

fn abbreviate_home(path: &str) -> String {
    for var in ["USERPROFILE", "HOME"] {
        if let Some(home) = std::env::var_os(var).map(|h| h.to_string_lossy().into_owned())
            && !home.is_empty()
            && path.len() >= home.len()
            && path[..home.len()].eq_ignore_ascii_case(&home)
        {
            return format!("~{}", &path[home.len()..]);
        }
    }
    path.to_owned()
}

// ==========================================================================
// Session

type Pending = oneshot::Sender<Result<Value, ResearchError>>;

struct HelperIo {
    input: Arc<dyn LineSink>,
    control: Arc<dyn ProcessControl>,
}

struct SessionMut {
    state: SessionState,
    io: Option<HelperIo>,
    epoch: u64,
    kernel: Option<KernelDescriptor>,
    kernel_pid: Option<u32>,
    helper_pid: Option<u32>,
    helper_version: Option<String>,
    interpreter: Option<InterpreterProvenance>,
    packages: Vec<ToolVersion>,
    transport_encryption: String,
    interrupt_mode: String,
    active: Option<String>,
    queue: VecDeque<String>,
    generation: u32,
    last_seen: Instant,
    diagnostics: VecDeque<String>,
}

struct Session {
    id: String,
    owner: SessionOwner,
    env: RuntimeEnvironment,
    root: PathBuf,
    started_at_ms: u64,
    shared: Weak<Shared>,
    state: Mutex<SessionMut>,
    pending: Mutex<HashMap<u64, Pending>>,
    next_request: AtomicU64,
    /// Serializes state announcements so the last one sent always describes
    /// the latest state (a snapshot taken on one thread cannot be sent after
    /// a newer one taken on another).
    emitting: Mutex<()>,
}

/// Why a session of a folder whose trust was revoked stopped.
const TRUST_REVOKED: &str = "Workspace trust was revoked; this session's kernel was stopped.";

impl Session {
    fn new(shared: &Arc<Shared>, env: RuntimeEnvironment, owner: SessionOwner, root: PathBuf) -> Arc<Self> {
        let id = format!("session-{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
        Arc::new(Self {
            id,
            owner,
            env,
            root,
            started_at_ms: now_ms(),
            shared: Arc::downgrade(shared),
            state: Mutex::new(SessionMut {
                state: SessionState::Starting,
                io: None,
                epoch: 0,
                kernel: None,
                kernel_pid: None,
                helper_pid: None,
                helper_version: None,
                interpreter: None,
                packages: Vec::new(),
                transport_encryption: "none".into(),
                interrupt_mode: "none".into(),
                active: None,
                queue: VecDeque::new(),
                generation: 0,
                last_seen: Instant::now(),
                diagnostics: VecDeque::new(),
            }),
            pending: Mutex::new(HashMap::new()),
            next_request: AtomicU64::new(1),
            emitting: Mutex::new(()),
        })
    }

    /// The session's folder is no longer trusted (a revoke from any surface).
    /// A revoke kills the helper and kernel before anything else hears of it,
    /// so every path that sees them die asks this first.
    fn trust_revoked(&self) -> bool {
        self.shared().and_then(|shared| shared.config.launcher.workspace_trusted(&self.root)) == Some(false)
    }

    fn shared(&self) -> Option<Arc<Shared>> {
        self.shared.upgrade()
    }

    fn info(&self) -> SessionInfo {
        let state = lock(&self.state);
        SessionInfo {
            id: self.id.clone(),
            owner: self.owner.clone(),
            environment_id: self.env.id.clone(),
            language: self.env.language,
            display_name: self.env.display_name.clone(),
            state: state.state.clone(),
            kernel: state.kernel.clone(),
            kernel_pid: state.kernel_pid,
            helper_pid: state.helper_pid,
            helper_version: state.helper_version.clone(),
            transport_encryption: state.transport_encryption.clone(),
            interrupt_mode: state.interrupt_mode.clone(),
            active_run: state.active.clone().map(RunId),
            queued_runs: state.queue.iter().cloned().map(RunId).collect(),
            started_at_ms: self.started_at_ms,
            generation: state.generation,
        }
    }

    fn emit_changed(&self) {
        if let Some(shared) = self.shared() {
            let _order = lock(&self.emitting);
            shared.emit(ResearchEvent::SessionChanged { session: self.info() });
        }
    }

    /// Change state, except out of Detached or Stopped: those are final (a
    /// restart failing after a trust revoke must not turn Detached into
    /// Crashed).
    fn set_state(&self, state: SessionState) {
        {
            let mut current = lock(&self.state);
            if matches!(current.state, SessionState::Stopped | SessionState::Detached { .. }) {
                return;
            }
            current.state = state;
        }
        self.emit_changed();
    }

    // ---- helper process -------------------------------------------------

    async fn launch(self: &Arc<Self>) -> Result<(), ResearchError> {
        let shared = self.shared().ok_or(ResearchError::HelperGone)?;
        let helper = install_helper(&shared.research_dir("kernel-host")?)?;
        let runtime_dir = shared.research_dir("runtime")?;
        let host = self.env.host_interpreter.clone().ok_or(ResearchError::HelperGone)?;
        let mut extra: BTreeMap<String, String> = self
            .env
            .kernel_environment
            .iter()
            .filter(|(name, _)| !crate::launcher::looks_secret(name))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        // Connection files (with their HMAC keys) stay in an application-owned directory.
        extra.insert("JUPYTER_RUNTIME_DIR".to_owned(), runtime_dir.display().to_string());
        let spec = JobSpec {
            label: format!("research kernel host ({})", self.env.display_name),
            program: host,
            args: vec!["-u".into(), "-X".into(), "utf8".into(), helper.display().to_string()],
            environment: research_environment(extra),
            working_directory: display_path(&self.root),
            policy: ExecutionPolicy::TrustedNative,
            timeout_ms: None,
            log_limit_bytes: Some(64 * 1024),
            stdin: None,
        };
        let process = shared.config.launcher.launch_interactive(spec).await.map_err(|e| match e {
            LaunchError::Untrusted(_) => ResearchError::NotTrusted,
            LaunchError::Refused(message) => ResearchError::PolicyRefused(message),
            other => ResearchError::Launch(other.to_string()),
        })?;
        let epoch = {
            let mut state = lock(&self.state);
            state.epoch += 1;
            state.helper_pid = process.pid;
            state.io = Some(HelperIo { input: process.input.clone(), control: process.control.clone() });
            state.last_seen = Instant::now();
            state.epoch
        };
        self.spawn_reader(process.lines, epoch);
        self.spawn_diagnostics(process.diagnostics);

        let hello = self.request(json!({"cmd": "handshake", "protocol": PROTOCOL_VERSION})).await?;
        let protocol = hello.get("protocol").and_then(Value::as_u64).unwrap_or(0);
        if protocol != u64::from(PROTOCOL_VERSION) {
            return Err(ResearchError::Launch(format!(
                "helper speaks protocol {protocol}; this application needs {PROTOCOL_VERSION}"
            )));
        }
        {
            let mut state = lock(&self.state);
            state.helper_version = hello.get("helperVersion").and_then(Value::as_str).map(str::to_owned);
            let python = hello.get("python").cloned().unwrap_or_default();
            state.interpreter = Some(InterpreterProvenance {
                implementation: python.get("implementation").and_then(Value::as_str).unwrap_or("").to_owned(),
                version: python.get("version").and_then(Value::as_str).unwrap_or("").to_owned(),
                location: abbreviate_home(python.get("executable").and_then(Value::as_str).unwrap_or("")),
            });
            state.packages = hello
                .get("packages")
                .and_then(Value::as_object)
                .map(|packages| {
                    packages
                        .iter()
                        .filter_map(|(name, version)| {
                            version.as_str().map(|v| ToolVersion { name: name.clone(), version: v.to_owned() })
                        })
                        .collect()
                })
                .unwrap_or_default();
        }
        self.start_kernel().await
    }

    async fn start_kernel(self: &Arc<Self>) -> Result<(), ResearchError> {
        let kernel = match self.env.kernel.clone() {
            Some(KernelChoice::Kernelspec { name }) => json!({"kind": "kernelspec", "name": name}),
            _ => json!({"kind": "python"}),
        };
        let started = self.request(json!({"cmd": "start", "kernel": kernel, "cwd": display_path(&self.root)})).await?;
        self.adopt_kernel(&started);
        Ok(())
    }

    fn adopt_kernel(&self, started: &Value) {
        {
            let mut state = lock(&self.state);
            let kernel = started.get("kernel").cloned().unwrap_or_default();
            let text = |key: &str| kernel.get(key).and_then(Value::as_str).unwrap_or("").to_owned();
            state.kernel = Some(KernelDescriptor {
                name: text("name"),
                language: text("language"),
                language_version: text("languageVersion"),
                implementation: text("implementation"),
                implementation_version: text("implementationVersion"),
            });
            state.kernel_pid = started.get("pid").and_then(Value::as_u64).map(|p| p as u32);
            state.transport_encryption =
                started.get("transportEncryption").and_then(Value::as_str).unwrap_or("none").to_owned();
            state.interrupt_mode = started.get("interrupt").and_then(Value::as_str).unwrap_or("none").to_owned();
            state.generation += 1;
            if !matches!(state.state, SessionState::Stopped | SessionState::Detached { .. }) {
                state.state = SessionState::Idle;
            }
        }
        self.emit_changed();
    }

    fn spawn_reader(self: &Arc<Self>, mut lines: mpsc::UnboundedReceiver<String>, epoch: u64) {
        let session = Arc::downgrade(self);
        // A dedicated blocking thread: event handling writes manifests.
        tokio::task::spawn_blocking(move || {
            while let Some(line) = lines.blocking_recv() {
                let Some(session) = session.upgrade() else { return };
                session.on_line(line.trim_end(), epoch);
            }
            if let Some(session) = session.upgrade() {
                session.on_helper_exit(epoch);
            }
        });
    }

    fn spawn_diagnostics(self: &Arc<Self>, diagnostics: Option<mpsc::UnboundedReceiver<String>>) {
        let Some(mut diagnostics) = diagnostics else { return };
        let session = Arc::downgrade(self);
        tokio::spawn(async move {
            while let Some(line) = diagnostics.recv().await {
                let Some(session) = session.upgrade() else { return };
                let mut state = lock(&session.state);
                state.diagnostics.push_back(line.chars().take(2000).collect());
                while state.diagnostics.len() > DIAGNOSTIC_LINES {
                    state.diagnostics.pop_front();
                }
            }
        });
    }

    fn spawn_watchdog(self: &Arc<Self>) {
        let session = Arc::downgrade(self);
        let Some(timeout) = self.shared().map(|s| s.config.heartbeat_timeout) else { return };
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let Some(session) = session.upgrade() else { return };
                if !session.watchdog_tick(timeout) {
                    return;
                }
            }
        });
    }

    /// One watchdog check; false once the session is stopped for good.
    fn watchdog_tick(self: &Arc<Self>, timeout: Duration) -> bool {
        let (stale, stopped, control) = {
            let state = lock(&self.state);
            let watched = matches!(
                state.state,
                SessionState::Idle | SessionState::Busy | SessionState::Restarting | SessionState::Starting
            );
            (
                watched && state.io.is_some() && state.last_seen.elapsed() > timeout,
                matches!(state.state, SessionState::Stopped),
                state.io.as_ref().map(|io| io.control.clone()),
            )
        };
        if stopped {
            return false;
        }
        if stale {
            self.crash("The research helper stopped responding and was stopped. Restart the session.");
            if let Some(control) = control {
                control.kill_tree();
            }
        }
        // Trust revoked for the session's folder: stop its kernel.
        if self.is_live() && self.trust_revoked() {
            self.detach(TRUST_REVOKED);
        }
        true
    }

    /// True while the session has (or is getting) a kernel.
    fn is_live(&self) -> bool {
        !matches!(lock(&self.state).state, SessionState::Stopped | SessionState::Detached { .. })
    }

    /// Stop the kernel and refuse further code: the session's workspace was
    /// closed or switched, or its trust was revoked.
    fn detach(self: &Arc<Self>, reason: &str) {
        let (active, queued, control) = {
            let mut state = lock(&self.state);
            if matches!(state.state, SessionState::Stopped | SessionState::Detached { .. }) {
                return;
            }
            state.state = SessionState::Detached { reason: reason.to_owned() };
            (state.active.take(), state.queue.drain(..).collect::<Vec<_>>(), state.io.take().map(|io| io.control))
        };
        if let Some(control) = control {
            control.kill_tree();
        }
        for (_, tx) in lock(&self.pending).drain() {
            let _ = tx.send(Err(ResearchError::HelperGone));
        }
        if let Some(run) = active {
            self.finish_run(&run, RunStatus::Canceled, None, Some(reason), false);
        }
        for run in queued {
            self.finish_run(&run, RunStatus::Canceled, None, Some(reason), false);
        }
        self.emit_changed();
    }

    fn stop_process(&self) {
        let control = lock(&self.state).io.take().map(|io| io.control);
        if let Some(control) = control {
            control.kill_tree();
        }
    }

    async fn request(&self, mut message: Value) -> Result<Value, ResearchError> {
        let timeout = self.shared().map(|s| s.config.request_timeout).unwrap_or(Duration::from_secs(60));
        let id = self.next_request.fetch_add(1, Ordering::Relaxed);
        message["id"] = json!(id);
        let (tx, rx) = oneshot::channel();
        lock(&self.pending).insert(id, tx);
        let line = message.to_string();
        let written = {
            let state = lock(&self.state);
            match state.io.as_ref() {
                Some(io) => io.input.send_line(&line).is_ok(),
                None => false,
            }
        };
        if !written {
            lock(&self.pending).remove(&id);
            return Err(ResearchError::HelperGone);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(ResearchError::HelperGone),
            Err(_) => {
                lock(&self.pending).remove(&id);
                Err(ResearchError::Timeout(timeout.as_secs()))
            }
        }
    }

    fn on_line(self: &Arc<Self>, line: &str, epoch: u64) {
        {
            let mut state = lock(&self.state);
            if state.epoch != epoch {
                return;
            }
            state.last_seen = Instant::now();
        }
        match parse_line(line) {
            Some(HelperMessage::Response { id, result }) => {
                if let Some(tx) = lock(&self.pending).remove(&id) {
                    let _ = tx.send(
                        result.map_err(|HelperFailure { code, message }| ResearchError::Helper { code, message }),
                    );
                }
            }
            Some(HelperMessage::Event(event)) => self.on_event(event),
            Some(HelperMessage::Unknown(_)) | None => {}
        }
    }

    fn on_helper_exit(self: &Arc<Self>, epoch: u64) {
        let (current, control) = {
            let state = lock(&self.state);
            (state.epoch == epoch, state.io.as_ref().map(|io| io.control.clone()))
        };
        if !current {
            return;
        }
        let code = control.as_ref().and_then(|c| {
            for _ in 0..50 {
                if let Some(exit) = c.try_wait() {
                    return Some(exit.code);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            None
        });
        lock(&self.state).io = None;
        for (_, tx) in lock(&self.pending).drain() {
            let _ = tx.send(Err(ResearchError::HelperGone));
        }
        let stopped = matches!(lock(&self.state).state, SessionState::Stopped | SessionState::Detached { .. });
        if !stopped {
            let detail = match code.flatten() {
                Some(code) => format!(" (exit code {code})"),
                None => String::new(),
            };
            self.crash(&format!("The research helper exited unexpectedly{detail}. Restart the session."));
        }
    }

    /// Mark the session crashed, failing the active run and canceling queued
    /// runs. In a folder whose trust was revoked the helper and kernel were
    /// killed by the revoke, whichever way their death was noticed: the
    /// session is detached for trust instead.
    fn crash(self: &Arc<Self>, reason: &str) {
        if self.trust_revoked() {
            self.detach(TRUST_REVOKED);
            return;
        }
        let (active, queued) = {
            let mut state = lock(&self.state);
            if matches!(
                state.state,
                SessionState::Crashed { .. } | SessionState::Stopped | SessionState::Detached { .. }
            ) {
                return;
            }
            state.state = SessionState::Crashed { reason: reason.to_owned() };
            (state.active.take(), state.queue.drain(..).collect::<Vec<_>>())
        };
        if let Some(run) = active {
            self.finish_run(&run, RunStatus::Failed { message: reason.to_owned() }, None, None, false);
        }
        for run in queued {
            self.finish_run(
                &run,
                RunStatus::Canceled,
                None,
                Some("The session crashed before this run started."),
                false,
            );
        }
        self.emit_changed();
    }

    // ---- events ---------------------------------------------------------

    fn on_event(self: &Arc<Self>, event: HelperEvent) {
        let Some(shared) = self.shared() else { return };
        match event {
            HelperEvent::Heartbeat { .. } | HelperEvent::KernelStatus { .. } => {}
            HelperEvent::Log { level, message } => {
                let mut state = lock(&self.state);
                state.diagnostics.push_back(format!("[{level}] {}", message.chars().take(2000).collect::<String>()));
                while state.diagnostics.len() > DIAGNOSTIC_LINES {
                    state.diagnostics.pop_front();
                }
            }
            HelperEvent::RunStarted { run_id, execution_count } => {
                if let Some(run) = lock(&shared.runs).get_mut(&run_id) {
                    run.manifest.execution_count = execution_count;
                }
                shared.emit(ResearchEvent::RunStarted {
                    run_id: RunId(run_id),
                    session_id: self.id.clone(),
                    execution_count,
                });
            }
            HelperEvent::Stream { run_id, name, text } => {
                let name = if name == "stderr" { StreamName::Stderr } else { StreamName::Stdout };
                self.push_output(&shared, &run_id, OutputItem::Stream { name, text });
            }
            HelperEvent::Display { run_id, output_type, execution_count, text, files } => {
                let artifacts = {
                    // Text artifacts are kernel output too: redacted like streams.
                    let redact = |text: &str| shared.config.launcher.redact(text);
                    let mut runs = lock(&shared.runs);
                    let Some(run) = runs.get_mut(&run_id) else { return };
                    files.iter().filter_map(|f| run.register_file(f, &redact)).map(|a| a.id).collect()
                };
                if files.iter().any(|f| f.truncated) {
                    self.push_output(
                        &shared,
                        &run_id,
                        OutputItem::Notice {
                            message:
                                "The table was cut to 500 rows and 50 columns; the HTML output keeps the original."
                                    .into(),
                        },
                    );
                }
                self.push_output(
                    &shared,
                    &run_id,
                    OutputItem::Display { output_type, execution_count, text, artifacts },
                );
            }
            HelperEvent::ClearOutput { run_id, wait } => {
                self.push_output(&shared, &run_id, OutputItem::ClearOutput { wait });
            }
            HelperEvent::Error { run_id, ename, evalue, traceback } => {
                if let Some(run) = lock(&shared.runs).get_mut(&run_id) {
                    let redact = |text: &str| shared.config.launcher.redact(text);
                    run.manifest.error = Some(RunErrorInfo { ename: redact(&ename), evalue: redact(&evalue) });
                }
                self.push_output(&shared, &run_id, OutputItem::Error { ename, evalue, traceback });
            }
            HelperEvent::RunFinished { run_id, status, reason, execution_count, stream_dropped, files_dropped } => {
                let (interrupted, canceled, ename) = {
                    let mut runs = lock(&shared.runs);
                    let Some(run) = runs.get_mut(&run_id) else { return };
                    run.manifest.output.dropped_stream_chars += stream_dropped;
                    run.manifest.output.dropped_files += files_dropped;
                    (
                        run.interrupt_requested,
                        run.cancel_requested,
                        run.manifest.error.as_ref().map(|e| (e.ename.clone(), e.evalue.clone())),
                    )
                };
                let status_text = status;
                // A run lost because a trust revoke killed its kernel was canceled, not failed.
                let revoked = status_text == "lost" && self.trust_revoked();
                let status = if canceled || revoked {
                    RunStatus::Canceled
                } else {
                    match status_text.as_str() {
                        "ok" => RunStatus::Succeeded,
                        "aborted" => RunStatus::Canceled,
                        "lost" => match reason.as_deref() {
                            Some("kernelDied") => {
                                RunStatus::Failed { message: "The kernel died during this run.".into() }
                            }
                            Some("restarted") | Some("shutdown") => RunStatus::Canceled,
                            _ => RunStatus::Failed { message: "The research helper stopped.".into() },
                        },
                        _ => match ename {
                            Some((name, _)) if name == "KeyboardInterrupt" || interrupted => RunStatus::Interrupted,
                            Some((name, value)) => RunStatus::Failed { message: format!("{name}: {value}") },
                            None => RunStatus::Failed { message: "The run failed.".into() },
                        },
                    }
                };
                let notice = match (stream_dropped, files_dropped) {
                    _ if revoked => Some(TRUST_REVOKED.to_owned()),
                    (0, 0) => None,
                    _ => Some(format!(
                        "Output limit reached: {stream_dropped} characters and {files_dropped} files were not kept."
                    )),
                };
                let lost = matches!(status_text.as_str(), "lost");
                let was_active = {
                    let mut state = lock(&self.state);
                    if state.active.as_deref() == Some(run_id.as_str()) {
                        state.active = None;
                        true
                    } else {
                        false
                    }
                };
                // A lost run (kernel died/restarted) must not start queued work;
                // the crash or restart path settles the queue.
                self.finish_run(&run_id, status, execution_count, notice.as_deref(), was_active && !lost);
            }
            HelperEvent::KernelDied { exit_code, .. } => {
                let detail = exit_code.map(|c| format!(" (exit code {c})")).unwrap_or_default();
                self.crash(&format!(
                    "The kernel process exited{detail}. Restart the session to continue with a fresh kernel."
                ));
            }
        }
    }

    fn push_output(&self, shared: &Shared, run_id: &str, item: OutputItem) {
        // Kernel output is untrusted text: redact secrets before it is kept,
        // written to the transcript, or sent to the webview.
        let redact = |text: String| shared.config.launcher.redact(&text);
        let item = match item {
            OutputItem::Stream { name, text } => OutputItem::Stream { name, text: redact(text) },
            OutputItem::Display { output_type, execution_count, text, artifacts } => {
                OutputItem::Display { output_type, execution_count, text: text.map(redact), artifacts }
            }
            OutputItem::Error { ename, evalue, traceback } => OutputItem::Error {
                ename: redact(ename),
                evalue: redact(evalue),
                traceback: traceback.into_iter().map(redact).collect(),
            },
            other => other,
        };
        let entry = {
            let mut runs = lock(&shared.runs);
            let Some(run) = runs.get_mut(run_id) else { return };
            run.push(item)
        };
        shared.emit(ResearchEvent::Output { run_id: RunId(run_id.to_owned()), entry });
    }

    // ---- runs -----------------------------------------------------------

    fn enqueue(
        self: &Arc<Self>,
        code: String,
        origin: CodeOrigin,
        inputs: Vec<RunInput>,
        unsaved_inputs: Vec<WorkspacePath>,
    ) -> Result<RunSummary, ResearchError> {
        let shared = self.shared().ok_or(ResearchError::HelperGone)?;
        // The session lock is held until the run is registered and placed, so
        // concurrent requests cannot both become the active run.
        let mut state = lock(&self.state);
        let (provenance, environment) = {
            match &state.state {
                SessionState::Idle | SessionState::Busy => {}
                other => {
                    let (name, hint) = match other {
                        SessionState::Crashed { .. } => ("crashed", "restart the session to continue"),
                        SessionState::Detached { .. } => {
                            ("detached from its folder", "start a new session for the open folder")
                        }
                        SessionState::Starting | SessionState::Restarting => ("starting", "wait until it is ready"),
                        _ => ("stopped", "start a new session"),
                    };
                    return Err(ResearchError::SessionNotReady {
                        session: self.id.clone(),
                        state: name.into(),
                        hint: hint.into(),
                    });
                }
            }
            let kernel = state.kernel.clone().unwrap_or_default();
            let provenance = SessionProvenance {
                session_id: self.id.clone(),
                owner: self.owner.clone(),
                environment_id: self.env.id.clone(),
                generation: state.generation,
                interpreter: state.interpreter.clone().unwrap_or(InterpreterProvenance {
                    implementation: String::new(),
                    version: String::new(),
                    location: String::new(),
                }),
                kernel: kernel.clone(),
                helper_version: state.helper_version.clone().unwrap_or_else(|| HELPER_VERSION.to_owned()),
                protocol: PROTOCOL_VERSION,
            };
            let mut environment =
                vec![ToolVersion { name: "exposition-kernel-host".into(), version: provenance.helper_version.clone() }];
            if !kernel.language.is_empty() {
                environment
                    .push(ToolVersion { name: kernel.language.clone(), version: kernel.language_version.clone() });
            }
            if !kernel.implementation.is_empty() {
                environment.push(ToolVersion {
                    name: format!("kernel:{}", kernel.implementation),
                    version: kernel.implementation_version.clone(),
                });
            }
            environment.extend(self.env.packages.iter().filter(|p| p.name == "r" || p.name == "julia").cloned());
            environment.extend(state.packages.iter().cloned());
            (provenance, environment)
        };
        let dispatch_now = state.active.is_none();
        let run_id = new_run_id();
        let code_bytes = code.len();
        let manifest = RunManifest {
            format: MANIFEST_FORMAT.into(),
            record: RunRecord {
                id: RunId(run_id.clone()),
                producer: Producer::KernelSession {
                    session_id: self.id.clone(),
                    runtime: self.env.language.as_str().to_owned(),
                },
                inputs,
                environment,
                outputs: Vec::new(),
                status: RunStatus::Running,
                started_at_ms: now_ms(),
                finished_at_ms: None,
            },
            session: provenance,
            code: CodeProvenance {
                origin,
                sha256: sha256_hex(code.as_bytes()),
                bytes: code_bytes as u64,
                text: (code_bytes <= MANIFEST_CODE_LIMIT).then(|| code.clone()),
            },
            execution_count: None,
            error: None,
            output: OutputSummary::default(),
            unsaved_inputs,
            execution: None,
        };
        let run = RunState {
            manifest,
            session_id: self.id.clone(),
            owner: self.owner.clone(),
            root: self.root.clone(),
            code,
            queued: !dispatch_now,
            output_dir: None,
            entries: VecDeque::new(),
            next_seq: 0,
            retained_chars: 0,
            interrupt_requested: false,
            cancel_requested: false,
            finished: false,
        };
        let summary = run.summary();
        lock(&shared.runs).insert(run_id.clone(), run);
        if dispatch_now {
            state.active = Some(run_id.clone());
            state.state = SessionState::Busy;
        } else {
            state.queue.push_back(run_id.clone());
        }
        drop(state);
        shared.emit(ResearchEvent::RunQueued { run: summary.clone() });
        self.emit_changed();
        if dispatch_now {
            self.spawn_dispatch(run_id);
        }
        Ok(summary)
    }

    fn spawn_dispatch(self: &Arc<Self>, run_id: String) {
        let session = self.clone();
        tokio::spawn(async move { session.dispatch(run_id).await });
    }

    async fn dispatch(self: Arc<Self>, run_id: String) {
        let Some(shared) = self.shared() else { return };
        let prepared = (|| {
            let dir = ensure_dir_within(&self.root, &artifact_dir_path(&run_id))?;
            let mut runs = lock(&shared.runs);
            let run = runs.get_mut(&run_id).ok_or_else(|| ResearchError::UnknownRun(run_id.clone()))?;
            run.queued = false;
            run.output_dir = Some(dir.clone());
            run.manifest.record.started_at_ms = now_ms();
            write_manifest(&self.root, &run.manifest)?;
            Ok::<_, ResearchError>((run.code.clone(), dir, run.summary()))
        })();
        let (code, dir, summary) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                self.fail_dispatch(&run_id, error);
                return;
            }
        };
        shared.emit(ResearchEvent::RunQueued { run: summary });
        let result = self
            .request(json!({
                "cmd": "execute",
                "run_id": run_id,
                "code": code,
                "output_dir": dir.display().to_string(),
            }))
            .await;
        if let Err(error) = result {
            self.fail_dispatch(&run_id, error);
        }
    }

    fn fail_dispatch(self: &Arc<Self>, run_id: &str, error: ResearchError) {
        let was_active = {
            let mut state = lock(&self.state);
            if state.active.as_deref() == Some(run_id) {
                state.active = None;
                true
            } else {
                false
            }
        };
        self.finish_run(run_id, RunStatus::Failed { message: error.to_string() }, None, None, was_active);
    }

    /// Finalize a run: status, manifest, transcript, events; then start the next queued run.
    fn finish_run(
        self: &Arc<Self>,
        run_id: &str,
        status: RunStatus,
        execution_count: Option<u32>,
        notice: Option<&str>,
        dispatch_next: bool,
    ) {
        let Some(shared) = self.shared() else { return };
        let summary = {
            let mut runs = lock(&shared.runs);
            let Some(run) = runs.get_mut(run_id) else { return };
            if run.finished {
                return;
            }
            if let Some(message) = notice {
                let entry = run.push(OutputItem::Notice { message: message.to_owned() });
                shared.emit(ResearchEvent::Output { run_id: RunId(run_id.to_owned()), entry });
            }
            run.finished = true;
            run.queued = false;
            run.manifest.record.status = status;
            run.manifest.record.finished_at_ms = Some(now_ms());
            if execution_count.is_some() {
                run.manifest.execution_count = execution_count;
            }
            let entries: Vec<_> = run.entries.iter().cloned().collect();
            let _ = write_manifest(&run.root, &run.manifest);
            let _ = write_transcript(&run.root, run_id, &entries);
            run.summary()
        };
        shared.emit(ResearchEvent::RunFinished { run: summary });
        if dispatch_next {
            self.dispatch_next();
        } else {
            self.emit_changed();
        }
    }

    fn dispatch_next(self: &Arc<Self>) {
        let next = {
            let mut state = lock(&self.state);
            if state.active.is_some() || !matches!(state.state, SessionState::Idle | SessionState::Busy) {
                None
            } else {
                let next = state.queue.pop_front();
                state.active = next.clone();
                state.state = if next.is_some() { SessionState::Busy } else { SessionState::Idle };
                next
            }
        };
        self.emit_changed();
        if let Some(next) = next {
            self.spawn_dispatch(next);
        }
    }

    async fn interrupt(self: &Arc<Self>) -> Result<(), ResearchError> {
        let active = {
            let state = lock(&self.state);
            if !matches!(state.state, SessionState::Idle | SessionState::Busy) {
                return Err(ResearchError::SessionNotReady {
                    session: self.id.clone(),
                    state: "not running".into(),
                    hint: "restart the session".into(),
                });
            }
            state.active.clone()
        };
        if let (Some(run_id), Some(shared)) = (&active, self.shared()) {
            if let Some(run) = lock(&shared.runs).get_mut(run_id) {
                run.interrupt_requested = true;
            }
            let message = if lock(&self.state).interrupt_mode == "unsupported" {
                "Interrupt requested, but this kernel does not support interrupts on this platform; use Cancel to stop the run (it restarts the kernel)."
            } else {
                "Interrupt requested."
            };
            self.push_output(&shared, run_id, OutputItem::Notice { message: message.into() });
        }
        self.request(json!({"cmd": "interrupt"})).await.map(|_| ())
    }

    async fn cancel(self: &Arc<Self>, run_id: &str) -> Result<(), ResearchError> {
        let (queued, active) = {
            let mut state = lock(&self.state);
            let position = state.queue.iter().position(|r| r == run_id);
            if let Some(index) = position {
                state.queue.remove(index);
            }
            (position.is_some(), state.active.as_deref() == Some(run_id))
        };
        if queued {
            self.finish_run(run_id, RunStatus::Canceled, None, Some("Canceled before it started."), false);
            self.emit_changed();
            return Ok(());
        }
        if !active {
            return Err(ResearchError::UnknownRun(run_id.to_owned()));
        }
        let shared = self.shared().ok_or(ResearchError::HelperGone)?;
        if let Some(run) = lock(&shared.runs).get_mut(run_id) {
            run.cancel_requested = true;
        }
        self.push_output(&shared, run_id, OutputItem::Notice { message: "Cancel requested.".into() });
        let _ = self.request(json!({"cmd": "interrupt"})).await;
        // Escalate: if the run ignores the interrupt, restart the kernel.
        let grace = shared.config.cancel_grace;
        let session = self.clone();
        let run_id = run_id.to_owned();
        tokio::spawn(async move {
            tokio::time::sleep(grace).await;
            let still_active = lock(&session.state).active.as_deref() == Some(run_id.as_str());
            if still_active {
                if let Some(shared) = session.shared() {
                    session.push_output(
                        &shared,
                        &run_id,
                        OutputItem::Notice {
                            message: "The run did not stop after the interrupt; restarting the kernel.".into(),
                        },
                    );
                }
                let _ = session.restart().await;
            }
        });
        Ok(())
    }

    async fn restart(self: &Arc<Self>) -> Result<(), ResearchError> {
        let (queued, helper_alive) = {
            let mut state = lock(&self.state);
            if let SessionState::Detached { .. } = state.state {
                return Err(ResearchError::SessionNotReady {
                    session: self.id.clone(),
                    state: "detached from its folder".into(),
                    hint: "start a new session for the open folder".into(),
                });
            }
            if matches!(state.state, SessionState::Stopped | SessionState::Starting | SessionState::Restarting) {
                return Err(ResearchError::SessionNotReady {
                    session: self.id.clone(),
                    state: "busy starting or stopped".into(),
                    hint: "wait or start a new session".into(),
                });
            }
            state.state = SessionState::Restarting;
            let alive = state.io.as_ref().is_some_and(|io| io.control.try_wait().is_none());
            (state.queue.drain(..).collect::<Vec<_>>(), alive)
        };
        self.emit_changed();
        for run in queued {
            self.finish_run(
                &run,
                RunStatus::Canceled,
                None,
                Some("The session was restarted before this run started."),
                false,
            );
        }
        let result = if helper_alive {
            match self.request(json!({"cmd": "restart"})).await {
                Ok(started) => {
                    self.adopt_kernel(&started);
                    Ok(())
                }
                Err(ResearchError::HelperGone) => self.relaunch().await,
                Err(error) => Err(error),
            }
        } else {
            self.relaunch().await
        };
        // Any run that was active is finished by the helper's run_finished("lost");
        // make sure nothing stays marked active after a restart.
        let stale_active = lock(&self.state).active.take();
        if let Some(run) = stale_active {
            self.finish_run(&run, RunStatus::Canceled, None, Some("The session was restarted."), false);
        }
        match result {
            Ok(()) => {
                self.set_state(SessionState::Idle);
                Ok(())
            }
            Err(error) => {
                self.set_state(SessionState::Crashed { reason: format!("Restart failed: {error}") });
                Err(error)
            }
        }
    }

    async fn relaunch(self: &Arc<Self>) -> Result<(), ResearchError> {
        self.stop_process();
        for (_, tx) in lock(&self.pending).drain() {
            let _ = tx.send(Err(ResearchError::HelperGone));
        }
        self.launch().await
    }

    async fn shutdown(self: &Arc<Self>) {
        let (queued, control) = {
            let mut state = lock(&self.state);
            state.state = SessionState::Stopped;
            (state.queue.drain(..).collect::<Vec<_>>(), state.io.as_ref().map(|io| io.control.clone()))
        };
        for run in queued {
            self.finish_run(&run, RunStatus::Canceled, None, Some("The session was shut down."), false);
        }
        let _ = tokio::time::timeout(Duration::from_secs(20), self.request(json!({"cmd": "shutdown"}))).await;
        if let Some(control) = control {
            for _ in 0..50 {
                if control.try_wait().is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            control.kill_tree();
        }
        lock(&self.state).io = None;
        let active = lock(&self.state).active.take();
        if let Some(run) = active {
            self.finish_run(&run, RunStatus::Canceled, None, Some("The session was shut down."), false);
        }
        self.emit_changed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_paths_drop_the_verbatim_prefix() {
        assert_eq!(display_path(Path::new(r"\\?\C:\Users\x\project")), r"C:\Users\x\project");
        assert_eq!(display_path(Path::new(r"\\?\UNC\server\share\p")), r"\\server\share\p");
        assert_eq!(display_path(Path::new("/home/x/p")), "/home/x/p");
    }

    // ---- trust revocation races ----------------------------------------
    //
    // A revoke kills the session's helper and kernel (the execution service
    // stops the folder's jobs) and the session notices through several paths
    // in any order: the helper reporting its kernel died, the helper's stdout
    // closing, the watchdog seeing the folder untrusted, or a restart in
    // flight failing. Whatever the order, the session must end Detached with
    // the trust reason and never pass through Crashed.

    use std::sync::atomic::AtomicBool;

    use crate::launcher::{ExitInfo, InputClosed, LaunchFuture, ProbeOutput};

    const KERNEL_DIED: &str = r#"{"type":"event","event":"kernel_died","exitCode":1,"runs":[]}"#;

    struct FakeLauncher {
        trusted: AtomicBool,
        /// Held by a test to keep a relaunch in flight.
        launch_gate: tokio::sync::Mutex<()>,
        launching: tokio::sync::Notify,
    }

    impl KernelProcessLauncher for FakeLauncher {
        fn launch_interactive(&self, _spec: JobSpec) -> LaunchFuture<'_> {
            Box::pin(async move {
                self.launching.notify_one();
                let _gate = self.launch_gate.lock().await;
                Err(LaunchError::Untrusted("the workspace is not trusted".into()))
            })
        }
        fn run_probe(&self, _spec: &JobSpec) -> Result<ProbeOutput, LaunchError> {
            Err(LaunchError::Refused("no probes in this test".into()))
        }
        fn run_project_probe(&self, _spec: &JobSpec) -> Result<ProbeOutput, LaunchError> {
            Err(LaunchError::Refused("no probes in this test".into()))
        }
        fn workspace_trusted(&self, _root: &Path) -> Option<bool> {
            Some(self.trusted.load(Ordering::SeqCst))
        }
    }

    /// The helper and kernel were killed: the process has exited.
    struct DeadProcess;

    impl ProcessControl for DeadProcess {
        fn kill_tree(&self) {}
        fn try_wait(&self) -> Option<ExitInfo> {
            Some(ExitInfo { code: Some(1) })
        }
    }

    struct ClosedInput;

    impl LineSink for ClosedInput {
        fn send_line(&self, _line: &str) -> Result<(), InputClosed> {
            Err(InputClosed)
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        launcher: Arc<FakeLauncher>,
        _service: ResearchService,
        session: Arc<Session>,
        events: broadcast::Receiver<ResearchEvent>,
    }

    impl Fixture {
        /// A live (idle) session of a trusted folder whose helper was just killed.
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("project");
            std::fs::create_dir_all(&root).unwrap();
            let launcher = Arc::new(FakeLauncher {
                trusted: AtomicBool::new(true),
                launch_gate: tokio::sync::Mutex::new(()),
                launching: tokio::sync::Notify::new(),
            });
            let config =
                ResearchConfig::new(dir.path().join("data"), launcher.clone(), Arc::new(DocumentService::new()));
            let service = ResearchService::new(config);
            service.set_workspace(&root, true).unwrap();
            let env = RuntimeEnvironment {
                id: "python:test".into(),
                language: RuntimeLanguage::Python,
                display_name: "Python (test)".into(),
                executable: None,
                version: None,
                kernel: None,
                host_interpreter: Some("python".into()),
                packages: Vec::new(),
                kernel_environment: BTreeMap::new(),
                availability: Availability::Available,
            };
            let root = service.workspace_root().unwrap();
            let session = Session::new(&service.shared, env, SessionOwner::User, root);
            lock(&service.shared.sessions).insert(session.id.clone(), session.clone());
            {
                let mut state = lock(&session.state);
                state.state = SessionState::Idle;
                state.epoch = 1;
                state.io = Some(HelperIo { input: Arc::new(ClosedInput), control: Arc::new(DeadProcess) });
            }
            let events = service.subscribe();
            Fixture { _dir: dir, launcher, _service: service, session, events }
        }

        fn revoke(&self) {
            self.launcher.trusted.store(false, Ordering::SeqCst);
        }

        fn kernel_died(&self) {
            self.session.on_line(KERNEL_DIED, 1);
        }

        fn helper_exited(&self) {
            self.session.on_helper_exit(1);
        }

        fn watchdog(&self) {
            self.session.watchdog_tick(Duration::from_secs(20));
        }

        /// Every state the session announced, in order.
        fn announced(&mut self) -> Vec<SessionState> {
            let mut states = Vec::new();
            while let Ok(event) = self.events.try_recv() {
                if let ResearchEvent::SessionChanged { session } = event {
                    states.push(session.state);
                }
            }
            states
        }

        fn assert_detached_for_trust(&mut self, order: &str) {
            let announced = self.announced();
            assert!(
                !announced.iter().any(|s| matches!(s, SessionState::Crashed { .. })),
                "{order}: the session passed through Crashed: {announced:?}"
            );
            let last = announced.last().cloned();
            for state in [Some(self.session.info().state), last] {
                match state {
                    Some(SessionState::Detached { reason }) => {
                        assert!(reason.contains("trust was revoked"), "{order}: {reason}")
                    }
                    other => panic!("{order}: expected Detached for trust, got {other:?} (announced {announced:?})"),
                }
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revoke_then_kernel_death_then_helper_exit_then_watchdog() {
        let mut f = Fixture::new();
        f.revoke();
        f.kernel_died();
        f.helper_exited();
        f.watchdog();
        f.assert_detached_for_trust("kernel death first");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revoke_then_helper_exit_then_kernel_death() {
        let mut f = Fixture::new();
        f.revoke();
        f.helper_exited();
        f.kernel_died();
        f.watchdog();
        f.assert_detached_for_trust("helper exit first");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revoke_then_watchdog_first() {
        let mut f = Fixture::new();
        f.revoke();
        f.watchdog();
        f.kernel_died();
        f.helper_exited();
        f.assert_detached_for_trust("watchdog first");
    }

    /// The kernel died while the folder was still trusted (a real crash),
    /// then trust was revoked: the session ends Detached for trust.
    #[tokio::test(flavor = "multi_thread")]
    async fn death_seen_before_the_revoke_still_ends_detached_for_trust() {
        let mut f = Fixture::new();
        f.kernel_died();
        assert!(matches!(f.session.info().state, SessionState::Crashed { .. }), "a real crash is reported");
        f.announced();
        f.revoke();
        f.helper_exited();
        f.watchdog();
        f.assert_detached_for_trust("death before the revoke");
    }

    /// A restart in flight when trust is revoked fails afterwards; its failure
    /// must not replace the Detached state with Crashed.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_restart_failing_after_the_revoke_keeps_the_trust_reason() {
        let mut f = Fixture::new();
        let gate = f.launcher.launch_gate.lock().await;
        let launching = f.launcher.launching.notified();
        let session = f.session.clone();
        let restart = tokio::spawn(async move { session.restart().await });
        launching.await;
        f.revoke();
        f.watchdog();
        drop(gate);
        assert!(restart.await.unwrap().is_err());
        let announced = f.announced();
        let after_revoke: Vec<_> =
            announced.iter().skip_while(|s| !matches!(s, SessionState::Detached { .. })).cloned().collect();
        assert!(
            !after_revoke.iter().any(|s| matches!(s, SessionState::Crashed { .. })),
            "Crashed after the trust detach: {announced:?}"
        );
        match f.session.info().state {
            SessionState::Detached { reason } => assert!(reason.contains("trust was revoked"), "{reason}"),
            other => panic!("expected Detached for trust, got {other:?}"),
        }
    }

    #[test]
    fn language_checks_file_extensions() {
        assert!(check_language(RuntimeLanguage::Python, Some("py")).is_ok());
        assert!(check_language(RuntimeLanguage::Python, Some("typ")).is_err());
        assert!(check_language(RuntimeLanguage::R, Some("R")).is_ok());
        assert!(check_language(RuntimeLanguage::Julia, Some("jl")).is_ok());
        assert!(check_language(RuntimeLanguage::Julia, None).is_err());
    }
}
