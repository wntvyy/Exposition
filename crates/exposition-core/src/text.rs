//! Text positions.
//!
//! Positions at the application boundary are UTF-16 code-unit offsets over the
//! exact current text (matching JavaScript strings and Monaco). Conversions to
//! Rust byte offsets, compiler spans, and LSP positions are explicit and reject
//! offsets that fall inside a code point or beyond the text.

use std::ops::Range;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Half-open range `[start, end)` in UTF-16 code units.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TextRange {
    pub start: u32,
    pub end: u32,
}

impl TextRange {
    pub const fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    pub const fn empty(at: u32) -> Self {
        Self { start: at, end: at }
    }

    pub const fn len(&self) -> u32 {
        self.end.saturating_sub(self.start)
    }

    pub const fn is_empty(&self) -> bool {
        self.start == self.end
    }

    pub const fn is_ordered(&self) -> bool {
        self.start <= self.end
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TextError {
    #[error("offset {offset} is beyond the text length {len}")]
    OutOfBounds { offset: usize, len: usize },
    #[error("offset {offset} falls inside a code point")]
    NotCharBoundary { offset: usize },
    #[error("range start {start} is after end {end}")]
    Inverted { start: usize, end: usize },
}

/// Length of `text` in UTF-16 code units.
pub fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// Convert a UTF-16 offset to a byte offset, rejecting offsets inside a
/// surrogate pair or beyond the end of the text.
pub fn utf16_to_byte(text: &str, offset: usize) -> Result<usize, TextError> {
    let mut units = 0usize;
    for (byte, ch) in text.char_indices() {
        if units == offset {
            return Ok(byte);
        }
        let width = ch.len_utf16();
        if units + width > offset {
            return Err(TextError::NotCharBoundary { offset });
        }
        units += width;
    }
    if units == offset { Ok(text.len()) } else { Err(TextError::OutOfBounds { offset, len: units }) }
}

/// Convert a byte offset to a UTF-16 offset, rejecting non-boundary offsets.
pub fn byte_to_utf16(text: &str, byte: usize) -> Result<usize, TextError> {
    if byte > text.len() {
        return Err(TextError::OutOfBounds { offset: byte, len: text.len() });
    }
    if !text.is_char_boundary(byte) {
        return Err(TextError::NotCharBoundary { offset: byte });
    }
    Ok(utf16_len(&text[..byte]))
}

/// Convert a UTF-16 range to a byte range over `text`.
pub fn utf16_range_to_bytes(text: &str, range: TextRange) -> Result<Range<usize>, TextError> {
    if !range.is_ordered() {
        return Err(TextError::Inverted { start: range.start as usize, end: range.end as usize });
    }
    let start = utf16_to_byte(text, range.start as usize)?;
    let rest = &text[start..];
    let end = start
        + utf16_to_byte(rest, (range.end - range.start) as usize).map_err(|e| match e {
            TextError::OutOfBounds { .. } => {
                TextError::OutOfBounds { offset: range.end as usize, len: utf16_len(text) }
            }
            TextError::NotCharBoundary { .. } => TextError::NotCharBoundary { offset: range.end as usize },
            other => other,
        })?;
    Ok(start..end)
}

/// Convert a byte range over `text` to a UTF-16 range.
pub fn byte_range_to_utf16(text: &str, range: Range<usize>) -> Result<TextRange, TextError> {
    if range.start > range.end {
        return Err(TextError::Inverted { start: range.start, end: range.end });
    }
    let start = byte_to_utf16(text, range.start)?;
    let end = start + byte_to_utf16(&text[range.start..], range.end - range.start)?;
    Ok(TextRange::new(to_u32(start), to_u32(end)))
}

fn to_u32(value: usize) -> u32 {
    u32::try_from(value).expect("document text exceeds u32 UTF-16 code units")
}

/// Zero-based line/column position. `character` is in UTF-16 code units.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LineColumn {
    pub line: u32,
    pub character: u32,
}

/// Line index over a text, recognising `\n`, `\r\n`, and lone `\r` terminators
/// (as LSP does). Offsets are UTF-16 code units.
#[derive(Debug, Clone)]
pub struct LineIndex {
    /// UTF-16 offset where each line starts.
    starts: Vec<usize>,
    /// UTF-16 offset where each line's content ends (excluding the terminator).
    content_ends: Vec<usize>,
    len: usize,
}

impl LineIndex {
    pub fn new(text: &str) -> Self {
        let mut starts = vec![0];
        let mut content_ends = Vec::new();
        let mut units = 0usize;
        let mut chars = text.chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                '\n' => {
                    content_ends.push(units);
                    units += 1;
                    starts.push(units);
                }
                '\r' => {
                    content_ends.push(units);
                    units += 1;
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                        units += 1;
                    }
                    starts.push(units);
                }
                other => units += other.len_utf16(),
            }
        }
        content_ends.push(units);
        Self { starts, content_ends, len: units }
    }

    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    pub fn len_utf16(&self) -> usize {
        self.len
    }

    /// Line/column of a UTF-16 offset. Offsets inside a `\r\n` terminator are
    /// clamped to the end of the line's content.
    pub fn line_column(&self, offset: usize) -> Result<LineColumn, TextError> {
        if offset > self.len {
            return Err(TextError::OutOfBounds { offset, len: self.len });
        }
        let line = match self.starts.binary_search(&offset) {
            Ok(line) => line,
            Err(next) => next - 1,
        };
        let character = offset.min(self.content_ends[line]) - self.starts[line];
        Ok(LineColumn { line: to_u32(line), character: to_u32(character) })
    }

    /// UTF-16 offset of a line/column. Columns past the line content are rejected.
    pub fn offset(&self, position: LineColumn) -> Result<usize, TextError> {
        let line = position.line as usize;
        let Some(&start) = self.starts.get(line) else {
            return Err(TextError::OutOfBounds { offset: line, len: self.starts.len() });
        };
        let offset = start + position.character as usize;
        if offset > self.content_ends[line] {
            return Err(TextError::OutOfBounds { offset, len: self.content_ends[line] });
        }
        Ok(offset)
    }

    pub fn line_start(&self, line: usize) -> Option<usize> {
        self.starts.get(line).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_conversion_handles_astral_characters() {
        let text = "a😀b"; // 😀 is a surrogate pair in UTF-16 and four bytes in UTF-8
        assert_eq!(utf16_len(text), 4);
        assert_eq!(utf16_to_byte(text, 0), Ok(0));
        assert_eq!(utf16_to_byte(text, 1), Ok(1));
        assert_eq!(utf16_to_byte(text, 2), Err(TextError::NotCharBoundary { offset: 2 }));
        assert_eq!(utf16_to_byte(text, 3), Ok(5));
        assert_eq!(utf16_to_byte(text, 4), Ok(6));
        assert_eq!(utf16_to_byte(text, 5), Err(TextError::OutOfBounds { offset: 5, len: 4 }));
        assert_eq!(byte_to_utf16(text, 5), Ok(3));
        assert_eq!(byte_to_utf16(text, 2), Err(TextError::NotCharBoundary { offset: 2 }));
    }

    #[test]
    fn range_conversion_round_trips() {
        let text = "αβ😀γ";
        let range = TextRange::new(1, 4);
        let bytes = utf16_range_to_bytes(text, range).unwrap();
        assert_eq!(&text[bytes.clone()], "β😀");
        assert_eq!(byte_range_to_utf16(text, bytes).unwrap(), range);
        assert!(utf16_range_to_bytes(text, TextRange::new(3, 2)).is_err());
        assert!(utf16_range_to_bytes(text, TextRange::new(0, 3)).is_err());
    }

    #[test]
    fn line_index_recognises_all_terminators() {
        let text = "ab\r\ncd\ref\ng";
        let index = LineIndex::new(text);
        assert_eq!(index.line_count(), 4);
        assert_eq!(index.line_column(0).unwrap(), LineColumn { line: 0, character: 0 });
        assert_eq!(index.line_column(3).unwrap(), LineColumn { line: 0, character: 2 });
        assert_eq!(index.line_column(4).unwrap(), LineColumn { line: 1, character: 0 });
        assert_eq!(index.line_column(7).unwrap(), LineColumn { line: 2, character: 0 });
        assert_eq!(index.line_column(10).unwrap(), LineColumn { line: 3, character: 0 });
        assert_eq!(index.offset(LineColumn { line: 2, character: 2 }).unwrap(), 9);
        assert!(index.offset(LineColumn { line: 0, character: 3 }).is_err());
        assert!(index.line_column(12).is_err());
    }
}
