//! Exact-page editing commands.
//!
//! Geometry queries name the build whose page map they use; offsets are UTF-16
//! positions in that build's snapshot of the file. Edits go through the
//! document service as `EditOrigin::Page` transactions.

use std::sync::Arc;

use exposition_core::document::TransactionError;
use exposition_core::path::WorkspacePath;
use exposition_page::map::Affinity;
use exposition_page::{
    CaretGeometry, Motion, NavigationResult, PageEditOutcome, PageEditRequest, PageHit, PageMap, SelectionGeometry,
    TypstInput, apply_page_edit,
};
use tauri::State;

use crate::error::CommandError;
use crate::state::AppState;

fn map_of(state: &State<'_, AppState>, build: &str) -> Result<Arc<PageMap>, CommandError> {
    let record = state
        .typesetting
        .service
        .record(build)
        .ok_or_else(|| CommandError::new("page.unknownBuild", "the build is no longer available"))?;
    record.map.clone().ok_or_else(|| CommandError::new("page.noMap", "the build has no page map"))
}

fn finite(values: &[f64]) -> Result<(), CommandError> {
    if values.iter().all(|v| v.is_finite()) {
        Ok(())
    } else {
        Err(CommandError::new("page.invalidPoint", "coordinates must be finite numbers"))
    }
}

/// Resolve a point on a page (points from the top-left) to a caret position.
#[tauri::command]
pub async fn page_hit_test(
    state: State<'_, AppState>,
    build: String,
    page: u32,
    x: f64,
    y: f64,
) -> Result<PageHit, CommandError> {
    finite(&[x, y])?;
    Ok(map_of(&state, &build)?.hit_test(page, x, y))
}

#[tauri::command]
pub async fn page_caret(
    state: State<'_, AppState>,
    build: String,
    path: WorkspacePath,
    offset: u32,
    affinity: Affinity,
) -> Result<Option<CaretGeometry>, CommandError> {
    Ok(map_of(&state, &build)?.caret_geometry(&path, offset, affinity))
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn page_navigate(
    state: State<'_, AppState>,
    build: String,
    path: WorkspacePath,
    offset: u32,
    affinity: Affinity,
    motion: Motion,
    goal_x: Option<f64>,
) -> Result<Option<NavigationResult>, CommandError> {
    if let Some(x) = goal_x {
        finite(&[x])?;
    }
    Ok(map_of(&state, &build)?.navigate(&path, offset, affinity, motion, goal_x))
}

/// Highlight rectangles and rendered text of a source range.
#[tauri::command]
pub async fn page_selection(
    state: State<'_, AppState>,
    build: String,
    path: WorkspacePath,
    start: u32,
    end: u32,
) -> Result<SelectionGeometry, CommandError> {
    Ok(map_of(&state, &build)?.selection(&path, start, end))
}

/// Apply typing, deletion, or a paragraph break at a caret/selection.
#[tauri::command]
pub async fn page_edit(
    state: State<'_, AppState>,
    request: PageEditRequest,
) -> Result<PageEditOutcome, TransactionError> {
    let service = &state.typesetting.service;
    apply_page_edit(&state.documents, &TypstInput, &request, &|document, revision| {
        service.revision_text(document, revision)
    })
}

/// DEBUG BUILDS ONLY — remove at integration once the workspace service opens
/// projects and files. Opens `path` (an absolute `.typ` file) as a document,
/// using its folder as the typesetting workspace.
#[cfg(debug_assertions)]
#[tauri::command]
pub async fn dev_open_document(
    state: State<'_, AppState>,
    path: String,
) -> Result<exposition_core::document::DocumentInfo, CommandError> {
    use exposition_core::document::TextEncoding;
    use exposition_workspace::OpenDocument;

    let file = std::path::PathBuf::from(&path);
    if !file.is_absolute() || file.extension().and_then(|e| e.to_str()) != Some("typ") {
        return Err(CommandError::new("dev.invalidPath", "an absolute .typ path is required"));
    }
    let root = file.parent().ok_or_else(|| CommandError::new("dev.invalidPath", "no parent folder"))?;
    let root =
        state.typesetting.service.open_workspace(root).map_err(|e| CommandError::new("dev.open", e.to_string()))?;
    let canonical = std::fs::canonicalize(&file).map_err(|e| CommandError::new("dev.open", e.to_string()))?;
    let name = canonical
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| CommandError::new("dev.invalidPath", "invalid file name"))?;
    let workspace_path = WorkspacePath::parse(name).map_err(|e| CommandError::new("dev.invalidPath", e.to_string()))?;
    let _ = root;
    let bytes = std::fs::read(&canonical).map_err(|e| CommandError::new("dev.open", e.to_string()))?;
    let text = String::from_utf8(bytes).map_err(|_| CommandError::new("dev.open", "the file is not UTF-8"))?;
    if let Some(id) = state.documents.find_by_path(&workspace_path) {
        return state.documents.info(&id).map_err(|e| CommandError::new("dev.open", e.to_string()));
    }
    Ok(state.documents.open(OpenDocument {
        path: Some(workspace_path),
        text,
        language: "typst".into(),
        encoding: TextEncoding::Utf8,
        read_only: None,
    }))
}
