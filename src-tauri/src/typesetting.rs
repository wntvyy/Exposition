//! Typesetting commands, the build scheduler, and build events.
//!
//! Page views "watch" an entry file. Every accepted document change schedules
//! a rebuild of the watched entries on one background thread; requests that
//! arrive during a build coalesce into the next build, so typing never waits
//! for the compiler. Each finished build is emitted as `typesetting:build`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

use exposition_core::build::{BuildId, BuildResult, BuildTarget, DocumentFormat, Engine, ExportArtifact};
use exposition_core::document::{DocumentChanged, DocumentEvent};
use exposition_core::path::WorkspacePath;
use exposition_execution::JobRefusal;
use exposition_typesetting::{BuildRecord, BuildTimings, TypesettingService};
use exposition_workspace::WorkspaceService;
use exposition_workspace::types::WorkspaceEvent;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
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
    /// Entries taken from `dirty` whose build has not finished yet.
    active: BTreeSet<WorkspacePath>,
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

    /// The watched entries a document change must rebuild. An entry is left
    /// alone only when no build of it is queued or unfinished and its latest
    /// build is known not to depend on the change
    /// ([`TypesettingService::change_concerns`]); a document without a path is
    /// read by no build. An unknown document rebuilds everything.
    fn entries_for_change(&self, change: &DocumentChanged) -> Vec<WorkspacePath> {
        let watched: Vec<_> = self.watched.lock().unwrap_or_else(|p| p.into_inner()).keys().cloned().collect();
        match self.service.workspace().documents().info(&change.document) {
            Ok(info) if info.path.is_none() => Vec::new(),
            Ok(_) => watched
                .into_iter()
                .filter(|entry| {
                    let unfinished = {
                        let queue = self.queue.lock().unwrap_or_else(|p| p.into_inner());
                        queue.dirty.contains(entry) || queue.active.contains(entry)
                    };
                    unfinished || self.service.change_concerns(entry, &change.document, change.revision)
                })
                .collect(),
            Err(_) => watched,
        }
    }

    fn request_for_change(&self, app: &AppHandle, change: &DocumentChanged) {
        for entry in self.entries_for_change(change) {
            self.request(entry, app);
        }
    }

    /// A build of `entry` finished (or could not start).
    fn finished(&self, entry: &WorkspacePath) {
        self.queue.lock().unwrap_or_else(|p| p.into_inner()).active.remove(entry);
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
                        // Unfinished until each is built: a change that arrives
                        // meanwhile cannot be judged against the previous build.
                        let entries: Vec<WorkspacePath> = std::mem::take(&mut queue.dirty).into_iter().collect();
                        queue.active.extend(entries.iter().cloned());
                        entries
                    };
                    for entry in entries {
                        // Watched entries were checked by `typesetting_watch`.
                        let Ok(target) = target(&entry) else {
                            runtime.finished(&entry);
                            continue;
                        };
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
                        // The record (if any) is retained by now.
                        runtime.finished(&entry);
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
                    Ok(DocumentEvent::Changed(change)) => runtime.request_for_change(&documents_app, &change),
                    // Changes were missed: every entry may be concerned.
                    Err(RecvError::Lagged(_)) => runtime.request_watched(&documents_app),
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

/// The service that owns a build, as the export needs it.
pub(crate) trait PdfExporter {
    /// The entry a retained build of the open folder was made from; an error
    /// when the build is unknown here or has no pages to export.
    fn exportable_entry(&self, build: &str) -> Result<WorkspacePath, CommandError>;
    /// Write the build's PDF to a checked destination.
    fn export(&self, build: &str, destination: PathBuf) -> Result<ExportArtifact, CommandError>;
}

impl PdfExporter for TypesettingService {
    fn exportable_entry(&self, build: &str) -> Result<WorkspacePath, CommandError> {
        let record = self.record(build).ok_or_else(|| export_error(format!("build {build} is not available")))?;
        if record.document.is_none() {
            return Err(export_error(format!("build {build} has no output")));
        }
        Ok(record.entry.clone())
    }

    fn export(&self, build: &str, destination: PathBuf) -> Result<ExportArtifact, CommandError> {
        self.export_pdf(build, Some(destination)).map_err(export_error)
    }
}

impl PdfExporter for exposition_tex::TexService {
    fn exportable_entry(&self, build: &str) -> Result<WorkspacePath, CommandError> {
        let stored = self
            .build(&BuildId(build.to_owned()))
            .ok_or_else(|| export_error(exposition_tex::TexServiceError::UnknownBuild))?;
        if !stored.summary.has_output {
            return Err(export_error(exposition_tex::TexServiceError::NoOutput));
        }
        Ok(stored.summary.result.target.entry.clone())
    }

    fn export(&self, build: &str, destination: PathBuf) -> Result<ExportArtifact, CommandError> {
        self.export_pdf(&BuildId(build.to_owned()), Some(destination)).map_err(export_error)
    }
}

fn export_error(error: impl std::fmt::Display) -> CommandError {
    command_error("typesetting.export", error)
}

/// Where the save dialog starts: the entry's folder and `<entry stem>.pdf`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExportSuggestion {
    pub folder: PathBuf,
    pub file_name: String,
}

/// Export `build` of `entry` to PDF as an output copy.
///
/// The build is bound to its document: it must be a retained build of the
/// open folder, have pages, and be a build of `entry` (the document the
/// person exported from). This is checked before the dialog opens, and the
/// owning service looks the build up again when it writes, so nothing of
/// another document or project is exported.
///
/// `destination` none: `choose` asks the person (the native save dialog) and
/// its answer is the only place a location outside the project comes from; no
/// answer means canceled and nothing is written. The project that was open
/// when the dialog opened must still be the open one.
/// `destination` some: a location given by the caller, accepted only inside
/// the open project (where its file commands already write).
///
/// Either way the destination is checked (`exposition_workspace::export`),
/// never inside the project's `.exposition` records or the app's data folder,
/// and written atomically by the service, which refuses to replace anything
/// but an earlier PDF that is not a source of the document. The location is
/// used for this one write and is not remembered or admitted anywhere.
pub(crate) fn export_pdf_to(
    exporter: &dyn PdfExporter,
    workspace: &WorkspaceService,
    data_dir: &Path,
    build: &str,
    entry: &WorkspacePath,
    destination: Option<String>,
    choose: impl FnOnce(&ExportSuggestion) -> Option<PathBuf>,
) -> Result<ExportArtifact, CommandError> {
    let open_root = || workspace.open_root().and_then(|root| std::fs::canonicalize(root).ok());
    let root = open_root().ok_or_else(|| CommandError::new("typesetting.noWorkspace", "no workspace is open"))?;
    let built = exporter.exportable_entry(build)?;
    if &built != entry {
        return Err(export_error(format!("build {build} is a build of {built}, not of {entry}; nothing was exported")));
    }
    let checked = |path: &Path| exposition_workspace::export::checked_destination(path, "pdf").map_err(export_error);
    let target = match destination {
        Some(raw) => {
            let target = checked(Path::new(raw.trim()))?;
            if !target.starts_with(&root) {
                return Err(export_error(
                    "a destination outside the project is chosen in the Export PDF dialog; nothing was exported",
                ));
            }
            target
        }
        None => {
            let folder =
                entry.parent().map_or(root.clone(), |parent| parent.components().fold(root.clone(), |p, c| p.join(c)));
            let folder = if folder.is_dir() { folder } else { root.clone() };
            let stem = entry.file_name().rsplit_once('.').map_or(entry.file_name(), |(stem, _)| stem);
            let suggestion = ExportSuggestion {
                folder: PathBuf::from(exposition_workspace::root::display_path(&folder)),
                file_name: format!("{stem}.pdf"),
            };
            let picked = choose(&suggestion)
                .ok_or_else(|| CommandError::new("typesetting.exportCanceled", "The export was canceled."))?;
            if open_root().as_ref() != Some(&root) {
                return Err(export_error(
                    "the open project changed while the destination was chosen; nothing was exported",
                ));
            }
            checked(&picked)?
        }
    };
    let data = std::fs::canonicalize(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
    if target.starts_with(root.join(".exposition")) || target.starts_with(data) {
        return Err(export_error("that location holds Exposition's own records; choose another folder"));
    }
    exporter.export(build, target)
}

/// Export a build of `entry` to PDF from the same layout as its pages, as an
/// output copy. `destination` none: the backend opens the native save dialog
/// (starting at `<entry stem>.pdf` next to the entry) and the person chooses
/// any location, inside or outside the project; `typesetting.exportCanceled`
/// when dismissed, and nothing is written. The dialog asks before replacing
/// an existing file. `destination` some: an absolute `.pdf` path inside the
/// open project. See [`export_pdf_to`] for the checks.
#[tauri::command]
pub async fn typesetting_export_pdf(
    app: AppHandle,
    state: State<'_, AppState>,
    build: String,
    entry: WorkspacePath,
    destination: Option<String>,
) -> Result<ExportArtifact, CommandError> {
    let typesetting = Arc::clone(&runtime(&state).service);
    let tex = state.tex.clone();
    let workspace = Arc::clone(&state.workspace);
    let data_dir = state.data_dir.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let exporter: &dyn PdfExporter = if crate::tex::is_tex_build(&build) { &tex } else { typesetting.as_ref() };
        export_pdf_to(exporter, &workspace, &data_dir, &build, &entry, destination, |suggestion| {
            let mut dialog = app
                .dialog()
                .file()
                .set_title("Export PDF")
                .add_filter("PDF document", &["pdf"])
                .set_directory(&suggestion.folder)
                .set_file_name(&suggestion.file_name);
            // Modal to the workbench: no other tab or project is reached
            // while the destination is chosen.
            if let Some(window) = app.get_webview_window("main") {
                dialog = dialog.set_parent(&window);
            }
            dialog.blocking_save_file().and_then(|picked| picked.as_path().map(Path::to_path_buf))
        })
    })
    .await
    .map_err(export_error)?
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

    // ----- PDF export to a chosen destination (QC 2.4) -----

    use std::path::{Path, PathBuf};

    use exposition_core::build::{BuildTarget, DocumentFormat, Engine};

    use super::{ExportSuggestion, export_pdf_to};

    /// A temporary project (`main.typ`, `chapters/intro.typ`, `broken.typ`),
    /// an app data folder, and a folder outside both. src-tauri has no
    /// tempfile dev-dependency.
    struct ExportFixture {
        base: PathBuf,
        project: PathBuf,
        data: PathBuf,
        outside: PathBuf,
        workspace: Arc<WorkspaceService>,
        service: TypesettingService,
    }

    impl Drop for ExportFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    impl ExportFixture {
        fn new(name: &str) -> Self {
            let base = std::env::temp_dir().join(format!("xp-export-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            let project = base.join("project");
            let data = base.join("data");
            // Unicode and spaces in the outside folder.
            let outside = base.join("Mes exports — été 数学");
            for dir in [&project.join("chapters"), &data, &outside] {
                std::fs::create_dir_all(dir).unwrap();
            }
            std::fs::write(project.join("main.typ"), "= Main\n\nThe main document.\n").unwrap();
            std::fs::write(project.join("chapters").join("intro.typ"), "= Intro\n\nAnother document.\n").unwrap();
            std::fs::write(project.join("broken.typ"), "#this-function-does-not-exist()\n").unwrap();
            let documents = Arc::new(DocumentService::new());
            let workspace = WorkspaceService::new(documents, data.clone());
            workspace.open_workspace(&project).expect("open workspace");
            let service = TypesettingService::new(Arc::clone(&workspace), Fonts::embedded_only);
            Self { base, project, data, outside, workspace, service }
        }

        fn build(&self, entry: &str) -> String {
            let entry = WorkspacePath::parse(entry).unwrap();
            let target = BuildTarget { format: DocumentFormat::Typst, engine: Engine::Typst, entry };
            self.service.build(&target).expect("build runs").id().to_owned()
        }

        fn export(
            &self,
            build: &str,
            entry: &str,
            destination: Option<&Path>,
            choose: impl FnOnce(&ExportSuggestion) -> Option<PathBuf>,
        ) -> Result<exposition_core::build::ExportArtifact, crate::error::CommandError> {
            export_pdf_to(
                &self.service,
                &self.workspace,
                &self.data,
                build,
                &WorkspacePath::parse(entry).unwrap(),
                destination.map(|d| d.display().to_string()),
                choose,
            )
        }

        /// Every file under the base folder with its bytes, except the app
        /// data folder (its list of recent projects follows the open project).
        fn files(&self) -> Vec<(PathBuf, Vec<u8>)> {
            fn walk(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
                let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
                entries.sort();
                for path in entries {
                    if path.is_dir() {
                        walk(&path, out)
                    } else {
                        out.push((path.clone(), std::fs::read(&path).unwrap()))
                    }
                }
            }
            let mut out = Vec::new();
            walk(&self.base, &mut out);
            out.retain(|(path, _)| !path.starts_with(&self.data));
            out
        }
    }

    fn never(_: &ExportSuggestion) -> Option<PathBuf> {
        panic!("the save dialog must not open")
    }

    #[test]
    fn export_writes_the_chosen_destination_outside_the_project_and_nothing_else() {
        let fx = ExportFixture::new("outside");
        let build = fx.build("main.typ");
        let before = fx.files();
        let documents_before = fx.workspace.documents().list().len();
        let destination = fx.outside.join("Thèse finale (v2).pdf");
        let mut suggested = None;
        let artifact = fx
            .export(&build, "main.typ", None, |suggestion| {
                suggested = Some(suggestion.clone());
                Some(destination.clone())
            })
            .unwrap();
        // The dialog starts at `<entry stem>.pdf` in the entry's folder.
        let suggested = suggested.unwrap();
        assert_eq!(suggested.file_name, "main.pdf");
        assert_eq!(std::fs::canonicalize(&suggested.folder).unwrap(), std::fs::canonicalize(&fx.project).unwrap());
        assert!(!suggested.folder.display().to_string().starts_with(r"\\?\"), "{suggested:?}");

        let written = std::fs::read(&destination).unwrap();
        assert!(written.starts_with(b"%PDF-"));
        assert_eq!(std::fs::canonicalize(&artifact.path).unwrap(), std::fs::canonicalize(&destination).unwrap());
        assert!(!artifact.path.starts_with(r"\\?\"), "{}", artifact.path);
        // An output copy: the one new file, no project file, no document.
        let after = fx.files();
        let added: Vec<_> = after.iter().filter(|f| !before.contains(f)).map(|(p, _)| p.clone()).collect();
        assert_eq!(added, vec![destination.clone()]);
        assert!(before.iter().all(|f| after.contains(f)), "an existing file changed");
        assert_eq!(fx.workspace.documents().list().len(), documents_before, "no document was opened or admitted");

        // Replacing the earlier export (the dialog asked) works; the entry
        // in a subfolder is suggested next to itself.
        let intro = fx.build("chapters/intro.typ");
        let replaced = fx
            .export(&intro, "chapters/intro.typ", None, |suggestion| {
                assert_eq!(suggestion.file_name, "intro.pdf");
                assert!(suggestion.folder.ends_with("chapters"), "{suggestion:?}");
                Some(destination.clone())
            })
            .unwrap();
        assert_ne!(replaced.content_hash, artifact.content_hash, "the other document's build was written");
        assert_ne!(std::fs::read(&destination).unwrap(), written);
    }

    #[test]
    fn a_canceled_export_writes_nothing() {
        let fx = ExportFixture::new("cancel");
        let build = fx.build("main.typ");
        let before = fx.files();
        let error = fx.export(&build, "main.typ", None, |_| None).unwrap_err();
        assert_eq!(error.code, "typesetting.exportCanceled");
        assert_eq!(fx.files(), before);
    }

    #[test]
    fn export_is_bound_to_the_document_and_build_of_the_open_project() {
        let fx = ExportFixture::new("identity");
        let main = fx.build("main.typ");
        let intro = fx.build("chapters/intro.typ");
        let broken = fx.build("broken.typ");
        let before = fx.files();
        let destination = fx.outside.join("out.pdf");

        // Another document's build, an unknown build, a build without pages:
        // refused before any dialog.
        let mismatch = fx.export(&intro, "main.typ", None, never).unwrap_err();
        assert_eq!(mismatch.code, "typesetting.export");
        assert!(mismatch.message.contains("chapters/intro.typ") && mismatch.message.contains("main.typ"), "{mismatch}");
        assert!(fx.export("typst-no-such-build", "main.typ", None, never).is_err());
        let no_pages = fx.export(&broken, "broken.typ", None, never).unwrap_err();
        assert!(no_pages.message.contains("no output"), "{no_pages}");
        assert_eq!(fx.files(), before);

        // Another project is opened while the dialog is open: nothing is written.
        let other = fx.base.join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("main.typ"), "= Other\n").unwrap();
        let before = fx.files();
        let switched = fx
            .export(&main, "main.typ", None, |_| {
                fx.workspace.open_workspace(&other).unwrap();
                Some(destination.clone())
            })
            .unwrap_err();
        assert!(switched.message.contains("project changed"), "{switched}");
        // The build of the folder that is no longer open is not exported.
        let stale = fx.export(&main, "main.typ", None, never).unwrap_err();
        assert!(stale.message.contains("not available"), "{stale}");
        assert!(fx.export(&main, "main.typ", Some(&other.join("main.pdf")), never).is_err());
        assert_eq!(fx.files(), before);
    }

    #[test]
    fn a_destination_given_by_the_caller_stays_inside_the_project() {
        let fx = ExportFixture::new("caller");
        let build = fx.build("main.typ");
        let before = fx.files();
        // Outside the project: only the dialog chooses that.
        let outside = fx.export(&build, "main.typ", Some(&fx.outside.join("out.pdf")), never).unwrap_err();
        assert!(outside.message.contains("outside the project"), "{outside}");
        // `..` out of the project is resolved before the check.
        let escaping = fx.project.join("chapters").join("..").join("..").join("escaped.pdf");
        assert!(fx.export(&build, "main.typ", Some(&escaping), never).is_err());
        // Exposition's own records, given or chosen.
        std::fs::create_dir_all(fx.project.join(".exposition")).unwrap();
        let before = {
            drop(before);
            fx.files()
        };
        let records = fx.project.join(".exposition").join("out.pdf");
        assert!(fx.export(&build, "main.typ", Some(&records), never).is_err());
        assert!(fx.export(&build, "main.typ", None, |_| Some(records.clone())).is_err());
        assert!(fx.export(&build, "main.typ", None, |_| Some(fx.data.join("out.pdf"))).is_err());
        assert!(!fx.data.join("out.pdf").exists());
        assert_eq!(fx.files(), before);

        // Inside the project it is written (and replaced by a later export).
        let inside = fx.project.join("chapters").join("main copy.pdf");
        let artifact = fx.export(&build, "main.typ", Some(&inside), never).unwrap();
        fx.export(&build, "main.typ", Some(&inside), never).unwrap();
        assert!(std::fs::read(&inside).unwrap().starts_with(b"%PDF-"));
        assert!(artifact.path.ends_with("main copy.pdf"), "{}", artifact.path);
    }

    #[test]
    fn a_refused_or_failed_export_keeps_the_existing_file() {
        let fx = ExportFixture::new("failure");
        let build = fx.build("main.typ");
        // A file that is not a PDF is never replaced, whatever the dialog confirmed.
        let notes = fx.outside.join("notes.pdf");
        std::fs::write(&notes, "my notes, not a PDF").unwrap();
        let refused = fx.export(&build, "main.typ", None, |_| Some(notes.clone())).unwrap_err();
        assert!(refused.message.contains("not a PDF"), "{refused}");
        assert_eq!(std::fs::read(&notes).unwrap(), b"my notes, not a PDF");
        // A wrong extension, a folder that does not exist, a device name.
        for unavailable in [
            fx.outside.join("out.txt"),
            fx.outside.join("missing").join("out.pdf"),
            fx.outside.join("NUL.pdf"),
            PathBuf::from("relative.pdf"),
        ] {
            let error = fx.export(&build, "main.typ", None, |_| Some(unavailable.clone())).unwrap_err();
            assert_eq!(error.code, "typesetting.export", "{}", unavailable.display());
        }
        assert!(!fx.outside.join("missing").exists(), "no folder is created");
        // The destination is open in another program: the old export stays.
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            let out = fx.outside.join("out.pdf");
            fx.export(&build, "main.typ", None, |_| Some(out.clone())).unwrap();
            let old = std::fs::read(&out).unwrap();
            let intro = fx.build("chapters/intro.typ");
            let held = std::fs::OpenOptions::new().read(true).share_mode(1).open(&out).unwrap();
            let busy = fx.export(&intro, "chapters/intro.typ", None, |_| Some(out.clone())).unwrap_err();
            assert!(busy.message.contains("another program"), "{busy}");
            assert!(busy.message.contains("The existing file was not changed."), "{busy}");
            drop(held);
            assert_eq!(std::fs::read(&out).unwrap(), old);
            let names: Vec<_> = std::fs::read_dir(&fx.outside)
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            assert!(names.iter().all(|n| !n.contains(".tmp")), "temporary files left: {names:?}");
        }
    }
}
