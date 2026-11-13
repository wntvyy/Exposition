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
//! Bundles carry definitions only (never trust, approvals, or triggers);
//! imported bundles become drafts with fresh ids and rewritten composite
//! references, and nothing runs.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use exposition_core::path::WorkspacePath;
use exposition_graph::workflow::presets::{WorkflowPresetInfo, WorkflowPresetRequest};
use exposition_graph::workflow::{
    MAX_BUNDLE_BYTES, OperationContract, WorkflowBundleExport, WorkflowBundleImport, WorkflowDefinition,
    WorkflowValidation, bundle_text,
};
use exposition_graph::{
    DraftState, GraphService, Origin, SaveWorkflowRequest, SavedWorkflow, ValidationContext, WorkflowId,
    WorkflowSummary,
};
use exposition_workflow::WorkflowRunService;
use tauri::State;

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
fn chosen_file(raw: &str, root: &Option<PathBuf>, data_dir: &Path) -> Result<PathBuf, CommandError> {
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

/// Write `text` to `path` through a temporary file in the same folder, so a
/// failed export never leaves a half-written file in place.
fn write_replacing(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let parent = path.parent().unwrap_or(Path::new("."));
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("workflow.json");
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let temp = parent.join(format!(".{name}.{}-{nonce}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
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
/// every composite child it runs as a workflow bundle, to a `.json` file the
/// person chose. Definitions only: no trust, approvals, triggers, or results.
#[tauri::command]
pub async fn workflow_bundle_export(
    state: State<'_, AppState>,
    id: WorkflowId,
    revision: Option<u64>,
    path: String,
) -> Result<WorkflowBundleExport, CommandError> {
    let data_dir = state.data_dir.clone();
    blocking(&state, move |graph, root, _| {
        let target = chosen_file(&path, &root, &data_dir)?;
        let bundle = graph.export_workflow_bundle(&id, revision).map_err(CommandError::from)?;
        let text = bundle_text(&bundle).map_err(|m| CommandError::new("workflow.exportInvalid", m))?;
        write_replacing(&target, &text).map_err(|e| {
            CommandError::new("workflow.file", format!("The workflow could not be written to {path}: {e}"))
        })?;
        Ok(WorkflowBundleExport { path, bytes: text.len() as u64, children: bundle.children.len() as u32 })
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

    #[test]
    fn writes_replace_through_a_temporary_file_and_reads_are_bounded() {
        let out = tempfile::tempdir().unwrap();
        let path = out.path().join("flow.json");
        std::fs::write(&path, "old").unwrap();
        write_replacing(&path, "{\"new\":true}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"new\":true}");
        let left: Vec<_> = std::fs::read_dir(out.path()).unwrap().collect();
        assert_eq!(left.len(), 1, "no temporary file left behind");
        assert_eq!(read_bounded(&path).unwrap(), "{\"new\":true}");
        let big = out.path().join("big.json");
        std::fs::write(&big, vec![b' '; MAX_BUNDLE_BYTES + 1]).unwrap();
        assert_eq!(read_bounded(&big).unwrap_err().code, "workflow.bundleInvalid");
        let binary = out.path().join("bin.json");
        std::fs::write(&binary, [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(read_bounded(&binary).unwrap_err().code, "workflow.bundleInvalid");
    }
}
