//! The seam between page views and format adapters.
//!
//! Every format that supports exact-page editing (Typst, LaTeX, Markdown)
//! retains its builds and exposes them through a [`PageMapProvider`]: the
//! page map of a build, its rendered page images (SVG, PNG, ...), and the
//! texts of the document revisions it compiled. Page commands consult the
//! providers by build id; nothing here is format-specific. Format-specific
//! input rules implement [`crate::PageInputRules`].

use std::sync::Arc;

use exposition_core::document::DocumentId;

use crate::map::PageMap;

/// Largest raster a provider renders for one page image: 32 Mi pixels
/// (128 MiB as RGBA, an A4 page at 8 px/pt). The document chooses its page
/// size, so a page that would exceed this at the requested scale is rendered
/// at a lower scale ([`capped_raster_scale`]). Vector images (Typst's SVG)
/// are not rasterized by the app; their size does not grow with the page.
pub const MAX_PAGE_IMAGE_PIXELS: u64 = 32 * 1024 * 1024;

/// The scale (pixels per point) at which to raster a `width_pt` × `height_pt`
/// page requested at `scale`, and whether it was lowered to stay within
/// [`MAX_PAGE_IMAGE_PIXELS`].
pub fn capped_raster_scale(width_pt: f64, height_pt: f64, scale: f32) -> (f32, bool) {
    let pixels = |s: f64| (width_pt * s).ceil().max(1.0) * (height_pt * s).ceil().max(1.0);
    let requested = f64::from(scale);
    if !(width_pt.is_finite() && height_pt.is_finite()) || pixels(requested) <= MAX_PAGE_IMAGE_PIXELS as f64 {
        return (scale, false);
    }
    // Shrink by the area ratio, then step down until the rounded size fits.
    let mut capped = requested * (MAX_PAGE_IMAGE_PIXELS as f64 / pixels(requested)).sqrt();
    while capped > 0.0 && pixels(capped) > MAX_PAGE_IMAGE_PIXELS as f64 {
        capped *= 0.99;
    }
    (capped as f32, true)
}

/// A rendered page image.
#[derive(Clone, Debug)]
pub struct PageImage {
    /// Media type, e.g. `image/svg+xml` or `image/png`.
    pub media_type: &'static str,
    pub bytes: Arc<[u8]>,
}

pub trait PageMapProvider: Send + Sync {
    /// The page map of a retained build (`None` if unknown to this provider).
    fn page_map(&self, build: &str) -> Option<Arc<PageMap>>;

    /// A rendered page by its `PageArtifact::render_ref`.
    fn page_image(&self, render_ref: &str) -> Option<PageImage>;

    /// The text of `document` at `revision`, if a retained build compiled it
    /// (lets page edits based on a map's revision be planned and rebased).
    fn revision_text(&self, document: &DocumentId, revision: u64) -> Option<Arc<str>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixels(width_pt: f64, height_pt: f64, scale: f32) -> u64 {
        let s = f64::from(scale);
        ((width_pt * s).ceil() * (height_pt * s).ceil()) as u64
    }

    #[test]
    fn ordinary_pages_keep_their_scale() {
        assert_eq!(capped_raster_scale(595.0, 842.0, 3.0), (3.0, false));
        // An A4 page at the largest page-view zoom still fits.
        assert_eq!(capped_raster_scale(595.0, 842.0, 8.0), (8.0, false));
    }

    #[test]
    fn huge_pages_are_rendered_at_a_lower_scale_within_the_cap() {
        // pdfTeX's \maxdimen square page at the page-view scale (~9.7 GB as RGBA).
        for (w, h, scale) in [(16383.0, 16383.0, 3.0), (4000.0, 4000.0, 3.0), (16383.0, 100.0, 8.0)] {
            let (capped, lowered) = capped_raster_scale(w, h, scale);
            assert!(lowered, "{w}x{h} at {scale}");
            assert!(capped > 0.0 && capped < scale);
            assert!(pixels(w, h, capped) <= MAX_PAGE_IMAGE_PIXELS, "{w}x{h}: {} px", pixels(w, h, capped));
            // Not needlessly small: within a few percent of the cap.
            assert!(pixels(w, h, capped) as f64 > MAX_PAGE_IMAGE_PIXELS as f64 * 0.95);
        }
    }
}
