//! The Lean service: one actor task owns every Lean server session and the
//! checking state of every open Lean document.
//!
//! Inputs, processed in order by the actor:
//! - document events (open/change/close/save) from the document service —
//!   the unsaved buffer is what Lean checks; revisions are LSP versions;
//! - workspace events (folder opened/closed, `lean-toolchain`/lakefile changes);
//! - trust grants and revokes from any surface (servers start when the folder
//!   becomes trusted and are stopped when its trust is revoked);
//! - language-server notifications (diagnostics, `$/lean/fileProgress`) and
//!   process exits;
//! - API requests (goals, hover, definition, restart, install, check).
//!
//! A revision is reported as checked only after `textDocument/waitForDiagnostics`
//! for exactly that version returned while it was still the latest version
//! sent to the server; Lean answers that request once every diagnostic for the
//! version has been published, so the diagnostics are final. If a newer
//! version was sent first, the answer is ignored (the older run may have been
//! cut short).

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use exposition_core::build::{Diagnostic, Severity};
use exposition_core::document::{DocumentChanged, DocumentEvent, DocumentId, DocumentInfo};
use exposition_core::job::{EnvironmentSpec, ExecutionPolicy, JobId, JobOutcome, JobResult, JobSpec};
use exposition_core::path::WorkspacePath;
use exposition_core::text::{LineIndex, TextRange};
use exposition_execution::paths::canonicalize;
use exposition_execution::{ExecutionService, Located, TrustChanged};
use exposition_workspace::types::WorkspaceEvent;
use exposition_workspace::{DocumentService, WorkspaceService};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::audit;
use crate::convert::{self, is_sorry_message};
use crate::lsp::{Incoming, LspClient, LspError, PendingRequest};
use crate::project::{self, LAKEFILES, LeanProject, PinSource, ProjectKind, TOOLCHAIN_FILE};
use crate::server::{self, StartError, StartedServer};
use crate::toolchain::{self, Resolution, ResolvedToolchain};
use crate::types::*;
use crate::uri::{path_to_uri, same_path, uri_to_path};

mod actor;
use actor::{Actor, ServerKey};

/// Longest a single check may take before it is reported as unfinished.
const WAIT_DIAGNOSTICS_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const POSITION_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_CHECK_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_CHECK_TIMEOUT: Duration = Duration::from_secs(600);
const TICK: Duration = Duration::from_millis(120);
const INSTALL_TIMEOUT_MS: u64 = 60 * 60 * 1000;

/// Service configuration.
#[derive(Clone, Debug)]
pub struct LeanConfig {
    /// elan's home (`ELAN_HOME` or `~/.elan` by default).
    pub elan_home: Option<PathBuf>,
    /// Delay before each automatic restart after a crash; its length is the
    /// number of automatic restarts before the server is left stopped.
    pub restart_backoff: Vec<Duration>,
    /// Stop a server this long after its last document closed.
    pub idle_shutdown: Duration,
    /// Application-owned scratch directory (axiom audits write temporary
    /// copies of the checked text here, never into the project).
    pub scratch_dir: PathBuf,
}

impl Default for LeanConfig {
    fn default() -> Self {
        Self {
            elan_home: toolchain::elan_home(|name| std::env::var(name).ok()),
            restart_backoff: [1, 2, 4, 8, 16].into_iter().map(Duration::from_secs).collect(),
            idle_shutdown: Duration::from_secs(120),
            scratch_dir: std::env::temp_dir().join("exposition-lean"),
        }
    }
}

/// Handle to the Lean service. Cheap to clone.
#[derive(Clone)]
pub struct LeanService {
    tx: mpsc::UnboundedSender<Msg>,
    events: broadcast::Sender<LeanEvent>,
    workspace: Arc<WorkspaceService>,
}

#[derive(Clone, Copy, Debug)]
enum PosKind {
    Goals,
    Hover,
    Definition,
}

struct PositionReq {
    requests: Vec<PendingRequest>,
    index: Arc<LineIndex>,
    root: PathBuf,
    doc_abs: Option<PathBuf>,
    doc_text: Arc<str>,
}

struct CheckTicket {
    doc: DocumentId,
    temp: bool,
    path: WorkspacePath,
    revision: u64,
    text_hash: String,
    wait: oneshot::Receiver<()>,
}

struct CheckSnapshot {
    state: LeanDocumentState,
    sorries: Vec<SorryLocation>,
    axioms: Vec<String>,
    axiom_uses: Vec<AxiomUse>,
}

type Reply<T> = oneshot::Sender<T>;

enum Msg {
    States(Reply<Vec<LeanDocumentState>>),
    Projects(Reply<Vec<LeanProjectInfo>>),
    Position {
        document: DocumentId,
        revision: u64,
        offset: u32,
        kind: PosKind,
        reply: Reply<Result<PositionReq, LeanError>>,
    },
    Restart {
        document: DocumentId,
        reply: Reply<Result<(), LeanError>>,
    },
    RestartFile {
        document: DocumentId,
        reply: Reply<Result<(), LeanError>>,
    },
    Install {
        document: DocumentId,
        reply: Reply<Result<JobId, LeanError>>,
    },
    BeginCheck {
        target: LeanCheckTarget,
        reply: Reply<Result<CheckTicket, LeanError>>,
    },
    FinishCheck {
        doc: DocumentId,
        reply: Reply<Option<CheckSnapshot>>,
    },
    DropTemp {
        doc: DocumentId,
    },
    ServerPid {
        document: DocumentId,
        reply: Reply<Option<u32>>,
    },
    Shutdown {
        reply: Reply<()>,
    },
    Started {
        key: ServerKey,
        generation: u64,
        result: Result<StartedServer, StartError>,
    },
    Incoming {
        key: ServerKey,
        generation: u64,
        incoming: Incoming,
    },
    JobExited {
        key: ServerKey,
        generation: u64,
        result: Option<JobResult>,
    },
    WaitDone {
        generation: u64,
        doc: DocumentId,
        version: u64,
        result: Result<(), LspError>,
    },
    StartAudit {
        generation: u64,
        doc: DocumentId,
        version: u64,
    },
    Audited {
        generation: u64,
        doc: DocumentId,
        version: u64,
        outcome: audit::AuditOutcome,
    },
    SorryTokens {
        generation: u64,
        doc: DocumentId,
        version: u64,
        tokens: Vec<TextRange>,
    },
    Retry {
        key: ServerKey,
        generation: u64,
    },
    Stopped {
        key: ServerKey,
        generation: u64,
    },
    Version {
        dir: PathBuf,
        version: Option<String>,
    },
    InstallDone,
}

impl LeanService {
    /// Create the service. The returned future is the actor; spawn it once on
    /// the application's Tokio runtime.
    pub fn new(
        execution: ExecutionService,
        workspace: Arc<WorkspaceService>,
        config: LeanConfig,
    ) -> (LeanService, impl Future<Output = ()> + Send + 'static) {
        let (tx, rx) = mpsc::unbounded_channel();
        let (events, _) = broadcast::channel(4096);
        let documents = Arc::clone(workspace.documents());
        let doc_rx = documents.subscribe();
        let ws_rx = workspace.subscribe();
        // Subscribed now, so a grant or revoke made before the actor runs is not missed.
        let trust_rx = execution.subscribe_trust();
        let actor = Actor::new(execution, Arc::clone(&workspace), documents, config, events.clone(), tx.clone());
        (LeanService { tx, events, workspace }, actor.run(rx, doc_rx, ws_rx, trust_rx))
    }

    pub fn subscribe(&self) -> broadcast::Receiver<LeanEvent> {
        self.events.subscribe()
    }

    async fn ask<T>(&self, make: impl FnOnce(Reply<T>) -> Msg) -> Result<T, LeanError> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(make(reply)).map_err(|_| stopped())?;
        rx.await.map_err(|_| stopped())
    }

    /// State of every tracked Lean document.
    pub async fn documents(&self) -> Vec<LeanDocumentState> {
        self.ask(Msg::States).await.unwrap_or_default()
    }

    /// Lean projects in the open workspace with their toolchain and server state.
    pub async fn projects(&self) -> Vec<LeanProjectInfo> {
        self.ask(Msg::Projects).await.unwrap_or_default()
    }

    /// Goals at a UTF-16 offset of `revision` (must be the current revision).
    pub async fn goals(&self, document: DocumentId, revision: u64, offset: u32) -> Result<LeanGoals, LeanError> {
        let req = self
            .ask(|reply| Msg::Position { document: document.clone(), revision, offset, kind: PosKind::Goals, reply })
            .await??;
        let mut requests = req.requests.into_iter();
        let plain = requests.next().expect("plainGoal request");
        let term = requests.next().expect("plainTermGoal request");
        let (plain, term) = tokio::join!(plain.response(POSITION_TIMEOUT), term.response(POSITION_TIMEOUT));
        let plain = plain.map_err(lsp_error)?;
        let goals = if plain.is_null() {
            None
        } else {
            Some(
                plain
                    .get("goals")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                    .unwrap_or_default(),
            )
        };
        let term_goal = match term {
            Ok(v) if !v.is_null() => v.get("goal").and_then(Value::as_str).map(|goal| LeanTermGoal {
                goal: goal.to_owned(),
                range: v.get("range").and_then(|r| convert::range_of(&req.index, r)),
            }),
            Ok(_) => None,
            Err(e) if e.is_superseded() => return Err(lsp_error(e)),
            Err(_) => None,
        };
        Ok(LeanGoals { document, revision, offset, goals, term_goal })
    }

    pub async fn hover(
        &self,
        document: DocumentId,
        revision: u64,
        offset: u32,
    ) -> Result<Option<LeanHover>, LeanError> {
        let req = self.ask(|reply| Msg::Position { document, revision, offset, kind: PosKind::Hover, reply }).await??;
        let value =
            req.requests.into_iter().next().expect("hover").response(POSITION_TIMEOUT).await.map_err(lsp_error)?;
        if value.is_null() {
            return Ok(None);
        }
        let contents = value.get("contents").cloned().unwrap_or(Value::Null);
        let markdown = match &contents {
            Value::String(s) => s.clone(),
            Value::Object(o) => o.get("value").and_then(Value::as_str).unwrap_or("").to_owned(),
            Value::Array(items) => items
                .iter()
                .map(|i| {
                    i.as_str()
                        .map(str::to_owned)
                        .or_else(|| i.get("value").and_then(Value::as_str).map(str::to_owned))
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>()
                .join("\n\n"),
            _ => String::new(),
        };
        if markdown.trim().is_empty() {
            return Ok(None);
        }
        let range = value.get("range").and_then(|r| convert::range_of(&req.index, r));
        Ok(Some(LeanHover { markdown: markdown.chars().take(50_000).collect(), range }))
    }

    pub async fn definition(
        &self,
        document: DocumentId,
        revision: u64,
        offset: u32,
    ) -> Result<Vec<LeanLocation>, LeanError> {
        let req =
            self.ask(|reply| Msg::Position { document, revision, offset, kind: PosKind::Definition, reply }).await??;
        let value =
            req.requests.into_iter().next().expect("definition").response(POSITION_TIMEOUT).await.map_err(lsp_error)?;
        let items = match value {
            Value::Array(items) => items,
            Value::Null => Vec::new(),
            single => vec![single],
        };
        let mut out = Vec::new();
        for item in items.iter().take(50) {
            let uri = item.get("targetUri").or_else(|| item.get("uri")).and_then(Value::as_str);
            let range = item.get("targetSelectionRange").or_else(|| item.get("range"));
            let (Some(uri), Some(range)) = (uri, range) else { continue };
            let Some(file) = uri_to_path(uri) else { continue };
            let start = range.get("start");
            let line = start.and_then(|s| s.get("line")).and_then(Value::as_u64).unwrap_or(0) as u32;
            let character = start.and_then(|s| s.get("character")).and_then(Value::as_u64).unwrap_or(0) as u32;
            let path = relative_to(&req.root, &file);
            // Ranges in the current text: the document itself, an open buffer,
            // or the workspace file (read through the workspace path checks).
            let text: Option<Arc<str>> = if req.doc_abs.as_ref().is_some_and(|d| same_path(d, &file)) {
                Some(req.doc_text.clone())
            } else {
                None
            };
            let text = match (text, &path) {
                (Some(t), _) => Some(t),
                (None, Some(p)) => self.read_workspace_text(p).await,
                _ => None,
            };
            let range = text.and_then(|t| convert::range_of(&LineIndex::new(&t), range));
            out.push(LeanLocation { path, file: file.display().to_string(), line, character, range });
        }
        Ok(out)
    }

    async fn read_workspace_text(&self, path: &WorkspacePath) -> Option<Arc<str>> {
        // The workspace snapshot resolves the path inside the root (no
        // traversal or link escapes) and prefers an open buffer over disk.
        let workspace = Arc::clone(&self.workspace);
        let path = path.clone();
        tokio::task::spawn_blocking(move || workspace.snapshot().ok()?.read(&path).ok()?.text).await.ok().flatten()
    }

    /// Restart the language server serving `document` (re-resolving its
    /// project and toolchain, and re-checking trust).
    pub async fn restart(&self, document: DocumentId) -> Result<(), LeanError> {
        self.ask(|reply| Msg::Restart { document, reply }).await?
    }

    /// Close and reopen the document in its server (Lean's "Restart File":
    /// rebuilds out-of-date imports).
    pub async fn restart_file(&self, document: DocumentId) -> Result<(), LeanError> {
        self.ask(|reply| Msg::RestartFile { document, reply }).await?
    }

    /// Explicit user action: install the missing toolchain of `document`'s
    /// project with `elan toolchain install` (a trusted job). Returns the job.
    pub async fn install_toolchain(&self, document: DocumentId) -> Result<JobId, LeanError> {
        self.ask(|reply| Msg::Install { document, reply }).await?
    }

    /// The process id of the server serving `document` (diagnostics/tests).
    pub async fn server_pid(&self, document: DocumentId) -> Option<u32> {
        self.ask(|reply| Msg::ServerPid { document, reply }).await.ok().flatten()
    }

    /// Stop every server (bounded). Call on application exit.
    pub async fn shutdown(&self) {
        let _ = tokio::time::timeout(Duration::from_secs(8), self.ask(|reply| Msg::Shutdown { reply })).await;
    }

    /// Check a Lean file and report the result for the exact text checked.
    ///
    /// The report says `checked: true` only for [`LeanStatus::Checked`]: Lean
    /// elaborated exactly this revision (or disk text) completely with no
    /// errors, and the axiom audit of that text found no `sorry` (also none
    /// imported or with a suppressed warning), kernel checking enabled, and
    /// only the standard axioms (listed in `axioms`). Incomplete material,
    /// extra axioms, compiled-code trust, `#exit`, a timeout, an edit during
    /// the check, a crash, a missing toolchain, or an untrusted workspace all
    /// produce `checked: false` with the corresponding status.
    pub async fn check(&self, request: LeanCheckRequest) -> Result<LeanCheckReport, LeanError> {
        let started = Instant::now();
        let timeout =
            request.timeout_ms.map(Duration::from_millis).unwrap_or(DEFAULT_CHECK_TIMEOUT).min(MAX_CHECK_TIMEOUT);
        let ticket = self.ask(|reply| Msg::BeginCheck { target: request.target.clone(), reply }).await??;
        let _ = tokio::time::timeout(timeout, ticket.wait).await;
        let snapshot = self.ask(|reply| Msg::FinishCheck { doc: ticket.doc.clone(), reply }).await?;
        let Some(snapshot) = snapshot else {
            return Err(LeanError::new("lean.closed", "The document was closed during the check."));
        };
        let finished = snapshot.state.status.finished_revision() == Some(ticket.revision);
        let mut goals = Vec::new();
        if finished {
            for &offset in request.goals_at.iter().take(32) {
                match self.goals(ticket.doc.clone(), ticket.revision, offset).await {
                    Ok(g) => goals.push(LeanCheckGoals { offset, goals: g.goals, term_goal: g.term_goal, error: None }),
                    Err(e) => {
                        goals.push(LeanCheckGoals { offset, goals: None, term_goal: None, error: Some(e.message) })
                    }
                }
            }
        }
        if ticket.temp {
            let _ = self.tx.send(Msg::DropTemp { doc: ticket.doc.clone() });
        }
        let state = snapshot.state;
        let checked = matches!(state.status, LeanStatus::Checked { revision, .. } if revision == ticket.revision);
        let (diagnostics, sorries, axioms, axiom_uses) =
            if finished && state.diagnostics_revision == Some(ticket.revision) {
                (state.diagnostics, snapshot.sorries, snapshot.axioms, snapshot.axiom_uses)
            } else {
                (Vec::new(), Vec::new(), Vec::new(), Vec::new())
            };
        Ok(LeanCheckReport {
            path: ticket.path,
            document: (!ticket.temp).then_some(ticket.doc),
            revision: (!ticket.temp).then_some(ticket.revision),
            content_hash: ticket.text_hash,
            project_root: state.project_root,
            toolchain: state.toolchain,
            status: state.status,
            checked,
            diagnostics,
            sorries,
            axioms,
            axiom_uses,
            goals,
            duration_ms: started.elapsed().as_millis() as u64,
        })
    }
}

fn stopped() -> LeanError {
    LeanError::new("lean.stopped", "The Lean service is not running.")
}

fn lsp_error(e: LspError) -> LeanError {
    match e {
        e if e.is_superseded() => LeanError::new("lean.cancelled", "The request was superseded by a newer one."),
        LspError::Timeout => LeanError::new("lean.timeout", "Lean did not answer in time."),
        LspError::Closed => LeanError::new("lean.serverStopped", "The Lean server is not running."),
        other => LeanError::new("lean.server", other.to_string()),
    }
}

/// `abs` relative to `root` as a workspace path (case-insensitive on Windows).
fn relative_to(root: &Path, abs: &Path) -> Option<WorkspacePath> {
    let root_parts: Vec<_> = root.components().collect();
    let parts: Vec<_> = abs.components().collect();
    if parts.len() <= root_parts.len() {
        return None;
    }
    for (a, b) in root_parts.iter().zip(&parts) {
        if !same_path(Path::new(a.as_os_str()), Path::new(b.as_os_str())) {
            return None;
        }
    }
    let rest: Vec<String> =
        parts[root_parts.len()..].iter().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    WorkspacePath::parse(&rest.join("/")).ok()
}

fn sha256_hex(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn is_lean(info: &DocumentInfo) -> bool {
    info.path.as_ref().is_some_and(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("lean")))
}

fn unknown_document() -> LeanError {
    LeanError::new("lean.unknownDocument", "That document is not an open Lean file.")
}

fn join(root: &Path, path: &WorkspacePath) -> PathBuf {
    let mut out = root.to_path_buf();
    for part in path.components() {
        out.push(part);
    }
    out
}
