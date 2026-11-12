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
    Crashed { message: String, restarting: bool },
    Untrusted { workspace_root: String },
    Stopping { restart: bool },
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
    /// Latest `publishDiagnostics` for `version`.
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
    sync: Option<SyncState>,
    waiters: Vec<(u64, oneshot::Sender<()>)>,
    dirty: bool,
    goal_requests: Vec<i64>,
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
            sync: None,
            waiters: Vec::new(),
            dirty: false,
            goal_requests: Vec::new(),
        }
    }

    fn is_temp(&self) -> bool {
        self.temp_text.is_some()
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
        let dirty: Vec<DocumentId> = self.docs.values().filter(|d| d.dirty).map(|d| d.id.clone()).collect();
        for id in dirty {
            self.emit(&id);
        }
        // Stop servers nobody uses any more.
        let now = Instant::now();
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
        let mut watched: HashMap<ServerKey, Vec<Value>> = HashMap::new();
        for change in changes {
            if change.path.components().any(|c| c == ".lake" || c == "lake-packages") {
                continue;
            }
            let abs = join(&root, &change.path);
            let name = change.path.file_name();
            if name == TOOLCHAIN_FILE || LAKEFILES.contains(&name) {
                // The toolchain or Lake configuration changed: statuses computed
                // with the old one are no longer valid.
                let dir = abs.parent().map(Path::to_path_buf).unwrap_or_default();
                for (key, server) in &self.servers {
                    let affected = same_path(&key.root, &dir)
                        || (server.project.kind == ProjectKind::Standalone && name == TOOLCHAIN_FILE);
                    if affected && !restart.contains(key) {
                        restart.push(key.clone());
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
        let Some(doc) = self.docs.remove(id) else { return };
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
                    LeanStatus::Checking { revision, toolchain: toolchain_name, starting: true, processing: vec![] },
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
                && matches!(self.servers.get(&key).map(|s| &s.phase), Some(Phase::Untrusted { .. }))
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
            LeanStatus::Checking { revision: version, toolchain, starting: false, processing: vec![] },
        );
        self.spawn_wait(&id, generation, client, uri, version);
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
                LeanStatus::Checking { revision, toolchain: name.clone(), starting: true, processing: vec![] },
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
        self.set_status(id, LeanStatus::Checking { revision, toolchain, starting: false, processing: vec![] });
        self.spawn_wait(id, generation, client, uri, revision);
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
                let has_sorry = sync
                    .lsp_diagnostics
                    .iter()
                    .any(|d| d.get("message").and_then(Value::as_str).is_some_and(is_sorry_message));
                let running = doc.key.as_ref().and_then(|k| self.servers.get(k)).and_then(|s| s.running());
                match running.and_then(|r| r.sorry_type.map(|t| (r.client.clone(), t))).filter(|_| has_sorry) {
                    Some((client, token_type)) => {
                        // Locate the exact sorry/admit tokens before reporting.
                        let (tx, uri, index) =
                            (self.tx.clone(), doc.uri.clone().unwrap_or_default(), sync.index.clone());
                        tokio::spawn(async move {
                            let tokens = client
                                .call(
                                    "textDocument/semanticTokens/full",
                                    json!({ "textDocument": { "uri": uri } }),
                                    Duration::from_secs(30),
                                )
                                .await
                                .ok()
                                .and_then(|v| v.get("data").and_then(Value::as_array).cloned())
                                .map(|data| {
                                    let data: Vec<u64> = data.iter().filter_map(Value::as_u64).collect();
                                    convert::tokens_of_type(&index, &data, token_type)
                                })
                                .unwrap_or_default();
                            let _ = tx.send(Msg::SorryTokens { generation, doc: id, version, tokens });
                        });
                    }
                    None => self.finalize(&id, Vec::new()),
                }
            }
            Err(e) if e.is_superseded() || e == LspError::Closed => {}
            Err(e) => {
                let reason = format!("Lean could not finish checking revision {version}: {e}");
                self.set_status(&id, LeanStatus::Unavailable { reason });
            }
        }
    }

    /// Lean finished the document's current version: compute its final status
    /// from the final diagnostics of exactly that version.
    fn finalize(&mut self, id: &DocumentId, tokens: Vec<TextRange>) {
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
                SorryLocation { range: loc.range, line, declaration: name, tokens: Vec::new() }
            })
            .collect();
        sorries.sort_by_key(|s| s.range.start);
        for token in tokens {
            // A token belongs to the last declaration that starts before it.
            let owner = sorries.iter().rposition(|s| s.range.start <= token.start).unwrap_or(0);
            if let Some(s) = sorries.get_mut(owner) {
                s.tokens.push(token);
            }
        }
        let sorry_count = sorries.len() as u32;
        let warnings =
            diagnostics.iter().filter(|d| d.severity == Severity::Warning && !is_sorry_message(&d.message)).count()
                as u32;
        let status = if errors > 0 {
            LeanStatus::Errors { revision: version, toolchain, count: errors, sorry_count }
        } else if sorry_count > 0 {
            LeanStatus::CheckedWithSorry {
                revision: version,
                toolchain,
                count: sorry_count,
                locations: sorries.clone(),
                warnings,
            }
        } else {
            LeanStatus::Checked { revision: version, toolchain, warnings }
        };
        if let Some(server) = self.servers.get_mut(&key) {
            server.attempts = 0; // the server demonstrably works again
        }
        let doc = self.docs.get_mut(id).expect("present");
        doc.diagnostics = diagnostics;
        doc.diagnostics_revision = Some(version);
        doc.sorries = sorries;
        self.set_status(id, status);
    }

    fn on_incoming(&mut self, key: ServerKey, generation: u64, incoming: Incoming) {
        let live = self.servers.get(&key).is_some_and(|s| s.generation == generation && s.running().is_some());
        if !live {
            return;
        }
        match incoming {
            Incoming::Notification { method, params } => match method.as_str() {
                "textDocument/publishDiagnostics" => self.on_diagnostics(&key, generation, &params),
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

    fn on_diagnostics(&mut self, key: &ServerKey, generation: u64, params: &Value) {
        let Some(uri) = params.get("uri").and_then(Value::as_str) else { return };
        let Some(id) = self.doc_for_uri(key, generation, uri) else { return };
        let doc = self.docs.get_mut(&id).expect("found");
        let sync = doc.sync.as_mut().expect("found with sync");
        let version = params.get("version").and_then(Value::as_u64).unwrap_or(sync.version);
        if version != sync.version {
            return;
        }
        let list = params.get("diagnostics").and_then(Value::as_array).cloned().unwrap_or_default();
        doc.diagnostics = list.iter().map(|d| convert::diagnostic(d, &sync.index, &doc.path)).collect();
        doc.diagnostics_revision = Some(version);
        sync.lsp_diagnostics = list;
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
                    LeanStatus::Checking { revision, toolchain: name.clone(), starting: true, processing: vec![] },
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
                    Some(CheckSnapshot { state, sorries: d.sorries.clone() })
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
            Msg::SorryTokens { generation, doc, version, tokens } => {
                if self.current_sync(&doc, generation, version) {
                    self.finalize(&doc, tokens);
                }
            }
            Msg::Retry { key, generation } => {
                let retry = self.servers.get(&key).is_some_and(|s| {
                    s.generation == generation && matches!(s.phase, Phase::Crashed { restarting: true, .. })
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
