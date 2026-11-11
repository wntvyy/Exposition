//! LaTeX build commands and event forwarding.
//!
//! The webview can request builds of a LaTeX target in an open workspace,
//! read target status, cancel, and fetch the immutable pages of a finished
//! build (PNG bytes for blob URLs, and the glyph mapping). Builds run in Rust
//! through the shared execution service and require a trusted workspace; a
//! refusal arrives as a finished build carrying `refusal.code ==
//! "untrustedWorkspace"` so the frontend can offer `requestWorkspaceTrust`.

use std::path::PathBuf;

use exposition_core::build::{BuildId, BuildTarget};
use exposition_tex::mapping::PageMapping;
use exposition_tex::{TexBuildMode, TexServiceError, TexTargetRef, TexTargetStatus};
use tauri::ipc::Response;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::broadcast::error::RecvError;

use crate::CommandError;
use crate::state::AppState;

/// Every [`exposition_tex::TexEvent`] (queued, started, finished), in order.
pub const TEX_EVENT: &str = "tex:event";
/// Emitted if the frontend fell behind and must re-read target status.
pub const TEX_RESYNC: &str = "tex:resync";

const MAX_PAGE: u32 = 10_000;

fn service_error(error: TexServiceError) -> CommandError {
    let code = match error {
        TexServiceError::UnsupportedTarget => "tex.unsupportedTarget",
        TexServiceError::WorkspaceNotOpen(_) => "tex.workspaceNotOpen",
        TexServiceError::UnknownBuild => "tex.unknownBuild",
        TexServiceError::NoOutput => "tex.noOutput",
        TexServiceError::NoSuchPage(_) => "tex.noSuchPage",
        TexServiceError::InvalidScale(_) => "tex.invalidScale",
        TexServiceError::Io(_) => "tex.io",
    };
    CommandError::new(code, error.to_string())
}

fn validate_root(root: &str) -> Result<PathBuf, CommandError> {
    if root.is_empty() || root.len() > 4096 || root.contains('\0') {
        return Err(CommandError::new("tex.invalidRoot", "invalid workspace root"));
    }
    Ok(PathBuf::from(root))
}

/// Build ids are `tex-<n>-<n>`; anything else is rejected before lookup.
fn validate_build_id(build: &BuildId) -> Result<(), CommandError> {
    let valid = build.0.len() <= 64
        && build.0.starts_with("tex-")
        && build.0.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if valid { Ok(()) } else { Err(CommandError::new("tex.invalidBuildId", "invalid build id")) }
}

fn validate_page(page: u32) -> Result<(), CommandError> {
    if page < MAX_PAGE { Ok(()) } else { Err(CommandError::new("tex.noSuchPage", "page index out of range")) }
}

/// Queue a build of `target` in the open workspace `root`. Requests coalesce;
/// a newer request cancels a running build of the same target.
#[tauri::command]
pub async fn tex_build(
    state: State<'_, AppState>,
    root: String,
    target: BuildTarget,
    mode: TexBuildMode,
) -> Result<TexTargetRef, CommandError> {
    let root = validate_root(&root)?;
    state.tex.request_build(&root, target, mode).map_err(service_error)
}

/// Current state of a target (`null` before its first build request).
#[tauri::command]
pub async fn tex_status(
    state: State<'_, AppState>,
    root: String,
    target: BuildTarget,
) -> Result<Option<TexTargetStatus>, CommandError> {
    let root = validate_root(&root)?;
    Ok(state.tex.status(&root, &target))
}

/// Cancel the running build and drop the pending request; `false` when idle.
#[tauri::command]
pub async fn tex_cancel(state: State<'_, AppState>, root: String, target: BuildTarget) -> Result<bool, CommandError> {
    let root = validate_root(&root)?;
    Ok(state.tex.cancel(&root, &target))
}

/// PNG bytes of a page of a finished build (`scale` pixels per point).
#[tauri::command]
pub async fn tex_page_image(
    state: State<'_, AppState>,
    build: BuildId,
    page: u32,
    scale: f32,
) -> Result<Response, CommandError> {
    validate_build_id(&build)?;
    validate_page(page)?;
    let tex = state.tex.clone();
    let png = tauri::async_runtime::spawn_blocking(move || tex.page_png(&build, page, scale))
        .await
        .map_err(|e| CommandError::new("tex.io", e.to_string()))?
        .map_err(service_error)?;
    Ok(Response::new(png.as_ref().clone()))
}

/// Glyph-level mapping of a page of a finished build.
#[tauri::command]
pub async fn tex_page_map(state: State<'_, AppState>, build: BuildId, page: u32) -> Result<PageMapping, CommandError> {
    validate_build_id(&build)?;
    validate_page(page)?;
    let tex = state.tex.clone();
    let map = tauri::async_runtime::spawn_blocking(move || tex.page_map(&build, page))
        .await
        .map_err(|e| CommandError::new("tex.io", e.to_string()))?
        .map_err(service_error)?;
    Ok(map.as_ref().clone())
}

/// Forward TeX build events to the webview in order.
pub fn forward_events(app: AppHandle) {
    let mut rx = app.state::<AppState>().tex.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let _ = app.emit(TEX_EVENT, &event);
                }
                Err(RecvError::Lagged(_)) => {
                    let _ = app.emit(TEX_RESYNC, ());
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
    fn build_ids_and_pages_are_validated() {
        assert!(validate_build_id(&BuildId("tex-123-4".into())).is_ok());
        for bad in ["", "../../etc", "tex-1/2", "job-1", &"tex-".repeat(20)] {
            assert!(validate_build_id(&BuildId(bad.into())).is_err(), "{bad}");
        }
        assert!(validate_page(MAX_PAGE).is_err());
        assert!(validate_root("").is_err() && validate_root("a\0b").is_err());
    }
}
