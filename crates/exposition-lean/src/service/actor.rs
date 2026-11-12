//! The actor behind [`super::LeanService`]: owns servers and document state.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct ServerKey {
    root: PathBuf,
    toolchain: PathBuf,
}

struct Running {
    client: Arc<LspClient>,
    job: JobId,
    pid: Option<u32>,
    sorry_type: Option<u64>,
}

enum Phase {
    Stopped,
    Starting,
    Running(Running),
    Crashed {
        message: String,
        restarting: bool,
    },
    Untrusted {
        workspace_root: String,
    },
    /// The server could not be started for a reason that may pass (e.g. too
    /// many interactive sessions); retried periodically and on any change.
    Unavailable {
        reason: String,
    },
    Stopping {
        restart: bool,
    },
}

struct Server {
    project: LeanProject,
    toolchain: ResolvedToolchain,
    info: LeanToolchainInfo,
    generation: u64,
    phase: Phase,
    attempts: u32,
    idle_since: Option<Instant>,
}

impl Server {
    fn running(&self) -> Option<&Running> {
        match &self.phase {
            Phase::Running(r) => Some(r),
            _ => None,
        }
    }
}

/// What the current server generation has for a document.
struct SyncState {
    generation: u64,
    version: u64,
    text: Arc<str>,
    index: Arc<LineIndex>,
    /// Latest `publishDiagnostics` for `version` (converted lazily).
    lsp_diagnostics: Vec<Value>,
    /// Lean reported a fatal error for `version` (`$/lean/fileProgress` kind 2).
    fatal: bool,
}

struct Doc {
    id: DocumentId,
    path: WorkspacePath,
    abs: Option<PathBuf>,
    uri: Option<String>,
    /// Latest document revision seen.
    revision: u64,
    /// Disk text for a temporary (check-only) document.
    temp_text: Option<Arc<str>>,
    project_root: Option<PathBuf>,
    key: Option<ServerKey>,
    status: LeanStatus,
    diagnostics: Vec<Diagnostic>,
    diagnostics_revision: Option<u64>,
    sorries: Vec<SorryLocation>,
    /// Every axiom the declarations depend on, and those beyond the standard
    /// ones, from the axiom audit of `diagnostics_revision`.
    axioms: Vec<String>,
    axiom_uses: Vec<AxiomUse>,
    sync: Option<SyncState>,
    waiters: Vec<(u64, oneshot::Sender<()>)>,
    dirty: bool,
    /// `sync.lsp_diagnostics` changed and `diagnostics` is not yet converted.
    diagnostics_pending: bool,
    last_emit: Option<Instant>,
    goal_requests: Vec<i64>,
    /// Work towards the final status of the current version: the axiom
    /// audit (started speculatively once the version is stable, so it runs
    /// alongside Lean), `sorry` token locations, and Lean's own completion.
    work: Option<CheckWork>,
}

struct CheckWork {
    generation: u64,
    version: u64,
    /// Cancels the audit jobs when the work is dropped or superseded.
    cancel: watch::Sender<bool>,
    audit: Option<audit::AuditOutcome>,
    /// `None` while the `sorry` token request is pending.
    tokens: Option<Vec<TextRange>>,
    /// Lean answered `waitForDiagnostics` for `version` with no errors.
    lean_done: bool,
}

impl Drop for CheckWork {
    fn drop(&mut self) {
        let _ = self.cancel.send(true);
    }
}

impl Doc {
    fn new(id: DocumentId, path: WorkspacePath, revision: u64) -> Self {
        Doc {
            id,
            path,
            abs: None,
            uri: None,
            revision,
            temp_text: None,
            project_root: None,
            key: None,
            status: LeanStatus::NotStarted,
            diagnostics: Vec::new(),
            diagnostics_revision: None,
            sorries: Vec::new(),
            axioms: Vec::new(),
            axiom_uses: Vec::new(),
            sync: None,
            waiters: Vec::new(),
            dirty: false,
            diagnostics_pending: false,
            last_emit: None,
            goal_requests: Vec::new(),
            work: None,
        }
    }

    fn is_temp(&self) -> bool {
        self.temp_text.is_some()
    }

    /// Drop (and cancel) work for an older version.
    fn cancel_audit(&mut self) {
        self.work = None;
    }

    /// Convert the latest published diagnostics (done lazily: Lean republishes
    /// the whole list after every command, which is far more often than the UI
    /// needs it).
    fn convert_pending(&mut self) {
        if !self.diagnostics_pending {
            return;
        }
        self.diagnostics_pending = false;
        if let Some(sync) = &self.sync {
            self.diagnostics =
                sync.lsp_diagnostics.iter().map(|d| convert::diagnostic(d, &sync.index, &self.path)).collect();
            self.diagnostics_revision = Some(sync.version);
        }
    }
}

pub(super) struct Actor {
    exec: ExecutionService,
    workspace: Arc<WorkspaceService>,
    documents: Arc<DocumentService>,
    config: LeanConfig,
    events: broadcast::Sender<LeanEvent>,
    tx: mpsc::UnboundedSender<Msg>,
    /// Canonical workspace root (without a verbatim prefix).
    root: Option<PathBuf>,
    docs: HashMap<DocumentId, Doc>,
    servers: HashMap<ServerKey, Server>,
    /// `lean --version` per toolchain directory (`None` while probing or if it failed).
    versions: HashMap<PathBuf, Option<String>>,
    next_generation: u64,
    next_temp: u64,
}

impl Actor {
    pub(super) fn new(
        exec: ExecutionService,
        workspace: Arc<WorkspaceService>,
        documents: Arc<DocumentService>,
        config: LeanConfig,
        events: broadcast::Sender<LeanEvent>,
        tx: mpsc::UnboundedSender<Msg>,
    ) -> Self {
        Actor {
            exec,
            workspace,
            documents,
            config,
            events,
            tx,
            root: None,
            docs: HashMap::new(),
            servers: HashMap::new(),
            versions: HashMap::new(),
            next_generation: 0,
            next_temp: 0,
        }
    }

    pub(super) async fn run(
        mut self,
        mut rx: mpsc::UnboundedReceiver<Msg>,
        mut doc_rx: broadcast::Receiver<DocumentEvent>,
        mut ws_rx: broadcast::Receiver<WorkspaceEvent>,
    ) {
        self.set_root(self.workspace.current().map(|info| info.root));
        for info in self.documents.list() {
            self.track(&info);
        }
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                // Document events first: an API request issued after an edit
                // was acknowledged must see that edit.
                biased;
                event = doc_rx.recv() => match event {
                    Ok(event) => self.on_document(event),
                    Err(RecvError::Lagged(_)) => self.resync_documents(),
                    Err(RecvError::Closed) => break,
                },
                event = ws_rx.recv() => match event {
                    Ok(event) => self.on_workspace(event),
                    Err(RecvError::Lagged(_)) => self.restart_all(),
                    Err(RecvError::Closed) => {}
                },
                msg = rx.recv() => match msg {
                    Some(Msg::Shutdown { reply }) => {
                        self.shutdown().await;
                        let _ = reply.send(());
                    }
                    Some(msg) => self.on_message(msg),
                    None => break,
                },
                _ = tick.tick() => self.on_tick(),
            }
        }
        self.shutdown().await;
    }

    // ----- events ----------------------------------------------------------------

    fn emit(&mut self, id: &DocumentId) {
        let Some(doc) = self.docs.get_mut(id) else { return };
        doc.dirty = false;
        doc.last_emit = Some(Instant::now());
        doc.convert_pending();
        if doc.status.is_settled() {
            for (_, waiter) in doc.waiters.drain(..) {
                let _ = waiter.send(());
            }
        }
        if doc.is_temp() {
            return;
        }
        if let Some(state) = self.state_of(id) {
            let _ = self.events.send(LeanEvent::Document { state: Box::new(state) });
        }
    }

    fn set_status(&mut self, id: &DocumentId, status: LeanStatus) {
        if let Some(doc) = self.docs.get_mut(id) {
            doc.status = status;
        }
        self.emit(id);
    }

    fn state_of(&self, id: &DocumentId) -> Option<LeanDocumentState> {
        let doc = self.docs.get(id)?;
        let server = doc.key.as_ref().and_then(|k| self.servers.get(k));
        Some(LeanDocumentState {
            document: doc.id.clone(),
            path: doc.path.clone(),
            revision: doc.revision,
            project_root: doc.project_root.as_ref().map(|p| p.display().to_string()),
            toolchain: server.map(|s| s.info.clone()),
            status: doc.status.clone(),
            diagnostics: doc.diagnostics.clone(),
            diagnostics_revision: doc.diagnostics_revision,
            job: server.and_then(|s| s.running()).map(|r| r.job.clone()),
        })
    }

    fn docs_of(&self, key: &ServerKey) -> Vec<DocumentId> {
        self.docs.values().filter(|d| d.key.as_ref() == Some(key)).map(|d| d.id.clone()).collect()
    }

    fn toolchain_name(&self, key: &ServerKey) -> String {
        self.servers.get(key).map(|s| s.info.name.clone()).unwrap_or_default()
    }

    fn on_tick(&mut self) {
        // Progress and partial diagnostics are coalesced: at most every tick,
        // and at most once a second for documents with many diagnostics.
        let now = Instant::now();
        let dirty: Vec<DocumentId> = self
            .docs
            .values()
            .filter(|d| d.dirty)
            .filter(|d| {
                let interval = if d.sync.as_ref().is_some_and(|s| s.lsp_diagnostics.len() > 500) {
                    Duration::from_secs(1)
                } else {
                    TICK
                };
                d.last_emit.is_none_or(|t| now.duration_since(t) >= interval)
            })
            .map(|d| d.id.clone())
            .collect();
        for id in dirty {
            self.emit(&id);
        }
        // Stop servers nobody uses any more.
        let keys: Vec<ServerKey> = self.servers.keys().cloned().collect();
        for key in keys {
            let used = self.docs.values().any(|d| d.key.as_ref() == Some(&key));
            let idle_limit = self.config.idle_shutdown;
            let Some(server) = self.servers.get_mut(&key) else { continue };
            if used {
                server.idle_since = None;
                continue;
            }
            match &server.phase {
                Phase::Running(_) => {
                    let since = *server.idle_since.get_or_insert(now);
                    if now.duration_since(since) >= idle_limit {
                        self.stop_server(&key, false);
                    }
                }
                Phase::Stopping { .. } | Phase::Starting => {}
                _ => {
                    self.servers.remove(&key);
                }
            }
        }
    }

    // ----- workspace ---------------------------------------------------------------

    fn set_root(&mut self, root: Option<String>) {
        self.root = root.and_then(|r| canonicalize(Path::new(&r)).ok());
    }

    fn reattach_all(&mut self) {
        let ids: Vec<DocumentId> = self.docs.keys().cloned().collect();
        for id in ids {
            self.reattach(&id);
        }
    }

    fn on_workspace(&mut self, event: WorkspaceEvent) {
        match event {
            WorkspaceEvent::Opened { info } => {
                let previous = self.root.clone();
                self.set_root(Some(info.root));
                if previous != self.root {
                    self.stop_all_servers();
                    self.reattach_all();
                }
            }
            WorkspaceEvent::Closed => {
                self.stop_all_servers();
                self.root = None;
                self.reattach_all();
            }
            WorkspaceEvent::FilesChanged { changes, rescan } => self.on_files_changed(&changes, rescan),
            _ => {}
        }
    }

    fn on_files_changed(&mut self, changes: &[exposition_workspace::types::FileChange], rescan: bool) {
        use exposition_workspace::types::FileChangeKind;
        let Some(root) = self.root.clone() else { return };
        let mut restart: Vec<ServerKey> = Vec::new();
        let mut reattach: Vec<DocumentId> = Vec::new();
        let mut watched: HashMap<ServerKey, Vec<Value>> = HashMap::new();
        for change in changes {
            if change.path.components().any(|c| c == ".lake" || c == "lake-packages") {
                continue;
            }
            let abs = join(&root, &change.path);
            let name = change.path.file_name();
            if name == TOOLCHAIN_FILE || LAKEFILES.contains(&name) {
                // The toolchain or Lake configuration changed: statuses computed
                // with the old one are no longer valid. Restart the servers it
                // concerns, and re-resolve documents that have no server (e.g.
                // a toolchain that was missing may now be installed).
                let dir = abs.parent().map(Path::to_path_buf).unwrap_or_default();
                for (key, server) in &self.servers {
                    let affected = same_path(&key.root, &dir)
                        || (server.project.kind == ProjectKind::Standalone && name == TOOLCHAIN_FILE);
                    if affected && !restart.contains(key) {
                        restart.push(key.clone());
                    }
                }
                for doc in self.docs.values().filter(|d| d.abs.as_ref().is_some_and(|a| a.starts_with(&dir))) {
                    match &doc.key {
                        Some(key) if !restart.contains(key) => restart.push(key.clone()),
                        Some(_) => {}
                        None if !reattach.contains(&doc.id) => reattach.push(doc.id.clone()),
                        None => {}
                    }
                }
            } else if change.path.extension() == Some("lean") {
                let kind = match change.kind {
                    FileChangeKind::Created => 1,
                    FileChangeKind::Modified => 2,
                    FileChangeKind::Removed => 3,
                };
                for (key, server) in &self.servers {
                    if server.running().is_some() && abs.starts_with(&key.root) {
                        watched.entry(key.clone()).or_default().push(json!({ "uri": path_to_uri(&abs), "type": kind }));
                    }
                }
            }
        }
        let _ = rescan;
        for (key, changes) in watched {
            if let Some(r) = self.servers.get(&key).and_then(|s| s.running()) {
                let _ = r.client.notify("workspace/didChangeWatchedFiles", json!({ "changes": changes }));
            }
        }
        for key in restart {
            self.restart_server(&key);
        }
        for id in reattach {
            self.reattach(&id);
        }
    }

    // ----- documents -----------------------------------------------------------------

    fn on_document(&mut self, event: DocumentEvent) {
        match event {
            DocumentEvent::Opened { info } => self.track(&info),
            DocumentEvent::Changed(changed) => self.on_changed(changed),
            DocumentEvent::Closed { document } => self.untrack(&document),
            DocumentEvent::InfoChanged { info } => self.on_info(info),
        }
    }

    fn track(&mut self, info: &DocumentInfo) {
        if !is_lean(info) || self.docs.contains_key(&info.id) {
            return;
        }
        let Some(path) = info.path.clone() else { return };
        // A check-only copy of the same file must not stay open beside it.
        let temps: Vec<DocumentId> =
            self.docs.values().filter(|d| d.is_temp() && d.path == path).map(|d| d.id.clone()).collect();
        for temp in temps {
            self.untrack(&temp);
        }
        self.docs.insert(info.id.clone(), Doc::new(info.id.clone(), path, info.revision));
        self.attach(&info.id);
    }

    fn untrack(&mut self, id: &DocumentId) {
        let Some(mut doc) = self.docs.remove(id) else { return };
        doc.cancel_audit();
        if let (Some(sync), Some(uri), Some(key)) = (&doc.sync, &doc.uri, &doc.key)
            && let Some(server) = self.servers.get(key)
            && server.generation == sync.generation
            && let Some(r) = server.running()
        {
            let _ = r.client.notify("textDocument/didClose", json!({ "textDocument": { "uri": uri } }));
        }
        if !doc.is_temp() {
            let _ = self.events.send(LeanEvent::Removed { document: id.clone() });
        }
    }

    fn on_info(&mut self, info: DocumentInfo) {
        let Some(doc) = self.docs.get(&info.id) else {
            self.track(&info);
            return;
        };
        if info.path.as_ref() != Some(&doc.path) {
            // Renamed or moved: a different file for Lean.
            self.untrack(&info.id);
            self.track(&info);
            return;
        }
        let (Some(sync), Some(uri), Some(key)) = (&doc.sync, &doc.uri, &doc.key) else { return };
        // Saving lets Lean tell dependent files that their imports are out of date.
        if info.saved_revision == Some(sync.version)
            && info.revision == sync.version
            && let Some(r) = self.servers.get(key).and_then(|s| s.running())
        {
            let _ =
                r.client.notify("textDocument/didSave", json!({ "textDocument": { "uri": uri }, "text": &*sync.text }));
        }
    }

    /// Resolve project, toolchain, and trust for a document and open it in its
    /// server (starting the server when needed).
    fn attach(&mut self, id: &DocumentId) {
        let Some(root) = self.root.clone() else {
            self.set_status(
                id,
                LeanStatus::Unavailable {
                    reason: "Open the folder that contains this file to check it with Lean.".into(),
                },
            );
            return;
        };
        let Some(doc) = self.docs.get_mut(id) else { return };
        let abs = join(&root, &doc.path);
        doc.uri = Some(path_to_uri(&abs));
        doc.abs = Some(abs.clone());
        doc.key = None;
        doc.sync = None;
        let project = project::project_for(&root, &abs);
        doc.project_root = Some(project.root.clone());
        let elan_home = self.config.elan_home.clone();
        let pin = match &project.pin {
            Err(reason) => {
                let reason = reason.clone();
                self.set_status(id, LeanStatus::Unavailable { reason });
                return;
            }
            Ok(pin) => toolchain::effective_pin(pin.clone(), elan_home.as_deref()),
        };
        let Some(pin) = pin else {
            self.set_status(
                id,
                LeanStatus::ToolchainMissing {
                    toolchain: "(not specified)".into(),
                    install: None,
                    guidance: "This file has no lean-toolchain file above it and elan has no default toolchain. Add a lean-toolchain file (for example `leanprover/lean4:v4.35.0`) to the project.".into(),
                },
            );
            return;
        };
        let resolved = match toolchain::resolve(elan_home.as_deref(), &pin.requested) {
            Resolution::Installed(t) => t,
            Resolution::Missing { requested, install, guidance } => {
                self.set_status(
                    id,
                    LeanStatus::ToolchainMissing { toolchain: requested, install: Some(install), guidance },
                );
                return;
            }
            Resolution::Invalid { reason, .. } => {
                self.set_status(id, LeanStatus::Unavailable { reason });
                return;
            }
        };
        let key = ServerKey { root: project.root.clone(), toolchain: resolved.dir.clone() };
        if let Some(doc) = self.docs.get_mut(id) {
            doc.key = Some(key.clone());
        }
        if !self.servers.contains_key(&key) {
            let source = match &pin.source {
                PinSource::File(path) => path.display().to_string(),
                PinSource::ElanDefault => "elan default toolchain".into(),
            };
            let info = LeanToolchainInfo {
                requested: resolved.requested.clone(),
                name: resolved.name.clone(),
                version: self.versions.get(&resolved.dir).cloned().flatten(),
                source,
                note: resolved.note.clone(),
            };
            self.servers.insert(
                key.clone(),
                Server {
                    project,
                    toolchain: resolved,
                    info,
                    generation: 0,
                    phase: Phase::Stopped,
                    attempts: 0,
                    idle_since: None,
                },
            );
        }
        let server = &self.servers[&key];
        let toolchain_name = server.info.name.clone();
        match &server.phase {
            Phase::Running(_) => self.open_in_server(id),
            Phase::Starting | Phase::Stopping { .. } => {
                let revision = self.docs[id].revision;
                self.set_status(
                    id,
                    LeanStatus::Checking {
                        revision,
                        toolchain: toolchain_name,
                        starting: true,
                        auditing: false,
                        processing: vec![],
                    },
                );
            }
            Phase::Crashed { message, restarting } => {
                let status = LeanStatus::ServerCrashed {
                    message: message.clone(),
                    restarting: *restarting,
                    attempts: server.attempts,
                };
                self.set_status(id, status);
            }
            Phase::Unavailable { reason } => {
                let status = LeanStatus::ServerUnavailable { reason: reason.clone() };
                self.set_status(id, status);
            }
            Phase::Stopped | Phase::Untrusted { .. } => self.ensure_started(&key),
        }
    }

    fn reattach(&mut self, id: &DocumentId) {
        if let Some(doc) = self.docs.get_mut(id) {
            doc.sync = None;
            doc.key = None;
        }
        self.attach(id);
    }

    fn on_changed(&mut self, changed: DocumentChanged) {
        let id = changed.document.clone();
        let Some(doc) = self.docs.get_mut(&id) else { return };
        if doc.sync.as_ref().is_some_and(|s| s.version >= changed.revision) {
            return; // already sent (a resync read a later snapshot)
        }
        doc.revision = doc.revision.max(changed.revision);
        // Waiters for older revisions will never see them finished.
        let revision = doc.revision;
        let (stale, keep): (Vec<_>, Vec<_>) = doc.waiters.drain(..).partition(|(r, _)| *r < revision);
        doc.waiters = keep;
        for (_, w) in stale {
            let _ = w.send(());
        }
        let key = doc.key.clone();
        let running = key
            .as_ref()
            .and_then(|k| self.servers.get(k))
            .and_then(|s| s.running().map(|r| (s.generation, r.client.clone())));
        let synced = matches!((&self.docs[&id].sync, &running), (Some(s), Some((g, _))) if s.generation == *g);
        if !synced {
            // Not open in a running server. An untrusted server re-checks trust
            // (the user may have trusted the folder since); a server that is
            // starting picks up the latest text when it opens the document.
            if let Some(key) = key
                && matches!(
                    self.servers.get(&key).map(|s| &s.phase),
                    Some(Phase::Untrusted { .. } | Phase::Unavailable { .. })
                )
            {
                self.ensure_started(&key);
            } else if let Some(doc) = self.docs.get_mut(&id)
                && let LeanStatus::Checking { revision, .. } = &mut doc.status
            {
                *revision = doc.revision;
                self.emit(&id);
            }
            return;
        }
        let (generation, client) = running.expect("synced");
        let doc = self.docs.get(&id).expect("present");
        let sync = doc.sync.as_ref().expect("synced");
        let uri = doc.uri.clone().unwrap_or_default();
        let incremental = if sync.version == changed.previous_revision {
            convert::apply_patches(&sync.text, &changed.patches)
                .zip(convert::content_changes(&sync.index, &changed.patches))
                .map(|(text, changes)| (text, changes, changed.revision))
        } else {
            None
        };
        let (text, changes, version) = match incremental {
            Some(v) => v,
            // A gap (missed events) or patches we cannot express: send the
            // whole current text.
            None => match self.documents.snapshot(&id) {
                Ok(s) => (s.text.to_string(), vec![json!({ "text": &*s.text })], s.revision),
                Err(_) => return,
            },
        };
        let _ = client.notify(
            "textDocument/didChange",
            json!({ "textDocument": { "uri": uri, "version": version }, "contentChanges": changes }),
        );
        let doc = self.docs.get_mut(&id).expect("present");
        doc.cancel_audit();
        let sync = doc.sync.as_mut().expect("synced");
        let text: Arc<str> = Arc::from(text);
        sync.index = Arc::new(LineIndex::new(&text));
        sync.text = text;
        sync.version = version;
        sync.lsp_diagnostics.clear();
        sync.fatal = false;
        doc.revision = doc.revision.max(version);
        let toolchain = key.as_ref().map(|k| self.toolchain_name(k)).unwrap_or_default();
        self.set_status(
            &id,
            LeanStatus::Checking { revision: version, toolchain, starting: false, auditing: false, processing: vec![] },
        );
        self.spawn_wait(&id, generation, client, uri, version);
        self.schedule_audit(&id, generation, version);
    }

    fn resync_documents(&mut self) {
        let open: Vec<DocumentInfo> = self.documents.list();
        let tracked: Vec<DocumentId> = self.docs.values().filter(|d| !d.is_temp()).map(|d| d.id.clone()).collect();
        for id in tracked {
            if !open.iter().any(|i| i.id == id) {
                self.untrack(&id);
            }
        }
        for info in &open {
            if !self.docs.contains_key(&info.id) {
                self.track(info);
            } else if self.docs[&info.id].sync.as_ref().is_some_and(|s| s.version != info.revision) {
                // Force a full-text change at the current revision.
                self.on_changed(DocumentChanged {
                    document: info.id.clone(),
                    transaction: None,
                    origin: exposition_core::document::EditOrigin::System,
                    change: exposition_core::document::ChangeKind::Edit,
                    previous_revision: u64::MAX,
                    revision: info.revision,
                    patches: vec![],
                    selection: None,
                    can_undo: info.can_undo,
                    can_redo: info.can_redo,
                });
            }
        }
    }

    // ----- servers -------------------------------------------------------------------

    /// `Err(root)` when the workspace is not trusted.
    fn workspace_trusted(&self) -> Result<(), String> {
        let Some(root) = &self.root else { return Err(String::new()) };
        let display = root.display().to_string();
        match self.exec.trust_status(&display) {
            Ok(status) if status.trusted => Ok(()),
            Ok(status) => Err(status.root),
            Err(_) => Err(display),
        }
    }

    /// Start a server unless the workspace is untrusted. Opening a project
    /// never runs it: an untrusted workspace stays `Untrusted` until the user
    /// trusts it.
    fn ensure_started(&mut self, key: &ServerKey) {
        if let Err(workspace_root) = self.workspace_trusted() {
            if let Some(server) = self.servers.get_mut(key) {
                server.phase = Phase::Untrusted { workspace_root: workspace_root.clone() };
            }
            for id in self.docs_of(key) {
                if let Some(doc) = self.docs.get_mut(&id) {
                    doc.sync = None;
                }
                self.set_status(&id, LeanStatus::Untrusted { workspace_root: workspace_root.clone() });
            }
            return;
        }
        self.start_server(key);
    }

    fn start_server(&mut self, key: &ServerKey) {
        self.next_generation += 1;
        let generation = self.next_generation;
        let Some(server) = self.servers.get_mut(key) else { return };
        server.generation = generation;
        server.phase = Phase::Starting;
        server.idle_since = None;
        let project = server.project.clone();
        let toolchain = server.toolchain.clone();
        let name = server.info.name.clone();
        for id in self.docs_of(key) {
            let revision = self.docs[&id].revision;
            if let Some(doc) = self.docs.get_mut(&id) {
                doc.sync = None;
            }
            self.set_status(
                &id,
                LeanStatus::Checking {
                    revision,
                    toolchain: name.clone(),
                    starting: true,
                    auditing: false,
                    processing: vec![],
                },
            );
        }
        if !self.versions.contains_key(&toolchain.dir) {
            self.versions.insert(toolchain.dir.clone(), None);
            let (exec, tx, toolchain) = (self.exec.clone(), self.tx.clone(), toolchain.clone());
            tokio::spawn(async move {
                let version = server::probe_version(&exec, &toolchain).await;
                let _ = tx.send(Msg::Version { dir: toolchain.dir.clone(), version });
            });
        }
        let (exec, tx, key) = (self.exec.clone(), self.tx.clone(), key.clone());
        tokio::spawn(async move {
            let result = server::start(&exec, &project, &toolchain).await;
            let _ = tx.send(Msg::Started { key, generation, result });
        });
    }

    fn on_started(&mut self, key: ServerKey, generation: u64, result: Result<StartedServer, StartError>) {
        let current =
            self.servers.get(&key).is_some_and(|s| s.generation == generation && matches!(s.phase, Phase::Starting));
        if !current {
            if let Ok(started) = result {
                let exec = self.exec.clone();
                tokio::spawn(async move { server::stop(&exec, &started.client, &started.job).await });
            }
            return;
        }
        match result {
            Ok(started) => {
                let StartedServer { client, mut incoming, job, pid, sorry_token_type } = started;
                let (tx, k) = (self.tx.clone(), key.clone());
                tokio::spawn(async move {
                    while let Some(message) = incoming.recv().await {
                        let closed = matches!(message, Incoming::Closed { .. });
                        if tx.send(Msg::Incoming { key: k.clone(), generation, incoming: message }).is_err() || closed {
                            break;
                        }
                    }
                });
                let (exec, tx, k, j) = (self.exec.clone(), self.tx.clone(), key.clone(), job.clone());
                tokio::spawn(async move {
                    let result = exec.wait(&j).await;
                    let _ = tx.send(Msg::JobExited { key: k, generation, result });
                });
                let server = self.servers.get_mut(&key).expect("checked");
                server.phase = Phase::Running(Running { client, job, pid, sorry_type: sorry_token_type });
                for id in self.docs_of(&key) {
                    self.open_in_server(&id);
                }
            }
            Err(StartError::Untrusted { workspace_root }) => {
                if let Some(server) = self.servers.get_mut(&key) {
                    server.phase = Phase::Untrusted { workspace_root: workspace_root.clone() };
                }
                for id in self.docs_of(&key) {
                    self.set_status(&id, LeanStatus::Untrusted { workspace_root: workspace_root.clone() });
                }
            }
            Err(StartError::Unavailable { reason }) => self.unavailable(&key, reason),
            Err(StartError::Failed { message, .. }) => self.crashed(&key, message),
        }
    }

    fn open_in_server(&mut self, id: &DocumentId) {
        let Some(doc) = self.docs.get(id) else { return };
        let Some(key) = doc.key.clone() else { return };
        let Some(server) = self.servers.get(&key) else { return };
        let Some(running) = server.running() else { return };
        let (client, generation, toolchain) = (running.client.clone(), server.generation, server.info.name.clone());
        let (text, revision): (Arc<str>, u64) = match &doc.temp_text {
            Some(text) => (text.clone(), doc.revision),
            None => match self.documents.snapshot(id) {
                Ok(s) => (s.text.clone(), s.revision),
                Err(_) => return,
            },
        };
        let uri = doc.uri.clone().unwrap_or_default();
        let _ = client.notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": "lean4", "version": revision, "text": &*text } }),
        );
        let doc = self.docs.get_mut(id).expect("present");
        doc.sync = Some(SyncState {
            generation,
            version: revision,
            index: Arc::new(LineIndex::new(&text)),
            text,
            lsp_diagnostics: Vec::new(),
            fatal: false,
        });
        doc.revision = doc.revision.max(revision);
        self.set_status(
            id,
            LeanStatus::Checking { revision, toolchain, starting: false, auditing: false, processing: vec![] },
        );
        self.spawn_wait(id, generation, client, uri, revision);
        self.schedule_audit(id, generation, revision);
    }

    fn spawn_wait(&self, id: &DocumentId, generation: u64, client: Arc<LspClient>, uri: String, version: u64) {
        let (tx, doc) = (self.tx.clone(), id.clone());
        tokio::spawn(async move {
            let result = client
                .call(
                    "textDocument/waitForDiagnostics",
                    json!({ "uri": uri, "version": version }),
                    WAIT_DIAGNOSTICS_TIMEOUT,
                )
                .await
                .map(|_| ());
            let _ = tx.send(Msg::WaitDone { generation, doc, version, result });
        });
    }

    /// True when the document's sync state is exactly (generation, version).
    fn current_sync(&self, id: &DocumentId, generation: u64, version: u64) -> bool {
        self.docs
            .get(id)
            .and_then(|d| d.sync.as_ref())
            .is_some_and(|s| s.generation == generation && s.version == version)
    }

    fn on_wait_done(&mut self, generation: u64, id: DocumentId, version: u64, result: Result<(), LspError>) {
        if !self.current_sync(&id, generation, version) {
            return; // a newer version was sent, or the server restarted
        }
        match result {
            Ok(()) => {
                let doc = &self.docs[&id];
                let sync = doc.sync.as_ref().expect("current");
                let errors = sync.fatal
                    || sync
                        .lsp_diagnostics
                        .iter()
                        .any(|d| matches!(d.get("severity").and_then(Value::as_u64), Some(1) | None));
                if errors {
                    // Errors are final without an audit: this is not a checked proof.
                    self.docs.get_mut(&id).expect("present").cancel_audit();
                    self.finalize(&id, Vec::new(), None);
                    return;
                }
                let has_sorry = sync
                    .lsp_diagnostics
                    .iter()
                    .any(|d| d.get("message").and_then(Value::as_str).is_some_and(is_sorry_message));
                if !self.docs[&id].work.as_ref().is_some_and(|w| w.version == version && w.generation == generation) {
                    self.start_audit(&id, generation, version);
                }
                self.request_tokens(&id, generation, version, has_sorry);
                if let Some(work) = self.docs.get_mut(&id).and_then(|d| d.work.as_mut()) {
                    work.lean_done = true;
                }
                if !self.try_finalize(&id) {
                    let toolchain = self.docs[&id].key.as_ref().map(|k| self.toolchain_name(k)).unwrap_or_default();
                    self.set_status(
                        &id,
                        LeanStatus::Checking {
                            revision: version,
                            toolchain,
                            starting: false,
                            auditing: true,
                            processing: vec![],
                        },
                    );
                }
            }
            Err(e) if e.is_superseded() || e == LspError::Closed => {}
            Err(e) => {
                let reason = format!("Lean could not finish checking revision {version}: {e}");
                self.set_status(&id, LeanStatus::Unavailable { reason });
            }
        }
    }

    /// Start the axiom audit of a version once it has been stable briefly, so
    /// it runs alongside Lean instead of after it (typing never starts one).
    fn schedule_audit(&self, id: &DocumentId, generation: u64, version: u64) {
        let (tx, doc) = (self.tx.clone(), id.clone());
        tokio::spawn(async move {
            tokio::time::sleep(AUDIT_DEBOUNCE).await;
            let _ = tx.send(Msg::StartAudit { generation, doc, version });
        });
    }

    fn on_start_audit(&mut self, id: &DocumentId, generation: u64, version: u64) {
        if !self.current_sync(id, generation, version) {
            return;
        }
        let doc = &self.docs[id];
        let busy = doc.work.as_ref().is_some_and(|w| w.version == version && w.generation == generation);
        if busy || doc.status.finished_revision() == Some(version) {
            return;
        }
        self.start_audit(id, generation, version);
    }

    /// Run the axiom audit of exactly the text of `version`.
    fn start_audit(&mut self, id: &DocumentId, generation: u64, version: u64) {
        let doc = &self.docs[id];
        let Some(sync) = doc.sync.as_ref() else { return };
        let Some(key) = doc.key.clone() else { return };
        let Some(server) = self.servers.get(&key) else { return };
        if server.running().is_none() {
            return;
        }
        let (exec, tx) = (self.exec.clone(), self.tx.clone());
        let (project, toolchain) = (server.project.clone(), server.toolchain.clone());
        let text = sync.text.clone();
        let file_name = format!("{} (revision {version})", doc.path.file_name());
        let scratch = self.config.scratch_dir.join("audit");
        let (cancel, cancel_rx) = watch::channel(false);
        let doc = self.docs.get_mut(id).expect("present");
        doc.work = Some(CheckWork { generation, version, cancel, audit: None, tokens: None, lean_done: false });
        let doc_id = id.clone();
        tokio::spawn(async move {
            let request = audit::AuditRequest {
                project: &project,
                toolchain: &toolchain,
                text,
                scratch,
                nonce: audit::nonce(),
                file_name,
            };
            let outcome = audit::run_audit(&exec, request, cancel_rx).await;
            let _ = tx.send(Msg::Audited { generation, doc: doc_id, version, outcome });
        });
    }

    /// Locate the exact `sorry`/`admit` tokens (needs Lean to have finished).
    fn request_tokens(&mut self, id: &DocumentId, generation: u64, version: u64, has_sorry: bool) {
        let doc = &self.docs[id];
        let running = doc.key.as_ref().and_then(|k| self.servers.get(k)).and_then(|s| s.running());
        let request = running.and_then(|r| r.sorry_type.map(|t| (r.client.clone(), t))).filter(|_| has_sorry);
        let Some((client, token_type)) = request else {
            if let Some(work) = self.docs.get_mut(id).and_then(|d| d.work.as_mut()) {
                work.tokens = Some(Vec::new());
            }
            return;
        };
        let sync = doc.sync.as_ref().expect("current");
        let (tx, uri, index, doc_id) =
            (self.tx.clone(), doc.uri.clone().unwrap_or_default(), sync.index.clone(), id.clone());
        tokio::spawn(async move {
            let tokens = client
                .call(
                    "textDocument/semanticTokens/full",
                    json!({ "textDocument": { "uri": uri } }),
                    Duration::from_secs(120),
                )
                .await
                .ok()
                .and_then(|v| v.get("data").and_then(Value::as_array).cloned())
                .map(|data| {
                    let data: Vec<u64> = data.iter().filter_map(Value::as_u64).collect();
                    convert::tokens_of_type(&index, &data, token_type)
                })
                .unwrap_or_default();
            let _ = tx.send(Msg::SorryTokens { generation, doc: doc_id, version, tokens });
        });
    }

    /// Finalize when Lean, the audit, and the token request are all done.
    fn try_finalize(&mut self, id: &DocumentId) -> bool {
        let Some(doc) = self.docs.get_mut(id) else { return false };
        let ready = doc.work.as_ref().is_some_and(|w| w.lean_done && w.audit.is_some() && w.tokens.is_some());
        if !ready {
            return false;
        }
        let mut work = doc.work.take().expect("ready");
        let (tokens, outcome) = (work.tokens.take().unwrap_or_default(), work.audit.take());
        self.finalize(id, tokens, outcome);
        true
    }

    fn work_mut(&mut self, id: &DocumentId, generation: u64, version: u64) -> Option<&mut CheckWork> {
        if !self.current_sync(id, generation, version) {
            return None;
        }
        self.docs.get_mut(id)?.work.as_mut().filter(|w| w.generation == generation && w.version == version)
    }

    /// Lean finished the document's current version: compute its final status
    /// from the final diagnostics of exactly that version and, when Lean
    /// reported no errors, the axiom audit of exactly that text.
    fn finalize(&mut self, id: &DocumentId, tokens: Vec<TextRange>, audit: Option<audit::AuditOutcome>) {
        let Some(doc) = self.docs.get(id) else { return };
        let (Some(sync), Some(key)) = (&doc.sync, doc.key.clone()) else { return };
        let toolchain = self.toolchain_name(&key);
        let version = sync.version;
        let mut diagnostics: Vec<Diagnostic> =
            sync.lsp_diagnostics.iter().map(|d| convert::diagnostic(d, &sync.index, &doc.path)).collect();
        let mut errors = diagnostics.iter().filter(|d| d.severity == Severity::Error).count() as u32;
        if sync.fatal && errors == 0 {
            diagnostics.push(Diagnostic {
                severity: Severity::Error,
                message: "Lean stopped processing this file before its end (fatal error). Fix the reported problem, then restart the file.".into(),
                location: None,
                hints: vec![],
            });
            errors = 1;
        }
        let warnings =
            diagnostics.iter().filter(|d| d.severity == Severity::Warning && !is_sorry_message(&d.message)).count()
                as u32;
        let mut sorries: Vec<SorryLocation> = diagnostics
            .iter()
            .filter(|d| is_sorry_message(&d.message))
            .filter_map(|d| d.location.as_ref())
            .map(|loc| {
                let line = sync.index.line_column(loc.range.start as usize).map(|lc| lc.line).unwrap_or(0);
                let name = exposition_core::text::utf16_range_to_bytes(&sync.text, loc.range)
                    .ok()
                    .map(|b| sync.text[b].trim().to_owned())
                    .filter(|n| !n.is_empty() && n.len() <= 200 && !n.contains(char::is_whitespace));
                SorryLocation {
                    range: loc.range,
                    line,
                    declaration: name,
                    tokens: Vec::new(),
                    source: SorrySource::Warning,
                }
            })
            .collect();
        let mut classification = None;
        if errors == 0 {
            let audit = audit.unwrap_or_else(|| audit::AuditOutcome::Failed("the file was not audited".into()));
            let c = Classification::new(&audit, &sync.text, &sync.index);
            // Declarations that depend on `sorry` without a warning (imported,
            // reached through another declaration, or warning suppressed).
            for d in &c.sorry_decls {
                let overlaps = |r: TextRange| {
                    sorries
                        .iter()
                        .any(|s| s.range.start < r.end.max(r.start + 1) && r.start < s.range.end.max(s.range.start + 1))
                };
                if d.range.is_some_and(overlaps) {
                    continue;
                }
                sorries.push(SorryLocation {
                    range: d.range.unwrap_or(TextRange::empty(0)),
                    line: d.line.unwrap_or(0),
                    declaration: Some(d.declaration.clone()),
                    tokens: Vec::new(),
                    source: SorrySource::AxiomAudit,
                });
                diagnostics.push(audit_diagnostic(
                    &doc.path,
                    d.range,
                    format!(
                        "`{}` depends on `sorry` (found by the axiom audit: the `sorry` is imported, reached through another declaration, or its warning is suppressed).",
                        d.declaration
                    ),
                ));
            }
            for d in c.compiled.iter().chain(&c.nonstandard) {
                diagnostics.push(audit_diagnostic(
                    &doc.path,
                    d.range,
                    format!(
                        "`{}` depends on axioms beyond Lean's standard ones: {}.",
                        d.declaration,
                        d.axioms.join(", ")
                    ),
                ));
            }
            if let Some(reason) = c.unverified.as_ref().or(c.stop_reason.as_ref()) {
                diagnostics.push(Diagnostic {
                    severity: Severity::Warning,
                    message: reason.clone(),
                    location: None,
                    hints: vec![AUDIT_HINT.into()],
                });
            }
            classification = Some(c);
        }
        sorries.sort_by_key(|s| s.range.start);
        for token in tokens {
            // A token belongs to the last declaration that starts before it.
            let owner = sorries.iter().rposition(|s| s.range.start <= token.start).unwrap_or(0);
            if let Some(s) = sorries.get_mut(owner) {
                s.tokens.push(token);
            }
        }
        let sorry_count = sorries.len() as u32;
        let (axioms, axiom_uses) =
            classification.as_ref().map(|c| (c.all_axioms.clone(), c.axiom_uses.clone())).unwrap_or_default();
        let status = match classification {
            None => LeanStatus::Errors { revision: version, toolchain, count: errors, sorry_count },
            Some(c) => {
                if let Some(reason) = c.unverified {
                    LeanStatus::Unverified { revision: version, toolchain, reason }
                } else if let Some(reason) = c.stop_reason {
                    LeanStatus::PartiallyChecked { revision: version, toolchain, line: c.stop_line, reason }
                } else if sorry_count > 0 {
                    LeanStatus::CheckedWithSorry {
                        revision: version,
                        toolchain,
                        count: sorry_count,
                        locations: sorries.clone(),
                        warnings,
                    }
                } else if !c.compiled.is_empty() {
                    LeanStatus::ReliesOnCompiledCode {
                        revision: version,
                        toolchain,
                        axioms: c.nonstandard_axioms,
                        declarations: c.compiled,
                        warnings,
                    }
                } else if !c.nonstandard.is_empty() {
                    LeanStatus::CheckedWithAxioms {
                        revision: version,
                        toolchain,
                        axioms: c.nonstandard_axioms,
                        declarations: c.nonstandard,
                        warnings,
                    }
                } else {
                    LeanStatus::Checked { revision: version, toolchain, warnings, axioms: c.all_axioms }
                }
            }
        };
        if let Some(server) = self.servers.get_mut(&key) {
            server.attempts = 0; // the server demonstrably works again
        }
        let doc = self.docs.get_mut(id).expect("present");
        doc.work = None;
        doc.diagnostics = diagnostics;
        doc.diagnostics_pending = false;
        doc.diagnostics_revision = Some(version);
        doc.sorries = sorries;
        doc.axioms = axioms;
        doc.axiom_uses = axiom_uses;
        self.set_status(id, status);
    }

    fn on_incoming(&mut self, key: ServerKey, generation: u64, incoming: Incoming) {
        let live = self.servers.get(&key).is_some_and(|s| s.generation == generation && s.running().is_some());
        if !live {
            return;
        }
        match incoming {
            Incoming::Notification { method, params } => match method.as_str() {
                "textDocument/publishDiagnostics" => self.on_diagnostics(&key, generation, params),
                "$/lean/fileProgress" => self.on_progress(&key, generation, &params),
                _ => {}
            },
            Incoming::Closed { .. } => {
                // The stream ended: wait (briefly) for the process result to
                // describe the crash, killing the tree if it lingers.
                let job = self.servers[&key].running().map(|r| r.job.clone()).expect("live");
                let (exec, tx) = (self.exec.clone(), self.tx.clone());
                tokio::spawn(async move {
                    let result = match tokio::time::timeout(Duration::from_secs(5), exec.wait(&job)).await {
                        Ok(r) => r,
                        Err(_) => {
                            exec.cancel(&job);
                            exec.wait(&job).await
                        }
                    };
                    let _ = tx.send(Msg::JobExited { key, generation, result });
                });
            }
        }
    }

    fn doc_for_uri(&self, key: &ServerKey, generation: u64, uri: &str) -> Option<DocumentId> {
        let path = uri_to_path(uri);
        self.docs
            .values()
            .filter(|d| d.key.as_ref() == Some(key) && d.sync.as_ref().is_some_and(|s| s.generation == generation))
            .find(|d| d.uri.as_deref() == Some(uri) || matches!((&path, &d.abs), (Some(p), Some(a)) if same_path(p, a)))
            .map(|d| d.id.clone())
    }

    fn on_diagnostics(&mut self, key: &ServerKey, generation: u64, mut params: Value) {
        let Some(uri) = params.get("uri").and_then(Value::as_str) else { return };
        let Some(id) = self.doc_for_uri(key, generation, uri) else { return };
        let doc = self.docs.get_mut(&id).expect("found");
        let sync = doc.sync.as_mut().expect("found with sync");
        let version = params.get("version").and_then(Value::as_u64).unwrap_or(sync.version);
        if version != sync.version {
            return;
        }
        // Keep the latest list as received; it is converted when the UI is
        // updated (coalesced) and once more at the end.
        sync.lsp_diagnostics = match params.get_mut("diagnostics").map(Value::take) {
            Some(Value::Array(list)) => list,
            _ => Vec::new(),
        };
        doc.diagnostics_pending = true;
        doc.dirty = true;
    }

    fn on_progress(&mut self, key: &ServerKey, generation: u64, params: &Value) {
        let Some(uri) = params.pointer("/textDocument/uri").and_then(Value::as_str) else { return };
        let Some(id) = self.doc_for_uri(key, generation, uri) else { return };
        let doc = self.docs.get_mut(&id).expect("found");
        let sync = doc.sync.as_mut().expect("found with sync");
        let version = params.pointer("/textDocument/version").and_then(Value::as_u64).unwrap_or(sync.version);
        if version != sync.version {
            return;
        }
        let mut ranges = Vec::new();
        for item in params.get("processing").and_then(Value::as_array).into_iter().flatten() {
            // kind 1 = processing, 2 = fatal error
            if item.get("kind").and_then(Value::as_u64) == Some(2) {
                sync.fatal = true;
            }
            if let Some(range) = item.get("range").and_then(|r| convert::range_of(&sync.index, r)) {
                ranges.push(range);
            }
        }
        if let LeanStatus::Checking { revision, processing, .. } = &mut doc.status
            && *revision == version
            && *processing != ranges
        {
            *processing = ranges;
            doc.dirty = true;
        }
    }

    fn on_job_exited(&mut self, key: ServerKey, generation: u64, result: Option<JobResult>) {
        let Some(server) = self.servers.get(&key) else { return };
        if server.generation != generation {
            return;
        }
        // Stopping servers finish through `Stopped`; only a running server crashed.
        let Some(running) = server.running() else { return };
        let job = running.job.clone();
        let outcome = match result.map(|r| r.outcome) {
            Some(JobOutcome::Exited { code }) => format!("The Lean server exited unexpectedly with code {code}."),
            Some(JobOutcome::Terminated { description }) => format!("The Lean server was terminated ({description})."),
            Some(JobOutcome::Canceled) => "The Lean server was stopped.".to_owned(),
            Some(JobOutcome::TimedOut) => "The Lean server timed out.".to_owned(),
            Some(JobOutcome::FailedToStart { reason }) => format!("The Lean server failed: {reason}"),
            None => "The Lean server stopped.".to_owned(),
        };
        let tail = server::stderr_tail(&self.exec, &job).map(|t| format!("\n{t}")).unwrap_or_default();
        self.crashed(&key, format!("{outcome}{tail}"));
    }

    /// The server could not start for a reason that may pass: say so (it
    /// never ran, so it did not crash) and retry later.
    fn unavailable(&mut self, key: &ServerKey, reason: String) {
        let Some(server) = self.servers.get_mut(key) else { return };
        server.phase = Phase::Unavailable { reason: reason.clone() };
        let generation = server.generation;
        for id in self.docs_of(key) {
            if let Some(doc) = self.docs.get_mut(&id) {
                doc.sync = None;
            }
            self.set_status(&id, LeanStatus::ServerUnavailable { reason: reason.clone() });
        }
        let (tx, key) = (self.tx.clone(), key.clone());
        tokio::spawn(async move {
            tokio::time::sleep(UNAVAILABLE_RETRY).await;
            let _ = tx.send(Msg::Retry { key, generation });
        });
    }

    fn crashed(&mut self, key: &ServerKey, message: String) {
        let Some(server) = self.servers.get_mut(key) else { return };
        server.attempts += 1;
        let attempts = server.attempts;
        let delay = self.config.restart_backoff.get(attempts as usize - 1).copied();
        let restarting = delay.is_some();
        server.phase = Phase::Crashed { message: message.clone(), restarting };
        let generation = server.generation;
        for id in self.docs_of(key) {
            if let Some(doc) = self.docs.get_mut(&id) {
                doc.sync = None;
                doc.goal_requests.clear();
                doc.cancel_audit();
            }
            self.set_status(&id, LeanStatus::ServerCrashed { message: message.clone(), restarting, attempts });
        }
        if let Some(delay) = delay {
            let (tx, key) = (self.tx.clone(), key.clone());
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                let _ = tx.send(Msg::Retry { key, generation });
            });
        }
    }

    /// Stop a server; afterwards its documents are re-attached (which starts
    /// a new server when `restart`, or when documents still need it).
    fn stop_server(&mut self, key: &ServerKey, restart: bool) {
        let Some(server) = self.servers.get_mut(key) else { return };
        let phase = std::mem::replace(&mut server.phase, Phase::Stopping { restart });
        let generation = server.generation;
        match phase {
            Phase::Running(running) => {
                let (exec, tx, key) = (self.exec.clone(), self.tx.clone(), key.clone());
                tokio::spawn(async move {
                    server::stop(&exec, &running.client, &running.job).await;
                    let _ = tx.send(Msg::Stopped { key, generation });
                });
            }
            Phase::Stopping { restart: was } => server.phase = Phase::Stopping { restart: restart || was },
            // Starting: `on_started` sees a phase other than `Starting` and
            // stops the new process itself.
            _ => {
                let _ = self.tx.send(Msg::Stopped { key: key.clone(), generation });
            }
        }
        let name = self.toolchain_name(key);
        for id in self.docs_of(key) {
            if let Some(doc) = self.docs.get_mut(&id) {
                doc.sync = None;
            }
            if restart {
                let revision = self.docs[&id].revision;
                self.set_status(
                    &id,
                    LeanStatus::Checking {
                        revision,
                        toolchain: name.clone(),
                        starting: true,
                        auditing: false,
                        processing: vec![],
                    },
                );
            }
        }
    }

    fn on_stopped(&mut self, key: ServerKey, generation: u64) {
        let Some(server) = self.servers.get(&key) else { return };
        if server.generation != generation || !matches!(server.phase, Phase::Stopping { .. }) {
            return;
        }
        let ids = self.docs_of(&key);
        self.servers.remove(&key);
        for id in ids {
            self.reattach(&id);
        }
    }

    /// Restart a server: stop it, then re-resolve project/toolchain/trust for
    /// its documents (which starts a new server).
    fn restart_server(&mut self, key: &ServerKey) {
        let active = self
            .servers
            .get(key)
            .is_some_and(|s| matches!(s.phase, Phase::Running(_) | Phase::Starting | Phase::Stopping { .. }));
        if active {
            self.stop_server(key, true);
        } else {
            let ids = self.docs_of(key);
            self.servers.remove(key);
            for id in ids {
                self.reattach(&id);
            }
        }
    }

    fn restart_all(&mut self) {
        let keys: Vec<ServerKey> = self.servers.keys().cloned().collect();
        for key in keys {
            self.restart_server(&key);
        }
    }

    fn stop_all_servers(&mut self) {
        for (_, server) in self.servers.drain() {
            if let Phase::Running(running) = server.phase {
                let exec = self.exec.clone();
                tokio::spawn(async move { server::stop(&exec, &running.client, &running.job).await });
            }
        }
        for doc in self.docs.values_mut() {
            doc.sync = None;
            doc.key = None;
        }
    }

    async fn shutdown(&mut self) {
        let mut tasks = Vec::new();
        for (_, server) in self.servers.drain() {
            if let Phase::Running(running) = server.phase {
                let exec = self.exec.clone();
                tasks.push(tokio::spawn(async move { server::stop(&exec, &running.client, &running.job).await }));
            }
        }
        for task in tasks {
            let _ = tokio::time::timeout(Duration::from_secs(6), task).await;
        }
    }

    // ----- API ---------------------------------------------------------------------

    fn on_message(&mut self, msg: Msg) {
        match msg {
            Msg::States(reply) => {
                let mut states: Vec<_> =
                    self.docs.values().filter(|d| !d.is_temp()).filter_map(|d| self.state_of(&d.id)).collect();
                states.sort_by(|a, b| a.path.cmp(&b.path));
                let _ = reply.send(states);
            }
            Msg::Projects(reply) => {
                let _ = reply.send(self.projects());
            }
            Msg::Position { document, revision, offset, kind, reply } => {
                let _ = reply.send(self.position(&document, revision, offset, kind));
            }
            Msg::Restart { document, reply } => {
                let result = match self.docs.get(&document).map(|d| d.key.clone()) {
                    None => Err(unknown_document()),
                    Some(Some(key)) => {
                        if let Some(server) = self.servers.get_mut(&key) {
                            server.attempts = 0;
                        }
                        self.restart_server(&key);
                        Ok(())
                    }
                    Some(None) => {
                        self.reattach(&document);
                        Ok(())
                    }
                };
                let _ = reply.send(result);
            }
            Msg::RestartFile { document, reply } => {
                let result = match self.docs.get(&document) {
                    None => Err(unknown_document()),
                    Some(doc) if doc.sync.is_some() => {
                        let uri = doc.uri.clone().unwrap_or_default();
                        if let Some(r) = doc.key.as_ref().and_then(|k| self.servers.get(k)).and_then(|s| s.running()) {
                            let _ = r.client.notify("textDocument/didClose", json!({ "textDocument": { "uri": uri } }));
                        }
                        self.open_in_server(&document);
                        Ok(())
                    }
                    Some(_) => {
                        self.reattach(&document);
                        Ok(())
                    }
                };
                let _ = reply.send(result);
            }
            Msg::Install { document, reply } => {
                let _ = reply.send(self.install(&document));
            }
            Msg::BeginCheck { target, reply } => {
                let _ = reply.send(self.begin_check(target));
            }
            Msg::FinishCheck { doc, reply } => {
                let snapshot = self.docs.get(&doc).and_then(|d| {
                    let state = self.state_of(&doc)?;
                    Some(CheckSnapshot {
                        state,
                        sorries: d.sorries.clone(),
                        axioms: d.axioms.clone(),
                        axiom_uses: d.axiom_uses.clone(),
                    })
                });
                let _ = reply.send(snapshot);
            }
            Msg::DropTemp { doc } => {
                if self.docs.get(&doc).is_some_and(Doc::is_temp) {
                    self.untrack(&doc);
                }
            }
            Msg::ServerPid { document, reply } => {
                let pid = self
                    .docs
                    .get(&document)
                    .and_then(|d| d.key.as_ref())
                    .and_then(|k| self.servers.get(k))
                    .and_then(|s| s.running())
                    .and_then(|r| r.pid);
                let _ = reply.send(pid);
            }
            Msg::Shutdown { reply } => {
                // Handled in `run`; reaching here means a nested call.
                let _ = reply.send(());
            }
            Msg::Started { key, generation, result } => self.on_started(key, generation, result),
            Msg::Incoming { key, generation, incoming } => self.on_incoming(key, generation, incoming),
            Msg::JobExited { key, generation, result } => self.on_job_exited(key, generation, result),
            Msg::WaitDone { generation, doc, version, result } => self.on_wait_done(generation, doc, version, result),
            Msg::StartAudit { generation, doc, version } => self.on_start_audit(&doc, generation, version),
            Msg::Audited { generation, doc, version, outcome } => {
                if let Some(work) = self.work_mut(&doc, generation, version) {
                    work.audit = Some(outcome);
                    self.try_finalize(&doc);
                }
            }
            Msg::SorryTokens { generation, doc, version, tokens } => {
                if let Some(work) = self.work_mut(&doc, generation, version) {
                    work.tokens = Some(tokens);
                    self.try_finalize(&doc);
                }
            }
            Msg::Retry { key, generation } => {
                let retry = self.servers.get(&key).is_some_and(|s| {
                    s.generation == generation
                        && matches!(s.phase, Phase::Crashed { restarting: true, .. } | Phase::Unavailable { .. })
                });
                if retry {
                    self.ensure_started(&key);
                }
            }
            Msg::Stopped { key, generation } => self.on_stopped(key, generation),
            Msg::Version { dir, version } => {
                self.versions.insert(dir.clone(), version.clone());
                let keys: Vec<ServerKey> = self.servers.keys().filter(|k| k.toolchain == dir).cloned().collect();
                for key in keys {
                    if let Some(server) = self.servers.get_mut(&key) {
                        server.info.version = version.clone();
                    }
                    for id in self.docs_of(&key) {
                        self.emit(&id);
                    }
                }
            }
            Msg::InstallDone => {
                let missing: Vec<DocumentId> = self
                    .docs
                    .values()
                    .filter(|d| matches!(d.status, LeanStatus::ToolchainMissing { .. }))
                    .map(|d| d.id.clone())
                    .collect();
                for id in missing {
                    self.reattach(&id);
                }
            }
        }
    }

    fn position(
        &mut self,
        id: &DocumentId,
        revision: u64,
        offset: u32,
        kind: PosKind,
    ) -> Result<PositionReq, LeanError> {
        let root = self.root.clone().ok_or_else(|| LeanError::new("lean.noWorkspace", "No folder is open."))?;
        let doc = self.docs.get(id).ok_or_else(unknown_document)?;
        if revision < doc.revision {
            return Err(LeanError {
                current_revision: Some(doc.revision),
                ..LeanError::new(
                    "lean.stale",
                    format!("Revision {revision} is outdated; the document is at {}.", doc.revision),
                )
            });
        }
        let not_ready = || LeanError::new("lean.notReady", "The Lean server has not received this revision yet.");
        let sync = doc.sync.as_ref().ok_or_else(not_ready)?;
        if sync.version != revision {
            return Err(not_ready());
        }
        let server = doc.key.as_ref().and_then(|k| self.servers.get(k)).filter(|s| s.generation == sync.generation);
        let client = server.and_then(|s| s.running()).map(|r| r.client.clone()).ok_or_else(not_ready)?;
        let position = convert::position(&sync.index, offset).map_err(|e| {
            LeanError::new("lean.invalidOffset", format!("Offset {offset} is not a valid position: {e}."))
        })?;
        let params = json!({ "textDocument": { "uri": doc.uri.clone().unwrap_or_default() }, "position": position });
        let requests = match kind {
            PosKind::Goals => {
                // A new goal query supersedes the previous one for this document.
                for previous in &doc.goal_requests {
                    client.cancel(*previous);
                }
                vec![client.request("$/lean/plainGoal", params.clone()), client.request("$/lean/plainTermGoal", params)]
            }
            PosKind::Hover => vec![client.request("textDocument/hover", params)],
            PosKind::Definition => vec![client.request("textDocument/definition", params)],
        };
        let (index, doc_text, doc_abs) = (sync.index.clone(), sync.text.clone(), doc.abs.clone());
        if matches!(kind, PosKind::Goals) {
            self.docs.get_mut(id).expect("present").goal_requests = requests.iter().map(PendingRequest::id).collect();
        }
        Ok(PositionReq { requests, index, root, doc_abs, doc_text })
    }

    fn projects(&self) -> Vec<LeanProjectInfo> {
        let Some(root) = &self.root else { return Vec::new() };
        let elan_home = self.config.elan_home.as_deref();
        let mut projects = project::find_projects(root);
        // Standalone servers have no lakefile to find.
        for server in self.servers.values() {
            if server.project.kind == ProjectKind::Standalone
                && !projects.iter().any(|p| p.kind == ProjectKind::Standalone)
            {
                projects.push(server.project.clone());
            }
        }
        projects
            .into_iter()
            .map(|p| {
                let toolchain = match &p.pin {
                    Err(reason) => LeanToolchainState::Invalid { reason: reason.clone() },
                    Ok(pin) => match toolchain::effective_pin(pin.clone(), elan_home) {
                        None => LeanToolchainState::Invalid {
                            reason: "No lean-toolchain file and no elan default toolchain.".into(),
                        },
                        Some(pin) => match toolchain::resolve(elan_home, &pin.requested) {
                            Resolution::Installed(t) => LeanToolchainState::Installed {
                                info: LeanToolchainInfo {
                                    requested: t.requested.clone(),
                                    name: t.name.clone(),
                                    version: self.versions.get(&t.dir).cloned().flatten(),
                                    source: match &pin.source {
                                        PinSource::File(f) => f.display().to_string(),
                                        PinSource::ElanDefault => "elan default toolchain".into(),
                                    },
                                    note: t.note.clone(),
                                },
                            },
                            Resolution::Missing { requested, install, guidance } => {
                                LeanToolchainState::Missing { requested, install, guidance }
                            }
                            Resolution::Invalid { reason, .. } => LeanToolchainState::Invalid { reason },
                        },
                    },
                };
                let server = self
                    .servers
                    .iter()
                    .find(|(k, _)| same_path(&k.root, &p.root))
                    .map(|(_, s)| match &s.phase {
                        Phase::Stopped | Phase::Stopping { .. } => LeanServerState::Stopped,
                        Phase::Starting => LeanServerState::Starting,
                        Phase::Running(r) => LeanServerState::Running { job: r.job.clone(), pid: r.pid },
                        Phase::Crashed { message, restarting } => LeanServerState::Crashed {
                            message: message.clone(),
                            attempts: s.attempts,
                            restarting: *restarting,
                        },
                        Phase::Untrusted { workspace_root } => {
                            LeanServerState::Untrusted { workspace_root: workspace_root.clone() }
                        }
                        Phase::Unavailable { reason } => LeanServerState::Unavailable { reason: reason.clone() },
                    })
                    .unwrap_or(LeanServerState::Stopped);
                LeanProjectInfo {
                    path: relative_to(root, &p.root),
                    root: p.root.display().to_string(),
                    kind: match &p.kind {
                        ProjectKind::Lake { lakefile } => LeanProjectKind::Lake { lakefile: lakefile.clone() },
                        ProjectKind::Standalone => LeanProjectKind::Standalone,
                    },
                    toolchain,
                    server,
                }
            })
            .collect()
    }

    /// Explicit user action: `elan toolchain install <toolchain>` as a trusted job.
    fn install(&mut self, id: &DocumentId) -> Result<JobId, LeanError> {
        let doc = self.docs.get(id).ok_or_else(unknown_document)?;
        let LeanStatus::ToolchainMissing { install: Some(toolchain), .. } = &doc.status else {
            return Err(LeanError::new("lean.toolchainInstalled", "This project's Lean toolchain is not missing."));
        };
        project::validate_toolchain_name(toolchain).map_err(|e| LeanError::new("lean.invalidToolchain", e))?;
        let toolchain = toolchain.clone();
        let cwd = doc
            .project_root
            .clone()
            .or_else(|| self.root.clone())
            .ok_or_else(|| LeanError::new("lean.noWorkspace", "Open the folder that contains this file first."))?;
        if let Err(root) = self.workspace_trusted() {
            return Err(LeanError::new(
                "lean.untrusted",
                format!("Trust the workspace {root} before installing a toolchain for it."),
            ));
        }
        if !matches!(self.exec.locate_tool("elan"), Located::Found { .. }) {
            return Err(LeanError::new(
                "lean.noElan",
                "elan was not found. Install elan (https://lean-lang.org/install/), then try again.",
            ));
        }
        let spec = JobSpec {
            label: format!("Install Lean toolchain {toolchain}"),
            program: "elan".into(),
            args: vec!["toolchain".into(), "install".into(), toolchain],
            environment: EnvironmentSpec::default(),
            working_directory: cwd.display().to_string(),
            policy: ExecutionPolicy::TrustedNative,
            timeout_ms: Some(INSTALL_TIMEOUT_MS),
            log_limit_bytes: Some(512 * 1024),
            stdin: None,
        };
        let job = self.exec.launch(spec);
        let (exec, tx, j) = (self.exec.clone(), self.tx.clone(), job.clone());
        tokio::spawn(async move {
            let _ = exec.wait(&j).await;
            let _ = tx.send(Msg::InstallDone);
        });
        Ok(job)
    }

    fn begin_check(&mut self, target: LeanCheckTarget) -> Result<CheckTicket, LeanError> {
        if self.root.is_none() {
            return Err(LeanError::new("lean.noWorkspace", "No folder is open."));
        }
        let ensure_tracked = |this: &mut Self, document: &DocumentId| -> Result<(), LeanError> {
            if !this.docs.contains_key(document) {
                let info = this.documents.info(document).map_err(|_| unknown_document())?;
                if !is_lean(&info) {
                    return Err(LeanError::new("lean.notLean", "The document is not a Lean file."));
                }
                this.track(&info);
            }
            Ok(())
        };
        let id = match target {
            LeanCheckTarget::Document { document } => {
                ensure_tracked(self, &document)?;
                document
            }
            LeanCheckTarget::Path { path } => {
                if path.extension() != Some("lean") {
                    return Err(LeanError::new("lean.notLean", format!("{path} is not a Lean file.")));
                }
                match self.documents.find_by_path(&path) {
                    Some(document) => {
                        ensure_tracked(self, &document)?;
                        document
                    }
                    None => {
                        let input = self
                            .workspace
                            .snapshot()
                            .and_then(|s| s.read(&path))
                            .map_err(|e| LeanError::new("lean.unreadable", e.message))?;
                        let text = input
                            .text
                            .ok_or_else(|| LeanError::new("lean.unreadable", format!("{path} is not a text file.")))?;
                        self.next_temp += 1;
                        let id = DocumentId(format!("lean-check-{}", self.next_temp));
                        let mut doc = Doc::new(id.clone(), path, 1);
                        doc.temp_text = Some(text);
                        self.docs.insert(id.clone(), doc);
                        self.attach(&id);
                        id
                    }
                }
            }
        };
        let doc = self.docs.get(&id).ok_or_else(unknown_document)?;
        let (revision, text): (u64, Arc<str>) = match &doc.temp_text {
            Some(t) => (doc.revision, t.clone()),
            None => {
                let snapshot = self.documents.snapshot(&id).map_err(|_| unknown_document())?;
                (snapshot.revision, snapshot.text)
            }
        };
        let (tx, wait) = oneshot::channel();
        let already = doc.status.finished_revision() == Some(revision)
            || (doc.status.is_settled() && doc.status.finished_revision().is_none());
        let doc = self.docs.get_mut(&id).expect("present");
        if already {
            let _ = tx.send(());
        } else {
            doc.waiters.push((revision, tx));
        }
        Ok(CheckTicket {
            doc: id,
            temp: doc.is_temp(),
            path: doc.path.clone(),
            revision,
            text_hash: sha256_hex(&text),
            wait,
        })
    }
}

const AUDIT_HINT: &str = "Reported by Exposition's axiom audit of this revision.";
const UNAVAILABLE_RETRY: Duration = Duration::from_secs(10);
/// How long a version must stay unchanged before its axiom audit starts.
const AUDIT_DEBOUNCE: Duration = Duration::from_millis(800);

fn audit_diagnostic(path: &WorkspacePath, range: Option<TextRange>, message: String) -> Diagnostic {
    Diagnostic {
        severity: Severity::Warning,
        message,
        location: range.map(|range| exposition_core::build::SourceLocation { path: path.clone(), range }),
        hints: vec![AUDIT_HINT.into()],
    }
}

/// What the axiom audit says about a revision Lean reported without errors.
struct Classification {
    /// Kernel checking disabled, or the audit could not run.
    unverified: Option<String>,
    /// Elaboration stopped before the end (`#exit`).
    stop_reason: Option<String>,
    stop_line: Option<u32>,
    sorry_decls: Vec<AxiomUse>,
    compiled: Vec<AxiomUse>,
    nonstandard: Vec<AxiomUse>,
    /// Axioms beyond the standard ones (excluding `sorryAx`), sorted.
    nonstandard_axioms: Vec<String>,
    /// Every axiom used, sorted.
    all_axioms: Vec<String>,
    /// Declarations using anything beyond the standard axioms.
    axiom_uses: Vec<AxiomUse>,
}

impl Classification {
    fn new(outcome: &audit::AuditOutcome, text: &str, index: &LineIndex) -> Self {
        let mut c = Classification {
            unverified: audit::trust_weakening(text),
            stop_reason: None,
            stop_line: None,
            sorry_decls: Vec::new(),
            compiled: Vec::new(),
            nonstandard: Vec::new(),
            nonstandard_axioms: Vec::new(),
            all_axioms: Vec::new(),
            axiom_uses: Vec::new(),
        };
        let decls = match outcome {
            audit::AuditOutcome::Complete(d) => d,
            audit::AuditOutcome::Stopped(d) => {
                c.stop_line = audit::exit_line(text);
                c.stop_reason = Some(match c.stop_line {
                    Some(line) => format!(
                        "Lean stopped elaborating at `#exit` on line {}; everything after it was never checked.",
                        line + 1
                    ),
                    None => {
                        "Lean stopped elaborating before the end of the file; the rest was never checked.".to_owned()
                    }
                });
                d
            }
            audit::AuditOutcome::Failed(reason) => {
                c.unverified.get_or_insert_with(|| {
                    format!("The axiom audit could not run, so this result is not verified: {reason}.")
                });
                return c;
            }
        };
        let to_use = |d: &audit::DeclAxioms, axioms: Vec<String>| {
            let range = d.range.and_then(|((l1, c1), (l2, c2))| {
                let start = convert::offset_at(index, l1, c1)?;
                let end = convert::offset_at(index, l2, c2)?;
                Some(TextRange::new(start.min(end), end.max(start)))
            });
            AxiomUse { declaration: d.name.clone(), range, line: d.range.map(|((l, _), _)| l), axioms }
        };
        let standard = |a: &str| audit::STANDARD_AXIOMS.contains(&a);
        let mut all = std::collections::BTreeSet::new();
        let mut nonstandard_all = std::collections::BTreeSet::new();
        for d in decls {
            all.extend(d.axioms.iter().cloned());
            nonstandard_all.extend(d.axioms.iter().filter(|a| !standard(a) && *a != audit::SORRY_AXIOM).cloned());
        }
        // Report declarations by name; internal (auxiliary) ones only when no
        // visible declaration carries the same finding.
        let pick = |pred: &dyn Fn(&str) -> bool| -> Vec<&audit::DeclAxioms> {
            let visible: Vec<_> = decls.iter().filter(|d| !d.internal && d.axioms.iter().any(|a| pred(a))).collect();
            if visible.is_empty() {
                decls.iter().filter(|d| d.axioms.iter().any(|a| pred(a))).collect()
            } else {
                visible
            }
        };
        c.sorry_decls = pick(&|a| a == audit::SORRY_AXIOM)
            .into_iter()
            .map(|d| to_use(d, vec![audit::SORRY_AXIOM.into()]))
            .collect();
        c.compiled = pick(&audit::is_compiled_code_axiom)
            .into_iter()
            .map(|d| to_use(d, d.axioms.iter().filter(|a| audit::is_compiled_code_axiom(a)).cloned().collect()))
            .collect();
        let other = |a: &str| !standard(a) && a != audit::SORRY_AXIOM && !audit::is_compiled_code_axiom(a);
        c.nonstandard = pick(&other)
            .into_iter()
            .map(|d| to_use(d, d.axioms.iter().filter(|a| other(a)).cloned().collect()))
            .collect();
        c.axiom_uses = decls
            .iter()
            .filter(|d| !d.internal && d.axioms.iter().any(|a| !standard(a)))
            .map(|d| to_use(d, d.axioms.iter().filter(|a| !standard(a)).cloned().collect()))
            .collect();
        c.all_axioms = all.into_iter().collect();
        c.nonstandard_axioms = nonstandard_all.into_iter().collect();
        c
    }
}
