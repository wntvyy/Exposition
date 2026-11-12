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
