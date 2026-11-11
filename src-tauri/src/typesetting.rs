//! Typesetting commands, the build scheduler, and build events.
//!
//! Page views "watch" an entry file. Every accepted document change schedules
//! a rebuild of the watched entries on one background thread; requests that
//! arrive during a build coalesce into the next build, so typing never waits
//! for the compiler. Each finished build is emitted as `typesetting:build`.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};

use exposition_core::build::{BuildResult, BuildTarget, DocumentFormat, Engine, ExportArtifact};
use exposition_core::document::DocumentEvent;
use exposition_core::path::WorkspacePath;
use exposition_typesetting::{BuildRecord, BuildTimings, TypesettingService};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::broadcast::error::RecvError;
use ts_rs::TS;

use crate::error::CommandError;
use crate::state::AppState;

pub const BUILD_EVENT: &str = "typesetting:build";
pub const STATUS_EVENT: &str = "typesetting:status";

/// A finished build as seen by views.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct BuildEvent {
    pub result: BuildResult,
    pub timings: BuildTimings,
    /// The last successful build of the same entry (equals `result.id` when
    /// this build succeeded); its pages stay visible, marked stale, while the
    /// source has errors.
    pub last_good: Option<BuildResult>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum BuildActivity {
    Queued,
    Building,
    Idle,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct BuildStatusEvent {
    pub entry: WorkspacePath,
    pub activity: BuildActivity,
}

#[derive(Default)]
struct Queue {
    dirty: BTreeSet<WorkspacePath>,
}

/// Owns the typesetting service and its build thread.
pub struct TypesettingRuntime {
    pub service: Arc<TypesettingService>,
    watched: Mutex<BTreeSet<WorkspacePath>>,
    queue: Mutex<Queue>,
    wake: Condvar,
}

fn target(entry: &WorkspacePath) -> BuildTarget {
    BuildTarget { format: DocumentFormat::Typst, engine: Engine::Typst, entry: entry.clone() }
}

impl TypesettingRuntime {
    pub fn new(service: Arc<TypesettingService>) -> Arc<Self> {
        Arc::new(Self {
            service,
            watched: Mutex::new(BTreeSet::new()),
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
        })
    }

    fn event_for(&self, record: &BuildRecord) -> BuildEvent {
        BuildEvent {
            result: record.result.clone(),
            timings: record.timings,
            last_good: self.service.last_good(&record.entry).map(|r| r.result.clone()),
        }
    }

    fn request(&self, entry: WorkspacePath, app: &AppHandle) {
        let mut queue = self.queue.lock().unwrap_or_else(|p| p.into_inner());
        if queue.dirty.insert(entry.clone()) {
            let _ = app.emit(STATUS_EVENT, BuildStatusEvent { entry, activity: BuildActivity::Queued });
        }
        self.wake.notify_one();
    }

    fn request_watched(&self, app: &AppHandle) {
        let watched: Vec<_> = self.watched.lock().unwrap_or_else(|p| p.into_inner()).iter().cloned().collect();
        for entry in watched {
            self.request(entry, app);
        }
    }

    /// Start the build thread and forward document changes to it.
    pub fn start(self: &Arc<Self>, app: AppHandle) {
        let runtime = Arc::clone(self);
        let handle = app.clone();
        std::thread::Builder::new()
            .name("typesetting".into())
            .spawn(move || {
                loop {
                    let entries: Vec<WorkspacePath> = {
                        let mut queue = runtime.queue.lock().unwrap_or_else(|p| p.into_inner());
                        while queue.dirty.is_empty() {
                            queue = runtime.wake.wait(queue).unwrap_or_else(|p| p.into_inner());
                        }
                        std::mem::take(&mut queue.dirty).into_iter().collect()
                    };
                    for entry in entries {
                        let _ = handle.emit(
                            STATUS_EVENT,
                            BuildStatusEvent { entry: entry.clone(), activity: BuildActivity::Building },
                        );
                        // A compiler panic must not stop future builds.
                        let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            runtime.service.build(&target(&entry))
                        }));
                        match built {
                            Ok(Ok(record)) => {
                                let _ = handle.emit(BUILD_EVENT, runtime.event_for(&record));
                            }
                            Ok(Err(_)) => {}
                            Err(_) => eprintln!("typesetting: the compiler panicked while building {entry}"),
                        }
                        let _ = handle.emit(STATUS_EVENT, BuildStatusEvent { entry, activity: BuildActivity::Idle });
                    }
                }
            })
            .expect("spawn typesetting thread");

        let runtime = Arc::clone(self);
        let mut rx = app.state::<AppState>().documents.subscribe();
        tauri::async_runtime::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(DocumentEvent::Changed(_)) | Err(RecvError::Lagged(_)) => runtime.request_watched(&app),
                    Ok(_) => {}
                    Err(RecvError::Closed) => break,
                }
            }
        });
    }
}

fn runtime<'a>(state: &'a State<'_, AppState>) -> &'a Arc<TypesettingRuntime> {
    &state.typesetting
}

fn command_error(code: &str, error: impl std::fmt::Display) -> CommandError {
    CommandError::new(code, error.to_string())
}

/// Watch an entry: build it now and after every document change.
/// Returns the latest retained build, if any.
#[tauri::command]
pub async fn typesetting_watch(
    app: AppHandle,
    state: State<'_, AppState>,
    entry: WorkspacePath,
) -> Result<Option<BuildEvent>, CommandError> {
    if entry.extension() != Some("typ") {
        return Err(CommandError::new("typesetting.unsupported", "only .typ entries can be typeset"));
    }
    let runtime = runtime(&state);
    if runtime.service.workspace_root().is_none() {
        return Err(CommandError::new("typesetting.noWorkspace", "no workspace is open"));
    }
    runtime.watched.lock().unwrap_or_else(|p| p.into_inner()).insert(entry.clone());
    runtime.request(entry.clone(), &app);
    Ok(runtime.service.latest(&entry).map(|r| runtime.event_for(&r)))
}

#[tauri::command]
pub async fn typesetting_unwatch(state: State<'_, AppState>, entry: WorkspacePath) -> Result<(), CommandError> {
    runtime(&state).watched.lock().unwrap_or_else(|p| p.into_inner()).remove(&entry);
    Ok(())
}

/// Build an entry now and return the result.
#[tauri::command]
pub async fn typesetting_build(state: State<'_, AppState>, entry: WorkspacePath) -> Result<BuildEvent, CommandError> {
    let runtime = Arc::clone(runtime(&state));
    tauri::async_runtime::spawn_blocking(move || {
        let record = runtime.service.build(&target(&entry)).map_err(|e| command_error("typesetting.build", e))?;
        Ok(runtime.event_for(&record))
    })
    .await
    .map_err(|e| command_error("typesetting.build", e))?
}

/// The SVG of a rendered page, by the page's `renderRef` from a build result.
#[tauri::command]
pub async fn typesetting_page_svg(state: State<'_, AppState>, render_ref: String) -> Result<String, CommandError> {
    if render_ref.len() != 32 || !render_ref.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(CommandError::new("typesetting.invalidRef", "invalid page reference"));
    }
    let runtime = Arc::clone(runtime(&state));
    tauri::async_runtime::spawn_blocking(move || runtime.service.page_svg(&render_ref))
        .await
        .map_err(|e| command_error("typesetting.render", e))?
        .map(|svg| svg.as_ref().clone())
        .ok_or_else(|| CommandError::new("typesetting.unknownPage", "the page is no longer available"))
}

/// Export a build to PDF from the same layout as its pages. Without a
/// destination the PDF is written next to the entry in the workspace.
#[tauri::command]
pub async fn typesetting_export_pdf(
    state: State<'_, AppState>,
    build: String,
    destination: Option<String>,
) -> Result<ExportArtifact, CommandError> {
    let runtime = Arc::clone(runtime(&state));
    let destination = destination.map(PathBuf::from);
    tauri::async_runtime::spawn_blocking(move || runtime.service.export_pdf(&build, destination))
        .await
        .map_err(|e| command_error("typesetting.export", e))?
        .map_err(|e| command_error("typesetting.export", e))
}
