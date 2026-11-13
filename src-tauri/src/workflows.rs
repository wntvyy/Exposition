//! Workflow draft commands: the operation registry, workflow presets,
//! validation, saving (new immutable revisions admitted as `Workflow`
//! entities), strict imports, reads, and workflow bundles (export to / import
//! from a file the person chose).
//!
//! Drafts are validated in Rust against the strict contract and saved with
//! their diagnostics. Nothing here runs a workflow (see src/workflow_runs.rs).
//! A draft is reported runnable only when it is well-formed AND every step's
//! runner is available in the open project now (trust, restricted backend,
//! installed adapters); a stored runnability claim is never trusted. Imported
//! definitions are untrusted data (strict schema, size limits, fresh ids).
//! Bundles carry definitions only (never trust, approvals, or triggers). An
//! export goes to a file the person picks in the native save dialog (or a
//! given path) and never replaces anything but an earlier bundle; imported
//! bundles become drafts with fresh ids and rewritten composite references,
//! and nothing runs.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use exposition_core::path::WorkspacePath;
use exposition_graph::workflow::presets::{WorkflowPresetInfo, WorkflowPresetRequest};
use exposition_graph::workflow::{
    BUNDLE_SCHEMA, MAX_BUNDLE_BYTES, OperationContract, WorkflowBundleExport, WorkflowBundleImport, WorkflowDefinition,
    WorkflowValidation, bundle_text,
};
use exposition_graph::{
    DraftState, GraphService, Origin, SaveWorkflowRequest, SavedWorkflow, ValidationContext, WorkflowId,
    WorkflowSummary,
};
use exposition_workflow::WorkflowRunService;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::CommandError;
use crate::state::AppState;

async fn blocking<T, F>(state: &State<'_, AppState>, f: F) -> Result<T, CommandError>
where
    T: Send + 'static,
    F: FnOnce(&GraphService, Option<PathBuf>, &WorkflowRunService) -> Result<T, CommandError> + Send + 'static,
{
    let graph = Arc::clone(&state.graph);
    let runs = state.workflow_runs.clone();
    let root = state.workspace.open_root();
    tauri::async_runtime::spawn_blocking(move || f(&graph, root, &runs))
        .await
        .map_err(|e| CommandError::new("internal.task", format!("Workflow task failed: {e}")))?
}

/// Report runnability for the open project (well-formed AND every runner available).
fn with_runnability(runs: &WorkflowRunService, mut saved: SavedWorkflow) -> SavedWorkflow {
    runs.apply_runnability(&mut saved.validation, &saved.definition);
    saved
}

/// Whether a project file exists inside `root` (links leading outside count
/// as missing).
fn exists_in(root: &Option<PathBuf>, path: &WorkspacePath) -> bool {
    let Some(root) = root else { return false };
    let mut p = root.clone();
    for c in path.components() {
        p.push(c);
    }
    std::fs::canonicalize(&p).is_ok_and(|c| c.starts_with(root) && c.is_file())
}

/// The registered operation contracts (the only operations a step can name),
/// with each runner's availability in the open project.
#[tauri::command]
pub async fn workflow_operations(state: State<'_, AppState>) -> Result<Vec<OperationContract>, CommandError> {
    blocking(&state, |_, _, runs| Ok(runs.operations())).await
}

#[tauri::command]
pub async fn workflow_presets() -> Result<Vec<WorkflowPresetInfo>, CommandError> {
    Ok(exposition_graph::workflow::presets::catalog())
}

/// A new (unsaved) draft from a workflow preset.
#[tauri::command]
pub async fn workflow_from_preset(request: WorkflowPresetRequest) -> Result<WorkflowDefinition, CommandError> {
    exposition_graph::workflow::presets::draft(&request)
        .ok_or_else(|| CommandError::new("workflow.unknownPreset", "Unknown workflow preset."))
}

/// Validate a draft against the strict contract (files are checked in the
/// open project; composite steps against its saved revisions). `id`: the
/// saved workflow the draft edits, if any (a workflow cannot include itself).
#[tauri::command]
pub async fn workflow_validate(
    state: State<'_, AppState>,
    definition: WorkflowDefinition,
    id: Option<WorkflowId>,
) -> Result<WorkflowValidation, CommandError> {
    blocking(&state, move |graph, root, runs| {
        let exists = |p: &WorkspacePath| exists_in(&root, p);
        let resolve = |wf: &WorkflowId, revision: u64| graph.workflow_definition(wf, revision);
        let ctx = ValidationContext {
            ceiling: None,
            file_exists: Some(&exists),
            resolve_workflow: Some(&resolve),
            workflow_id: id.as_ref(),
        };
        let mut validation = exposition_graph::validate_workflow(&definition, &ctx);
        runs.apply_runnability(&mut validation, &definition);
        Ok(validation)
    })
    .await
}

/// Save a draft as a new revision (attributed to the user). Incomplete
/// drafts are saved with their diagnostics; saving never runs anything.
#[tauri::command]
pub async fn workflow_save(
    state: State<'_, AppState>,
    request: SaveWorkflowRequest,
) -> Result<SavedWorkflow, CommandError> {
    blocking(&state, move |graph, root, runs| {
        let exists = |p: &WorkspacePath| exists_in(&root, p);
        let ctx = ValidationContext { ceiling: None, file_exists: Some(&exists), ..Default::default() };
        let saved = graph.save_workflow(&Origin::User, &request, &ctx).map_err(CommandError::from)?;
        Ok(with_runnability(runs, saved))
    })
    .await
}

/// Import a definition from JSON text as a new draft (fresh id, never run).
#[tauri::command]
pub async fn workflow_import(state: State<'_, AppState>, text: String) -> Result<SavedWorkflow, CommandError> {
    blocking(&state, move |graph, root, runs| {
        let exists = |p: &WorkspacePath| exists_in(&root, p);
        let ctx = ValidationContext { ceiling: None, file_exists: Some(&exists), ..Default::default() };
        let saved = graph.import_workflow(&Origin::User, &text, &ctx).map_err(CommandError::from)?;
        Ok(with_runnability(runs, saved))
    })
    .await
}

#[tauri::command]
pub async fn workflow_list(state: State<'_, AppState>) -> Result<Vec<WorkflowSummary>, CommandError> {
    blocking(&state, |graph, _, runs| {
        let mut list = graph.workflows().map_err(CommandError::from)?;
        // Each runner is checked once for the whole list.
        let availability = runs.availability();
        let check = |id: &str| {
            availability.get(id).cloned().unwrap_or_else(|| Err("No runner is installed for this operation.".into()))
        };
        for summary in list.iter_mut().filter(|s| s.state == DraftState::WellFormed) {
            if let Ok(mut saved) = graph.workflow(&summary.id, Some(summary.revision)) {
                exposition_graph::workflow::apply_runnability(&mut saved.validation, &saved.definition, &check);
                summary.runnable = saved.validation.runnable;
            }
        }
        Ok(list)
    })
    .await
}

#[tauri::command]
pub async fn workflow_get(
    state: State<'_, AppState>,
    id: WorkflowId,
    revision: Option<u64>,
) -> Result<SavedWorkflow, CommandError> {
    blocking(&state, move |graph, _, runs| {
        let saved = graph.workflow(&id, revision).map_err(CommandError::from)?;
        Ok(with_runnability(runs, saved))
    })
    .await
}

// ----- workflow bundles (declarative interchange) -----

/// A `.json` file location the person chose (in a file dialog), checked
/// before anything is read or written: an absolute path, never inside the
/// project's `.exposition` metadata or the app's data folder, never a link
/// or a folder.
/// Shared with the run-manifest export (src/workflow_repro.rs).
pub(crate) fn chosen_file(raw: &str, root: &Option<PathBuf>, data_dir: &Path) -> Result<PathBuf, CommandError> {
    let refuse = |m: String| Err(CommandError::new("workflow.file", m));
    let path = PathBuf::from(raw.trim());
    if raw.trim().is_empty() || !path.is_absolute() {
        return refuse("Choose a file by its full location.".into());
    }
    let json = path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("json"));
    if !json {
        return refuse("Workflow files are .json files.".into());
    }
    let Some(name) = path.file_name() else { return refuse("Choose a file, not a folder.".into()) };
    let parent = path.parent().ok_or_else(|| CommandError::new("workflow.file", "Choose a file in a folder."))?;
    let parent = std::fs::canonicalize(parent)
        .map_err(|_| CommandError::new("workflow.file", format!("The folder of {raw} does not exist.")))?;
    let protected = |p: &Path| {
        let metadata = root.as_ref().map(|r| r.join(".exposition"));
        let data = std::fs::canonicalize(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
        metadata.is_some_and(|m| p.starts_with(m)) || p.starts_with(data)
    };
    if protected(&parent) {
        return refuse("That location holds Exposition's own records; choose another folder.".into());
    }
    let full = parent.join(name);
    if let Ok(meta) = std::fs::symlink_metadata(&full)
        && !meta.is_file()
    {
        return refuse(format!("{raw} is not a regular file."));
    }
    Ok(full)
}

/// Whether `bytes` are an earlier workflow bundle (the only file an export
/// may replace).
fn is_bundle(bytes: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .is_some_and(|v| v.get("schema").and_then(|s| s.as_str()) == Some(BUNDLE_SCHEMA))
}

/// Write an exported bundle to `target` (a checked [`chosen_file`]). Exports
/// never replace sources or other files: an existing destination is replaced
/// only when it is a regular file holding an earlier workflow bundle. The
/// write is atomic (a temporary file in the same folder, then a rename).
fn write_export(target: &Path, text: &str) -> Result<(), CommandError> {
    let invalid = |m: String| CommandError::new("workflow.invalidDestination", m);
    match std::fs::symlink_metadata(target) {
        Ok(meta) => {
            if !meta.is_file() {
                return Err(invalid("The destination exists and is not a regular file.".into()));
            }
            let earlier =
                meta.len() <= MAX_BUNDLE_BYTES as u64 && std::fs::read(target).is_ok_and(|bytes| is_bundle(&bytes));
            if !earlier {
                return Err(invalid(
                    "Refusing to replace a file that is not an Exposition workflow bundle; choose another name.".into(),
                ));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(invalid(format!("The destination cannot be checked: {e}"))),
    }
    exposition_workspace::fsutil::write_atomic(target, text.as_bytes())
        .map_err(|e| CommandError::new("workflow.exportFailed", format!("The workflow could not be written: {e}")))
}

/// A plain suggested file name for the save dialog.
fn file_name_for(title: &str) -> String {
    let stem: String = title
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(60)
        .collect();
    format!("{}.workflow.json", if stem.is_empty() { "workflow" } else { &stem })
}

/// Read a chosen interchange file (bounded before reading).
fn read_bounded(path: &Path) -> Result<String, CommandError> {
    let meta = std::fs::symlink_metadata(path)
        .map_err(|e| CommandError::new("workflow.file", format!("{} cannot be read: {e}", path.display())))?;
    if !meta.is_file() {
        return Err(CommandError::new("workflow.file", format!("{} is not a regular file.", path.display())));
    }
    if meta.len() > MAX_BUNDLE_BYTES as u64 {
        return Err(CommandError::new(
            "workflow.bundleInvalid",
            format!("The file is larger than {} MiB.", MAX_BUNDLE_BYTES / (1024 * 1024)),
        ));
    }
    let bytes = std::fs::read(path)
        .map_err(|e| CommandError::new("workflow.file", format!("{} cannot be read: {e}", path.display())))?;
    if bytes.len() > MAX_BUNDLE_BYTES {
        return Err(CommandError::new("workflow.bundleInvalid", "The file grew while it was read."));
    }
    String::from_utf8(bytes).map_err(|_| CommandError::new("workflow.bundleInvalid", "The file is not UTF-8 text."))
}

/// Export a saved workflow revision (the latest when `revision` is none) and
/// every composite child it runs as a workflow bundle. `path` none: the
/// person chooses the file in the save dialog (`workflow.exportCanceled` when
/// dismissed). The destination is an absolute `.json` path outside the
/// project's records and the app data folder; an existing file is replaced
/// only when it is an earlier workflow bundle (`workflow.invalidDestination`
/// otherwise). Definitions only: no trust, approvals, triggers, or results.
#[tauri::command]
pub async fn workflow_bundle_export(
    app: AppHandle,
    state: State<'_, AppState>,
    id: WorkflowId,
    revision: Option<u64>,
    path: Option<String>,
) -> Result<WorkflowBundleExport, CommandError> {
    let data_dir = state.data_dir.clone();
    blocking(&state, move |graph, root, _| {
        let bundle = graph.export_workflow_bundle(&id, revision).map_err(CommandError::from)?;
        let text = bundle_text(&bundle).map_err(|m| CommandError::new("workflow.exportInvalid", m))?;
        let raw = match path {
            Some(path) => path,
            None => app
                .dialog()
                .file()
                .set_title("Export workflow")
                .add_filter("Workflow bundle", &["json"])
                .set_file_name(file_name_for(&bundle.root.definition.title))
                .blocking_save_file()
                .and_then(|picked| picked.as_path().map(|p| p.display().to_string()))
                .ok_or_else(|| CommandError::new("workflow.exportCanceled", "The export was canceled."))?,
        };
        let target = chosen_file(&raw, &root, &data_dir)?;
        write_export(&target, &text)?;
        Ok(WorkflowBundleExport { path: raw, bytes: text.len() as u64, children: bundle.children.len() as u32 })
    })
    .await
}

/// Import a workflow bundle (or a single definition) from a `.json` file the
/// person chose: untrusted data, strictly parsed and bounded; children are
/// saved first as imported drafts with fresh ids and the references rewritten.
/// Nothing is trusted, approved, triggered, or run.
#[tauri::command]
pub async fn workflow_bundle_import(
    state: State<'_, AppState>,
    path: String,
) -> Result<WorkflowBundleImport, CommandError> {
    let data_dir = state.data_dir.clone();
    blocking(&state, move |graph, root, runs| {
        let source = chosen_file(&path, &root, &data_dir)?;
        let text = read_bounded(&source)?;
        let exists = |p: &WorkspacePath| exists_in(&root, p);
        let ctx = ValidationContext { ceiling: None, file_exists: Some(&exists), ..Default::default() };
        let mut out = graph.import_workflow_bundle(&Origin::User, &text, &ctx).map_err(CommandError::from)?;
        out.root = with_runnability(runs, out.root);
        Ok(out)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique temporary folder removed on drop (no dev-dependencies here).
    struct Temp(PathBuf);

    impl Temp {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    mod tempfile {
        pub fn tempdir() -> std::io::Result<super::Temp> {
            use std::sync::atomic::{AtomicU64, Ordering};
            static N: AtomicU64 = AtomicU64::new(0);
            let nonce =
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
            let p = std::env::temp_dir().join(format!(
                "exposition-workflows-test-{}-{nonce}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::create_dir_all(&p)?;
            Ok(super::Temp(std::fs::canonicalize(&p)?))
        }
    }

    #[test]
    fn chosen_files_are_absolute_json_files_outside_exposition_records() {
        let data = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let root = Some(std::fs::canonicalize(project.path()).unwrap());
        let meta = project.path().join(".exposition");
        std::fs::create_dir_all(&meta).unwrap();
        let out = tempfile::tempdir().unwrap();
        let ok = out.path().join("flow.json");
        let chosen = chosen_file(ok.to_str().unwrap(), &root, data.path()).unwrap();
        assert_eq!(chosen, std::fs::canonicalize(out.path()).unwrap().join("flow.json"));
        for bad in [
            String::new(),
            "flow.json".to_owned(),
            out.path().join("flow.txt").to_string_lossy().into_owned(),
            out.path().join("missing").join("flow.json").to_string_lossy().into_owned(),
            meta.join("graph.json").to_string_lossy().into_owned(),
            data.path().join("settings.json").to_string_lossy().into_owned(),
        ] {
            assert!(chosen_file(&bad, &root, data.path()).is_err(), "{bad:?}");
        }
        // A folder named like a JSON file is not a file.
        let dir = out.path().join("folder.json");
        std::fs::create_dir(&dir).unwrap();
        assert!(chosen_file(dir.to_str().unwrap(), &root, data.path()).is_err());
    }

    const EARLIER_BUNDLE: &str = r#"{"schema":"exposition.workflow-bundle/1","root":{},"children":[]}"#;

    #[test]
    fn export_never_replaces_unrelated_files() {
        let out = tempfile::tempdir().unwrap();
        for (name, content) in [
            ("settings.json", r#"{"editor.fontSize": 12}"#),
            ("package.json", r#"{"name":"x","schema":"exposition.workflow-bundle/2"}"#),
            ("definition.json", r#"{"schema":"exposition.workflow/1","title":"x"}"#),
            ("nested.json", r#"{"other":{"schema":"exposition.workflow-bundle/1"}}"#),
            ("notes.json", "not json at all"),
            ("empty.json", ""),
        ] {
            let path = out.path().join(name);
            std::fs::write(&path, content).unwrap();
            let err = write_export(&path, "{}").expect_err(name);
            assert_eq!(err.code, "workflow.invalidDestination", "{name}: {}", err.message);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), content, "{name} was changed");
        }
    }

    #[test]
    fn export_replaces_an_earlier_bundle_or_creates_a_new_file() {
        let out = tempfile::tempdir().unwrap();
        let earlier = out.path().join("flow.json");
        std::fs::write(&earlier, EARLIER_BUNDLE).unwrap();
        write_export(&earlier, "{\"replaced\":1}").unwrap();
        assert_eq!(std::fs::read_to_string(&earlier).unwrap(), "{\"replaced\":1}");
        let fresh = out.path().join("new.json");
        write_export(&fresh, EARLIER_BUNDLE).unwrap();
        assert_eq!(std::fs::read_to_string(&fresh).unwrap(), EARLIER_BUNDLE);
        assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 2, "no temporary file left behind");
    }

    #[test]
    fn suggested_file_names_are_plain() {
        assert_eq!(file_name_for("Numerical experiment"), "Numerical-experiment.workflow.json");
        assert_eq!(file_name_for("../x:y"), "x-y.workflow.json");
        assert_eq!(file_name_for("///"), "workflow.workflow.json");
    }

    #[test]
    fn reads_are_bounded() {
        let out = tempfile::tempdir().unwrap();
        let path = out.path().join("flow.json");
        std::fs::write(&path, "{\"new\":true}").unwrap();
        assert_eq!(read_bounded(&path).unwrap(), "{\"new\":true}");
        let big = out.path().join("big.json");
        std::fs::write(&big, vec![b' '; MAX_BUNDLE_BYTES + 1]).unwrap();
        assert_eq!(read_bounded(&big).unwrap_err().code, "workflow.bundleInvalid");
        let binary = out.path().join("bin.json");
        std::fs::write(&binary, [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(read_bounded(&binary).unwrap_err().code, "workflow.bundleInvalid");
    }
}
