//! Lean commands and event forwarding.
//!
//! The webview reads checking state, asks for goals/hover/definitions at a
//! position of an exact document revision, restarts servers, and requests a
//! toolchain installation. It can never choose a program, a path to execute,
//! or a toolchain name: everything is derived in Rust from the open document
//! and its project. Starting a server or installing a toolchain requires
//! workspace trust (enforced by the execution service).

use exposition_core::document::DocumentId;
use exposition_core::job::JobId;
use exposition_lean::{
    LeanCheckReport, LeanCheckRequest, LeanCheckTarget, LeanDocumentState, LeanError, LeanGoals, LeanHover,
    LeanLocation, LeanProjectInfo,
};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::broadcast::error::RecvError;

use crate::state::AppState;

/// Event carrying every [`exposition_lean::LeanEvent`].
pub const LEAN_EVENT: &str = "lean:event";
/// Emitted if the frontend fell behind and must re-read `lean_documents`.
pub const LEAN_RESYNC: &str = "lean:resync";

const MAX_GOAL_POSITIONS: usize = 32;
const MAX_CHECK_TIMEOUT_MS: u64 = 600_000;

fn validate_document(document: &DocumentId) -> Result<(), LeanError> {
    let id = &document.0;
    let valid = !id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if valid { Ok(()) } else { Err(LeanError::new("lean.invalidDocument", "invalid document id")) }
}

#[tauri::command]
pub async fn lean_documents(state: State<'_, AppState>) -> Result<Vec<LeanDocumentState>, LeanError> {
    Ok(state.lean.documents().await)
}

#[tauri::command]
pub async fn lean_projects(state: State<'_, AppState>) -> Result<Vec<LeanProjectInfo>, LeanError> {
    Ok(state.lean.projects().await)
}

/// Goals at a UTF-16 offset of exactly `revision` (refused as `lean.stale`
/// when the document has moved on).
#[tauri::command]
pub async fn lean_goals(
    state: State<'_, AppState>,
    document: DocumentId,
    revision: u64,
    offset: u32,
) -> Result<LeanGoals, LeanError> {
    validate_document(&document)?;
    state.lean.goals(document, revision, offset).await
}

#[tauri::command]
pub async fn lean_hover(
    state: State<'_, AppState>,
    document: DocumentId,
    revision: u64,
    offset: u32,
) -> Result<Option<LeanHover>, LeanError> {
    validate_document(&document)?;
    state.lean.hover(document, revision, offset).await
}

#[tauri::command]
pub async fn lean_definition(
    state: State<'_, AppState>,
    document: DocumentId,
    revision: u64,
    offset: u32,
) -> Result<Vec<LeanLocation>, LeanError> {
    validate_document(&document)?;
    state.lean.definition(document, revision, offset).await
}

/// Restart the server for a document's project (also re-checks trust and the toolchain).
#[tauri::command]
pub async fn lean_restart(state: State<'_, AppState>, document: DocumentId) -> Result<(), LeanError> {
    validate_document(&document)?;
    state.lean.restart(document).await
}

/// Re-elaborate one file, rebuilding out-of-date imports.
#[tauri::command]
pub async fn lean_restart_file(state: State<'_, AppState>, document: DocumentId) -> Result<(), LeanError> {
    validate_document(&document)?;
    state.lean.restart_file(document).await
}

/// Explicit user action: install the toolchain the document's project pins.
#[tauri::command]
pub async fn lean_install_toolchain(state: State<'_, AppState>, document: DocumentId) -> Result<JobId, LeanError> {
    validate_document(&document)?;
    state.lean.install_toolchain(document).await
}

/// Check a Lean file and report the result for the exact text checked.
#[tauri::command]
pub async fn lean_check(state: State<'_, AppState>, request: LeanCheckRequest) -> Result<LeanCheckReport, LeanError> {
    if let LeanCheckTarget::Document { document } = &request.target {
        validate_document(document)?;
    }
    if request.goals_at.len() > MAX_GOAL_POSITIONS {
        return Err(LeanError::new("lean.invalidRequest", format!("at most {MAX_GOAL_POSITIONS} goal positions")));
    }
    let request = LeanCheckRequest { timeout_ms: request.timeout_ms.map(|t| t.min(MAX_CHECK_TIMEOUT_MS)), ..request };
    state.lean.check(request).await
}

/// Forward Lean events to the webview in order.
pub fn forward_events(app: AppHandle) {
    let mut rx = app.state::<AppState>().lean.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let _ = app.emit(LEAN_EVENT, &event);
                }
                Err(RecvError::Lagged(_)) => {
                    let _ = app.emit(LEAN_RESYNC, ());
                }
                Err(RecvError::Closed) => break,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_ids_are_validated() {
        assert!(validate_document(&DocumentId("doc-12".into())).is_ok());
        for bad in ["", "../x", "doc 1", "doc-1\u{0}", &"x".repeat(129)] {
            assert!(validate_document(&DocumentId(bad.into())).is_err(), "accepted {bad:?}");
        }
    }
}
