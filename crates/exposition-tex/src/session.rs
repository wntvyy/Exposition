//! The pages of one finished build: rendering, glyph extraction, mapping.

use crate::compile::TexBuild;
use crate::mapping::{PageMapping, SourceCoverage, SourceSet, map_page, source_coverage};
use crate::pdf::{PdfDocument, PdfError, RenderedPage};
use crate::synctex::{SyncTex, SyncTexError};

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("the build produced no output")]
    NoOutput,
    #[error("the build produced no SyncTeX data")]
    NoSyncTex,
    #[error(transparent)]
    Pdf(#[from] PdfError),
    #[error(transparent)]
    SyncTex(#[from] SyncTexError),
    #[error("could not read build output: {0}")]
    Io(#[from] std::io::Error),
}

/// Rendering and mapping access to one build's immutable output.
pub struct TexPageSession {
    pub pdf: PdfDocument,
    pub synctex: SyncTex,
    pub sources: SourceSet,
}

impl TexPageSession {
    pub fn open(build: &TexBuild) -> Result<Self, SessionError> {
        let output = build.output.as_ref().ok_or(SessionError::NoOutput)?;
        let snapshot = build.snapshot.as_ref().ok_or(SessionError::NoOutput)?;
        let synctex_path = output.synctex.as_ref().ok_or(SessionError::NoSyncTex)?;
        let pdf = PdfDocument::open(std::fs::read(&output.pdf)?)?;
        let synctex = SyncTex::read(synctex_path)?;
        let sources = snapshot.source_set(&synctex);
        Ok(Self { pdf, synctex, sources })
    }

    pub fn page_count(&self) -> usize {
        self.pdf.page_count()
    }

    pub fn render_page(&self, index: usize, scale: f32) -> Result<RenderedPage, PdfError> {
        self.pdf.render_page(index, scale)
    }

    pub fn map_page(&self, index: usize) -> Result<PageMapping, PdfError> {
        let glyphs = self.pdf.extract_glyphs(index)?;
        Ok(map_page(index as u32, glyphs, &self.synctex, &self.sources))
    }

    pub fn map_all(&self) -> Result<Vec<PageMapping>, PdfError> {
        (0..self.page_count()).map(|i| self.map_page(i)).collect()
    }

    pub fn coverage(&self, pages: &[PageMapping]) -> SourceCoverage {
        source_coverage(pages, &self.sources)
    }
}
