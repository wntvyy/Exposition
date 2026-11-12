//! Generated-text builder: appends Typst source while recording segments.

use std::ops::Range;

use exposition_core::text::TextRange;

use crate::map::{Segment, SegmentKind};

/// UTF-16 offsets of every char boundary of a text.
pub(crate) struct Utf16Index {
    /// `units[b]` is the UTF-16 offset of byte `b` (valid at char boundaries).
    units: Vec<u32>,
}

impl Utf16Index {
    pub(crate) fn new(text: &str) -> Self {
        let mut units = vec![u32::MAX; text.len() + 1];
        let mut u = 0u32;
        for (b, ch) in text.char_indices() {
            units[b] = u;
            u += ch.len_utf16() as u32;
        }
        units[text.len()] = u;
        Self { units }
    }

    pub(crate) fn at(&self, byte: usize) -> u32 {
        let v = self.units[byte];
        debug_assert!(v != u32::MAX, "byte offset {byte} is not a char boundary");
        v
    }

    pub(crate) fn range(&self, bytes: &Range<usize>) -> TextRange {
        TextRange::new(self.at(bytes.start), self.at(bytes.end))
    }

    pub(crate) fn len(&self) -> u32 {
        *self.units.last().unwrap_or(&0)
    }
}

/// Where the next markup character lands, for Typst's line-start rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineState {
    /// Only whitespace since a newline or the start of a content block.
    Start,
    /// Only ASCII digits since the line start (a possible `1.` enum marker).
    Digits,
    Other,
}

/// Characters Typst always interprets in markup.
const ALWAYS_ESCAPE: &[char] = &['\\', '#', '$', '[', ']', '`', '<', '@', '*', '_', '~'];

pub(crate) struct Emitter<'s> {
    pub(crate) md: &'s str,
    pub(crate) md16: Utf16Index,
    out: String,
    out16: u32,
    segments: Vec<Segment>,
    line: LineState,
    /// The last thing emitted in markup ended an embedded code expression,
    /// which `(`, `[` or `.` would continue.
    after_code: bool,
}

impl<'s> Emitter<'s> {
    pub(crate) fn new(md: &'s str) -> Self {
        Self {
            md,
            md16: Utf16Index::new(md),
            out: String::with_capacity(md.len() * 2 + 4096),
            out16: 0,
            segments: Vec::new(),
            line: LineState::Start,
            after_code: false,
        }
    }

    pub(crate) fn finish(self) -> (String, Vec<Segment>, u32) {
        let len = self.out16;
        (self.out, self.segments, len)
    }

    pub(crate) fn generated_len(&self) -> u32 {
        self.out16
    }

    fn push_raw(&mut self, text: &str, markdown: TextRange, kind: SegmentKind) {
        if text.is_empty() {
            return;
        }
        let start = self.out16;
        let len: u32 = text.chars().map(|c| c.len_utf16() as u32).sum();
        self.out.push_str(text);
        self.out16 += len;
        let generated = TextRange::new(start, start + len);
        // Merge with the previous segment when both sides continue it.
        if let Some(last) = self.segments.last_mut()
            && last.kind == kind
            && last.generated.end == start
        {
            let mergeable = match kind {
                SegmentKind::Text | SegmentKind::MathText | SegmentKind::Raw => {
                    last.is_linear() && last.markdown.end == markdown.start && generated.len() == markdown.len()
                }
                SegmentKind::Syntax => last.markdown == markdown,
                _ => false,
            };
            if mergeable {
                last.generated.end = generated.end;
                if kind != SegmentKind::Syntax {
                    last.markdown.end = markdown.end;
                }
                return;
            }
        }
        self.segments.push(Segment { generated, markdown, kind });
    }

    /// Emit generated scaffolding owned by Markdown bytes `owner`.
    pub(crate) fn syntax(&mut self, text: &str, owner: &Range<usize>) {
        let range = self.md16.range(owner);
        self.push_raw(text, range, SegmentKind::Syntax);
        self.track(text);
    }

    /// Emit template text that belongs to no Markdown construct.
    pub(crate) fn template(&mut self, text: &str) {
        self.push_raw(text, TextRange::empty(0), SegmentKind::Syntax);
        self.track(text);
    }

    /// Emit generated text for an unsupported construct at `owner`.
    pub(crate) fn unsupported(&mut self, text: &str, owner: &Range<usize>) {
        let range = self.md16.range(owner);
        self.push_raw(text, range, SegmentKind::Unsupported);
        self.track(text);
    }

    /// Emit a segment of any kind, mapped to Markdown bytes `md`.
    pub(crate) fn mapped(&mut self, text: &str, md: &Range<usize>, kind: SegmentKind) {
        let range = self.md16.range(md);
        self.push_raw(text, range, kind);
        self.track(text);
    }

    /// Mark that an embedded code expression (`#f(..)[..]`) just ended.
    pub(crate) fn end_code(&mut self) {
        self.after_code = true;
    }

    /// Mark the start of markup content (content block, after a newline).
    pub(crate) fn line_start(&mut self) {
        self.line = LineState::Start;
    }

    fn track(&mut self, text: &str) {
        for c in text.chars() {
            self.line = match (self.line, c) {
                (_, '\n') => LineState::Start,
                (LineState::Start, c) if c == ' ' || c == '\t' => LineState::Start,
                (LineState::Start | LineState::Digits, c) if c.is_ascii_digit() => LineState::Digits,
                _ => LineState::Other,
            };
        }
        if !text.is_empty() {
            self.after_code = false;
        }
    }

    /// Whether markup character `c` must be escaped here. `next` is the next
    /// Markdown-rendered character when known in the same text run.
    fn needs_escape(&self, c: char, next: Option<char>) -> bool {
        if ALWAYS_ESCAPE.contains(&c) {
            return true;
        }
        let at_start = self.line == LineState::Start;
        match c {
            // `//` and `/*` start comments (and stop `http://` auto-links);
            // `/ ` at a line start is a term marker.
            '/' => at_start || matches!(next, None | Some('/' | '*')),
            // `-?` soft hyphen, `-1` minus sign, `- ` list marker.
            '-' => at_start || next.is_none_or(|n| n == '?' || n.is_numeric()),
            '+' | '=' => at_start,
            // `1. ` enum marker; `#f[..].x` field access.
            '.' => {
                (self.line == LineState::Digits && next.is_none_or(char::is_whitespace))
                    || (self.after_code && next.is_none_or(|n| n.is_alphabetic() || n == '_'))
            }
            '(' => self.after_code,
            _ => false,
        }
    }

    /// Emit one rendered Markdown character `c` taken from Markdown bytes
    /// `md` (the character itself, or an escape/entity spelling of it).
    /// `linear` says the Markdown spelling is exactly `c`.
    pub(crate) fn markup_char(&mut self, c: char, next: Option<char>, md: &Range<usize>, linear: bool) {
        self.markup_char_as(c, next, md, linear, false);
    }

    /// [`Self::markup_char`] with mathematics segment kinds when `math`
    /// (markup text inside an equation, e.g. `\text{...}`).
    pub(crate) fn markup_char_as(&mut self, c: char, next: Option<char>, md: &Range<usize>, linear: bool, math: bool) {
        let (text_kind, atom_kind) =
            if math { (SegmentKind::MathText, SegmentKind::MathAtom) } else { (SegmentKind::Text, SegmentKind::Atom) };
        if c == '\n' || c == '\r' {
            self.mapped(" ", md, atom_kind);
            return;
        }
        let escape = self.needs_escape(c, next);
        let mut buf = [0u8; 4];
        if escape {
            let mut s = String::with_capacity(5);
            s.push('\\');
            s.push(c);
            self.mapped(&s, md, atom_kind);
        } else {
            let kind = if linear { text_kind } else { atom_kind };
            self.mapped(c.encode_utf8(&mut buf), md, kind);
        }
    }

    /// Emit a decoded string as one atomic unit (an entity, a code span that
    /// is not a plain copy) with markup escaping.
    pub(crate) fn markup_atom(&mut self, text: &str, md: &Range<usize>) {
        let mut s = String::with_capacity(text.len() + 4);
        let chars: Vec<char> = text.chars().collect();
        for (i, &c) in chars.iter().enumerate() {
            if c == '\n' || c == '\r' {
                s.push(' ');
                continue;
            }
            if self.needs_escape(c, chars.get(i + 1).copied()) {
                s.push('\\');
            }
            s.push(c);
        }
        if s.is_empty() {
            return;
        }
        self.mapped(&s, md, SegmentKind::Atom);
    }

    /// Emit a Typst string literal whose characters map to Markdown bytes:
    /// `pieces` are (char, markdown byte range) pairs. Characters that need a
    /// string escape become non-linear raw segments.
    pub(crate) fn string_literal(&mut self, pieces: &[(char, Range<usize>)], owner: &Range<usize>) {
        self.syntax("\"", owner);
        for (c, md) in pieces {
            let mut buf = [0u8; 4];
            let text: &str = match c {
                '"' => "\\\"",
                '\\' => "\\\\",
                '\r' => "\\r",
                '\t' => "\\t",
                c => c.encode_utf8(&mut buf),
            };
            self.mapped(text, md, SegmentKind::Raw);
        }
        self.syntax("\"", owner);
    }
}

/// A Typst string literal for generated (unmapped) text.
pub(crate) fn typst_string(text: &str) -> String {
    let mut s = String::with_capacity(text.len() + 2);
    s.push('"');
    for c in text.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            '\n' => s.push_str("\\n"),
            '\r' => s.push_str("\\r"),
            '\t' => s.push_str("\\t"),
            c => s.push(c),
        }
    }
    s.push('"');
    s
}
