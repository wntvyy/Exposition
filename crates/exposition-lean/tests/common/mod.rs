//! Shared helpers for tests that run the real Lean language server.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use exposition_core::document::{
    ConflictPolicy, DocumentEdit, DocumentId, EditOrigin, TextPatch, TransactionId, TransactionRequest, UndoBehavior,
};
use exposition_core::path::WorkspacePath;
use exposition_execution::env::HostEnv;
use exposition_execution::{ExecutionConfig, ExecutionService, ExecutionSettings};
use exposition_lean::{LeanConfig, LeanDocumentState, LeanEvent, LeanService, LeanStatus};
use exposition_workspace::{DocumentService, WorkspaceService};
use tokio::sync::broadcast;

pub const TOOLCHAIN: &str = "leanprover/lean4:v4.35.0";

/// The pinned fixture toolchain, if installed. Missing Lean skips the calling
/// test with an explicit message, unless `EXPOSITION_REQUIRE_LEAN=1`.
pub fn lean_available() -> bool {
    let home = exposition_lean::toolchain::elan_home(|n| std::env::var(n).ok());
    let installed = matches!(
        exposition_lean::toolchain::resolve(home.as_deref(), TOOLCHAIN),
        exposition_lean::toolchain::Resolution::Installed(_)
    );
    if !installed {
        let message = format!("SKIPPED: Lean toolchain {TOOLCHAIN} is not installed through elan");
        if std::env::var("EXPOSITION_REQUIRE_LEAN").as_deref() == Ok("1") {
            panic!("{message}");
        }
        eprintln!("{message}");
    }
    installed
}

pub fn fixture_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures").join(name)
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let name = entry.file_name();
        if name == ".lake" || name == "lake-manifest.json" {
            continue; // never reuse build output from a developer run
        }
        let dest = to.join(&name);
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), dest).unwrap();
        }
    }
}

pub struct Harness {
    pub tmp: tempfile::TempDir,
    pub root: PathBuf,
    pub exec: ExecutionService,
    pub workspace: Arc<WorkspaceService>,
    pub documents: Arc<DocumentService>,
    pub lean: LeanService,
    pub events: broadcast::Receiver<LeanEvent>,
}

pub struct Options {
    pub trusted: bool,
    pub restart_backoff: Vec<Duration>,
    /// Rewrite `lean-toolchain` before opening.
    pub toolchain: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self { trusted: true, restart_backoff: vec![Duration::from_millis(300); 3], toolchain: None }
    }
}

pub async fn harness(fixture: &str, options: Options) -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("workspace");
    copy_dir(&fixture_dir(fixture), &project);
    if let Some(toolchain) = &options.toolchain {
        std::fs::write(project.join("lean-toolchain"), format!("{toolchain}\n")).unwrap();
    }
    let exec = ExecutionService::with_config(ExecutionConfig {
        data_dir: tmp.path().join("data"),
        host_env: HostEnv::capture(),
        settings: Some(ExecutionSettings::default()),
        too_broad_to_trust: Some(vec![]),
        tool_cache_dir: None,
    })
    .unwrap();
    let root = exec.register_workspace_root(&project).unwrap();
    if options.trusted {
        exec.grant_trust(&root.display().to_string()).unwrap();
    }
    let documents = Arc::new(DocumentService::new());
    let workspace = WorkspaceService::new(Arc::clone(&documents), tmp.path().join("app"));
    workspace.open_workspace(&root).unwrap();
    tokio::spawn(Arc::clone(&workspace).run());
    let config = LeanConfig { restart_backoff: options.restart_backoff, ..LeanConfig::default() };
    let (lean, actor) = LeanService::new(exec.clone(), Arc::clone(&workspace), config);
    let events = lean.subscribe();
    tokio::spawn(actor);
    Harness { tmp, root, exec, workspace, documents, lean, events }
}

impl Harness {
    pub fn open(&self, path: &str) -> DocumentId {
        self.workspace.open_document(&WorkspacePath::parse(path).unwrap()).unwrap().state.info.id
    }

    pub fn text(&self, id: &DocumentId) -> String {
        self.documents.state(id).unwrap().text
    }

    /// UTF-16 offset of the `nth` occurrence of `needle` plus `delta`.
    pub fn offset(&self, id: &DocumentId, needle: &str, delta: usize) -> u32 {
        let text = self.text(id);
        let byte = text.find(needle).unwrap_or_else(|| panic!("{needle:?} not in document"));
        (exposition_core::text::utf16_len(&text[..byte]) + delta) as u32
    }

    /// Replace the first occurrence of `from` with `to` as a source edit; returns the new revision.
    pub fn replace(&self, id: &DocumentId, from: &str, to: &str) -> u64 {
        let state = self.documents.state(id).unwrap();
        let start = self.offset(id, from, 0);
        let end = start + exposition_core::text::utf16_len(from) as u32;
        let outcome = self
            .documents
            .apply(TransactionRequest {
                id: TransactionId(format!("test-{start}-{to}")),
                origin: EditOrigin::Source,
                edits: vec![DocumentEdit {
                    document: id.clone(),
                    base_revision: state.info.revision,
                    patches: vec![TextPatch::replace(start, end, to)],
                }],
                undo: UndoBehavior::NewGroup,
                conflict: ConflictPolicy::Reject,
                label: None,
                selection: None,
            })
            .unwrap();
        outcome.applied[0].revision
    }

    /// Wait until `id`'s state satisfies `pred`; returns that state and every
    /// state observed on the way (in order).
    pub async fn wait_for(
        &mut self,
        id: &DocumentId,
        timeout: Duration,
        what: &str,
        pred: impl Fn(&LeanDocumentState) -> bool,
    ) -> (LeanDocumentState, Vec<LeanDocumentState>) {
        let deadline = Instant::now() + timeout;
        let mut seen = Vec::new();
        if let Some(state) = self.lean.documents().await.into_iter().find(|s| &s.document == id) {
            seen.push(state.clone());
            if pred(&state) {
                return (state, seen);
            }
        }
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match tokio::time::timeout(left, self.events.recv()).await {
                Ok(Ok(LeanEvent::Document { state })) if &state.document == id => {
                    seen.push((*state).clone());
                    if pred(&state) {
                        return (*state, seen);
                    }
                }
                Ok(Ok(_)) => {}
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => {
                    if let Some(state) = self.lean.documents().await.into_iter().find(|s| &s.document == id)
                        && pred(&state)
                    {
                        return (state, seen);
                    }
                }
                Ok(Err(e)) => panic!("event stream ended: {e}"),
                Err(_) => {
                    let last = seen.last().map(|s| format!("{:?}", s.status)).unwrap_or_default();
                    panic!("timed out waiting for {what}; last status: {last}")
                }
            }
        }
    }

    /// Wait for a finished check of exactly `revision`.
    pub async fn finished(&mut self, id: &DocumentId, revision: u64, timeout: Duration) -> LeanDocumentState {
        self.wait_for(id, timeout, &format!("finished check of revision {revision}"), |s| {
            s.status.finished_revision() == Some(revision)
        })
        .await
        .0
    }
}

pub fn status_name(status: &LeanStatus) -> &'static str {
    match status {
        LeanStatus::NotStarted => "notStarted",
        LeanStatus::Unavailable { .. } => "unavailable",
        LeanStatus::ToolchainMissing { .. } => "toolchainMissing",
        LeanStatus::Untrusted { .. } => "untrusted",
        LeanStatus::Checking { .. } => "checking",
        LeanStatus::Checked { .. } => "checked",
        LeanStatus::CheckedWithSorry { .. } => "checkedWithSorry",
        LeanStatus::Errors { .. } => "errors",
        LeanStatus::ServerCrashed { .. } => "serverCrashed",
    }
}
