//! Explorer file actions (QOL E/G): New File, Rename, and importing files
//! dropped from the operating system.
//!
//! - `file_create`: an empty file under a validated name in an existing
//!   folder, never replacing anything (workspace service).
//! - `file_rename_check` / `file_rename`: the workspace service validates and
//!   renames within the folder (never replacing; case-only changes allowed);
//!   open documents keep their identity, text, undo and recovery under the
//!   new path. The move and the project graph's record of it are ONE
//!   serialized step (`GraphService::reconcile_after` with both paths), so
//!   the file resource keeps its identity, curated relations and bindings,
//!   and no watcher pass can record the old path as removed in between. When
//!   the graph cannot record it, the outcome says so (`GraphRecording::Pending`),
//!   paths stay marked pending, and the record is retried (a reconciliation
//!   of the actual files: the rename itself is never repeated). Uses of the
//!   old name elsewhere (manuscript includes, workflow inputs, retained
//!   evidence, the project entry and build targets, triggers) are reported,
//!   never rewritten. Historical run and evidence records keep the paths they
//!   recorded.
//! - `file_import_start` / `file_import_cancel`: copies of dropped files into
//!   a project folder through the P1 orchestrator (paste of `External`
//!   sources: link/junction and protected-location refusals, collision-free
//!   names, journaled, graph indexing of each copy as a fresh identity), in
//!   small batches with progress events and cancellation between batches.
//!   Bound to the project that was open when the files were dropped: a batch
//!   never starts in another project (the rest is reported as skipped), and a
//!   plan made before a switch is refused at apply. A retry (`retry_of`) never
//!   copies a source its earlier attempt already imported.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use exposition_core::ops::{OpActor, OpApply, OpDestination, OpKind, OpRequest, OpStatus, OpTarget};
use exposition_core::path::WorkspacePath;
use exposition_graph::reconcile::{ReconcileOptions, Scope};
use exposition_workspace::explorer::{
    FileCreated, FileImportItem, FileImportProgress, FileImportRequest, FileRenamed, GraphRecording, PathReference,
    RenameCheck,
};
use exposition_workspace::root::display_path;
use tauri::{AppHandle, Emitter, Manager};

use crate::CommandError;
use crate::project_ops::{AppData, PROJECT_OP_EVENT, ProjectOps};
use crate::state::AppState;

/// Progress/result of an import (payload [`FileImportProgress`]).
pub const FILE_IMPORT_EVENT: &str = "file-import";
/// Sources per import (the orchestrator's target limit).
const MAX_SOURCES: usize = 1000;
/// Sources per plan/apply step (progress and cancellation granularity).
const BATCH: usize = 4;
/// Imports remembered for retries (oldest dropped first).
const MAX_LEDGERS: usize = 64;

/// Debug builds: the next rename's graph record fails (tests the pending path).
static FAIL_NEXT_GRAPH_RECORD: AtomicBool = AtomicBool::new(false);

fn err(code: &str, message: impl Into<String>) -> CommandError {
    CommandError::new(code, message)
}

async fn blocking<T, F>(app: AppHandle, f: F) -> Result<T, CommandError>
where
    T: Send + 'static,
    F: FnOnce(&AppState) -> Result<T, CommandError> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(move || f(&app.state::<AppState>()))
        .await
        .map_err(|e| err("internal.task", format!("The file operation stopped unexpectedly: {e}")))?
}

// ------------------------------------------------------------ New File ----

#[tauri::command]
pub async fn file_create(
    app: AppHandle,
    folder: Option<WorkspacePath>,
    name: String,
) -> Result<FileCreated, CommandError> {
    blocking(app, move |state| Ok(FileCreated { path: state.workspace.create_file(folder.as_ref(), &name)? })).await
}

// -------------------------------------------------------------- Rename ----

fn quoted(s: &str) -> String {
    format!("“{s}”")
}

/// What still uses `path` by that name (nothing is changed). `None` in the
/// second slot: complete as far as the services know.
fn references(state: &AppState, path: &WorkspacePath) -> (Vec<PathReference>, Option<String>) {
    let mut out: Vec<PathReference> = Vec::new();
    let mut unknown = None;
    let name = quoted(path.file_name());
    if state.graph.status().is_usable() {
        match state.graph.path_impacts(std::slice::from_ref(path)) {
            Ok(impacts) => {
                use exposition_core::ops::ImpactKind;
                for impact in impacts {
                    for consumer in impact.consumers {
                        let (kind, message) = match impact.kind {
                            ImpactKind::ManuscriptDependency => (
                                "manuscriptDependency",
                                format!(
                                    "Uses {name} by its current name (an include, input, image or bibliography); after the rename it shows it missing until you update the reference."
                                ),
                            ),
                            ImpactKind::WorkflowInput => (
                                "workflowInput",
                                format!(
                                    "Takes {name} as an input by its current path; choose the renamed file in the workflow."
                                ),
                            ),
                            ImpactKind::RetainedEvidence => (
                                "retainedEvidence",
                                format!("Records {name} at its current path; the record keeps that path as history."),
                            ),
                            _ => continue,
                        };
                        out.push(PathReference { kind: kind.into(), label: consumer.label, message });
                    }
                }
            }
            Err(e) => unknown = Some(format!("The project graph could not be read: {}", e.message)),
        }
    } else {
        unknown = Some("The project graph is not available, so uses in manuscripts and workflows are unknown.".into());
    }
    if let Ok(settings) = state.workspace.project_settings() {
        let under = |entry: &WorkspacePath| {
            entry == path || entry.as_str().strip_prefix(path.as_str()).is_some_and(|rest| rest.starts_with('/'))
        };
        if settings.settings.entry.as_ref().is_some_and(under) {
            out.push(PathReference {
                kind: "projectEntry".into(),
                label: "Project settings".into(),
                message: format!("The project's main document is {name}; change it in the project settings."),
            });
        }
        for target in settings.settings.targets.iter().filter(|t| under(&t.entry)) {
            out.push(PathReference {
                kind: "buildTarget".into(),
                label: format!("Build target {}", quoted(&target.name)),
                message: format!("Builds {name}; change its entry in the project settings."),
            });
        }
    }
    if let Ok(triggers) = state.workflow_triggers.try_watching(std::slice::from_ref(path)) {
        for t in triggers {
            out.push(PathReference {
                kind: "trigger".into(),
                label: format!("Trigger of {}", quoted(&t.title)),
                message: format!("Watches {name}; after the rename it reports the input missing."),
            });
        }
    }
    (out, unknown)
}

#[tauri::command]
pub async fn file_rename_check(app: AppHandle, path: WorkspacePath) -> Result<RenameCheck, CommandError> {
    blocking(app, move |state| {
        let root = state.workspace.open_root().ok_or_else(|| err("workspace.notOpen", "No folder is open."))?;
        let abs = path.components().fold(root, |p, c| p.join(c));
        let meta =
            std::fs::symlink_metadata(&abs).map_err(|_| err("path.notFound", format!("{path} does not exist.")))?;
        let (references, unknown) = references(state, &path);
        Ok(RenameCheck { path, is_dir: meta.is_dir(), references, unknown })
    })
    .await
}

/// Retry recording a rename the graph could not record at once: a
/// reconciliation of both paths from the files (the rename is not repeated).
fn retry_graph_record(app: AppHandle, paths: Vec<WorkspacePath>, root: PathBuf) {
    std::thread::spawn(move || {
        for attempt in 0..3u32 {
            std::thread::sleep(Duration::from_millis(600 * u64::from(attempt + 1)));
            let state = app.state::<AppState>();
            if state.workspace.open_root().as_deref() != Some(root.as_path()) || !state.graph.status().is_usable() {
                return;
            }
            let (_, result) = state
                .graph
                .reconcile_after(|| ((), Some(ReconcileOptions { scope: Scope::Paths(paths.clone()), force: false })));
            if matches!(result, Some(Ok(_))) {
                return;
            }
        }
    });
}

#[tauri::command]
pub async fn file_rename(app: AppHandle, path: WorkspacePath, name: String) -> Result<FileRenamed, CommandError> {
    let handle = app.clone();
    blocking(app, move |state| {
        let ws = &state.workspace;
        let plan = ws.plan_rename(&path, &name)?;
        let (references, _) = references(state, &plan.from);
        let graph = &state.graph;
        let usable = graph.status().is_usable();
        let paths = vec![plan.from.clone(), plan.to.clone()];
        if usable {
            let _ = graph.mark_pending(&paths);
        }
        let inject = cfg!(debug_assertions) && FAIL_NEXT_GRAPH_RECORD.swap(false, Ordering::SeqCst);
        let (renamed, recorded) = graph.reconcile_after(|| {
            let renamed = ws.apply_rename(&plan);
            let scope = (renamed.is_ok() && usable && !inject)
                .then(|| ReconcileOptions { scope: Scope::Paths(paths.clone()), force: false });
            (renamed, scope)
        });
        let renamed = renamed?;
        // Overlays of unsaved buffers were computed under the old paths.
        for doc in &renamed.documents {
            if let Some(new) = &doc.path
                && let Some(suffix) = new.as_str().get(renamed.to.as_str().len()..)
                && let Ok(old) = WorkspacePath::parse(&format!("{}{suffix}", renamed.from.as_str()))
            {
                graph.clear_overlay(&old);
            }
        }
        let graph_state = match recorded {
            _ if !usable => GraphRecording::Unavailable,
            Some(Ok(report)) => GraphRecording::Recorded { renamed: report.resources_renamed },
            Some(Err(e)) => GraphRecording::Pending { message: e.message },
            None => GraphRecording::Pending { message: "the graph write failed (injected test failure)".into() },
        };
        if matches!(graph_state, GraphRecording::Pending { .. })
            && let Some(root) = ws.open_root()
        {
            retry_graph_record(handle.clone(), paths, root);
        }
        Ok(FileRenamed {
            from: renamed.from,
            to: renamed.to,
            is_dir: renamed.is_dir,
            documents: renamed.documents,
            graph: graph_state,
            references,
        })
    })
    .await
}

/// Debug builds only (tests): the next rename's graph record fails, to
/// exercise the pending-and-retry path in the real application.
#[cfg(debug_assertions)]
#[tauri::command]
pub async fn files_dev_fail_next_graph_record() -> Result<(), CommandError> {
    FAIL_NEXT_GRAPH_RECORD.store(true, Ordering::SeqCst);
    Ok(())
}

/// Grant the debug-only command (never present in release builds).
#[cfg(debug_assertions)]
pub fn add_dev_capability(app: &AppHandle) -> tauri::Result<()> {
    use tauri::ipc::CapabilityBuilder;
    app.add_capability(
        CapabilityBuilder::new("dev-files-faults").window("main").permission("allow-files-dev-fail-next-graph-record"),
    )
}

// -------------------------------------------------------------- Import ----

struct Job {
    cancel: AtomicBool,
}

/// Running imports and what each import already copied (for retries).
#[derive(Default)]
pub struct Imports {
    jobs: Mutex<HashMap<String, Arc<Job>>>,
    /// import id → (source key → created path), oldest first.
    ledgers: Mutex<Vec<(String, BTreeMap<String, WorkspacePath>)>>,
    counter: AtomicU64,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Key of a dropped source (case-insensitive on Windows).
fn source_key(source: &str) -> String {
    let canonical = std::fs::canonicalize(source).map(|p| display_path(&p)).unwrap_or_else(|_| source.to_owned());
    if cfg!(windows) { canonical.to_lowercase() } else { canonical }
}

impl Imports {
    fn imported(&self, import: &str, key: &str) -> Option<WorkspacePath> {
        lock(&self.ledgers).iter().find(|(id, _)| id == import).and_then(|(_, l)| l.get(key).cloned())
    }

    fn record(&self, import: &str, key: String, created: WorkspacePath) {
        let mut ledgers = lock(&self.ledgers);
        if let Some((_, l)) = ledgers.iter_mut().find(|(id, _)| id == import) {
            l.insert(key, created);
            return;
        }
        if ledgers.len() >= MAX_LEDGERS {
            ledgers.remove(0);
        }
        ledgers.push((import.to_owned(), BTreeMap::from([(key, created)])));
    }
}

fn skipped(source: &str, code: &str, message: impl Into<String>) -> FileImportItem {
    FileImportItem {
        source: source.to_owned(),
        status: OpStatus::Skipped,
        created: None,
        code: Some(code.into()),
        message: Some(message.into()),
    }
}

#[tauri::command]
pub async fn file_import_start(app: AppHandle, request: FileImportRequest) -> Result<String, CommandError> {
    if request.sources.is_empty() {
        return Err(err("import.empty", "Nothing was dropped."));
    }
    if request.sources.len() > MAX_SOURCES {
        return Err(err("import.tooMany", format!("At most {MAX_SOURCES} items can be imported at once.")));
    }
    let state = app.state::<AppState>();
    let root = state.workspace.open_root().ok_or_else(|| err("workspace.notOpen", "No folder is open."))?;
    if display_path(&root) != request.root {
        return Err(err("import.projectChanged", "The files were dropped on a project that is no longer open."));
    }
    let imports = app.state::<Imports>();
    let n = imports.counter.fetch_add(1, Ordering::Relaxed);
    let import = request
        .retry_of
        .clone()
        .unwrap_or_else(|| format!("import-{:x}-{n}", exposition_workspace::fsutil::now_millis()));
    let job = Arc::new(Job { cancel: AtomicBool::new(false) });
    {
        let mut jobs = lock(&imports.jobs);
        if jobs.contains_key(&import) {
            return Err(err("import.running", "This import is still running."));
        }
        jobs.insert(import.clone(), Arc::clone(&job));
    }
    let id = import.clone();
    let handle = app.clone();
    std::thread::spawn(move || {
        run_import(&handle, &id, &request, root, &job);
        lock(&handle.state::<Imports>().jobs).remove(&id);
    });
    Ok(import)
}

#[tauri::command]
pub async fn file_import_cancel(app: AppHandle, import: String) -> Result<bool, CommandError> {
    let imports = app.state::<Imports>();
    let job = lock(&imports.jobs).get(&import).cloned();
    Ok(match job {
        Some(job) => {
            job.cancel.store(true, Ordering::SeqCst);
            true
        }
        None => false,
    })
}

fn run_import(app: &AppHandle, import: &str, request: &FileImportRequest, root: PathBuf, job: &Job) {
    let state = app.state::<AppState>();
    let ops = app.state::<ProjectOps>();
    let imports = app.state::<Imports>();
    let total = request.sources.len() as u32;
    let mut items: Vec<FileImportItem> = Vec::new();
    let progress = |items: &[FileImportItem], finished: bool, canceled: bool| FileImportProgress {
        import: import.to_owned(),
        root: request.root.clone(),
        folder: request.folder.clone(),
        total,
        done: items.len() as u32,
        finished,
        canceled,
        items: items.to_vec(),
    };
    let _ = app.emit(FILE_IMPORT_EVENT, progress(&items, false, false));
    let mut canceled = false;
    for batch in request.sources.chunks(BATCH) {
        if job.cancel.load(Ordering::SeqCst) {
            canceled = true;
        }
        if canceled {
            items.extend(batch.iter().map(|s| skipped(s, "import.canceled", "Not imported: the import was canceled.")));
            continue;
        }
        if state.workspace.open_root().as_deref() != Some(root.as_path()) {
            items.extend(batch.iter().map(|s| {
                skipped(s, "import.projectChanged", "Not imported: another project was opened before it was copied.")
            }));
            continue;
        }
        // Sources an earlier attempt of this import already copied are reported, not copied again.
        let mut todo: Vec<&String> = Vec::new();
        for source in batch {
            match imports.imported(import, &source_key(source)) {
                Some(created) => items.push(FileImportItem {
                    source: source.clone(),
                    status: OpStatus::Skipped,
                    message: Some(format!("Already imported as {created}.")),
                    created: Some(created),
                    code: Some("import.alreadyImported".into()),
                }),
                None => todo.push(source),
            }
        }
        if todo.is_empty() {
            let _ = app.emit(FILE_IMPORT_EVENT, progress(&items, false, false));
            continue;
        }
        let request_op = OpRequest {
            kind: OpKind::Paste { destination: OpDestination::Folder { folder: request.folder.clone() } },
            targets: todo.iter().map(|s| OpTarget::External { path: (*s).clone() }).collect(),
            actor: OpActor::User,
        };
        let data = AppData(&state);
        let applied = ops.plan(&state.workspace, &data, request_op).and_then(|plan| {
            // Planned in the project the files were dropped on, or not at all.
            if state.workspace.open_root().as_deref() != Some(root.as_path()) {
                return Err(exposition_core::ops::OpError::new(
                    "import.projectChanged",
                    "Not imported: another project was opened before it was copied.",
                ));
            }
            let apply =
                OpApply { plan: plan.plan.clone(), plan_hash: plan.plan_hash.clone(), choices: BTreeMap::new() };
            ops.apply(&state.workspace, &data, apply)
        });
        match applied {
            Ok(result) => {
                let _ = app.emit(PROJECT_OP_EVENT, &result);
                for source in &todo {
                    let outcome = result
                        .outcomes
                        .iter()
                        .find(|o| matches!(&o.target, OpTarget::External { path } if path == *source));
                    let item = match outcome {
                        Some(o) => {
                            let created = match &o.created {
                                Some(OpTarget::Path { path }) => Some(path.clone()),
                                _ => None,
                            };
                            if o.status == OpStatus::Succeeded
                                && let Some(created) = &created
                            {
                                imports.record(import, source_key(source), created.clone());
                            }
                            FileImportItem {
                                source: (*source).clone(),
                                status: o.status,
                                created,
                                code: o.code.clone(),
                                message: o.message.clone(),
                            }
                        }
                        None => skipped(source, "import.unknown", "Not imported."),
                    };
                    items.push(item);
                }
            }
            Err(e) => {
                let status = if e.code == "import.projectChanged" || e.code == "ops.planStale" {
                    OpStatus::Skipped
                } else {
                    OpStatus::Failed
                };
                items.extend(todo.iter().map(|s| FileImportItem {
                    source: (*s).clone(),
                    status,
                    created: None,
                    code: Some(e.code.clone()),
                    message: Some(e.message.clone()),
                }));
            }
        }
        let _ = app.emit(FILE_IMPORT_EVENT, progress(&items, false, false));
    }
    let _ = app.emit(FILE_IMPORT_EVENT, progress(&items, true, canceled));
}

/// Manage the import registry.
pub fn start(app: &AppHandle) {
    app.manage(Imports::default());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_import_remembers_what_it_copied_so_a_retry_never_copies_it_again() {
        let imports = Imports::default();
        let dir = std::env::temp_dir().join(format!("xp-import-ledger-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("a.csv");
        std::fs::write(&source, "x\n").unwrap();
        let raw = source.display().to_string();
        assert_eq!(imports.imported("import-1", &source_key(&raw)), None);
        imports.record("import-1", source_key(&raw), WorkspacePath::parse("data/a.csv").unwrap());
        // The same source, spelled differently (case on Windows), is the same item.
        let spelled = if cfg!(windows) { raw.to_uppercase() } else { raw.clone() };
        assert_eq!(imports.imported("import-1", &source_key(&spelled)).unwrap().as_str(), "data/a.csv");
        // Another import is a new request (its copies get fresh names).
        assert_eq!(imports.imported("import-2", &source_key(&raw)), None);
        // Bounded: the oldest imports are forgotten first.
        for i in 0..MAX_LEDGERS {
            imports.record(&format!("other-{i}"), "k".into(), WorkspacePath::parse("x").unwrap());
        }
        assert_eq!(imports.imported("import-1", &source_key(&raw)), None);
        assert!(imports.imported(&format!("other-{}", MAX_LEDGERS - 1), "k").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
