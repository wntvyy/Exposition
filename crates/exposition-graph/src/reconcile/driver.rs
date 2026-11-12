//! The reconciler's background driver: subscribes in Rust to the workspace
//! (file changes, rescans, reloads), the document service (edits, saves,
//! closes), and the graph (attach/detach), and keeps the graph synchronized
//! whether or not any frontend listens.
//!
//! It runs on its own thread, never on an IPC thread that applies document
//! edits: typing only enqueues an overlay recomputation here. File changes are
//! coalesced (a quiet period, bounded) into one pass; a burst (branch switch)
//! or a lagged event stream becomes a full pass. Freshness is reported through
//! [`GraphService::set_freshness`]: Pending (with the number of paths
//! waiting) → Reconciling → Synchronized with a checkpoint.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use exposition_core::document::{DocumentEvent, DocumentId};
use exposition_core::path::WorkspacePath;
use exposition_workspace::WorkspaceService;
use exposition_workspace::types::WorkspaceEvent;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use super::engine::{ReconcileOptions, ReconcileReport, Scope};
use crate::model::{GraphEvent, GraphFreshness, SyncState};
use crate::service::{GraphError, GraphResult, GraphService};

/// Quiet period after the last file event before a pass.
pub const SETTLE: Duration = Duration::from_millis(250);
/// Longest a burst of file events delays a pass.
pub const MAX_DELAY: Duration = Duration::from_secs(2);
/// Quiet period after the last edit before an overlay is recomputed.
pub const OVERLAY_SETTLE: Duration = Duration::from_millis(150);
/// More changed paths than this in one pass: scan the whole project instead.
pub const FULL_SCAN_THRESHOLD: usize = 200;

enum Command {
    Reindex { force: bool, reply: Option<oneshot::Sender<GraphResult<ReconcileReport>>> },
    Shutdown,
}

/// Handle to the running reconciler.
#[derive(Clone)]
pub struct ReconcilerHandle {
    tx: mpsc::UnboundedSender<Command>,
}

impl ReconcilerHandle {
    /// Rebuild derived projections now (`force`: re-read every file even
    /// when its checkpoint is unchanged) and wait for the result.
    pub fn reindex_blocking(&self, force: bool, timeout: Duration) -> GraphResult<ReconcileReport> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::Reindex { force, reply: Some(reply) })
            .map_err(|_| GraphError::new("graph.reconciler", "The reconciler is not running."))?;
        let deadline = std::time::Instant::now() + timeout;
        let mut rx = rx;
        loop {
            match rx.try_recv() {
                Ok(result) => return result,
                Err(oneshot::error::TryRecvError::Closed) => {
                    return Err(GraphError::new("graph.reconciler", "The reconciler stopped."));
                }
                Err(oneshot::error::TryRecvError::Empty) => {}
            }
            if std::time::Instant::now() > deadline {
                return Err(GraphError::new(
                    "graph.reconcileTimeout",
                    "Reindexing is still running; its progress is shown in the graph status.",
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Request a pass without waiting.
    pub fn request(&self, force: bool) {
        let _ = self.tx.send(Command::Reindex { force, reply: None });
    }

    pub fn shutdown(&self) {
        let _ = self.tx.send(Command::Shutdown);
    }
}

/// Start the reconciler thread.
pub fn spawn(graph: Arc<GraphService>, workspace: Arc<WorkspaceService>) -> ReconcilerHandle {
    let (tx, rx) = mpsc::unbounded_channel();
    // Subscribe before the thread starts so no event is missed.
    let graph_rx = graph.subscribe();
    let ws_rx = workspace.subscribe();
    let doc_rx = workspace.documents().subscribe();
    let spawned = std::thread::Builder::new().name("graph-reconciler".into()).spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread().enable_time().build() {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("graph reconciler could not start: {e}");
                return;
            }
        };
        runtime.block_on(Driver::new(graph, workspace).run(rx, graph_rx, ws_rx, doc_rx));
    });
    if let Err(e) = spawned {
        eprintln!("graph reconciler thread could not start: {e}");
    }
    ReconcilerHandle { tx }
}

struct Driver {
    graph: Arc<GraphService>,
    workspace: Arc<WorkspaceService>,
    generation: u64,
    attached: bool,
    paths: BTreeSet<WorkspacePath>,
    full: bool,
    force: bool,
    first_event: Option<Instant>,
    due: Option<Instant>,
    overlays: BTreeMap<DocumentId, Instant>,
    doc_paths: BTreeMap<DocumentId, WorkspacePath>,
    replies: Vec<oneshot::Sender<GraphResult<ReconcileReport>>>,
    backed_up: bool,
}

impl Driver {
    fn new(graph: Arc<GraphService>, workspace: Arc<WorkspaceService>) -> Self {
        Self {
            graph,
            workspace,
            generation: u64::MAX,
            attached: false,
            paths: BTreeSet::new(),
            full: false,
            force: false,
            first_event: None,
            due: None,
            overlays: BTreeMap::new(),
            doc_paths: BTreeMap::new(),
            replies: Vec::new(),
            backed_up: false,
        }
    }

    async fn run(
        mut self,
        mut commands: mpsc::UnboundedReceiver<Command>,
        mut graph_rx: tokio::sync::broadcast::Receiver<GraphEvent>,
        mut ws_rx: tokio::sync::broadcast::Receiver<WorkspaceEvent>,
        mut doc_rx: tokio::sync::broadcast::Receiver<DocumentEvent>,
    ) {
        self.check_attach();
        loop {
            let overlay_due = self.overlays.values().min().copied();
            let next = [self.due, overlay_due].into_iter().flatten().min();
            tokio::select! {
                cmd = commands.recv() => match cmd {
                    Some(Command::Reindex { force, reply }) => {
                        self.full = true;
                        self.force |= force;
                        if let Some(r) = reply {
                            self.replies.push(r);
                        }
                        self.due = Some(Instant::now());
                    }
                    Some(Command::Shutdown) | None => break,
                },
                event = graph_rx.recv() => match event {
                    Ok(GraphEvent::Status { .. }) | Err(RecvError::Lagged(_)) => self.check_attach(),
                    Ok(_) => {}
                    Err(RecvError::Closed) => break,
                },
                event = ws_rx.recv() => match event {
                    Ok(WorkspaceEvent::FilesChanged { changes, rescan }) => {
                        if rescan {
                            self.schedule_full();
                        } else {
                            self.schedule(changes.into_iter().map(|c| c.path));
                        }
                    }
                    Ok(WorkspaceEvent::DocumentReloaded { path, .. }) => self.schedule([path]),
                    Ok(_) => {}
                    Err(RecvError::Lagged(_)) => self.schedule_full(),
                    Err(RecvError::Closed) => break,
                },
                event = doc_rx.recv() => match event {
                    Ok(DocumentEvent::Changed(changed)) => self.document_changed(&changed.document),
                    Ok(DocumentEvent::Opened { info }) => {
                        if let Some(path) = info.path.clone() {
                            self.doc_paths.insert(info.id.clone(), path);
                        }
                    }
                    Ok(DocumentEvent::InfoChanged { info }) => {
                        if let Some(path) = info.path.clone() {
                            self.doc_paths.insert(info.id.clone(), path.clone());
                            if info.saved_revision == Some(info.revision) {
                                // Saved (or back at the saved text): the file is the truth.
                                self.overlays.remove(&info.id);
                                self.graph.clear_overlay(&path);
                                self.schedule([path]);
                            } else {
                                self.document_changed(&info.id);
                            }
                        }
                    }
                    Ok(DocumentEvent::Closed { document }) => {
                        self.overlays.remove(&document);
                        if let Some(path) = self.doc_paths.remove(&document) {
                            self.graph.clear_overlay(&path);
                        }
                    }
                    Err(RecvError::Lagged(_)) => {
                        for info in self.workspace.documents().list() {
                            self.document_changed(&info.id);
                        }
                    }
                    Err(RecvError::Closed) => break,
                },
                _ = tokio::time::sleep_until(next.unwrap_or_else(Instant::now)), if next.is_some() => {
                    let now = Instant::now();
                    if self.due.is_some_and(|d| d <= now) {
                        self.due = None;
                        self.pass();
                        // The saved projection changed under open buffers.
                        for info in self.workspace.documents().list() {
                            if info.path.is_some() && info.saved_revision != Some(info.revision) {
                                self.overlays.entry(info.id).or_insert(now);
                            }
                        }
                    }
                    let ready: Vec<DocumentId> =
                        self.overlays.iter().filter(|(_, at)| **at <= now).map(|(id, _)| id.clone()).collect();
                    for id in ready {
                        self.overlays.remove(&id);
                        self.compute_overlay(&id);
                    }
                }
            }
        }
        for r in self.replies.drain(..) {
            let _ = r.send(Err(GraphError::new("graph.reconciler", "The reconciler stopped.")));
        }
    }

    /// A project graph was attached (or detached): start over.
    fn check_attach(&mut self) {
        let generation = self.graph.generation();
        if generation == self.generation {
            return;
        }
        self.generation = generation;
        self.paths.clear();
        self.overlays.clear();
        self.first_event = None;
        self.backed_up = false;
        self.attached = self.graph.status().is_usable();
        if self.attached {
            self.full = true;
            self.due = Some(Instant::now());
            for info in self.workspace.documents().list() {
                if let Some(path) = &info.path {
                    self.doc_paths.insert(info.id.clone(), path.clone());
                    if info.saved_revision != Some(info.revision) {
                        self.overlays.insert(info.id, Instant::now());
                    }
                }
            }
        } else {
            self.full = false;
            self.due = None;
            for r in self.replies.drain(..) {
                let _ = r.send(Err(GraphError::new("graph.noProject", "No project graph is attached.")));
            }
        }
    }

    fn schedule_full(&mut self) {
        self.full = true;
        self.bump();
    }

    fn schedule(&mut self, paths: impl IntoIterator<Item = WorkspacePath>) {
        if !self.attached {
            return;
        }
        let mut marked: Vec<WorkspacePath> = Vec::new();
        for p in paths {
            // The graph's own storage, run records, and tooling/cache folders
            // never hold project resources: changes there need no pass.
            let excluded_dir = p.components().any(|c| crate::admission::EXCLUDED_DIRS.contains(&c))
                || p.as_str() == ".exposition/runs"
                || p.as_str().starts_with(".exposition/runs/");
            if crate::service::is_graph_storage(&p) || excluded_dir {
                continue;
            }
            marked.push(p.clone());
            self.paths.insert(p);
        }
        if self.paths.is_empty() && !self.full {
            return;
        }
        if !marked.is_empty() {
            let _ = self.graph.mark_pending(&marked);
        }
        if self.paths.len() > FULL_SCAN_THRESHOLD {
            self.full = true;
        }
        self.bump();
    }

    fn bump(&mut self) {
        if !self.attached {
            return;
        }
        let now = Instant::now();
        let first = *self.first_event.get_or_insert(now);
        let at = (now + SETTLE).min(first + MAX_DELAY);
        self.due = Some(at);
        let pending = if self.full { 0 } else { self.paths.len() as u32 };
        let current = self.graph.freshness();
        if current.state == SyncState::Synchronized
            || (current.state == SyncState::Pending && current.pending != pending)
        {
            self.graph.set_freshness(GraphFreshness { state: SyncState::Pending, pending, ..current });
        }
    }

    fn document_changed(&mut self, id: &DocumentId) {
        if !self.attached {
            return;
        }
        if let Ok(info) = self.workspace.documents().info(id)
            && let Some(path) = info.path
        {
            self.doc_paths.insert(id.clone(), path);
            self.overlays.insert(id.clone(), Instant::now() + OVERLAY_SETTLE);
        }
    }

    fn compute_overlay(&mut self, id: &DocumentId) {
        if !self.attached {
            return;
        }
        let documents = self.workspace.documents();
        let (Ok(info), Ok(snapshot)) = (documents.info(id), documents.snapshot(id)) else { return };
        let Some(path) = info.path.clone() else { return };
        if info.saved_revision == Some(snapshot.revision) {
            self.graph.clear_overlay(&path);
            return;
        }
        // Undo back to the saved text leaves the buffer at a new revision
        // with no unsaved difference: the bytes it would save are the indexed
        // file, so there is nothing to overlay.
        let would_save =
            super::scan::sha256_hex(&exposition_workspace::encoding::encode(&snapshot.text, info.encoding));
        let indexed = self.graph.with_store(|s| Ok(s.coverage(&path)?.and_then(|c| c.content_hash))).ok().flatten();
        if indexed.as_deref() == Some(would_save.as_str()) {
            self.graph.clear_overlay(&path);
            return;
        }
        let generation = self.graph.generation();
        match super::overlay::compute(&self.graph, &path, snapshot.revision, &snapshot.text) {
            Ok(Some(entry)) => self.graph.set_overlay(generation, entry),
            Ok(None) => self.graph.clear_overlay(&path),
            Err(_) => {}
        }
    }

    fn pass(&mut self) {
        if !self.attached {
            self.paths.clear();
            self.full = false;
            return;
        }
        let scope =
            if self.full { Scope::Full } else { Scope::Paths(std::mem::take(&mut self.paths).into_iter().collect()) };
        let force = self.force;
        self.full = false;
        self.force = false;
        self.paths.clear();
        self.first_event = None;
        let trace = std::env::var_os("EXPOSITION_GRAPH_TRACE").is_some();
        let described = match &scope {
            Scope::Full => "full".to_owned(),
            Scope::Paths(p) => format!("{} paths", p.len()),
        };
        let result = self.graph.reconcile(&ReconcileOptions { scope, force });
        if trace && let Ok(r) = &result {
            eprintln!(
                "graph pass ({described}, force {force}): {} ms, files {}, read {}, +{} ~{} -{} resources, +{} -{} declarations",
                r.duration_ms,
                r.files,
                r.read,
                r.resources_added,
                r.resources_renamed,
                r.resources_removed,
                r.declarations_added,
                r.declarations_removed
            );
        }
        if let Err(e) = &result {
            eprintln!("graph reconciliation failed: {e}");
        }
        for r in self.replies.drain(..) {
            let _ = r.send(result.clone());
        }
        if result.is_ok() && !self.backed_up {
            self.backed_up = true;
            let _ = self.graph.backup_if_stale(crate::backup::MAX_AGE_MS);
        }
    }
}
