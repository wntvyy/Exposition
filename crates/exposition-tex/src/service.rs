//! The TeX build service used by the application.
//!
//! * **Execution.** Every tool runs through the shared [`ExecutionService`]
//!   ([`ExecutionRunner`]). LaTeX builds require a **trusted workspace**: TeX
//!   can read any file the user can read (see [`crate::compile`]), so an
//!   untrusted workspace is refused up front — with the execution service's
//!   `untrustedWorkspace` refusal, before anything is written or run — and the
//!   service itself refuses any native job there as a second line. There is
//!   no fallback to another runner.
//! * **Workspace.** Builds always target the currently open workspace of the
//!   [`WorkspaceService`]; each build compiles one atomic
//!   [`exposition_workspace::WorkspaceSnapshot`] (open buffers as they would
//!   be saved, disk files through its path checks). Without an explicit target
//!   the default LaTeX target of `.exposition/project.json` is used.
//! * **Build roots.** Per workspace under `<data_dir>/tex/<hash>`, registered
//!   with the execution service as an application-owned directory owned by
//!   that workspace (so its jobs follow the workspace's trust).
//! * **Scheduling.** One worker per target (workspace, entry, engine). A new
//!   request coalesces with any pending one and cancels the running build
//!   (its snapshot is superseded). Draft builds (one engine pass) serve typing;
//!   after a draft, a Full build (bibliography + reruns) runs automatically
//!   once the target has been idle for `idle_full_delay`.
//! * **Results.** Core [`BuildResult`]s (diagnostics for the Problems view),
//!   delivered as [`TexEvent`]s. The last output-producing build is kept as
//!   *last good* and reported stale while newer edits are pending or its
//!   source revisions no longer match the open documents.
//! * **Pages.** Immutable per build: PNG renderings and glyph mappings are
//!   served from the build's frozen output through [`TexPageSession`].

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use exposition_core::build::{BuildId, BuildResult, BuildStatus, BuildTarget, DocumentFormat, Engine};
use exposition_core::path::WorkspacePath;
use exposition_execution::toolchains::Located;
use exposition_execution::{ExecutionService, JobRefusal, RefusalCode};
use exposition_workspace::WorkspaceService;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use ts_rs::TS;

use crate::compile::{BuildMode, TexBuild, TexBuildRequest, TexBuilder, hex_sha256};
use crate::mapping::PageMapping;
use crate::runner::{CancelToken, ExecutionRunner};
use crate::session::TexPageSession;
use crate::toolchain::{TexTool, TexToolchain};

/// How much a build does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum TexBuildMode {
    /// One engine pass reusing the previous `.aux`/`.bbl` (while typing).
    Draft,
    /// Bibliography and reruns until cross-references settle.
    Full,
}

impl From<TexBuildMode> for BuildMode {
    fn from(mode: TexBuildMode) -> Self {
        match mode {
            TexBuildMode::Draft => BuildMode::Draft,
            TexBuildMode::Full => BuildMode::Full,
        }
    }
}

/// Identifies one build target of one workspace.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TexTargetRef {
    /// Canonical workspace root.
    pub root: String,
    pub entry: WorkspacePath,
    pub engine: Engine,
}

/// One tool run of a build.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TexPass {
    pub tool: String,
    pub duration_ms: u64,
    pub exit_code: Option<i32>,
}

/// A finished build as reported to the application.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TexBuildSummary {
    pub result: BuildResult,
    pub mode: TexBuildMode,
    /// Started by the idle scheduler rather than a request.
    pub automatic: bool,
    /// Canceled because a newer request superseded its snapshot.
    pub superseded: bool,
    /// Why execution was refused (e.g. `untrustedWorkspace`).
    pub refusal: Option<JobRefusal>,
    pub passes: Vec<TexPass>,
    /// The build produced pages that can be shown and mapped.
    pub has_output: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum TexEvent {
    Queued { target: TexTargetRef, mode: TexBuildMode },
    Started { target: TexTargetRef, build: BuildId, mode: TexBuildMode, automatic: bool },
    Finished { target: TexTargetRef, summary: Box<TexBuildSummary> },
}

/// Current state of a target.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TexTargetStatus {
    pub target: TexTargetRef,
    pub running: Option<TexBuildMode>,
    pub pending: Option<TexBuildMode>,
    pub last: Option<TexBuildSummary>,
    /// The latest build with pages (kept while newer builds fail or run).
    pub last_good: Option<TexBuildSummary>,
    /// `last_good` no longer reflects the documents: a build is pending or
    /// running, or an open document it compiled has changed since.
    pub stale: bool,
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum TexServiceError {
    #[error("only LaTeX targets built with pdfLaTeX or LuaLaTeX are supported")]
    UnsupportedTarget,
    #[error("no workspace is open")]
    WorkspaceNotOpen,
    #[error(
        "no LaTeX target: add one to .exposition/project.json (targets/defaultTarget) or set a .tex entry document"
    )]
    NoTarget,
    #[error("no such build (it may have been evicted)")]
    UnknownBuild,
    #[error("the build has no pages")]
    NoOutput,
    #[error("page {0} does not exist")]
    NoSuchPage(u32),
    #[error("invalid scale {0}")]
    InvalidScale(f32),
    #[error("{0}")]
    Io(String),
}

pub struct TexServiceConfig {
    /// Application data directory; builds live in `<data_dir>/tex`.
    pub data_dir: PathBuf,
    /// Idle time after a draft before the automatic full build (`None`: off).
    pub idle_full_delay: Option<Duration>,
    /// Finished builds kept for page access (besides each target's last good).
    pub keep_builds: usize,
}

impl TexServiceConfig {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self { data_dir: data_dir.into(), idle_full_delay: Some(Duration::from_millis(1500)), keep_builds: 8 }
    }
}

/// The TeX build service. Cheap to clone.
#[derive(Clone)]
pub struct TexService {
    inner: Arc<Inner>,
}

struct Inner {
    exec: ExecutionService,
    workspace: Arc<WorkspaceService>,
    runtime: tokio::runtime::Handle,
    root: PathBuf,
    idle_full_delay: Option<Duration>,
    keep_builds: usize,
    targets: Mutex<HashMap<TexTargetRef, Arc<Target>>>,
    builds: Mutex<BuildStore>,
    events: broadcast::Sender<TexEvent>,
    counter: std::sync::atomic::AtomicU64,
}

struct Target {
    reference: TexTargetRef,
    root: PathBuf,
    builder: TexBuilder<ExecutionRunner>,
    cancel: CancelToken,
    control: Mutex<Control>,
    wake: Condvar,
}

#[derive(Default)]
struct Control {
    pending: Option<TexBuildMode>,
    running: Option<TexBuildMode>,
    /// The running build was superseded by a newer request.
    superseded: bool,
    /// Drafts ran since the last completed full build.
    needs_full: bool,
    last: Option<Arc<StoredBuild>>,
    last_good: Option<Arc<StoredBuild>>,
    shutdown: bool,
}

/// (page, scale in thousandths).
type PngKey = (u32, u32);

/// A finished build with lazily opened page access.
pub struct StoredBuild {
    pub build: TexBuild,
    pub summary: TexBuildSummary,
    session: OnceLock<Result<Arc<TexPageSession>, String>>,
    pngs: Mutex<HashMap<PngKey, Arc<Vec<u8>>>>,
    maps: Mutex<HashMap<u32, Arc<PageMapping>>>,
}

#[derive(Default)]
struct BuildStore {
    builds: HashMap<BuildId, Arc<StoredBuild>>,
    order: VecDeque<BuildId>,
    /// Each target's last good build is never evicted.
    last_good: HashMap<TexTargetRef, BuildId>,
}

impl TexService {
    pub fn new(
        exec: ExecutionService,
        workspace: Arc<WorkspaceService>,
        runtime: tokio::runtime::Handle,
        config: TexServiceConfig,
    ) -> std::io::Result<Self> {
        let root = config.data_dir.join("tex");
        std::fs::create_dir_all(&root)?;
        let (events, _) = broadcast::channel(1024);
        Ok(Self {
            inner: Arc::new(Inner {
                exec,
                workspace,
                runtime,
                root,
                idle_full_delay: config.idle_full_delay,
                keep_builds: config.keep_builds.max(1),
                targets: Mutex::new(HashMap::new()),
                builds: Mutex::new(BuildStore::default()),
                events,
                counter: std::sync::atomic::AtomicU64::new(0),
            }),
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<TexEvent> {
        self.inner.events.subscribe()
    }

    /// Queue a build of `target` (default: the project's LaTeX target) in the
    /// open workspace. Coalesces with a pending request and cancels a running
    /// (superseded) build.
    pub fn request_build(
        &self,
        target: Option<BuildTarget>,
        mode: TexBuildMode,
    ) -> Result<TexTargetRef, TexServiceError> {
        let target = match target {
            Some(target) => target,
            None => self.default_target()?,
        };
        let target = self.target(target)?;
        {
            let mut control = target.control.lock().unwrap_or_else(|p| p.into_inner());
            control.pending = Some(match control.pending {
                Some(TexBuildMode::Full) => TexBuildMode::Full,
                _ => mode,
            });
            if control.running.is_some() {
                control.superseded = true;
                target.cancel.cancel();
            }
            // Sent under the lock so it always precedes the worker's `Started`.
            let _ = self.inner.events.send(TexEvent::Queued { target: target.reference.clone(), mode });
            target.wake.notify_all();
        }
        Ok(target.reference.clone())
    }

    /// The project's default LaTeX target: `defaultTarget` (or the first LaTeX
    /// target) of `.exposition/project.json`, else its `.tex` entry document
    /// built with pdfLaTeX.
    pub fn default_target(&self) -> Result<BuildTarget, TexServiceError> {
        let settings = self.inner.workspace.project_settings().map_err(|_| TexServiceError::WorkspaceNotOpen)?.settings;
        let latex = |t: &&exposition_workspace::settings::ProjectTarget| {
            t.format == DocumentFormat::Latex && TexTool::for_engine(t.engine).is_some()
        };
        let named = settings
            .default_target
            .as_ref()
            .and_then(|name| settings.targets.iter().filter(latex).find(|t| &t.name == name));
        if let Some(t) = named.or_else(|| settings.targets.iter().find(latex)) {
            return Ok(BuildTarget { format: t.format, engine: t.engine, entry: t.entry.clone() });
        }
        match settings.entry {
            Some(entry) if entry.extension() == Some("tex") => {
                Ok(BuildTarget { format: DocumentFormat::Latex, engine: Engine::PdfLatex, entry })
            }
            _ => Err(TexServiceError::NoTarget),
        }
    }

    /// Cancel the running build and drop the pending request of a target.
    pub fn cancel(&self, target: Option<&BuildTarget>) -> bool {
        let Some(reference) = self.reference(target) else { return false };
        let Some(target) = self.inner.targets.lock().unwrap_or_else(|p| p.into_inner()).get(&reference).cloned() else {
            return false;
        };
        let mut control = target.control.lock().unwrap_or_else(|p| p.into_inner());
        let had = control.pending.take().is_some() || control.running.is_some();
        if control.running.is_some() {
            target.cancel.cancel();
        }
        control.needs_full = false;
        had
    }

    /// Status of a target of the open workspace (default target when `None`).
    pub fn status(&self, target: Option<&BuildTarget>) -> Option<TexTargetStatus> {
        let reference = self.reference(target)?;
        let target = self.inner.targets.lock().unwrap_or_else(|p| p.into_inner()).get(&reference).cloned()?;
        let control = target.control.lock().unwrap_or_else(|p| p.into_inner());
        let changed = control.last_good.as_ref().is_some_and(|b| self.sources_changed(&b.summary.result));
        Some(TexTargetStatus {
            target: reference,
            running: control.running,
            pending: control.pending,
            last: control.last.as_ref().map(|b| b.summary.clone()),
            last_good: control.last_good.as_ref().map(|b| b.summary.clone()),
            stale: control.pending.is_some() || control.running.is_some() || changed,
        })
    }

    /// A finished build kept for page access.
    pub fn build(&self, id: &BuildId) -> Option<Arc<StoredBuild>> {
        self.inner.builds.lock().unwrap_or_else(|p| p.into_inner()).builds.get(id).cloned()
    }

    /// PNG of a page at `scale` pixels per point (0.25..=8).
    pub fn page_png(&self, id: &BuildId, page: u32, scale: f32) -> Result<Arc<Vec<u8>>, TexServiceError> {
        if !(0.25..=8.0).contains(&scale) || !scale.is_finite() {
            return Err(TexServiceError::InvalidScale(scale));
        }
        let stored = self.build(id).ok_or(TexServiceError::UnknownBuild)?;
        let key = (page, (scale * 1000.0).round() as u32);
        if let Some(png) = stored.pngs.lock().unwrap_or_else(|p| p.into_inner()).get(&key) {
            return Ok(png.clone());
        }
        let session = stored.session()?;
        if page as usize >= session.page_count() {
            return Err(TexServiceError::NoSuchPage(page));
        }
        let rendered = session.render_page(page as usize, scale).map_err(|e| TexServiceError::Io(e.to_string()))?;
        let png = Arc::new(rendered.png);
        stored.pngs.lock().unwrap_or_else(|p| p.into_inner()).insert(key, png.clone());
        Ok(png)
    }

    /// Glyph-level mapping of a page.
    pub fn page_map(&self, id: &BuildId, page: u32) -> Result<Arc<PageMapping>, TexServiceError> {
        let stored = self.build(id).ok_or(TexServiceError::UnknownBuild)?;
        if let Some(map) = stored.maps.lock().unwrap_or_else(|p| p.into_inner()).get(&page) {
            return Ok(map.clone());
        }
        let session = stored.session()?;
        if page as usize >= session.page_count() {
            return Err(TexServiceError::NoSuchPage(page));
        }
        let map = Arc::new(session.map_page(page as usize).map_err(|e| TexServiceError::Io(e.to_string()))?);
        stored.maps.lock().unwrap_or_else(|p| p.into_inner()).insert(page, map.clone());
        Ok(map)
    }

    /// Stop all workers and cancel running builds (application exit).
    pub fn shutdown(&self) {
        let targets: Vec<Arc<Target>> =
            self.inner.targets.lock().unwrap_or_else(|p| p.into_inner()).values().cloned().collect();
        for target in targets {
            let mut control = target.control.lock().unwrap_or_else(|p| p.into_inner());
            control.shutdown = true;
            target.cancel.cancel();
            target.wake.notify_all();
        }
    }

    /// Canonical root of the open workspace.
    fn open_root(&self) -> Result<PathBuf, TexServiceError> {
        let snapshot = self.inner.workspace.snapshot().map_err(|_| TexServiceError::WorkspaceNotOpen)?;
        // Without the Windows verbatim prefix: shown to users, passed to TeX.
        Ok(crate::compile::plain_path(snapshot.root()))
    }

    fn reference(&self, target: Option<&BuildTarget>) -> Option<TexTargetRef> {
        let target = match target {
            Some(target) => target.clone(),
            None => self.default_target().ok()?,
        };
        let root = self.open_root().ok()?;
        Some(TexTargetRef { root: root.display().to_string(), entry: target.entry, engine: target.engine })
    }

    fn target(&self, target: BuildTarget) -> Result<Arc<Target>, TexServiceError> {
        if target.format != DocumentFormat::Latex || TexTool::for_engine(target.engine).is_none() {
            return Err(TexServiceError::UnsupportedTarget);
        }
        let canonical = self.open_root()?;
        let reference =
            TexTargetRef { root: canonical.display().to_string(), entry: target.entry.clone(), engine: target.engine };
        let mut targets = self.inner.targets.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(existing) = targets.get(&reference) {
            return Ok(existing.clone());
        }
        let build_root = self.inner.root.join(&hex_sha256(canonical.display().to_string().as_bytes())[..16]);
        std::fs::create_dir_all(&build_root).map_err(|e| TexServiceError::Io(e.to_string()))?;
        self.inner
            .exec
            .register_app_dir(&build_root, Some(&canonical))
            .map_err(|e| TexServiceError::Io(e.to_string()))?;
        let cancel = CancelToken::default();
        let runner = ExecutionRunner::new(self.inner.exec.clone(), self.inner.runtime.clone(), cancel.clone());
        let builder = TexBuilder::new(runner, None, build_root).map_err(|e| TexServiceError::Io(e.to_string()))?;
        let created = Arc::new(Target {
            reference: reference.clone(),
            root: canonical,
            builder,
            cancel,
            control: Mutex::new(Control::default()),
            wake: Condvar::new(),
        });
        targets.insert(reference, created.clone());
        let inner = self.inner.clone();
        let worker = created.clone();
        std::thread::Builder::new()
            .name("tex-build".into())
            .spawn(move || inner.work(worker, target))
            .map_err(|e| TexServiceError::Io(e.to_string()))?;
        Ok(created)
    }

    /// An open document the build compiled changed (or was closed) since, or
    /// a file the build read from disk is now open with edits.
    fn sources_changed(&self, result: &BuildResult) -> bool {
        let documents = self.inner.workspace.documents();
        let buffer_changed = result.sources.iter().any(|source| match (&source.document, source.revision) {
            (Some(document), Some(revision)) => {
                documents.info(document).map(|info| info.revision != revision).unwrap_or(true)
            }
            _ => false,
        });
        let opened_with_edits = documents.list().iter().any(|doc| {
            doc.path.as_ref().is_some_and(|p| {
                doc.revision > 1 && result.sources.iter().any(|s| &s.path == p && s.document.as_ref() != Some(&doc.id))
            })
        });
        buffer_changed || opened_with_edits
    }
}

impl StoredBuild {
    fn session(&self) -> Result<Arc<TexPageSession>, TexServiceError> {
        if !self.summary.has_output {
            return Err(TexServiceError::NoOutput);
        }
        self.session
            .get_or_init(|| TexPageSession::open(&self.build).map(Arc::new).map_err(|e| e.to_string()))
            .clone()
            .map_err(TexServiceError::Io)
    }
}

impl Inner {
    /// Worker loop of one target.
    fn work(self: Arc<Self>, target: Arc<Target>, build_target: BuildTarget) {
        loop {
            let (mode, automatic) = {
                let mut control = target.control.lock().unwrap_or_else(|p| p.into_inner());
                loop {
                    if control.shutdown {
                        return;
                    }
                    if let Some(mode) = control.pending.take() {
                        break (mode, false);
                    }
                    match (control.needs_full, self.idle_full_delay) {
                        (true, Some(delay)) => {
                            let (guard, timeout) =
                                target.wake.wait_timeout(control, delay).unwrap_or_else(|p| p.into_inner());
                            control = guard;
                            if timeout.timed_out() && control.pending.is_none() && control.needs_full {
                                break (TexBuildMode::Full, true);
                            }
                        }
                        _ => control = target.wake.wait(control).unwrap_or_else(|p| p.into_inner()),
                    }
                }
            };
            let id = BuildId(format!(
                "tex-{}-{}",
                std::process::id(),
                self.counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            {
                let mut control = target.control.lock().unwrap_or_else(|p| p.into_inner());
                control.running = Some(mode);
                control.superseded = false;
                target.cancel.reset();
            }
            let _ = self.events.send(TexEvent::Started {
                target: target.reference.clone(),
                build: id.clone(),
                mode,
                automatic,
            });
            let build = self.run_build(&target, &build_target, id, mode);
            let mut control = target.control.lock().unwrap_or_else(|p| p.into_inner());
            control.running = None;
            let superseded = control.superseded && build.result.status == BuildStatus::Canceled;
            let summary = TexBuildSummary {
                result: build.result.clone(),
                mode,
                automatic,
                superseded,
                refusal: build.refusal.clone(),
                passes: build
                    .passes
                    .iter()
                    .map(|p| TexPass { tool: p.tool.clone(), duration_ms: p.duration_ms, exit_code: p.exit_code })
                    .collect(),
                has_output: build.output.as_ref().is_some_and(|o| o.synctex.is_some())
                    && build.result.status != BuildStatus::Canceled,
            };
            let stored = Arc::new(StoredBuild {
                build,
                summary: summary.clone(),
                session: OnceLock::new(),
                pngs: Mutex::new(HashMap::new()),
                maps: Mutex::new(HashMap::new()),
            });
            match (mode, &summary.result.status) {
                (_, BuildStatus::Canceled) => {}
                (TexBuildMode::Draft, BuildStatus::Succeeded) => control.needs_full = true,
                _ => control.needs_full = false,
            }
            if summary.result.status != BuildStatus::Canceled {
                control.last = Some(stored.clone());
            }
            if summary.has_output {
                control.last_good = Some(stored.clone());
            }
            // Stored before the lock is released so a status reader can
            // always fetch the pages of `last_good`.
            self.store(&target.reference, stored, summary.has_output);
            drop(control);
            let _ =
                self.events.send(TexEvent::Finished { target: target.reference.clone(), summary: Box::new(summary) });
        }
    }

    fn run_build(&self, target: &Target, build_target: &BuildTarget, id: BuildId, mode: TexBuildMode) -> TexBuild {
        let request = TexBuildRequest {
            target: build_target.clone(),
            project_root: target.root.clone(),
            buffers: Vec::new(),
            mode: mode.into(),
            tounicode: false,
            workspace: None,
        };
        // Builds execute project content: refuse untrusted workspaces before
        // writing or running anything (the execution service refuses too).
        let trusted = self.exec.trust_status(&target.root.display().to_string()).map(|s| s.trusted).unwrap_or(false);
        if !trusted {
            let refusal = JobRefusal {
                code: RefusalCode::UntrustedWorkspace,
                message: format!(
                    "Workspace {} is not trusted. LaTeX builds run the TeX engine with your account's privileges \
                     and can read any of your files; trust the workspace to build it.",
                    target.root.display()
                ),
                workspace_root: Some(target.root.display().to_string()),
            };
            return refused_build(&request, id, refusal);
        }
        // The snapshot: open buffers captured atomically, disk through checks.
        let snapshot = match self.workspace.snapshot() {
            Ok(snapshot) if crate::compile::same_dir(snapshot.root(), &target.root) => snapshot,
            _ => {
                let mut build = refused_build(
                    &request,
                    id,
                    JobRefusal {
                        code: RefusalCode::OutsideAllowedRoots,
                        message: "the workspace this target belongs to is no longer open".into(),
                        workspace_root: Some(target.root.display().to_string()),
                    },
                );
                build.refusal = None;
                return build;
            }
        };
        target.builder.set_toolchain(self.toolchain());
        target.builder.build_with_id(id, &TexBuildRequest { workspace: Some(snapshot), ..request })
    }

    fn toolchain(&self) -> Option<TexToolchain> {
        let mut tools = BTreeMap::new();
        for tool in TexTool::ALL {
            if let Located::Found { path, .. } = self.exec.locate_tool(tool.name()) {
                tools.insert(tool, path);
            }
        }
        TexToolchain::from_tools(tools)
    }

    fn store(&self, target: &TexTargetRef, stored: Arc<StoredBuild>, last_good: bool) {
        let mut store = self.builds.lock().unwrap_or_else(|p| p.into_inner());
        let id = stored.summary.result.id.clone();
        store.builds.insert(id.clone(), stored);
        store.order.push_back(id.clone());
        if last_good {
            store.last_good.insert(target.clone(), id);
        }
        // Evict the oldest builds beyond the limit, never a last good build.
        while store.order.len() > self.keep_builds {
            let pinned: Vec<&BuildId> = store.last_good.values().collect();
            let Some(position) = store.order.iter().position(|id| !pinned.contains(&id)) else { break };
            if let Some(evicted) = store.order.remove(position) {
                store.builds.remove(&evicted);
            }
        }
    }
}

fn refused_build(request: &TexBuildRequest, id: BuildId, refusal: JobRefusal) -> TexBuild {
    use exposition_core::build::{MappingPrecision, SnapshotId};
    TexBuild {
        result: BuildResult {
            id,
            target: request.target.clone(),
            snapshot: SnapshotId(String::new()),
            sources: Vec::new(),
            configuration: None,
            status: BuildStatus::Error { message: refusal.message.clone() },
            diagnostics: Vec::new(),
            pages: Vec::new(),
            mapping: MappingPrecision::None,
            exports: Vec::new(),
            duration_ms: 0,
        },
        output: None,
        snapshot: None,
        passes: Vec::new(),
        refusal: Some(refusal),
    }
}
