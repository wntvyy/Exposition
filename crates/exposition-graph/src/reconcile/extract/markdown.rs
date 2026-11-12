//! markdown extractor (stub; implemented separately).

use exposition_core::path::WorkspacePath;

use super::Extraction;

pub const EXTRACTOR: &str = "markdown/1";

pub fn extract(_path: &WorkspacePath, _text: &str) -> Extraction {
    Extraction { extractor: EXTRACTOR.into(), ..Default::default() }
}
