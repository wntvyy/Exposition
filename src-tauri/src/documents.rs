//! Document commands and event forwarding.

use exposition_core::document::{
    DocumentChanged, DocumentId, DocumentInfo, DocumentState, EditOrigin, TransactionError, TransactionOutcome,
    TransactionRequest,
};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::broadcast::error::RecvError;

use crate::state::AppState;

/// Event carrying every [`exposition_core::document::DocumentEvent`], in order.
pub const DOCUMENT_EVENT: &str = "documents:event";
/// Emitted if the frontend fell behind and must re-read document state.
pub const DOCUMENT_RESYNC: &str = "documents:resync";

#[tauri::command]
pub async fn document_list(state: State<'_, AppState>) -> Result<Vec<DocumentInfo>, TransactionError> {
    Ok(state.documents.list())
}

#[tauri::command]
pub async fn document_state(
    state: State<'_, AppState>,
    document: DocumentId,
) -> Result<DocumentState, TransactionError> {
    state.documents.state(&document)
}

#[tauri::command]
pub async fn document_apply(
    state: State<'_, AppState>,
    request: TransactionRequest,
) -> Result<TransactionOutcome, TransactionError> {
    state.documents.apply(request)
}

#[tauri::command]
pub async fn document_undo(
    state: State<'_, AppState>,
    document: DocumentId,
    origin: EditOrigin,
) -> Result<DocumentChanged, TransactionError> {
    state.documents.undo(&document, origin)
}

#[tauri::command]
pub async fn document_redo(
    state: State<'_, AppState>,
    document: DocumentId,
    origin: EditOrigin,
) -> Result<DocumentChanged, TransactionError> {
    state.documents.redo(&document, origin)
}

/// Forward document events to the webview in order.
pub fn forward_events(app: AppHandle) {
    let mut rx = app.state::<AppState>().documents.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let _ = app.emit(DOCUMENT_EVENT, &event);
                }
                Err(RecvError::Lagged(_)) => {
                    let _ = app.emit(DOCUMENT_RESYNC, ());
                }
                Err(RecvError::Closed) => break,
            }
        }
    });
}
