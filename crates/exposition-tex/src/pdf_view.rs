//! Read-only views of project PDF files (the PDF editor tab).
//!
//! A small adapter over [`crate::pdf::PdfDocument`], the renderer the LaTeX
//! page view uses: a project PDF is read once, parsed, and its pages are
//! rasterized to PNG on request. There is no build, no SyncTeX, no glyph
//! mapping, and no source relationship: a view only shows pages.
//!
//! Rules:
//! - Bytes come only from [`WorkspaceService::read_pdf`]: a `.pdf` file inside
//!   the open folder (links and junctions out of it refused), at most
//!   `MAX_PDF_BYTES`. A view belongs to the folder it was read from; once
//!   another folder is open its pages are refused (`workspace.changed`).
//! - The PDF is only parsed and rasterized (hayro). Nothing in it is executed
//!   or followed: no JavaScript, no open/launch actions, no links, no form or
//!   annotation behavior, no external file or network access. The webview
//!   receives PNG bytes, never the PDF.
//! - Bounds: at most [`MAX_PDF_VIEW_PAGES`] pages are listed and rendered, a
//!   page raster holds at most [`MAX_PDF_VIEW_PAGE_PIXELS`] pixels, at most
//!   [`MAX_OPEN_PDF_VIEWS`] parsed files (and [`MAX_RETAINED_PDF_BYTES`]) are
//!   kept, and one page is rasterized at a time. No raster is cached here.
//! - A file the parser or the rasterizer cannot handle (or panics on) is an
//!   error for that file or page; it never takes the application down.
//!
//! A page being rasterized cannot be interrupted; a request whose view was
//! closed while it waited for the render slot is dropped without rendering.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use exposition_core::path::WorkspacePath;
use exposition_workspace::WorkspaceService;
use exposition_workspace::error::{WorkspaceError, WorkspaceResult};
use serde::{Deserialize, Serialize};

use crate::pdf::{PdfDocument, PdfError};

/// Pages of one PDF that are listed and rendered; later pages are not shown.
pub const MAX_PDF_VIEW_PAGES: usize = 2000;
/// Largest page raster (pixels); a page that would be larger is rendered at a
/// lower scale.
pub const MAX_PDF_VIEW_PAGE_PIXELS: u64 = 16 * 1024 * 1024;
/// Parsed PDF files kept at once (the least recently used one is dropped).
pub const MAX_OPEN_PDF_VIEWS: usize = 4;
/// Bytes of PDF files kept at once (the newest view is always kept).
pub const MAX_RETAINED_PDF_BYTES: u64 = 128 * 1024 * 1024;
/// Accepted render scales (pixels per point).
pub const MIN_PDF_VIEW_SCALE: f32 = 0.25;
pub const MAX_PDF_VIEW_SCALE: f32 = 8.0;

/// Size of one page in points (after crop box and rotation).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PdfPageSize {
    pub width: f64,
    pub height: f64,
}

/// An opened project PDF.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PdfViewInfo {
    /// Names this view in page requests; unknown once the view was closed,
    /// dropped (least recently used), or the project changed.
    pub token: u32,
    /// Pages in the file.
    pub page_count: u32,
    /// Sizes of the pages that are shown: the first [`MAX_PDF_VIEW_PAGES`].
    pub pages: Vec<PdfPageSize>,
    /// File size in bytes.
    pub bytes: u32,
}

struct View {
    token: u32,
    root: PathBuf,
    doc: PdfDocument,
    bytes: u64,
    shown_pages: usize,
}

#[derive(Default)]
struct State {
    next_token: u32,
    /// Least recently used first.
    views: Vec<Arc<View>>,
}

/// The application's open PDF views.
#[derive(Default)]
pub struct PdfViews {
    state: Mutex<State>,
    /// One page is rasterized at a time.
    render: Mutex<()>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn error(code: &'static str, message: impl Into<String>) -> WorkspaceError {
    WorkspaceError::new(code, message)
}

/// `%PDF-` within the first kilobyte (readers accept leading junk there).
fn has_pdf_header(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(1024)].windows(5).any(|w| w == b"%PDF-")
}

/// Pixels of a `width_pt` × `height_pt` page at `scale`, as the rasterizer rounds.
fn raster_pixels(width_pt: f64, height_pt: f64, scale: f32) -> u64 {
    let side = |pt: f64| (pt * f64::from(scale)).floor().max(1.0) as u64;
    side(width_pt) * side(height_pt)
}

/// `requested`, lowered until the page raster fits [`MAX_PDF_VIEW_PAGE_PIXELS`].
pub fn bounded_view_scale(width_pt: f64, height_pt: f64, requested: f32) -> f32 {
    if !(width_pt.is_finite() && height_pt.is_finite()) || width_pt <= 0.0 || height_pt <= 0.0 {
        return requested;
    }
    let pixels = raster_pixels(width_pt, height_pt, requested);
    if pixels <= MAX_PDF_VIEW_PAGE_PIXELS {
        return requested;
    }
    let mut scale = requested * (MAX_PDF_VIEW_PAGE_PIXELS as f64 / pixels as f64).sqrt() as f32;
    while scale > 0.0 && raster_pixels(width_pt, height_pt, scale) > MAX_PDF_VIEW_PAGE_PIXELS {
        scale *= 0.995;
    }
    scale
}

impl PdfViews {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read and parse the project PDF at `path` and keep it for page requests.
    pub fn open(&self, workspace: &WorkspaceService, path: &WorkspacePath) -> WorkspaceResult<PdfViewInfo> {
        let (root, bytes) = workspace.read_pdf(path)?;
        let size = bytes.len() as u64;
        if bytes.is_empty() {
            return Err(error("pdf.empty", format!("{path} is empty.")));
        }
        if !has_pdf_header(&bytes) {
            return Err(error("pdf.damaged", format!("{path} is not a PDF document.")));
        }
        let parsed = catch_unwind(AssertUnwindSafe(|| {
            let doc = PdfDocument::open(bytes)?;
            let page_count = doc.page_count();
            let shown = page_count.min(MAX_PDF_VIEW_PAGES);
            let mut pages = Vec::with_capacity(shown);
            for index in 0..shown {
                let (width, height) = doc.page_size(index)?;
                let finite = |v: f64| if v.is_finite() && v >= 1.0 { v } else { 1.0 };
                pages.push(PdfPageSize { width: finite(width), height: finite(height) });
            }
            Ok::<_, PdfError>((doc, page_count, pages))
        }));
        let (doc, page_count, pages) = match parsed {
            Ok(Ok(parsed)) => parsed,
            Ok(Err(PdfError::Encrypted(_))) => {
                return Err(error(
                    "pdf.encrypted",
                    format!("{path} is password-protected or uses encryption that is not supported."),
                ));
            }
            Ok(Err(_)) | Err(_) => {
                return Err(error("pdf.damaged", format!("{path} is damaged or is not a readable PDF document.")));
            }
        };
        if page_count == 0 {
            return Err(error("pdf.empty", format!("{path} has no pages.")));
        }
        // The folder may have changed while the file was parsed.
        if workspace.open_root().as_deref() != Some(root.as_path()) {
            return Err(error("workspace.changed", "The project changed while the file was being read."));
        }
        let mut state = lock(&self.state);
        state.next_token = state.next_token.wrapping_add(1).max(1);
        let token = state.next_token;
        // Views of another folder are never served again: drop them now.
        state.views.retain(|v| v.root == root);
        state.views.push(Arc::new(View { token, root, doc, bytes: size, shown_pages: pages.len() }));
        while state.views.len() > 1
            && (state.views.len() > MAX_OPEN_PDF_VIEWS
                || state.views.iter().map(|v| v.bytes).sum::<u64>() > MAX_RETAINED_PDF_BYTES)
        {
            state.views.remove(0);
        }
        Ok(PdfViewInfo { token, page_count: page_count.min(u32::MAX as usize) as u32, pages, bytes: size as u32 })
    }

    /// The view named `token`, marked most recently used.
    fn view(&self, token: u32) -> WorkspaceResult<Arc<View>> {
        let mut state = lock(&self.state);
        let at = state
            .views
            .iter()
            .position(|v| v.token == token)
            .ok_or_else(|| error("pdf.closed", "This PDF view is no longer open."))?;
        let view = state.views.remove(at);
        state.views.push(Arc::clone(&view));
        Ok(view)
    }

    /// PNG of page `page` (0-based) at `scale` pixels per point, on white. The
    /// scale is lowered for a page that would exceed [`MAX_PDF_VIEW_PAGE_PIXELS`].
    pub fn page_png(
        &self,
        workspace: &WorkspaceService,
        token: u32,
        page: u32,
        scale: f32,
    ) -> WorkspaceResult<Vec<u8>> {
        if !scale.is_finite() || !(MIN_PDF_VIEW_SCALE..=MAX_PDF_VIEW_SCALE).contains(&scale) {
            return Err(error("pdf.invalidScale", format!("{scale} is not a supported page scale.")));
        }
        let view = self.view(token)?;
        if workspace.open_root().as_deref() != Some(view.root.as_path()) {
            self.close(token);
            return Err(error("workspace.changed", "The project changed; this PDF view belongs to another project."));
        }
        let index = page as usize;
        if index >= view.shown_pages {
            return Err(error("pdf.noSuchPage", format!("Page {} is not shown.", u64::from(page) + 1)));
        }
        let _slot = lock(&self.render);
        // Closed (tab closed, file replaced, project switched) while waiting: do no work.
        if !lock(&self.state).views.iter().any(|v| v.token == token) {
            return Err(error("pdf.closed", "This PDF view is no longer open."));
        }
        let rendered = catch_unwind(AssertUnwindSafe(|| {
            let (width, height) = view.doc.page_size(index)?;
            view.doc.render_page(index, bounded_view_scale(width, height, scale))
        }));
        match rendered {
            Ok(Ok(rendered)) => Ok(rendered.png),
            Ok(Err(_)) | Err(_) => {
                Err(error("pdf.renderFailed", format!("Page {} could not be rendered.", u64::from(page) + 1)))
            }
        }
    }

    /// Forget a view (its tab closed or loaded the file again). Unknown tokens are ignored.
    pub fn close(&self, token: u32) {
        lock(&self.state).views.retain(|v| v.token != token);
    }

    /// Forget every view.
    pub fn clear(&self) {
        lock(&self.state).views.clear();
    }

    /// Views kept now.
    pub fn open_count(&self) -> usize {
        lock(&self.state).views.len()
    }
}
