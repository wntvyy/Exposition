//! Lean commands and event forwarding.
//!
//! The webview reads checking state, asks for goals/hover/definitions at a
//! position of an exact document revision, restarts servers, and requests a
//! toolchain installation. It can never choose a program, a path to execute,
//! or a toolchain name: everything is derived in Rust from the open document
//! and its project. Starting a server or installing a toolchain requires
//! workspace trust (enforced by the execution service).

use std::sync::Arc;

use exposition_core::document::DocumentId;
use exposition_core::job::JobId;
use exposition_graph::GraphError;
use exposition_lean::evidence::{
    self, DeclarationEvidence, DeclarationRef, EvidenceOutcomeReport, LeanAssociateRequest, LeanAssociation,
};
use exposition_lean::{
    LeanAuditRecord, LeanCheckReport, LeanCheckRequest, LeanCheckTarget, LeanDocumentState, LeanError, LeanGoals,
    LeanHover, LeanLocation, LeanProjectInfo,
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

// ---- formal evidence in the project graph -------------------------------------

fn graph_error(e: GraphError) -> LeanError {
    LeanError::new(&e.code, e.message)
}

/// The audit record of a document, as the Lean service keeps it.
async fn audit_record(state: &State<'_, AppState>, document: &DocumentId) -> Result<LeanAuditRecord, LeanError> {
    validate_document(document)?;
    state.lean.audit_record(document.clone()).await?.ok_or_else(|| {
        LeanError::new(
            "lean.noAudit",
            "No revision of this file has a finished axiom audit (it has errors, is not trusted, or is still being checked).",
        )
    })
}

/// Whether `revision` of `document` is its saved text.
fn saved(state: &AppState, document: &DocumentId, revision: u64) -> bool {
    state.documents.info(document).ok().is_some_and(|info| info.saved_revision == Some(revision))
}

/// The axiom audit of the last finished revision of an open Lean document:
/// every declaration it reported, with statements, axioms, and verdicts.
#[tauri::command]
pub async fn lean_audit_record(
    state: State<'_, AppState>,
    document: DocumentId,
) -> Result<Option<LeanAuditRecord>, LeanError> {
    validate_document(&document)?;
    state.lean.audit_record(document).await
}

/// Record service-owned formal evidence for declarations of the document's
/// last audit (exactly what the audit reported; unlisted names are refused).
#[tauri::command]
pub async fn lean_record_evidence(
    state: State<'_, AppState>,
    document: DocumentId,
    declarations: Vec<String>,
) -> Result<EvidenceOutcomeReport, LeanError> {
    if declarations.is_empty() || declarations.len() > 200 || declarations.iter().any(|d| d.len() > 1024) {
        return Err(LeanError::new("lean.invalidRequest", "name 1–200 declarations"));
    }
    let record = audit_record(&state, &document).await?;
    let saved = saved(&state, &document, record.revision);
    let graph = Arc::clone(&state.graph);
    tauri::async_runtime::spawn_blocking(move || {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        evidence::record_evidence(&graph, &record, &declarations, saved, now)
    })
    .await
    .map_err(|e| LeanError::new("internal.task", e.to_string()))?
    .map_err(graph_error)
}

/// Record a person's reviewed association of a formal declaration with an
/// informal statement, definition, assumption, or object (`formalizes`,
/// accepted). A correspondence assertion, not a proof of equivalence; the
/// verifier's verdict stays a separate evidence record. For an audited
/// declaration its evidence is recorded from the audit first.
#[tauri::command]
pub async fn lean_associate_declaration(
    state: State<'_, AppState>,
    request: LeanAssociateRequest,
) -> Result<LeanAssociation, LeanError> {
    if request.note.as_ref().is_some_and(|n| n.len() > 500) {
        return Err(LeanError::new("lean.invalidRequest", "the note is longer than 500 characters"));
    }
    let audited = match &request.declaration {
        DeclarationRef::Audited { document, name } => {
            let record = audit_record(&state, document).await?;
            let saved = saved(&state, document, record.revision);
            Some((record, name.clone(), saved))
        }
        DeclarationRef::Entity { .. } => None,
    };
    let graph = Arc::clone(&state.graph);
    tauri::async_runtime::spawn_blocking(move || {
        let (declaration, report) = match (&request.declaration, audited) {
            (DeclarationRef::Entity { entity }, _) => (entity.clone(), None),
            (_, Some((record, name, saved))) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                let report = evidence::record_evidence(&graph, &record, std::slice::from_ref(&name), saved, now)
                    .map_err(graph_error)?;
                let Some(recorded) = report.recorded.first() else {
                    let reason = report.refused.first().map(|r| r.reason.clone()).unwrap_or_default();
                    return Err(LeanError::new("lean.notAudited", reason));
                };
                (recorded.entity.clone(), Some(report))
            }
            _ => unreachable!("audited references carry a record"),
        };
        let association =
            evidence::associate(&graph, &declaration, &request.target, request.note.clone()).map_err(graph_error)?;
        Ok(LeanAssociation { association, evidence: report })
    })
    .await
    .map_err(|e| LeanError::new("internal.task", e.to_string()))?
}

/// Formal declarations of an open Lean document with their evidence history
/// (freshness against the current text) and reviewed associations.
#[tauri::command]
pub async fn lean_declaration_evidence(
    state: State<'_, AppState>,
    document: DocumentId,
) -> Result<Vec<DeclarationEvidence>, LeanError> {
    validate_document(&document)?;
    let snapshot = state
        .documents
        .snapshot(&document)
        .map_err(|_| LeanError::new("lean.unknownDocument", "That document is not open."))?;
    let Some(path) = snapshot.path.clone() else { return Ok(Vec::new()) };
    let digest = evidence::text_digest(&snapshot.text);
    let graph = Arc::clone(&state.graph);
    tauri::async_runtime::spawn_blocking(move || evidence::declaration_evidence(&graph, &path, Some(&digest)))
        .await
        .map_err(|e| LeanError::new("internal.task", e.to_string()))?
        .map_err(graph_error)
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
