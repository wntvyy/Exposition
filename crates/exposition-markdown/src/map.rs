//! The source map between the Markdown source and the generated Typst source.
//!
//! The generated text is covered, without gaps, by [`Segment`]s in generated
//! order. Each segment links a generated UTF-16 range to a Markdown UTF-16
//! range:
//!
//! - [`SegmentKind::Text`], [`SegmentKind::MathText`] and [`SegmentKind::Raw`]
//!   segments with equal lengths are *linear*: every offset inside maps to
//!   the offset at the same distance on the other side (the characters are
//!   identical).
//! - [`SegmentKind::Atom`] and [`SegmentKind::MathAtom`] segments are one
//!   rendered unit spelled differently on each side (a Markdown escape or
//!   entity, a Typst escape, `\alpha` → `α`, a soft line break). Only their
//!   boundaries correspond.
//! - [`SegmentKind::Syntax`] and [`SegmentKind::Unsupported`] segments are
//!   scaffolding (template, function calls, delimiters, markers). They map to
//!   the Markdown construct that owns them for navigation only.
//!
//! The Markdown ranges of all non-syntax segments are disjoint, so each
//! Markdown character is rendered at most once.

use exposition_core::text::TextRange;

/// How typed input is interpreted at a position (mirrors the page editor's
/// `EditMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EditMode {
    /// Prose/markup.
    Markup,
    /// Mathematics (the declared LaTeX subset).
    Math,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SegmentKind {
    /// Prose characters copied unchanged.
    Text,
    /// One rendered prose unit spelled differently on each side.
    Atom,
    /// Mathematics characters copied unchanged (letters, digits, operators).
    MathText,
    /// One rendered mathematics unit spelled differently (`\alpha` → `α`).
    MathAtom,
    /// Code, raw HTML and other verbatim content (navigation only).
    Raw,
    /// Template, markup scaffolding, numbering: navigation to the owner only.
    Syntax,
    /// Visible marker for content outside the profile (navigation only).
    Unsupported,
}

impl SegmentKind {
    /// The page edit mode of rendered content from this segment, or `None`
    /// when it is not editable on the page.
    pub fn edit_mode(self) -> Option<EditMode> {
        match self {
            SegmentKind::Text | SegmentKind::Atom => Some(EditMode::Markup),
            SegmentKind::MathText | SegmentKind::MathAtom => Some(EditMode::Math),
            SegmentKind::Raw | SegmentKind::Syntax | SegmentKind::Unsupported => None,
        }
    }

    pub fn is_syntax(self) -> bool {
        matches!(self, SegmentKind::Syntax | SegmentKind::Unsupported)
    }
}

/// One generated range and the Markdown range it comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    /// UTF-16 range in the generated Typst source.
    pub generated: TextRange,
    /// UTF-16 range in the Markdown source (the owning construct for syntax).
    pub markdown: TextRange,
    pub kind: SegmentKind,
}

impl Segment {
    /// Whether offsets inside the segment correspond one to one.
    pub fn is_linear(&self) -> bool {
        matches!(self.kind, SegmentKind::Text | SegmentKind::MathText | SegmentKind::Raw)
            && self.generated.len() == self.markdown.len()
    }

    pub fn is_editable(&self) -> bool {
        self.kind.edit_mode().is_some()
    }
}

/// A generated offset mapped to Markdown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MappedOffset {
    pub markdown: u32,
    pub kind: SegmentKind,
    /// Page edit mode at this position, `None` where it is not editable.
    pub mode: Option<EditMode>,
    /// False when the generated offset fell inside an atomic unit or syntax
    /// and was snapped to the unit's start.
    pub exact: bool,
}

/// A generated range mapped to Markdown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MappedRange {
    pub markdown: TextRange,
    /// Kind of the segment at the start of the range.
    pub kind: SegmentKind,
    /// Edit mode when every segment the range touches is editable in that
    /// mode; `None` otherwise.
    pub mode: Option<EditMode>,
}

#[derive(Clone, Debug, Default)]
pub struct SourceMap {
    segments: Vec<Segment>,
    /// Indices of non-syntax segments sorted by Markdown start.
    by_markdown: Vec<u32>,
    generated_len: u32,
    markdown_len: u32,
}

impl SourceMap {
    pub(crate) fn new(segments: Vec<Segment>, generated_len: u32, markdown_len: u32) -> Self {
        let mut by_markdown: Vec<u32> =
            (0..segments.len() as u32).filter(|&i| !segments[i as usize].kind.is_syntax()).collect();
        by_markdown.sort_by_key(|&i| {
            let s = &segments[i as usize];
            (s.markdown.start, s.markdown.end)
        });
        Self { segments, by_markdown, generated_len, markdown_len }
    }

    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    pub fn generated_len(&self) -> u32 {
        self.generated_len
    }

    pub fn markdown_len(&self) -> u32 {
        self.markdown_len
    }

    /// Index of the segment containing generated offset `g` (start-inclusive).
    fn index_at(&self, g: u32) -> Option<usize> {
        let i = self.segments.partition_point(|s| s.generated.end <= g);
        (i < self.segments.len() && self.segments[i].generated.start <= g).then_some(i)
    }

    /// The segment containing the generated character at `g`.
    pub fn segment_at_generated(&self, g: u32) -> Option<&Segment> {
        self.index_at(g).map(|i| &self.segments[i])
    }

    /// Map a generated offset (a caret position) to Markdown. At a boundary
    /// between two segments the editable one is preferred, then the one
    /// starting at `g`.
    pub fn generated_to_markdown(&self, g: u32) -> Option<MappedOffset> {
        if g > self.generated_len {
            return None;
        }
        let after = self.index_at(g);
        let before = g.checked_sub(1).and_then(|p| self.index_at(p)).filter(|&i| Some(i) != after);
        let pick = |i: usize, at_end: bool| -> MappedOffset {
            let s = &self.segments[i];
            let (markdown, exact) = if s.is_linear() {
                (s.markdown.start + (g - s.generated.start), true)
            } else if s.kind.is_syntax() {
                (s.markdown.start, false)
            } else if g == s.generated.start {
                (s.markdown.start, true)
            } else if at_end || g == s.generated.end {
                (s.markdown.end, true)
            } else {
                (s.markdown.start, false)
            };
            MappedOffset { markdown, kind: s.kind, mode: s.kind.edit_mode(), exact }
        };
        let candidates = [after.map(|i| (i, false)), before.map(|i| (i, true))];
        candidates
            .iter()
            .flatten()
            .find(|(i, _)| self.segments[*i].is_editable())
            .or_else(|| candidates.iter().flatten().next())
            .map(|&(i, at_end)| pick(i, at_end))
    }

    /// Map a generated range (e.g. the source range of a rendered glyph) to
    /// the Markdown range it was produced from.
    pub fn generated_range_to_markdown(&self, range: TextRange) -> Option<MappedRange> {
        if range.end > self.generated_len || range.start > range.end {
            return None;
        }
        let first = self.index_at(range.start)?;
        let last = if range.is_empty() { first } else { self.index_at(range.end - 1)? };
        let a = &self.segments[first];
        let b = &self.segments[last];
        let start = if a.is_linear() { a.markdown.start + (range.start - a.generated.start) } else { a.markdown.start };
        let end = if b.is_linear() {
            b.markdown.start + (range.end - b.generated.start)
        } else if range.is_empty() {
            start
        } else {
            b.markdown.end
        };
        let mode = a.kind.edit_mode().filter(|&mode| {
            self.segments[first..=last].iter().all(|s| s.kind.edit_mode() == Some(mode) || is_blank_syntax(s))
        });
        if end < start {
            return None;
        }
        Some(MappedRange { markdown: TextRange::new(start, end), kind: a.kind, mode })
    }

    /// Generated offsets corresponding to Markdown offset `m` (several when a
    /// boundary is shared by two segments; none inside an atomic unit or in
    /// Markdown syntax that is not rendered).
    pub fn markdown_to_generated(&self, m: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let i = self.by_markdown.partition_point(|&i| self.segments[i as usize].markdown.end < m);
        for &idx in &self.by_markdown[i..] {
            let s = &self.segments[idx as usize];
            if s.markdown.start > m {
                break;
            }
            if s.is_linear() {
                out.push(s.generated.start + (m - s.markdown.start));
            } else if m == s.markdown.start {
                out.push(s.generated.start);
            } else if m == s.markdown.end {
                out.push(s.generated.end);
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Generated ranges rendering the Markdown range `range` (rendered parts
    /// only; atomic units count when they lie fully inside).
    pub fn markdown_range_to_generated(&self, range: TextRange) -> Vec<TextRange> {
        let mut out: Vec<TextRange> = Vec::new();
        let i = self.by_markdown.partition_point(|&i| self.segments[i as usize].markdown.end <= range.start);
        for &idx in &self.by_markdown[i..] {
            let s = &self.segments[idx as usize];
            if s.markdown.start >= range.end && !(range.is_empty() && s.markdown.start == range.start) {
                break;
            }
            let piece = if s.is_linear() {
                let a = s.markdown.start.max(range.start);
                let b = s.markdown.end.min(range.end);
                if a > b {
                    continue;
                }
                TextRange::new(s.generated.start + (a - s.markdown.start), s.generated.start + (b - s.markdown.start))
            } else if s.markdown.start >= range.start && s.markdown.end <= range.end {
                s.generated
            } else {
                continue;
            };
            if piece.is_empty() && !range.is_empty() {
                continue;
            }
            match out.last_mut() {
                Some(last) if last.end == piece.start => last.end = piece.end,
                _ => out.push(piece),
            }
        }
        out
    }
}

/// Whitespace-only scaffolding between editable segments (e.g. separators
/// in mathematics) does not break the edit mode of a range.
fn is_blank_syntax(s: &Segment) -> bool {
    s.kind == SegmentKind::Syntax && s.generated.len() <= 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(g: (u32, u32), m: (u32, u32), kind: SegmentKind) -> Segment {
        Segment { generated: TextRange::new(g.0, g.1), markdown: TextRange::new(m.0, m.1), kind }
    }

    #[test]
    fn linear_atomic_and_syntax_lookups() {
        // Markdown "*a\*b*" -> Typst "#emph[a\*b]"
        let map = SourceMap::new(
            vec![
                seg((0, 6), (0, 6), SegmentKind::Syntax),
                seg((6, 7), (1, 2), SegmentKind::Text),
                seg((7, 9), (2, 4), SegmentKind::Atom),
                seg((9, 10), (4, 5), SegmentKind::Text),
                seg((10, 11), (0, 6), SegmentKind::Syntax),
            ],
            11,
            6,
        );
        let at = |g| map.generated_to_markdown(g).unwrap();
        assert_eq!(at(6).markdown, 1);
        assert_eq!(at(6).mode, Some(EditMode::Markup));
        assert_eq!(at(7).markdown, 2);
        assert_eq!(
            at(8),
            MappedOffset { markdown: 2, kind: SegmentKind::Atom, mode: Some(EditMode::Markup), exact: false }
        );
        assert_eq!(at(9).markdown, 4);
        assert_eq!(at(10).markdown, 5);
        assert_eq!(at(3).mode, None);
        let r = map.generated_range_to_markdown(TextRange::new(7, 9)).unwrap();
        assert_eq!(r.markdown, TextRange::new(2, 4));
        assert_eq!(r.kind, SegmentKind::Atom);
        let r = map.generated_range_to_markdown(TextRange::new(6, 10)).unwrap();
        assert_eq!((r.markdown, r.mode), (TextRange::new(1, 5), Some(EditMode::Markup)));
        assert_eq!(map.markdown_to_generated(2), vec![7]);
        assert_eq!(map.markdown_to_generated(3), Vec::<u32>::new());
        assert_eq!(map.markdown_to_generated(4), vec![9]);
        assert_eq!(map.markdown_range_to_generated(TextRange::new(1, 5)), vec![TextRange::new(6, 10)]);
        assert_eq!(map.markdown_range_to_generated(TextRange::new(1, 3)), vec![TextRange::new(6, 7)]);
    }
}
