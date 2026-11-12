//! Attachment marker blocks (stub; implemented separately).

use super::{Attachment, Utf16Index};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerFormat {
    Typst,
    Latex,
    Markdown,
}

pub fn scan(_format: MarkerFormat, _text: &str, _index: &Utf16Index<'_>) -> Vec<Attachment> {
    Vec::new()
}
