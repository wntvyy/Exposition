//! Project images for view-only display: the image editor tab and inline image
//! previews in Markdown/LaTeX/Typst source.
//!
//! Both commands are read-only and bound to the folder open when they start
//! (`workspace.changed` when another one was opened meanwhile). Image bytes
//! come only from image files inside the open folder (extension allow-list,
//! 32 MiB cap, links/junctions out of the folder refused; see
//! `exposition_workspace::media`). Reference detection runs the file's graph
//! extractor over the live buffer text transiently: it writes nothing to the
//! graph store, installs no overlay, and emits no graph event.

use std::sync::Arc;

use exposition_core::path::WorkspacePath;
use exposition_core::text::TextRange;
use exposition_graph::reconcile::assets::asset_references;
use exposition_workspace::media::ImageLocation;
use serde::{Deserialize, Serialize};
use tauri::State;
use tauri::ipc::Response;
use ts_rs::TS;

use crate::CommandError;
use crate::state::AppState;

/// One image reference in a source buffer and where it leads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ImageReference {
    /// UTF-16 range of the referencing syntax in the text that was sent.
    pub range: TextRange,
    /// The path as written in the source.
    pub written: String,
    pub location: ImageLocation,
}

/// The bytes of a project image (binary response; the webview sniffs the type).
#[tauri::command]
pub async fn media_read_image(state: State<'_, AppState>, path: WorkspacePath) -> Result<Response, CommandError> {
    let workspace = Arc::clone(&state.workspace);
    let bytes = tauri::async_runtime::spawn_blocking(move || workspace.read_image(&path))
        .await
        .map_err(|e| CommandError::new("internal.task", format!("Image task failed: {e}")))??;
    Ok(Response::new(bytes))
}

/// Image references in `text` (the current buffer of the project file `path`),
/// each located in the open folder.
#[tauri::command]
pub async fn media_image_references(
    state: State<'_, AppState>,
    path: WorkspacePath,
    text: String,
) -> Result<Vec<ImageReference>, CommandError> {
    let workspace = Arc::clone(&state.workspace);
    let graph = Arc::clone(&state.graph);
    tauri::async_runtime::spawn_blocking(move || -> Result<Vec<ImageReference>, CommandError> {
        let references = asset_references(&graph, &path, &text)?;
        let mut out = Vec::with_capacity(references.len());
        for reference in references {
            let location = if reference.candidates.is_empty() {
                ImageLocation::Blocked
            } else {
                workspace.locate_image(&reference.candidates)?
            };
            out.push(ImageReference { range: reference.range, written: reference.written, location });
        }
        Ok(out)
    })
    .await
    .map_err(|e| CommandError::new("internal.task", format!("Image task failed: {e}")))?
}
