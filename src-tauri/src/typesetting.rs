//! Typesetting commands, the build scheduler, and build events.
//!
//! Page views "watch" an entry file. Every accepted document change schedules
//! a rebuild of the watched entries on one background thread; requests that
//! arrive during a build coalesce into the next build, so typing never waits
//! for the compiler. Each finished build is emitted as `typesetting:build`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};

use exposition_core::build::{BuildResult, BuildTarget, DocumentFormat, Engine, ExportArtifact};
use exposition_core::document::DocumentEvent;
use exposition_core::path::WorkspacePath;
use exposition_execution::JobRefusal;
use exposition_typesetting::{BuildRecord, BuildTimings, TypesettingService};
use exposition_workspace::types::WorkspaceEvent;
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
    /// Why the build was refused (e.g. `untrustedWorkspace` for a LaTeX
    /// build in an untrusted workspace): the view offers the trust prompt.
    pub refusal: Option<JobRefusal>,
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
    /// Watched entries with the number of views watching each: an entry is
    /// rebuilt on changes until its last view unwatches it.
    watched: Mutex<BTreeMap<WorkspacePath, usize>>,
    queue: Mutex<Queue>,
    wake: Condvar,
}

/// The build target of an entry by its extension (`None` if not typeset here).
fn target_of(entry: &WorkspacePath) -> Option<BuildTarget> {
    let (format, engine) = match entry.extension()?.to_ascii_lowercase().as_str() {
        "typ" => (DocumentFormat::Typst, Engine::Typst),
        // Markdown: the declared print profile, laid out by Typst.
        "md" | "markdown" => (DocumentFormat::Markdown, Engine::MarkdownPrint),
        _ => return None,
    };
    Some(BuildTarget { format, engine, entry: entry.clone() })
}

/// The in-process build target of `entry` (`.typ` Typst, `.md`/`.markdown`
/// the Markdown print profile), or the error every typesetting command reports
/// for a file no format builds. `.tex` entries go to the TeX service before
/// this.
fn target(entry: &WorkspacePath) -> Result<BuildTarget, CommandError> {
    target_of(entry).ok_or_else(|| {
        CommandError::new(
            "typesetting.unsupported",
            format!("{entry} cannot be typeset: only .typ, .md, .markdown and .tex entries can be built"),
        )
    })
}

impl TypesettingRuntime {
    pub fn new(service: Arc<TypesettingService>) -> Arc<Self> {
        Arc::new(Self {
            service,
            watched: Mutex::new(BTreeMap::new()),
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
        })
    }

    /// Count one more view watching `entry`.
    pub fn watch(&self, entry: &WorkspacePath) {
        *self.watched.lock().unwrap_or_else(|p| p.into_inner()).entry(entry.clone()).or_default() += 1;
    }

    /// One view stopped watching `entry`; it stays watched while others remain.
    pub fn unwatch(&self, entry: &WorkspacePath) {
        let mut watched = self.watched.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(count) = watched.get_mut(entry) {
            *count -= 1;
            if *count == 0 {
                watched.remove(entry);
            }
        }
    }

    /// Number of views watching `entry`.
    pub fn watchers(&self, entry: &WorkspacePath) -> usize {
        self.watched.lock().unwrap_or_else(|p| p.into_inner()).get(entry).copied().unwrap_or(0)
    }

    fn event_for(&self, record: &BuildRecord) -> BuildEvent {
        BuildEvent {
            result: record.result.clone(),
            timings: record.timings,
            last_good: self.service.last_good(&record.entry).map(|r| r.result.clone()),
            refusal: None,
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
        let watched: Vec<_> = self.watched.lock().unwrap_or_else(|p| p.into_inner()).keys().cloned().collect();
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
                        // Watched entries were checked by `typesetting_watch`.
                        let Ok(target) = target(&entry) else { continue };
                        let _ = handle.emit(
                            STATUS_EVENT,
                            BuildStatusEvent { entry: entry.clone(), activity: BuildActivity::Building },
                        );
                        // A compiler panic must not stop future builds.
                        let built =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| runtime.service.build(&target)));
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
        let documents_app = app.clone();
        let mut rx = app.state::<AppState>().documents.subscribe();
        tauri::async_runtime::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(DocumentEvent::Changed(_)) | Err(RecvError::Lagged(_)) => {
                        runtime.request_watched(&documents_app)
                    }
                    Ok(_) => {}
                    Err(RecvError::Closed) => break,
                }
            }
        });

        // Files changed on disk (images, included files that are not open)
        // rebuild too; closing the folder forgets its builds.
        let runtime = Arc::clone(self);
        let mut rx = app.state::<AppState>().workspace.subscribe();
        tauri::async_runtime::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(WorkspaceEvent::FilesChanged { .. }) | Err(RecvError::Lagged(_)) => {
                        runtime.request_watched(&app)
                    }
                    Ok(WorkspaceEvent::Closed) => {
                        runtime.watched.lock().unwrap_or_else(|p| p.into_inner()).clear();
                        runtime.service.reset();
                    }
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
    // LaTeX entries build through the TeX service (exposition-tex).
    if crate::tex::is_tex(&entry) {
        return crate::tex::watch(&state, &entry);
    }
    target(&entry)?;
    let runtime = runtime(&state);
    if runtime.service.workspace().current().is_none() {
        return Err(CommandError::new("typesetting.noWorkspace", "no workspace is open"));
    }
    runtime.watch(&entry);
    runtime.request(entry.clone(), &app);
    Ok(runtime.service.latest(&entry).map(|r| runtime.event_for(&r)))
}

#[tauri::command]
pub async fn typesetting_unwatch(state: State<'_, AppState>, entry: WorkspacePath) -> Result<(), CommandError> {
    if crate::tex::is_tex(&entry) {
        crate::tex::unwatch(&state, &entry);
        return Ok(());
    }
    runtime(&state).unwatch(&entry);
    Ok(())
}

/// Build an entry now and return the result.
#[tauri::command]
pub async fn typesetting_build(state: State<'_, AppState>, entry: WorkspacePath) -> Result<BuildEvent, CommandError> {
    if crate::tex::is_tex(&entry) {
        return crate::tex::build_now(&state, &entry).await;
    }
    let target = target(&entry)?;
    let runtime = Arc::clone(runtime(&state));
    tauri::async_runtime::spawn_blocking(move || {
        let record = runtime.service.build(&target).map_err(|e| command_error("typesetting.build", e))?;
        Ok(runtime.event_for(&record))
    })
    .await
    .map_err(|e| command_error("typesetting.build", e))?
}

/// Export a build to PDF from the same layout as its pages. Without a
/// destination the PDF is written next to the entry in the workspace.
#[tauri::command]
pub async fn typesetting_export_pdf(
    state: State<'_, AppState>,
    build: String,
    destination: Option<String>,
) -> Result<ExportArtifact, CommandError> {
    if crate::tex::is_tex_build(&build) {
        return crate::tex::export_pdf(&state, build, destination).await;
    }
    let runtime = Arc::clone(runtime(&state));
    let destination = destination.map(PathBuf::from);
    tauri::async_runtime::spawn_blocking(move || runtime.service.export_pdf(&build, destination))
        .await
        .map_err(|e| command_error("typesetting.export", e))?
        .map_err(|e| command_error("typesetting.export", e))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use exposition_core::path::WorkspacePath;
    use exposition_typesetting::{Fonts, TypesettingService};
    use exposition_workspace::{DocumentService, WorkspaceService};

    use super::TypesettingRuntime;

    /// Regression (R-m23 F5): two views watch the same entry; closing one must
    /// keep the entry watched (rebuilt on edits) for the other.
    #[test]
    fn watching_is_counted_per_view() {
        // No folder is opened, so the data directory is never touched.
        let documents = Arc::new(DocumentService::new());
        let workspace = WorkspaceService::new(documents, std::env::temp_dir().join("exposition-unused"));
        let runtime = TypesettingRuntime::new(Arc::new(TypesettingService::new(workspace, Fonts::embedded_only)));
        let entry = WorkspacePath::parse("main.typ").unwrap();
        runtime.watch(&entry);
        runtime.watch(&entry);
        runtime.unwatch(&entry);
        assert_eq!(runtime.watchers(&entry), 1, "the remaining view still watches the entry");
        runtime.unwatch(&entry);
        assert_eq!(runtime.watchers(&entry), 0);
        runtime.unwatch(&entry);
        assert_eq!(runtime.watchers(&entry), 0, "extra unwatch is harmless");
    }

    /// In-process builds dispatch by extension; any other file is refused
    /// (it used to be compiled as Typst).
    #[test]
    fn build_targets_dispatch_by_extension() {
        use exposition_core::build::{DocumentFormat, Engine};
        let target = |path: &str| super::target(&WorkspacePath::parse(path).unwrap());
        let typst = target("chapters/Main.TYP").unwrap();
        assert_eq!((typst.format, typst.engine), (DocumentFormat::Typst, Engine::Typst));
        let markdown = target("README.md").unwrap();
        assert_eq!((markdown.format, markdown.engine), (DocumentFormat::Markdown, Engine::MarkdownPrint));
        // The message lists exactly the extensions that are dispatched.
        let long = target("notes/Intro.markdown").unwrap();
        assert_eq!((long.format, long.engine), (DocumentFormat::Markdown, Engine::MarkdownPrint));
        let message = target("notes.txt").unwrap_err().message;
        for listed in [".typ", ".md", ".markdown", ".tex"] {
            assert!(message.contains(listed), "{message}");
        }
        for refused in ["notes.txt", "figure.pdf", "Makefile", "main.typ.bak", "data.json"] {
            let error = target(refused).unwrap_err();
            assert_eq!(error.code, "typesetting.unsupported", "{refused}");
            assert!(error.message.contains(refused), "{}", error.message);
        }
    }
}
