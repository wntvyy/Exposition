//! LaTeX build commands and event forwarding.
//!
//! The webview can request builds of a LaTeX target of the open workspace
//! (default: the project's target from `.exposition/project.json`), read
//! target status, cancel, and fetch the immutable pages of a finished build
//! (PNG bytes for blob URLs, and the glyph mapping). Builds run in Rust through
//! the shared execution service and require a trusted workspace; a refusal
//! arrives as a finished build carrying `refusal.code == "untrustedWorkspace"`
//! so the frontend can offer `requestWorkspaceTrust`.

use std::path::PathBuf;
use std::time::Duration;

use exposition_core::build::{BuildId, BuildStatus, BuildTarget, ExportArtifact};
use exposition_core::document::DocumentEvent;
use exposition_core::path::WorkspacePath;
use exposition_tex::mapping::PageMapping;
use exposition_tex::{TexBuildMode, TexBuildSummary, TexEvent, TexServiceError, TexTargetRef, TexTargetStatus};
use exposition_typesetting::BuildTimings;
use exposition_workspace::types::WorkspaceEvent;
use tauri::ipc::Response;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::broadcast::error::RecvError;

use crate::CommandError;
use crate::state::AppState;
use crate::typesetting::{BUILD_EVENT, BuildActivity, BuildEvent, BuildStatusEvent, STATUS_EVENT};

/// Every [`exposition_tex::TexEvent`] (queued, started, finished), in order.
pub const TEX_EVENT: &str = "tex:event";
/// Emitted if the frontend fell behind and must re-read target status.
pub const TEX_RESYNC: &str = "tex:resync";

const MAX_PAGE: u32 = 10_000;

fn service_error(error: TexServiceError) -> CommandError {
    let code = match error {
        TexServiceError::UnsupportedTarget => "tex.unsupportedTarget",
        TexServiceError::WorkspaceNotOpen => "tex.workspaceNotOpen",
        TexServiceError::NoTarget => "tex.noTarget",
        TexServiceError::UnknownBuild => "tex.unknownBuild",
        TexServiceError::NoOutput => "tex.noOutput",
        TexServiceError::NoSuchPage(_) => "tex.noSuchPage",
        TexServiceError::InvalidScale(_) => "tex.invalidScale",
        TexServiceError::InvalidDestination(_) => "tex.invalidDestination",
        TexServiceError::Io(_) => "tex.io",
    };
    CommandError::new(code, error.to_string())
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

/// Queue a build of `target` (default: the project's LaTeX target) in the
/// open workspace. Requests coalesce; a newer request cancels a running build
/// of the same target.
#[tauri::command]
pub async fn tex_build(
    state: State<'_, AppState>,
    target: Option<BuildTarget>,
    mode: TexBuildMode,
) -> Result<TexTargetRef, CommandError> {
    let tex = state.tex.clone();
    tauri::async_runtime::spawn_blocking(move || tex.request_build(target, mode))
        .await
        .map_err(|e| CommandError::new("tex.io", e.to_string()))?
        .map_err(service_error)
}

/// Current state of a target (`null` before its first build request).
#[tauri::command]
pub async fn tex_status(
    state: State<'_, AppState>,
    target: Option<BuildTarget>,
) -> Result<Option<TexTargetStatus>, CommandError> {
    let tex = state.tex.clone();
    tauri::async_runtime::spawn_blocking(move || tex.status(target.as_ref()))
        .await
        .map_err(|e| CommandError::new("tex.io", e.to_string()))
}

/// Cancel the running build and drop the pending request; `false` when idle.
#[tauri::command]
pub async fn tex_cancel(state: State<'_, AppState>, target: Option<BuildTarget>) -> Result<bool, CommandError> {
    Ok(state.tex.cancel(target.as_ref()))
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

// ---- The shared page editor's typesetting flow for .tex entries ----------
//
// `typesetting_watch`/`unwatch`/`build`/`export_pdf` dispatch .tex entries
// here; TeX builds are reported as the same `typesetting:build` and
// `typesetting:status` events as Typst builds, so the page editor, Problems
// panel, and status item work unchanged.

pub fn is_tex(entry: &WorkspacePath) -> bool {
    entry.extension().is_some_and(|e| e.eq_ignore_ascii_case("tex"))
}

/// A finished TeX build as the shared build event.
fn build_event(summary: &TexBuildSummary, last_good: Option<&TexBuildSummary>) -> BuildEvent {
    let compile_ms: f64 = summary.passes.iter().map(|p| p.duration_ms as f64).sum();
    BuildEvent {
        result: summary.result.clone(),
        timings: BuildTimings {
            snapshot_ms: 0.0,
            compile_ms,
            map_ms: 0.0,
            total_ms: summary.result.duration_ms as f64,
        },
        last_good: last_good.map(|s| s.result.clone()),
        refusal: summary.refusal.clone(),
    }
}

/// Watch a .tex entry: draft build now and after every change (trust-gated
/// in the TeX service). Returns the latest finished build, if any.
pub fn watch(state: &State<'_, AppState>, entry: &WorkspacePath) -> Result<Option<BuildEvent>, CommandError> {
    let (_, status) = state.tex.watch(entry).map_err(service_error)?;
    Ok(status.and_then(|status| {
        let last = status.last.filter(|l| l.result.status != BuildStatus::Canceled)?;
        Some(build_event(&last, status.last_good.as_ref()))
    }))
}

pub fn unwatch(state: &State<'_, AppState>, entry: &WorkspacePath) {
    state.tex.unwatch(entry);
}

/// Build a .tex entry fully now and wait for the result.
pub async fn build_now(state: &State<'_, AppState>, entry: &WorkspacePath) -> Result<BuildEvent, CommandError> {
    let tex = state.tex.clone();
    let target = tex.target_for_entry(entry);
    let mut rx = tex.subscribe();
    let reference = tex.request_build(Some(target.clone()), TexBuildMode::Full).map_err(service_error)?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(600);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(TexEvent::Finished { target: t, summary }))
                if t == reference && summary.result.status != BuildStatus::Canceled =>
            {
                let last_good = tex.status(Some(&target)).and_then(|s| s.last_good);
                return Ok(build_event(&summary, last_good.as_ref()));
            }
            Ok(Ok(_)) | Ok(Err(RecvError::Lagged(_))) => continue,
            Ok(Err(RecvError::Closed)) | Err(_) => {
                return Err(CommandError::new("typesetting.build", "the LaTeX build did not finish"));
            }
        }
    }
}

pub fn is_tex_build(build: &str) -> bool {
    build.starts_with("tex-")
}

pub async fn export_pdf(
    state: &State<'_, AppState>,
    build: String,
    destination: Option<String>,
) -> Result<ExportArtifact, CommandError> {
    let tex = state.tex.clone();
    tauri::async_runtime::spawn_blocking(move || tex.export_pdf(&BuildId(build), destination.map(PathBuf::from)))
        .await
        .map_err(|e| CommandError::new("typesetting.export", e.to_string()))?
        .map_err(|e| CommandError::new("typesetting.export", e.to_string()))
}

fn shared_events(app: &AppHandle, event: &TexEvent) {
    let tex = app.state::<AppState>().tex.clone();
    let (entry, activity) = match event {
        TexEvent::Queued { target, .. } => (target.entry.clone(), BuildActivity::Queued),
        TexEvent::Started { target, .. } => (target.entry.clone(), BuildActivity::Building),
        TexEvent::Finished { target, summary } => {
            let build_target = BuildTarget {
                format: summary.result.target.format,
                engine: target.engine,
                entry: target.entry.clone(),
            };
            let status = tex.status(Some(&build_target));
            // Superseded/canceled builds are not results.
            if summary.result.status != BuildStatus::Canceled {
                let last_good = status.as_ref().and_then(|s| s.last_good.as_ref());
                let _ = app.emit(BUILD_EVENT, build_event(summary, last_good));
            }
            let busy = status.is_some_and(|s| s.pending.is_some() || s.running.is_some());
            (target.entry.clone(), if busy { BuildActivity::Queued } else { BuildActivity::Idle })
        }
    };
    let _ = app.emit(STATUS_EVENT, BuildStatusEvent { entry, activity });
}

/// Forward TeX build events to the webview in order (as `tex:event`, and as
/// the shared typesetting build/status events), and rebuild watched entries
/// when documents or files change.
pub fn forward_events(app: AppHandle) {
    let mut rx = app.state::<AppState>().tex.subscribe();
    let events_app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let _ = events_app.emit(TEX_EVENT, &event);
                    shared_events(&events_app, &event);
                }
                Err(RecvError::Lagged(_)) => {
                    let _ = events_app.emit(TEX_RESYNC, ());
                }
                Err(RecvError::Closed) => break,
            }
        }
    });

    // Document edits (typing on the page or in source) and file changes
    // rebuild the watched entries; closing the folder forgets them.
    let mut documents = app.state::<AppState>().documents.subscribe();
    let tex = app.state::<AppState>().tex.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            match documents.recv().await {
                Ok(DocumentEvent::Changed(_)) | Err(RecvError::Lagged(_)) => {
                    let tex = tex.clone();
                    let _ = tauri::async_runtime::spawn_blocking(move || tex.rebuild_watched()).await;
                }
                Ok(_) => {}
                Err(RecvError::Closed) => break,
            }
        }
    });
    let mut workspace = app.state::<AppState>().workspace.subscribe();
    let tex = app.state::<AppState>().tex.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            match workspace.recv().await {
                Ok(WorkspaceEvent::FilesChanged { .. }) | Err(RecvError::Lagged(_)) => {
                    let tex = tex.clone();
                    let _ = tauri::async_runtime::spawn_blocking(move || tex.rebuild_watched()).await;
                }
                Ok(WorkspaceEvent::Closed) => tex.forget_watched(),
                Ok(_) => {}
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
        assert!(validate_page(0).is_ok());
    }
}
