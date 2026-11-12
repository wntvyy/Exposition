//! The workspace service: the open project folder, its files, and the link
//! between project files and documents in the [`DocumentService`].
//!
//! Responsibilities: open/close a folder (canonical root, recent projects),
//! lazy listing, opening files as documents (strict decoding), safe saves
//! (external-modification check, atomic replace), crash-recovery journals,
//! the file watcher, external-change reloads/conflicts, project settings, and
//! build inputs that prefer open buffers over disk.
//!
//! Lock order: `io` (serializes disk comparisons/writes) → `state` → the
//! document service. The document service never calls back into this service.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use exposition_core::document::{
    ConflictPolicy, DocumentEdit, DocumentEvent, DocumentId, DocumentInfo, DocumentState, EditOrigin, LineEnding,
    TextEncoding, TextPatch, TransactionId, TransactionOutcome, TransactionRequest, UndoBehavior,
};
use exposition_core::path::WorkspacePath;
use exposition_core::text::TextRange;
use notify::event::{ModifyKind, RenameMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, mpsc};
use tokio::time::Instant;
use ts_rs::TS;

use crate::diff::diff_patches;
use crate::documents::{DocumentService, DocumentSnapshot, OpenDocument};
use crate::encoding::{
    DecodeError, Decoded, MAX_TEXT_BYTES, SaveRefusal, check_reopenable, decode, encode, encoding_label, language_for,
};
use crate::error::{WorkspaceError, WorkspaceResult};
use crate::fsutil::{FileIdentity, hash_file, read_limited, read_with_identity, sha256_hex, write_atomic};
use crate::recovery::{JOURNAL_VERSION, Journal, RecoveryStore, StoredEntry, workspace_key};
use crate::root::WorkspaceRoot;
use crate::settings::{
    self, AppSettings, Loaded, ProjectSettings, load_app_settings, load_project_settings, store_app_settings,
    store_project_settings,
};
use crate::tree::{is_ignored, list_dir};
use crate::types::{
    ConflictKind, DiskConflict, DocumentOpened, FileChange, FileChangeKind, FileEntry, RecentProject,
    RecoveryDiskState, RecoveryEntry, RestoreOutcome, SaveAllOutcome, SaveFailure, WorkspaceEvent, WorkspaceInfo,
};

/// Delay between a buffer change and its recovery journal write (throttled:
/// continuous typing is journaled at most once per interval).
pub const JOURNAL_DELAY: Duration = Duration::from_millis(300);
/// Time to let a burst of file-system events settle before processing it.
pub const FS_SETTLE_DELAY: Duration = Duration::from_millis(150);
/// Largest file read for build inputs.
const MAX_BUILD_INPUT_BYTES: u64 = 256 * 1024 * 1024;
const PROJECT_SETTINGS_PATH: &str = ".exposition/project.json";

/// Project settings with load diagnostics.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSettingsState {
    pub settings: ProjectSettings,
    /// `.exposition/project.json` exists.
    pub exists: bool,
    /// The file was invalid and has been set aside; defaults are in effect.
    pub warning: Option<String>,
}

/// Application settings with load diagnostics.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct AppSettingsState {
    pub settings: AppSettings,
    pub warning: Option<String>,
}

/// One file of a build snapshot: the open buffer when there is one, otherwise disk.
#[derive(Debug, Clone)]
pub struct BuildInput {
    pub path: WorkspacePath,
    /// Exact bytes as they would be saved (buffer encoded per its encoding) or as on disk.
    pub bytes: Arc<[u8]>,
    /// Decoded text, when the input is text.
    pub text: Option<Arc<str>>,
    /// Open document and revision, when taken from a buffer.
    pub document: Option<DocumentId>,
    pub revision: Option<u64>,
    /// SHA-256 of `bytes`.
    pub content_hash: String,
}

/// An immutable view of the workspace for one build: open buffers as of the
/// moment it was taken, and disk files read on demand through the workspace
/// path checks (no traversal, no link escapes). Compilers resolve every input,
/// including files they discover while compiling, through [`Self::read`].
#[derive(Debug, Clone)]
pub struct WorkspaceSnapshot {
    root: WorkspaceRoot,
    buffers: HashMap<WorkspacePath, (DocumentSnapshot, TextEncoding)>,
}

impl WorkspaceSnapshot {
    /// Canonical workspace root (e.g. a working directory for external tools).
    pub fn root(&self) -> &Path {
        self.root.path()
    }

    /// Open buffers captured in this snapshot.
    pub fn buffers(&self) -> impl Iterator<Item = &DocumentSnapshot> {
        self.buffers.values().map(|(snapshot, _)| snapshot)
    }

    /// Read one input: the captured buffer when the file is open, otherwise
    /// the file on disk (resolved inside the workspace).
    pub fn read(&self, path: &WorkspacePath) -> WorkspaceResult<BuildInput> {
        // An alias (case variant, short name, link inside the folder) of an open
        // file reads that file's buffer.
        let alias = if self.buffers.contains_key(path) {
            None
        } else {
            self.root.resolve_existing(path).ok().and_then(|target| self.root.relativize(&target))
        };
        let buffer = self.buffers.get(path).or_else(|| alias.as_ref().and_then(|p| self.buffers.get(p)));
        if let Some((snapshot, encoding)) = buffer {
            let bytes: Arc<[u8]> = encode(&snapshot.text, *encoding).into();
            return Ok(BuildInput {
                path: path.clone(),
                content_hash: sha256_hex(&bytes),
                bytes,
                text: Some(Arc::clone(&snapshot.text)),
                document: Some(snapshot.id.clone()),
                revision: Some(snapshot.revision),
            });
        }
        let target = self.root.resolve_existing(path)?;
        if fs::metadata(&target).map(|m| m.is_dir()).unwrap_or(false) {
            return Err(WorkspaceError::new("file.isDirectory", format!("{path} is a folder.")));
        }
        let bytes = match read_limited(&target, MAX_BUILD_INPUT_BYTES).map_err(|e| WorkspaceError::io(path, &e))? {
            Ok(bytes) => bytes,
            Err(size) => return Err(too_large(path, size)),
        };
        let text = decode(&bytes).ok().map(|d| Arc::<str>::from(d.text));
        Ok(BuildInput {
            path: path.clone(),
            content_hash: sha256_hex(&bytes),
            bytes: bytes.into(),
            text,
            document: None,
            revision: None,
        })
    }
}

/// A raw watcher notification tagged with the workspace generation it belongs to.
pub struct FsSignal {
    generation: u64,
    event: notify::Result<Event>,
}

pub struct WorkspaceService {
    documents: Arc<DocumentService>,
    data_dir: PathBuf,
    state: Mutex<State>,
    io: Mutex<()>,
    /// Serializes opening documents so aliases of one file resolve to one document.
    opening: Mutex<()>,
    events: broadcast::Sender<WorkspaceEvent>,
    fs_tx: mpsc::UnboundedSender<FsSignal>,
    fs_rx: Mutex<Option<mpsc::UnboundedReceiver<FsSignal>>>,
    counter: AtomicU64,
}

#[derive(Default)]
struct State {
    generation: u64,
    open: Option<OpenWorkspace>,
}

struct OpenWorkspace {
    generation: u64,
    root: WorkspaceRoot,
    recovery: RecoveryStore,
    /// Hash of each document's file as last loaded or saved (`None`: no file).
    disk: HashMap<DocumentId, Option<String>>,
    /// Physical identity of each document's file (unifies hard links).
    identities: HashMap<DocumentId, FileIdentity>,
    conflicts: HashMap<DocumentId, Conflict>,
    _watcher: Option<RecommendedWatcher>,
}

#[derive(Clone)]
struct Conflict {
    public: DiskConflict,
    /// Disk hash observed when the conflict was detected.
    disk_hash: Option<String>,
}

/// The current on-disk state of a document's file.
enum DiskRead {
    Missing,
    Text { hash: String, decoded: Decoded },
    Unreadable { hash: Option<String>, error: WorkspaceError },
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn decode_error(path: &WorkspacePath, error: DecodeError) -> WorkspaceError {
    match error {
        DecodeError::Binary => {
            WorkspaceError::new("file.binary", format!("{path} looks like a binary file and is not opened as text."))
        }
        DecodeError::Undecodable { encoding } => WorkspaceError::new(
            "file.undecodable",
            format!(
                "{path} is not valid {} text. It is not opened, so its bytes are never altered.",
                encoding_label(encoding)
            ),
        ),
    }
}

fn too_large(path: &WorkspacePath, size: u64) -> WorkspaceError {
    WorkspaceError::new(
        "file.tooLarge",
        format!(
            "{path} is {:.1} MB; Exposition opens text files up to {} MB.",
            size as f64 / 1_048_576.0,
            MAX_TEXT_BYTES / 1_048_576
        ),
    )
}

/// Read and decode a document's file for comparison with its buffer.
fn read_disk(root: &WorkspaceRoot, path: &WorkspacePath) -> DiskRead {
    let target = match root.resolve_existing(path) {
        Ok(target) => target,
        Err(e) if e.code == "path.notFound" => return DiskRead::Missing,
        Err(error) => return DiskRead::Unreadable { hash: None, error },
    };
    match read_limited(&target, MAX_TEXT_BYTES) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DiskRead::Missing,
        Err(e) => DiskRead::Unreadable { hash: None, error: WorkspaceError::io(path, &e) },
        Ok(Err(size)) => DiskRead::Unreadable { hash: hash_file(&target).ok(), error: too_large(path, size) },
        Ok(Ok(bytes)) => {
            let hash = sha256_hex(&bytes);
            match decode(&bytes) {
                Ok(decoded) => DiskRead::Text { hash, decoded },
                Err(e) => DiskRead::Unreadable { hash: Some(hash), error: decode_error(path, e) },
            }
        }
    }
}

impl WorkspaceService {
    pub fn new(documents: Arc<DocumentService>, data_dir: PathBuf) -> Arc<Self> {
        let (events, _) = broadcast::channel(1024);
        let (fs_tx, fs_rx) = mpsc::unbounded_channel();
        Arc::new(Self {
            documents,
            data_dir,
            state: Mutex::new(State::default()),
            io: Mutex::new(()),
            opening: Mutex::new(()),
            events,
            fs_tx,
            fs_rx: Mutex::new(Some(fs_rx)),
            counter: AtomicU64::new(0),
        })
    }

    pub fn documents(&self) -> &Arc<DocumentService> {
        &self.documents
    }

    pub fn subscribe(&self) -> broadcast::Receiver<WorkspaceEvent> {
        self.events.subscribe()
    }

    fn emit(&self, event: WorkspaceEvent) {
        let _ = self.events.send(event);
    }

    fn next_transaction(&self, kind: &str) -> TransactionId {
        TransactionId(format!("workspace-{kind}-{}", self.counter.fetch_add(1, Ordering::Relaxed)))
    }

    fn root(&self) -> WorkspaceResult<WorkspaceRoot> {
        lock(&self.state).open.as_ref().map(|o| o.root.clone()).ok_or_else(WorkspaceError::no_workspace)
    }

    /// Canonical root of the open folder (the path `WorkspaceSnapshot::root`
    /// reports), without taking a snapshot. Services that keep results per
    /// folder use it to refuse results of a folder that is no longer open.
    pub fn open_root(&self) -> Option<PathBuf> {
        lock(&self.state).open.as_ref().map(|o| o.root.path().to_path_buf())
    }

    fn recovery_store(&self) -> WorkspaceResult<RecoveryStore> {
        lock(&self.state).open.as_ref().map(|o| o.recovery.clone()).ok_or_else(WorkspaceError::no_workspace)
    }

    fn disk_base(&self, id: &DocumentId) -> Option<Option<String>> {
        lock(&self.state).open.as_ref().and_then(|o| o.disk.get(id).cloned())
    }

    fn set_disk_base(&self, id: &DocumentId, hash: Option<String>) {
        if let Some(open) = lock(&self.state).open.as_mut() {
            open.disk.insert(id.clone(), hash);
        }
    }

    fn take_conflict(&self, id: &DocumentId) -> Option<Conflict> {
        lock(&self.state).open.as_mut().and_then(|o| o.conflicts.remove(id))
    }

    fn set_conflict(&self, conflict: Conflict) {
        let public = conflict.public.clone();
        if let Some(open) = lock(&self.state).open.as_mut() {
            open.conflicts.insert(public.document.clone(), conflict);
        }
        self.emit(WorkspaceEvent::Conflict { conflict: public });
    }

    fn resolve_conflict(&self, id: &DocumentId) {
        if self.take_conflict(id).is_some() {
            self.emit(WorkspaceEvent::ConflictResolved { document: id.clone() });
        }
    }

    // ----- workspace lifecycle -------------------------------------------------

    /// Open a folder. Opening the folder that is already open returns it
    /// unchanged; opening another closes the current one first (journals of
    /// its unsaved buffers are kept for recovery).
    pub fn open_workspace(&self, path: &Path) -> WorkspaceResult<WorkspaceInfo> {
        let root = WorkspaceRoot::open(path)?;
        let already_open = lock(&self.state).open.as_ref().is_some_and(|o| o.root.path() == root.path());
        if !already_open {
            self.close_workspace();
            let recovery = RecoveryStore::new(&self.data_dir, &workspace_key(&root.display_path()));
            recovery.promote_live().map_err(|e| {
                WorkspaceError::new("recovery.io", format!("Recovery data could not be prepared: {e}."))
            })?;
            let mut state = lock(&self.state);
            state.generation += 1;
            let generation = state.generation;
            let watcher = self.start_watcher(&root, generation);
            state.open = Some(OpenWorkspace {
                generation,
                root: root.clone(),
                recovery,
                disk: HashMap::new(),
                identities: HashMap::new(),
                conflicts: HashMap::new(),
                _watcher: watcher,
            });
        }
        // Recent projects are a convenience; failing to record one is not an error.
        let _ = settings::touch_recent(&self.data_dir, &root.display_path(), &root.name());
        let info = self.current().ok_or_else(WorkspaceError::no_workspace)?;
        if !already_open {
            self.emit(WorkspaceEvent::Opened { info: info.clone() });
        }
        Ok(info)
    }

    fn start_watcher(&self, root: &WorkspaceRoot, generation: u64) -> Option<RecommendedWatcher> {
        let tx = self.fs_tx.clone();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = tx.send(FsSignal { generation, event });
        })
        .ok()?;
        watcher.watch(root.path(), RecursiveMode::Recursive).ok()?;
        Some(watcher)
    }

    /// Close the folder and every document from it. Unsaved buffers keep their
    /// recovery journals (the frontend asks about unsaved changes first).
    pub fn close_workspace(&self) {
        let closed = lock(&self.state).open.take();
        if closed.is_none() {
            return;
        }
        for info in self.documents.list() {
            if info.path.is_some() {
                let _ = self.documents.close(&info.id);
            }
        }
        self.emit(WorkspaceEvent::Closed);
    }

    pub fn current(&self) -> Option<WorkspaceInfo> {
        let root = self.root().ok()?;
        let recovery = self.recovery_entries().unwrap_or_default();
        Some(WorkspaceInfo { root: root.display_path(), name: root.name(), recovery })
    }

    pub fn recent_projects(&self) -> Vec<RecentProject> {
        settings::recent_projects(&self.data_dir)
    }

    pub fn forget_recent_project(&self, path: &str) -> WorkspaceResult<Vec<RecentProject>> {
        settings::forget_recent(&self.data_dir, path)
            .map_err(|e| WorkspaceError::new("settings.writeFailed", format!("Recent projects not saved: {e}.")))
    }

    pub fn list_dir(&self, dir: Option<&WorkspacePath>) -> WorkspaceResult<Vec<FileEntry>> {
        list_dir(&self.root()?, dir)
    }

    // ----- documents -----------------------------------------------------------

    /// Open a project file as a document (or return the already open one).
    ///
    /// Documents are identified by the physical file, not the spelling of the
    /// path: a case variant, an 8.3 short name, a path through a link inside
    /// the folder, or a hard link to an open file returns the existing
    /// document. A new document is recorded under the file's canonical
    /// workspace path (its final path under the root).
    pub fn open_document(&self, path: &WorkspacePath) -> WorkspaceResult<DocumentOpened> {
        let root = self.root()?;
        let _opening = lock(&self.opening);
        if let Some(id) = self.documents.find_by_path(path) {
            if self.disk_base(&id).is_none() {
                let hash = root.resolve_existing(path).ok().and_then(|p| hash_file(&p).ok());
                self.set_disk_base(&id, hash);
            }
            return self.opened(&id);
        }
        let target = root.resolve_existing(path)?;
        let canonical = root
            .relativize(&target)
            .ok_or_else(|| WorkspaceError::new("path.invalid", format!("{path} cannot be opened as a document.")))?;
        if let Some(id) = self.documents.find_by_path(&canonical) {
            return self.opened(&id);
        }
        let meta = fs::metadata(&target).map_err(|e| WorkspaceError::io(path, &e))?;
        if meta.is_dir() {
            return Err(WorkspaceError::new("file.isDirectory", format!("{path} is a folder.")));
        }
        let read = read_with_identity(&target, MAX_TEXT_BYTES).map_err(|e| WorkspaceError::io(path, &e))?;
        if let Some(identity) = read.identity
            && let Some(id) = self.document_with_identity(&identity)
        {
            return self.opened(&id);
        }
        let bytes = match read.bytes {
            Ok(bytes) => bytes,
            Err(size) => return Err(too_large(path, size)),
        };
        let decoded = decode(&bytes).map_err(|e| decode_error(path, e))?;
        let info = self.documents.open(OpenDocument {
            path: Some(canonical.clone()),
            text: decoded.text,
            language: language_for(canonical.file_name()).to_owned(),
            encoding: decoded.encoding,
            read_only: None,
        });
        if self.disk_base(&info.id).is_none() {
            self.set_disk_base(&info.id, Some(sha256_hex(&bytes)));
        }
        if let Some(identity) = read.identity
            && let Some(open) = lock(&self.state).open.as_mut()
        {
            open.identities.insert(info.id.clone(), identity);
        }
        self.opened(&info.id)
    }

    /// The open document backed by the physical file `identity`, if any.
    fn document_with_identity(&self, identity: &FileIdentity) -> Option<DocumentId> {
        let state = lock(&self.state);
        let open = state.open.as_ref()?;
        open.identities
            .iter()
            .find(|(id, known)| *known == identity && self.documents.info(id).is_ok())
            .map(|(id, _)| id.clone())
    }

    fn opened(&self, id: &DocumentId) -> WorkspaceResult<DocumentOpened> {
        let state = self.documents.state(id)?;
        let conflict = lock(&self.state).open.as_ref().and_then(|o| o.conflicts.get(id)).map(|c| c.public.clone());
        Ok(DocumentOpened { state, conflict })
    }

    /// Create a new text file inside the open folder and open it as a
    /// document. Never overwrites: refused when the file exists or a document
    /// with that path is open. The path is validated like every write (inside
    /// the root, no link escapes); missing folders are created; the file is
    /// written atomically as UTF-8.
    pub fn create_document(&self, path: &WorkspacePath, text: &str) -> WorkspaceResult<DocumentOpened> {
        let exists = || WorkspaceError::new("file.exists", format!("{path} already exists."));
        if text.len() as u64 > MAX_TEXT_BYTES {
            return Err(too_large(path, text.len() as u64));
        }
        {
            let _io = lock(&self.io);
            let root = self.root()?;
            if self.documents.find_by_path(path).is_some() {
                return Err(exists());
            }
            let target = root.resolve_for_write(path)?;
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|e| WorkspaceError::io(path, &e))?;
            }
            // Reserve the name first so an existing file is never replaced, then
            // write the content atomically over the (empty) reservation.
            match fs::OpenOptions::new().write(true).create_new(true).open(&target) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Err(exists()),
                Err(e) => return Err(WorkspaceError::io(path, &e)),
            }
            if let Err(e) = write_atomic(&target, text.as_bytes()) {
                let _ = fs::remove_file(&target);
                return Err(WorkspaceError::new("file.writeFailed", format!("{path} could not be created: {e}.")));
            }
        }
        self.open_document(path)
    }

    /// Close a document. Closing discards its unsaved changes and recovery journal.
    pub fn close_document(&self, id: &DocumentId) -> WorkspaceResult<()> {
        let info = self.documents.info(id)?;
        self.documents.close(id)?;
        let store = {
            let mut state = lock(&self.state);
            state.open.as_mut().map(|open| {
                open.disk.remove(id);
                open.identities.remove(id);
                open.conflicts.remove(id);
                open.recovery.clone()
            })
        };
        if let (Some(store), Some(path)) = (store, info.path) {
            let _io = lock(&self.io);
            let _ = store.remove_live(&path);
        }
        Ok(())
    }

    /// Save a document to its file.
    ///
    /// Refuses (unless `overwrite`) when the file changed on disk since it was
    /// loaded or last saved. Writes atomically; on failure the original file is
    /// untouched and the buffer stays dirty.
    pub fn save_document(&self, id: &DocumentId, overwrite: bool) -> WorkspaceResult<DocumentInfo> {
        let _io = lock(&self.io);
        let state = self.documents.state(id)?;
        let path = state
            .info
            .path
            .clone()
            .ok_or_else(|| WorkspaceError::new("document.noPath", "This document has no file to save to."))?;
        let root = self.root()?;
        let base = self.disk_base(id).unwrap_or(None);
        let target = root.resolve_for_write(&path)?;
        match fs::metadata(&target) {
            Ok(meta) => {
                if meta.is_dir() {
                    return Err(WorkspaceError::new("file.isDirectory", format!("{path} is a folder.")));
                }
                if meta.permissions().readonly() {
                    return Err(WorkspaceError::new(
                        "file.readOnly",
                        format!("{path} is read-only on disk. Your changes are kept in the editor."),
                    ));
                }
                if !overwrite {
                    let current = hash_file(&target).map_err(|e| WorkspaceError::io(&path, &e))?;
                    if Some(&current) != base.as_ref() {
                        return Err(WorkspaceError::new(
                            "file.modifiedOnDisk",
                            format!("{path} changed on disk since it was opened or last saved."),
                        ));
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Deleted on disk (or new): recreate it, including missing folders
                // (already verified to resolve inside the workspace).
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).map_err(|e| WorkspaceError::io(&path, &e))?;
                }
            }
            Err(e) => return Err(WorkspaceError::io(&path, &e)),
        }
        let bytes = encode(&state.text, state.info.encoding);
        // Never write a file Exposition would then refuse to reopen.
        check_reopenable(&state.text, bytes.len()).map_err(|refusal| match refusal {
            SaveRefusal::TooLarge { size } => WorkspaceError::new(
                "file.tooLarge",
                format!(
                    "{path} would be {:.1} MB; Exposition saves and opens text files up to {} MB. Your changes are kept in the editor.",
                    size as f64 / 1_048_576.0,
                    MAX_TEXT_BYTES / 1_048_576
                ),
            ),
            SaveRefusal::Binary => WorkspaceError::new(
                "file.binary",
                format!(
                    "{path} contains many control characters (such as NUL) and would be treated as binary when reopened, so it was not saved. Your changes are kept in the editor."
                ),
            ),
        })?;
        write_atomic(&target, &bytes).map_err(|e| match e.kind() {
            std::io::ErrorKind::PermissionDenied => WorkspaceError::new(
                "file.permissionDenied",
                format!("{path} could not be saved: permission denied (it may be read-only or locked)."),
            ),
            _ => WorkspaceError::new("file.writeFailed", format!("{path} could not be saved: {e}.")),
        })?;
        self.set_disk_base(id, Some(sha256_hex(&bytes)));
        let info = self.documents.mark_saved(id, state.info.revision)?;
        self.resolve_conflict(id);
        if info.saved_revision == Some(info.revision)
            && let Ok(store) = self.recovery_store()
        {
            let _ = store.remove_live(&path);
        }
        Ok(info)
    }

    /// Save every open project document with unsaved changes.
    pub fn save_all(&self) -> SaveAllOutcome {
        let mut outcome = SaveAllOutcome { saved: Vec::new(), failed: Vec::new() };
        for info in self.documents.list() {
            if info.path.is_none() || info.saved_revision == Some(info.revision) {
                continue;
            }
            match self.save_document(&info.id, false) {
                Ok(saved) => outcome.saved.push(saved),
                Err(e) => outcome.failed.push(SaveFailure {
                    document: info.id.clone(),
                    path: info.path.clone(),
                    code: e.code.to_owned(),
                    message: e.message,
                }),
            }
        }
        outcome
    }

    /// Replace the buffer with the file's current content (an undoable
    /// `external` transaction) and mark it saved.
    pub fn reload_document(&self, id: &DocumentId) -> WorkspaceResult<DocumentInfo> {
        let _io = lock(&self.io);
        let state = self.documents.state(id)?;
        let path = state.info.path.clone().ok_or_else(|| WorkspaceError::new("document.noPath", "No file."))?;
        match read_disk(&self.root()?, &path) {
            DiskRead::Text { hash, decoded } => {
                let info = self.apply_disk_text(id, &state, decoded, hash, "Reload from disk")?;
                self.emit(WorkspaceEvent::DocumentReloaded { document: id.clone(), path });
                Ok(info)
            }
            DiskRead::Missing => Err(WorkspaceError::not_found(&path)),
            DiskRead::Unreadable { error, .. } => Err(error),
        }
    }

    /// Keep the buffer after an external change: the next save overwrites the
    /// version on disk observed when the conflict was detected.
    pub fn keep_buffer(&self, id: &DocumentId) -> WorkspaceResult<()> {
        let _io = lock(&self.io);
        self.documents.info(id)?;
        if let Some(conflict) = self.take_conflict(id) {
            self.set_disk_base(id, conflict.disk_hash);
            self.emit(WorkspaceEvent::ConflictResolved { document: id.clone() });
        }
        Ok(())
    }

    /// Apply disk text to a buffer as one `external` transaction, then mark it saved.
    fn apply_disk_text(
        &self,
        id: &DocumentId,
        state: &DocumentState,
        decoded: Decoded,
        hash: String,
        label: &str,
    ) -> WorkspaceResult<DocumentInfo> {
        let patches = diff_patches(&state.text, &decoded.text);
        let mut revision = state.info.revision;
        if !patches.is_empty() {
            let outcome = self.documents.apply(TransactionRequest {
                id: self.next_transaction("external"),
                origin: EditOrigin::External,
                edits: vec![DocumentEdit { document: id.clone(), base_revision: state.info.revision, patches }],
                undo: UndoBehavior::NewGroup,
                conflict: ConflictPolicy::Reject,
                label: Some(label.to_owned()),
                selection: None,
            })?;
            revision = outcome.applied[0].revision;
        }
        if decoded.encoding != state.info.encoding {
            self.documents.set_encoding(id, decoded.encoding)?;
        }
        self.set_disk_base(id, Some(hash));
        let info = self.documents.mark_saved(id, revision)?;
        self.resolve_conflict(id);
        Ok(info)
    }

    /// Re-read a document's file after a change notification. Clean buffers are
    /// reloaded; dirty buffers enter a conflict that needs a user decision.
    pub fn check_document(&self, id: &DocumentId) {
        let _io = lock(&self.io);
        let Ok(state) = self.documents.state(id) else { return };
        let Some(path) = state.info.path.clone() else { return };
        let Ok(root) = self.root() else { return };
        let Some(base) = self.disk_base(id) else { return };
        let clean = state.info.saved_revision == Some(state.info.revision);
        match read_disk(&root, &path) {
            DiskRead::Missing => {
                if base.is_none() {
                    return;
                }
                self.set_disk_base(id, None);
                // The buffer is now the only copy: mark it unsaved so closing asks first.
                let _ = self.documents.mark_saved(id, 0);
                self.set_conflict(Conflict {
                    public: DiskConflict {
                        document: id.clone(),
                        path: path.clone(),
                        kind: ConflictKind::Deleted,
                        message: format!("{path} was deleted or moved on disk. Saving will recreate it."),
                    },
                    disk_hash: None,
                });
            }
            DiskRead::Unreadable { hash, error } => {
                if hash.is_some() && hash == base {
                    return;
                }
                self.set_conflict(Conflict {
                    public: DiskConflict {
                        document: id.clone(),
                        path,
                        kind: ConflictKind::Unreadable,
                        message: format!("The file on disk changed and can no longer be loaded: {}", error.message),
                    },
                    disk_hash: hash,
                });
            }
            DiskRead::Text { hash, decoded } => {
                if Some(&hash) == base.as_ref() {
                    return;
                }
                if decoded.text == *state.text && decoded.encoding == state.info.encoding {
                    // Disk now matches the buffer (e.g. restored by another tool).
                    self.set_disk_base(id, Some(hash));
                    let _ = self.documents.mark_saved(id, state.info.revision);
                    self.resolve_conflict(id);
                    return;
                }
                if clean {
                    match self.apply_disk_text(id, &state, decoded, hash.clone(), "External change") {
                        Ok(_) => {
                            self.emit(WorkspaceEvent::DocumentReloaded { document: id.clone(), path });
                            return;
                        }
                        Err(_) => {
                            // Edited concurrently: fall through to a conflict.
                        }
                    }
                }
                self.set_conflict(Conflict {
                    public: DiskConflict {
                        document: id.clone(),
                        path: path.clone(),
                        kind: ConflictKind::Modified,
                        message: format!("{path} changed on disk while it has unsaved changes."),
                    },
                    disk_hash: Some(hash),
                });
            }
        }
    }

    /// Rewrite every line terminator to `ending` (LF or CRLF) as one explicit,
    /// undoable transaction.
    pub fn normalize_line_endings(
        &self,
        id: &DocumentId,
        ending: LineEnding,
    ) -> WorkspaceResult<Option<TransactionOutcome>> {
        let target = match ending {
            LineEnding::Lf => "\n",
            LineEnding::Crlf => "\r\n",
            _ => return Err(WorkspaceError::new("document.invalidLineEnding", "Choose LF or CRLF.")),
        };
        let state = self.documents.state(id)?;
        let patches = line_ending_patches(&state.text, target);
        if patches.is_empty() {
            return Ok(None);
        }
        let outcome = self.documents.apply(TransactionRequest {
            id: self.next_transaction("eol"),
            origin: EditOrigin::System,
            edits: vec![DocumentEdit { document: id.clone(), base_revision: state.info.revision, patches }],
            undo: UndoBehavior::NewGroup,
            conflict: ConflictPolicy::Reject,
            label: Some("Normalize line endings".into()),
            selection: None,
        })?;
        Ok(Some(outcome))
    }

    // ----- recovery ------------------------------------------------------------

    /// Write (or remove) recovery journals for the given documents.
    pub fn journal_documents(&self, ids: &HashSet<DocumentId>) {
        let _io = lock(&self.io);
        self.journal_locked(ids);
    }

    fn journal_locked(&self, ids: &HashSet<DocumentId>) {
        let Ok(store) = self.recovery_store() else { return };
        for id in ids {
            let Ok(state) = self.documents.state(id) else { continue };
            let Some(path) = state.info.path.clone() else { continue };
            let Some(base) = self.disk_base(id) else { continue };
            let result = if state.info.saved_revision == Some(state.info.revision) {
                store.remove_live(&path)
            } else {
                store.write_live(&Journal {
                    version: JOURNAL_VERSION,
                    path: path.clone(),
                    base_hash: base,
                    revision: state.info.revision,
                    encoding: state.info.encoding,
                    saved_at: crate::fsutil::now_millis(),
                    text: state.text,
                })
            };
            if let Err(e) = result {
                self.emit(WorkspaceEvent::JournalFailed {
                    path,
                    message: format!("Unsaved changes could not be backed up for recovery: {e}."),
                });
            }
        }
    }

    /// Unsaved changes from earlier sessions. Entries whose text already
    /// matches the file on disk are removed.
    pub fn recovery_entries(&self) -> WorkspaceResult<Vec<RecoveryEntry>> {
        let store = self.recovery_store()?;
        let root = self.root()?;
        let mut entries = Vec::new();
        for stored in store.entries() {
            let (id, journal) = match stored {
                StoredEntry::Valid { id, journal } => (id, journal),
                StoredEntry::Unreadable { id, path, reason, location, modified } => {
                    entries.push(RecoveryEntry {
                        id,
                        path,
                        saved_at: modified,
                        length: 0,
                        disk: RecoveryDiskState::Unknown,
                        problem: Some(reason),
                        location: Some(crate::root::display_path(&location)),
                    });
                    continue;
                }
            };
            let disk = match read_disk(&root, &journal.path) {
                DiskRead::Missing => {
                    if journal.base_hash.is_none() {
                        RecoveryDiskState::Unchanged
                    } else {
                        RecoveryDiskState::Missing
                    }
                }
                DiskRead::Text { hash, decoded } => {
                    if decoded.text == journal.text {
                        let _ = store.remove_entry(&id);
                        continue;
                    }
                    if Some(&hash) == journal.base_hash.as_ref() {
                        RecoveryDiskState::Unchanged
                    } else {
                        RecoveryDiskState::Changed
                    }
                }
                DiskRead::Unreadable { .. } => RecoveryDiskState::Changed,
            };
            entries.push(RecoveryEntry {
                id,
                path: Some(journal.path),
                saved_at: journal.saved_at,
                length: u32::try_from(exposition_core::text::utf16_len(&journal.text)).unwrap_or(u32::MAX),
                disk,
                problem: None,
                location: None,
            });
        }
        Ok(entries)
    }

    /// Restore a recovery entry: open the file from disk, then apply the
    /// journaled text as one `recovery` transaction, so undo returns to the
    /// disk version. If the disk changed since the journal, both versions are
    /// kept (disk on disk, recovered text in the buffer).
    pub fn restore_recovery(&self, entry_id: &str) -> WorkspaceResult<RestoreOutcome> {
        let store = self.recovery_store()?;
        if entry_id.starts_with("bad-") {
            return Err(WorkspaceError::new(
                "recovery.unreadable",
                "This recovery data is unreadable and cannot be restored; it can only be discarded.",
            ));
        }
        let journal = store
            .read_entry(entry_id)
            .ok_or_else(|| WorkspaceError::new("recovery.notFound", "That recovery entry no longer exists."))?;
        let path = journal.path.clone();
        let id = match self.documents.find_by_path(&path) {
            Some(id) => {
                let info = self.documents.info(&id)?;
                if info.saved_revision != Some(info.revision) {
                    return Err(WorkspaceError::new(
                        "recovery.bufferDirty",
                        format!("{path} has unsaved changes. Save or revert them before restoring."),
                    ));
                }
                id
            }
            None => match self.open_document(&path) {
                Ok(opened) => opened.state.info.id,
                Err(e) if e.code == "path.notFound" => {
                    self.root()?.resolve_for_write(&path)?;
                    let info = self.documents.open(OpenDocument {
                        path: Some(path.clone()),
                        text: String::new(),
                        language: language_for(path.file_name()).to_owned(),
                        encoding: journal.encoding,
                        read_only: None,
                    });
                    self.set_disk_base(&info.id, None);
                    info.id
                }
                Err(e) => return Err(e),
            },
        };
        let disk_changed = self.disk_base(&id).unwrap_or(None) != journal.base_hash;
        let state = self.documents.state(&id)?;
        let patches = diff_patches(&state.text, &journal.text);
        if !patches.is_empty() {
            self.documents.apply(TransactionRequest {
                id: self.next_transaction("recovery"),
                origin: EditOrigin::Recovery,
                edits: vec![DocumentEdit { document: id.clone(), base_revision: state.info.revision, patches }],
                undo: UndoBehavior::NewGroup,
                conflict: ConflictPolicy::Reject,
                label: Some("Restore unsaved changes".into()),
                selection: None,
            })?;
        }
        // Journal the restored buffer before dropping the entry, so a crash in
        // between cannot lose it.
        self.journal_documents(&HashSet::from([id.clone()]));
        store
            .remove_entry(entry_id)
            .map_err(|e| WorkspaceError::new("recovery.io", format!("Recovery entry not removed: {e}.")))?;
        self.emit(WorkspaceEvent::RecoveryChanged { entries: self.recovery_entries().unwrap_or_default() });
        Ok(RestoreOutcome { state: self.documents.state(&id)?, disk_changed })
    }

    /// Permanently discard a recovery entry.
    pub fn discard_recovery(&self, entry_id: &str) -> WorkspaceResult<Vec<RecoveryEntry>> {
        let store = self.recovery_store()?;
        store
            .remove_entry(entry_id)
            .map_err(|e| WorkspaceError::new("recovery.io", format!("Recovery entry not removed: {e}.")))?;
        let entries = self.recovery_entries()?;
        self.emit(WorkspaceEvent::RecoveryChanged { entries: entries.clone() });
        Ok(entries)
    }

    // ----- settings ------------------------------------------------------------

    pub fn app_settings(&self) -> AppSettingsState {
        let Loaded { value, warning, .. } = load_app_settings(&self.data_dir);
        AppSettingsState { settings: value, warning }
    }

    pub fn update_app_settings(&self, settings: &AppSettings) -> WorkspaceResult<AppSettings> {
        store_app_settings(&self.data_dir, settings)?;
        Ok(settings.clone())
    }

    fn project_settings_file(&self) -> WorkspaceResult<PathBuf> {
        let path = WorkspacePath::parse(PROJECT_SETTINGS_PATH)?;
        self.root()?.resolve_for_write(&path)
    }

    pub fn project_settings(&self) -> WorkspaceResult<ProjectSettingsState> {
        let Loaded { value, exists, warning } = load_project_settings(&self.project_settings_file()?);
        Ok(ProjectSettingsState { settings: value, exists, warning })
    }

    pub fn update_project_settings(&self, settings: &ProjectSettings) -> WorkspaceResult<ProjectSettings> {
        store_project_settings(&self.project_settings_file()?, settings)?;
        Ok(settings.clone())
    }

    // ----- build inputs --------------------------------------------------------

    /// Capture an immutable build view: every open buffer at its current
    /// revision (taken atomically), plus path-checked access to disk files.
    pub fn snapshot(&self) -> WorkspaceResult<WorkspaceSnapshot> {
        let root = self.root()?;
        let encodings: HashMap<DocumentId, TextEncoding> =
            self.documents.list().into_iter().map(|i| (i.id, i.encoding)).collect();
        let buffers = self
            .documents
            .snapshots()
            .into_iter()
            .filter_map(|snapshot| {
                let path = snapshot.path.clone()?;
                let encoding = encodings.get(&snapshot.id).copied().unwrap_or(TextEncoding::Utf8);
                Some((path, (snapshot, encoding)))
            })
            .collect();
        Ok(WorkspaceSnapshot { root, buffers })
    }

    /// Read several files from one snapshot (see [`WorkspaceSnapshot::read`]).
    pub fn build_inputs(&self, paths: &[WorkspacePath]) -> WorkspaceResult<Vec<BuildInput>> {
        let snapshot = self.snapshot()?;
        paths.iter().map(|path| snapshot.read(path)).collect()
    }

    // ----- file watching -------------------------------------------------------

    /// Process a settled batch of watcher notifications: report file changes and
    /// check open documents whose files (or folders) changed.
    pub fn process_fs(&self, signals: Vec<FsSignal>) {
        let (generation, root) = {
            let state = lock(&self.state);
            match state.open.as_ref() {
                Some(open) => (open.generation, open.root.clone()),
                None => return,
            }
        };
        let mut rescan = false;
        let mut seen = HashSet::new();
        let mut changes = Vec::new();
        for signal in signals.into_iter().filter(|s| s.generation == generation) {
            let event = match signal.event {
                Ok(event) => event,
                Err(_) => {
                    rescan = true;
                    continue;
                }
            };
            if event.need_rescan() {
                rescan = true;
            }
            let kinds: Vec<FileChangeKind> = match event.kind {
                EventKind::Access(_) => continue,
                EventKind::Create(_) => vec![FileChangeKind::Created],
                EventKind::Remove(_) => vec![FileChangeKind::Removed],
                EventKind::Modify(ModifyKind::Name(RenameMode::From)) => vec![FileChangeKind::Removed],
                EventKind::Modify(ModifyKind::Name(RenameMode::To)) => vec![FileChangeKind::Created],
                EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
                    vec![FileChangeKind::Removed, FileChangeKind::Created]
                }
                _ => vec![FileChangeKind::Modified],
            };
            for (index, absolute) in event.paths.iter().enumerate() {
                let Some(path) = root.relativize(absolute) else { continue };
                if is_ignored(&path) {
                    continue;
                }
                let kind = kinds.get(index).or(kinds.last()).copied().unwrap_or(FileChangeKind::Modified);
                let change = FileChange { path, kind };
                if seen.insert(change.clone()) {
                    changes.push(change);
                }
            }
        }
        if changes.is_empty() && !rescan {
            return;
        }
        self.emit(WorkspaceEvent::FilesChanged { changes: changes.clone(), rescan });
        self.check_changed_documents(&changes, rescan);
    }

    /// Check open documents affected by `changes` (all of them on `rescan`).
    pub fn check_changed_documents(&self, changes: &[FileChange], rescan: bool) {
        for info in self.documents.list() {
            let Some(path) = &info.path else { continue };
            let affected = rescan
                || changes
                    .iter()
                    .any(|c| c.path == *path || path.as_str().starts_with(&format!("{}/", c.path.as_str())));
            if affected {
                self.check_document(&info.id);
            }
        }
    }

    /// Background loop: recovery journaling and watcher processing. Run once
    /// on the async runtime for the lifetime of the application.
    pub async fn run(self: Arc<Self>) {
        let Some(mut fs_rx) = lock(&self.fs_rx).take() else { return };
        let mut doc_rx = self.documents.subscribe();
        let mut pending_docs: HashSet<DocumentId> = HashSet::new();
        let mut journal_at: Option<Instant> = None;
        let mut pending_fs: Vec<FsSignal> = Vec::new();
        let mut fs_at: Option<Instant> = None;
        loop {
            let next = [journal_at, fs_at].into_iter().flatten().min();
            tokio::select! {
                event = doc_rx.recv() => match event {
                    Ok(DocumentEvent::Changed(changed)) => {
                        pending_docs.insert(changed.document);
                        journal_at.get_or_insert_with(|| Instant::now() + JOURNAL_DELAY);
                    }
                    Ok(DocumentEvent::InfoChanged { info }) => {
                        pending_docs.insert(info.id);
                        journal_at.get_or_insert_with(|| Instant::now() + JOURNAL_DELAY);
                    }
                    Ok(_) => {}
                    Err(RecvError::Lagged(_)) => {
                        pending_docs.extend(self.documents.list().into_iter().map(|i| i.id));
                        journal_at.get_or_insert_with(|| Instant::now() + JOURNAL_DELAY);
                    }
                    Err(RecvError::Closed) => break,
                },
                signal = fs_rx.recv() => match signal {
                    Some(signal) => {
                        pending_fs.push(signal);
                        fs_at.get_or_insert_with(|| Instant::now() + FS_SETTLE_DELAY);
                    }
                    None => break,
                },
                _ = tokio::time::sleep_until(next.unwrap_or_else(Instant::now)), if next.is_some() => {
                    let now = Instant::now();
                    if journal_at.is_some_and(|at| at <= now) {
                        journal_at = None;
                        let ids = std::mem::take(&mut pending_docs);
                        self.journal_documents(&ids);
                    }
                    if fs_at.is_some_and(|at| at <= now) {
                        fs_at = None;
                        let signals = std::mem::take(&mut pending_fs);
                        self.process_fs(signals);
                    }
                }
            }
        }
    }
}

/// Patches rewriting every line terminator (`\r\n`, `\r`, `\n`) that differs from `target`.
fn line_ending_patches(text: &str, target: &str) -> Vec<TextPatch> {
    let mut patches = Vec::new();
    let mut units = 0u32;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' if chars.peek() == Some(&'\n') => {
                chars.next();
                if target != "\r\n" {
                    patches.push(TextPatch { range: TextRange::new(units, units + 2), text: target.into() });
                }
                units += 2;
            }
            '\r' | '\n' => {
                let found = if ch == '\r' { "\r" } else { "\n" };
                if target != found {
                    patches.push(TextPatch { range: TextRange::new(units, units + 1), text: target.into() });
                }
                units += 1;
            }
            other => units += other.len_utf16() as u32,
        }
    }
    patches
}

#[cfg(test)]
mod tests;
