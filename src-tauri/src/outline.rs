//! Document outline command: the structure of an open document's current
//! text (its live buffer, saved or not), read with the graph's source
//! extractors. Read-only and transient: nothing is written to the document,
//! the files, or the project graph.

use exposition_core::document::DocumentId;
use exposition_graph::{DocumentOutline, document_outline};
use tauri::State;

use crate::CommandError;
use crate::state::AppState;

/// Outline of the document's text at its current revision (returned in the
/// result, so callers can drop answers for a revision they no longer show).
#[tauri::command]
pub async fn document_outline_get(
    state: State<'_, AppState>,
    document: DocumentId,
) -> Result<DocumentOutline, CommandError> {
    let current = state.documents.state(&document).map_err(|e| CommandError::new("document.unknown", e.to_string()))?;
    let Some(path) = current.info.path.clone() else {
        return Err(CommandError::new("outline.untitled", "This document has no file path, so it has no outline."));
    };
    let unsaved = current.info.saved_revision != Some(current.info.revision);
    let revision = current.info.revision;
    tauri::async_runtime::spawn_blocking(move || document_outline(&path, &current.text, revision, unsaved))
        .await
        .map_err(|e| CommandError::new("internal.task", format!("Outline task failed: {e}")))
}
