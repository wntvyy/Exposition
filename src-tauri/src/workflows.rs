//! Workflow draft commands: the operation registry, workflow presets,
//! validation, saving (new immutable revisions admitted as `Workflow`
//! entities), strict imports, and reads.
//!
//! Drafts are validated in Rust against the strict contract and saved with
//! their diagnostics. Nothing here runs a workflow (see src/workflow_runs.rs).
//! A draft is reported runnable only when it is well-formed AND every step's
//! runner is available in the open project now (trust, restricted backend,
//! installed adapters); a stored runnability claim is never trusted. Imported
//! definitions are untrusted data (strict schema, size limits, fresh ids).

use std::path::PathBuf;
use std::sync::Arc;

use exposition_core::path::WorkspacePath;
use exposition_graph::workflow::presets::{WorkflowPresetInfo, WorkflowPresetRequest};
use exposition_graph::workflow::{OperationContract, WorkflowDefinition, WorkflowValidation};
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

/// Validate a draft against the strict contract (files are checked in the open project).
#[tauri::command]
pub async fn workflow_validate(
    state: State<'_, AppState>,
    definition: WorkflowDefinition,
) -> Result<WorkflowValidation, CommandError> {
    blocking(&state, move |_, root, runs| {
        let exists = |p: &WorkspacePath| exists_in(&root, p);
        let ctx = ValidationContext { ceiling: None, file_exists: Some(&exists) };
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
        let ctx = ValidationContext { ceiling: None, file_exists: Some(&exists) };
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
        let ctx = ValidationContext { ceiling: None, file_exists: Some(&exists) };
        let saved = graph.import_workflow(&Origin::User, &text, &ctx).map_err(CommandError::from)?;
        Ok(with_runnability(runs, saved))
    })
    .await
}

#[tauri::command]
pub async fn workflow_list(state: State<'_, AppState>) -> Result<Vec<WorkflowSummary>, CommandError> {
    blocking(&state, |graph, _, runs| {
        let mut list = graph.workflows().map_err(CommandError::from)?;
        for summary in list.iter_mut().filter(|s| s.state == DraftState::WellFormed) {
            if let Ok(saved) = graph.workflow(&summary.id, Some(summary.revision)) {
                summary.runnable = with_runnability(runs, saved).validation.runnable;
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
