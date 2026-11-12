//! Exact-page editing commands (format-neutral).
//!
//! Geometry queries name the build whose page map they use; offsets are UTF-16
//! positions in that build's snapshot of the file. Builds, page images, and
//! revision texts come from the formats' [`PageMapProvider`]s; typed input is
//! translated by the format's [`PageInputRules`]. Edits go through the
//! document service as `EditOrigin::Page` transactions.

use std::sync::Arc;

use exposition_core::document::TransactionError;
use exposition_core::path::WorkspacePath;
use exposition_page::map::Affinity;
use exposition_page::{
    CaretGeometry, EditRefusal, Motion, NavigationResult, PageEditOutcome, PageEditRequest, PageHit, PageInputRules,
    PageMap, PageMapProvider, SelectionGeometry, TypstInput, apply_page_edit,
};
use tauri::State;
use tauri::ipc::Response;

use crate::error::CommandError;
use crate::state::AppState;

/// Every format's retained builds. Integration point: add the LaTeX/Markdown
/// providers here.
fn providers<'a>(state: &'a State<'_, AppState>) -> [&'a dyn PageMapProvider; 2] {
    [state.typesetting.service.as_ref(), &state.tex]
}

/// Page input rules by document path. Integration point: add `.tex`
/// (LaTeX escaping) and `.md` rules here.
fn rules_for(path: Option<&WorkspacePath>) -> Option<&'static dyn PageInputRules> {
    match path?.extension()?.to_ascii_lowercase().as_str() {
        "typ" => Some(&TypstInput),
        "tex" => Some(&exposition_tex::TexInput),
        _ => None,
    }
}

fn map_of(state: &State<'_, AppState>, build: &str) -> Result<Arc<PageMap>, CommandError> {
    providers(state)
        .iter()
        .find_map(|p| p.page_map(build))
        .ok_or_else(|| CommandError::new("page.unknownBuild", "the build or its page map is no longer available"))
}

/// A rendered page (SVG, PNG, ...) as raw bytes, by its `renderRef`.
#[tauri::command]
pub async fn page_image(state: State<'_, AppState>, render_ref: String) -> Result<Response, CommandError> {
    if render_ref.is_empty()
        || render_ref.len() > 128
        || !render_ref.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(CommandError::new("page.invalidRef", "invalid page reference"));
    }
    providers(&state)
        .iter()
        .find_map(|p| p.page_image(&render_ref))
        .map(|image| Response::new(image.bytes.to_vec()))
        .ok_or_else(|| CommandError::new("page.unknownPage", "the page is no longer available"))
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
    let path = state.documents.info(&request.document)?.path;
    let Some(rules) = rules_for(path.as_ref()) else {
        return Ok(PageEditOutcome::Refused {
            revision: request.base_revision,
            refusal: EditRefusal::NotEditable { reason: "no page editing for this format yet".into() },
        });
    };
    let providers = providers(&state);
    apply_page_edit(&state.documents, rules, &request, &|document, revision| {
        providers.iter().find_map(|p| p.revision_text(document, revision))
    })
}
