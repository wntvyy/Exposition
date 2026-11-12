//! Typesetting: format adapters, immutable build snapshots, the Typst adapter
//! (compile, diagnostics, SVG pages, PDF export, glyph maps), and the service
//! that retains build records for page views.

pub mod fonts;
pub mod glyphs;
pub mod markdown;
pub mod service;
pub mod snapshot;
pub mod typst_adapter;

use exposition_core::build::{DocumentFormat, Engine};
use exposition_page::PageInputRules;

pub use fonts::Fonts;
pub use markdown::{MarkdownAdapter, MarkdownInput};
pub use service::{BuildRecord, BuildTimings, TypesettingError, TypesettingService};
pub use snapshot::{BuildSnapshot, ReadFilter};

/// A format adapter: what a document format contributes to building and page
/// editing. LaTeX and Markdown adapters implement the same interface.
pub trait FormatAdapter: Send + Sync {
    fn format(&self) -> DocumentFormat;
    fn engine(&self) -> Engine;
    /// Page input rules (escaping, structure-preserving source patches).
    fn input_rules(&self) -> &dyn PageInputRules;
    /// Whether a workspace file is an entry document of this format.
    fn handles(&self, path: &exposition_core::path::WorkspacePath) -> bool;
}

/// The Typst adapter.
#[derive(Debug, Default, Clone, Copy)]
pub struct TypstAdapter;

impl FormatAdapter for TypstAdapter {
    fn format(&self) -> DocumentFormat {
        DocumentFormat::Typst
    }

    fn engine(&self) -> Engine {
        Engine::Typst
    }

    fn input_rules(&self) -> &dyn PageInputRules {
        &exposition_page::TypstInput
    }

    fn handles(&self, path: &exposition_core::path::WorkspacePath) -> bool {
        path.extension().is_some_and(|e| e.eq_ignore_ascii_case("typ"))
    }
}
