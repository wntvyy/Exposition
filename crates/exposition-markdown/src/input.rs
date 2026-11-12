//! Page input rules for Markdown: typing, deletion and paragraph breaks at a
//! caret or selection in the Markdown source become minimal Markdown patches.
//!
//! The interface mirrors the page editor's `PageInputRules` (`plan` and
//! `mode_at` with the same signatures and semantics as the Typst rules), so
//! the shared page editor can call either.
//!
//! Typed prose is escaped only where a character would otherwise change
//! meaning (`\`, `` ` ``, `$` always; `#`, `>`, `-`, `+`, `*`, `1.` at a
//! line start; `|` in tables; `<`/`&` before tag or entity text; more when
//! needed). Every plan is verified by re-parsing: the rendered token stream
//! of the new Markdown must equal the old stream with exactly the intended
//! change, and mathematics must not gain errors. Plans that would change
//! structure are retried with stronger escaping and otherwise refused.

use std::ops::Range;

use exposition_core::document::TextPatch;
use exposition_core::text::{TextRange, byte_to_utf16, utf16_range_to_bytes};
use pulldown_cmark::{Event, Parser, Tag};
use unicode_segmentation::UnicodeSegmentation;

use crate::front_matter::{self, FrontMatter, Scalar, ScalarStyle};
pub use crate::map::EditMode;
use crate::math::{self, UnitKind};
use crate::translate::{escaped_at, parser_options};

/// What the user did at the caret or selection (mirrors the page editor's
/// `PageEditOp`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PageEditOp {
    /// Typed or pasted plain text (newlines become paragraph breaks in prose).
    Insert {
        text: String,
    },
    DeleteBackward,
    DeleteForward,
    ParagraphBreak,
}

/// Why an edit was not applied (mirrors the page editor's `EditRefusal`).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EditRefusal {
    #[error("not editable on the page ({reason})")]
    NotEditable { reason: String },
    #[error("the edit would change document structure; edit in source")]
    WouldChangeStructure,
    #[error("invalid position")]
    InvalidPosition,
    #[error("nothing to change")]
    NoChange,
}

/// Planned source change for one page edit (mirrors the page editor's
/// `EditPlan`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditPlan {
    /// Sorted, non-overlapping patches relative to the planned text.
    pub patches: Vec<TextPatch>,
    /// Caret after the edit, as a UTF-16 offset in the new text.
    pub caret: u32,
    pub mode: EditMode,
}

/// Markdown input rules of the print profile.
#[derive(Debug, Default, Clone, Copy)]
pub struct MarkdownInput;

impl MarkdownInput {
    /// Plan `op` at `selection` (UTF-16, either order) over `text`.
    pub fn plan(
        &self,
        text: &str,
        selection: TextRange,
        op: &PageEditOp,
        line_ending: &str,
    ) -> Result<EditPlan, EditRefusal> {
        let start = selection.start.min(selection.end);
        let end = selection.start.max(selection.end);
        let bytes = utf16_range_to_bytes(text, TextRange::new(start, end)).map_err(|_| EditRefusal::InvalidPosition)?;
        let doc = Doc::new(text);
        match op {
            PageEditOp::Insert { text: typed } => doc.insert(bytes, typed, line_ending),
            PageEditOp::ParagraphBreak => doc.paragraph_break(bytes, line_ending),
            PageEditOp::DeleteBackward | PageEditOp::DeleteForward if !bytes.is_empty() => doc.replace(bytes),
            PageEditOp::DeleteBackward => doc.delete_unit(bytes.start, false),
            PageEditOp::DeleteForward => doc.delete_unit(bytes.start, true),
        }
    }

    /// The input mode at an offset, or `None` where page input is not accepted.
    pub fn mode_at(&self, text: &str, offset: u32) -> Option<EditMode> {
        let byte = utf16_range_to_bytes(text, TextRange::empty(offset)).ok()?.start;
        match Doc::new(text).context(byte) {
            Ok(Context::Markup) | Ok(Context::FrontMatter(_)) => Some(EditMode::Markup),
            Ok(Context::Math(_)) => Some(EditMode::Math),
            Err(_) => None,
        }
    }
}

// ------------------------------------------------------------- tokens --

/// One element of the rendered token stream used to verify edits.
#[derive(Clone, Debug, PartialEq)]
enum Tok {
    /// A rendered character.
    Ch(char),
    Space,
    /// Element start/end: tag key and whether it is a block.
    Open(String, bool),
    Close(String, bool),
    /// An equation: block flag and content (`None` when collapsed).
    Eq(bool, Option<String>),
    /// Code spans, HTML, footnote references, task markers, hard breaks,
    /// rules, front matter: compared verbatim.
    Atom(String),
}

#[derive(Clone, Debug)]
struct Token {
    tok: Tok,
    /// Insertion anchor: start (end for `Close`).
    pos: usize,
    /// Source range the token occupies (whole element for Open/Close).
    extent: Range<usize>,
}

impl Token {
    /// Position used when scanning backwards.
    fn back_key(&self) -> usize {
        match self.tok {
            Tok::Open(..) | Tok::Close(..) => self.pos,
            _ => self.extent.end,
        }
    }

    /// Position used when scanning forwards.
    fn fwd_key(&self) -> usize {
        match self.tok {
            Tok::Open(..) | Tok::Close(..) => self.pos,
            _ => self.extent.start,
        }
    }

    /// A plain character token (its source is exactly the character).
    fn plain(&self, text: &str) -> bool {
        matches!(self.tok, Tok::Ch(c) if text[self.extent.clone()].chars().eq(std::iter::once(c)))
    }
}

#[derive(Clone, Debug)]
struct Equation {
    /// Whole equation with delimiters.
    range: Range<usize>,
    content: Range<usize>,
}

#[derive(Clone, Debug)]
struct Block {
    key: String,
    range: Range<usize>,
    parent: Option<usize>,
    kind: BlockKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockKind {
    Paragraph,
    Heading,
    Item,
    List {
        ordered: bool,
    },
    Quote,
    Cell,
    Table,
    Code,
    Html,
    Footnote,
    Meta,
    /// Inline elements (emphasis, links, ...).
    Inline,
    Other,
}

fn tag_key(tag: &Tag) -> (String, BlockKind) {
    let kind = match tag {
        Tag::Paragraph => BlockKind::Paragraph,
        Tag::Heading { .. } => BlockKind::Heading,
        Tag::Item => BlockKind::Item,
        Tag::List(start) => BlockKind::List { ordered: start.is_some() },
        Tag::BlockQuote(_) => BlockKind::Quote,
        Tag::TableCell => BlockKind::Cell,
        Tag::Table(_) | Tag::TableHead | Tag::TableRow => BlockKind::Table,
        Tag::CodeBlock(_) => BlockKind::Code,
        Tag::HtmlBlock => BlockKind::Html,
        Tag::FootnoteDefinition(_) => BlockKind::Footnote,
        Tag::MetadataBlock(_) => BlockKind::Meta,
        Tag::Emphasis
        | Tag::Strong
        | Tag::Strikethrough
        | Tag::Superscript
        | Tag::Subscript
        | Tag::Link { .. }
        | Tag::Image { .. } => BlockKind::Inline,
        _ => BlockKind::Other,
    };
    (format!("{tag:?}"), kind)
}

/// Syntactic context of an insertion point.
#[derive(Clone, Debug)]
enum Context {
    Markup,
    Math(usize),
    FrontMatter(Scalar),
}

struct Doc<'a> {
    text: &'a str,
    tokens: Vec<Token>,
    equations: Vec<Equation>,
    blocks: Vec<Block>,
    front_matter: Option<FrontMatter>,
}

fn tokenize(text: &str) -> (Vec<Token>, Vec<Equation>, Vec<Block>, Option<FrontMatter>) {
    let mut tokens = Vec::new();
    let mut equations = Vec::new();
    let mut blocks: Vec<Block> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut front_matter = None;
    let mut in_meta: Option<Range<usize>> = None;
    for (ev, range) in Parser::new_ext(text, parser_options()).into_offset_iter() {
        match ev {
            Event::Start(tag) => {
                let (key, kind) = tag_key(&tag);
                if kind == BlockKind::Meta {
                    in_meta = Some(range.clone());
                }
                let block = kind != BlockKind::Inline;
                tokens.push(Token { tok: Tok::Open(key.clone(), block), pos: range.start, extent: range.clone() });
                blocks.push(Block { key, range, parent: stack.last().copied(), kind });
                stack.push(blocks.len() - 1);
            }
            Event::End(_) => {
                let i = stack.pop().expect("balanced events");
                let b = &blocks[i];
                if b.kind == BlockKind::Meta {
                    in_meta = None;
                }
                let block = b.kind != BlockKind::Inline;
                tokens.push(Token { tok: Tok::Close(b.key.clone(), block), pos: range.end, extent: range });
            }
            Event::Text(t) if in_meta.is_some() => {
                let block = in_meta.clone().expect("meta");
                front_matter = Some(front_matter::parse(&t, range.start, block));
                tokens.push(Token { tok: Tok::Atom(format!("meta:{t}")), pos: range.start, extent: range });
            }
            Event::Text(t) => {
                let src = &text[range.clone()];
                if src == t.as_ref() {
                    for (i, c) in src.char_indices() {
                        let at = range.start + i;
                        let start = if i == 0 && escaped_at(text, at) { at - 1 } else { at };
                        let tok = if c == ' ' || c == '\t' { Tok::Space } else { Tok::Ch(c) };
                        tokens.push(Token { tok, pos: start, extent: start..at + c.len_utf8() });
                    }
                } else {
                    for c in t.chars() {
                        let tok = if c.is_whitespace() { Tok::Space } else { Tok::Ch(c) };
                        tokens.push(Token { tok, pos: range.start, extent: range.clone() });
                    }
                }
            }
            Event::SoftBreak => tokens.push(Token { tok: Tok::Space, pos: range.start, extent: range }),
            Event::InlineMath(m) | Event::DisplayMath(m) => {
                let block = text[range.clone()].starts_with("$$");
                let d = if block { 2 } else { 1 };
                let content = range.start + d..range.end - d;
                debug_assert_eq!(&text[content.clone()], m.as_ref());
                equations.push(Equation { range: range.clone(), content });
                tokens.push(Token { tok: Tok::Eq(block, Some(m.to_string())), pos: range.start, extent: range });
            }
            Event::Code(c) => {
                tokens.push(Token { tok: Tok::Atom(format!("code:{c}")), pos: range.start, extent: range })
            }
            Event::Html(h) | Event::InlineHtml(h) => {
                tokens.push(Token { tok: Tok::Atom(format!("html:{h}")), pos: range.start, extent: range })
            }
            Event::FootnoteReference(l) => {
                tokens.push(Token { tok: Tok::Atom(format!("fn:{l}")), pos: range.start, extent: range })
            }
            Event::HardBreak => tokens.push(Token { tok: Tok::Atom("br".into()), pos: range.start, extent: range }),
            Event::Rule => tokens.push(Token { tok: Tok::Atom("rule".into()), pos: range.start, extent: range }),
            Event::TaskListMarker(done) => {
                tokens.push(Token { tok: Tok::Atom(format!("task:{done}")), pos: range.start, extent: range })
            }
        }
    }
    (tokens, equations, blocks, front_matter)
}

/// Collapse whitespace runs, drop whitespace at block edges and empty
/// blocks (none of them render).
fn normalize(tokens: impl IntoIterator<Item = Tok>) -> Vec<Tok> {
    let mut out: Vec<Tok> = Vec::new();
    for tok in tokens {
        match tok {
            Tok::Space => match out.last() {
                Some(Tok::Space) | Some(Tok::Open(_, true)) | None => {}
                // The text after a task box starts after its separating space.
                Some(Tok::Atom(a)) if a.starts_with("task:") => {}
                _ => out.push(Tok::Space),
            },
            Tok::Close(key, true) => {
                if out.last() == Some(&Tok::Space) {
                    out.pop();
                }
                if matches!(out.last(), Some(Tok::Open(k, true)) if *k == key) && !key.starts_with("Item") {
                    out.pop();
                } else {
                    out.push(Tok::Close(key, true));
                }
            }
            Tok::Open(key, true) => {
                if out.last() == Some(&Tok::Space) {
                    out.pop();
                }
                out.push(Tok::Open(key, true));
            }
            other => out.push(other),
        }
    }
    out
}

fn collapse_math(tok: &Tok) -> Tok {
    match tok {
        Tok::Eq(block, _) => Tok::Eq(*block, None),
        other => other.clone(),
    }
}

/// Inline syntax openers, escaped everywhere at the second level.
const LEVEL1: &[char] = &['*', '_', '~', '[', ']', '<', '>'];

/// Escape `typed` (no newlines) for insertion into Markdown prose.
fn escape_markdown(
    typed: &str,
    prev: Option<char>,
    next: Option<char>,
    line_start: bool,
    table: bool,
    level: u8,
) -> String {
    let chars: Vec<char> = typed.chars().collect();
    let mut out = String::with_capacity(typed.len() + 4);
    let mut at_start = line_start;
    let mut digits_only = line_start;
    let mut prev = prev;
    for (i, &c) in chars.iter().enumerate() {
        let following = chars.get(i + 1).copied().or(next);
        let space_after = following.is_none_or(|f| f == ' ' || f == '\t');
        let escape = match level {
            0 => match c {
                '\\' | '`' | '$' => true,
                '|' => table,
                '<' => following.is_some_and(|f| f.is_ascii_alphabetic() || matches!(f, '/' | '!' | '?')),
                '&' => following.is_some_and(|f| f.is_ascii_alphanumeric() || f == '#'),
                '#' | '>' => at_start,
                '-' | '+' | '*' => at_start && space_after,
                '=' => at_start,
                '.' | ')' => digits_only && prev.is_some_and(|p| p.is_ascii_digit()) && space_after,
                '~' => prev == Some('~') || following == Some('~'),
                '!' => following == Some('['),
                _ => false,
            },
            1 => {
                LEVEL1.contains(&c)
                    || matches!(c, '\\' | '`' | '$')
                    || (c == '|' && table)
                    || (c == '&' && following.is_some_and(|f| f.is_ascii_alphanumeric() || f == '#'))
                    || (c == '!' && following == Some('['))
                    || (at_start && matches!(c, '#' | '-' | '+' | '='))
                    || (digits_only && prev.is_some_and(|p| p.is_ascii_digit()) && matches!(c, '.' | ')'))
            }
            _ => c.is_ascii_punctuation(),
        };
        if escape {
            out.push('\\');
        }
        out.push(c);
        if c != ' ' && c != '\t' {
            at_start = false;
            if !c.is_ascii_digit() {
                digits_only = false;
            }
        }
        prev = Some(c);
    }
    out
}

fn apply(text: &str, patches: &[(Range<usize>, String)]) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let mut cursor = 0;
    for (range, with) in patches {
        out.push_str(&text[cursor..range.start]);
        out.push_str(with);
        cursor = range.end;
    }
    out.push_str(&text[cursor..]);
    out
}

fn to_u16(text: &str, byte: usize) -> Result<u32, EditRefusal> {
    byte_to_utf16(text, byte).map(|v| v as u32).map_err(|_| EditRefusal::InvalidPosition)
}

fn merge(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.retain(|r| !r.is_empty());
    ranges.sort_by_key(|r| (r.start, r.end));
    let mut merged: Vec<Range<usize>> = Vec::new();
    for r in ranges {
        match merged.last_mut() {
            Some(last) if last.end >= r.start => last.end = last.end.max(r.end),
            _ => merged.push(r),
        }
    }
    merged
}

fn contained(extent: &Range<usize>, deletions: &[Range<usize>]) -> bool {
    deletions.iter().any(|d| !d.is_empty() && extent.start >= d.start && extent.end <= d.end)
}

fn is_control_word_end(before: &str) -> bool {
    let letters = before.bytes().rev().take_while(|b| b.is_ascii_alphabetic()).count();
    letters > 0
        && before[..before.len() - letters].ends_with('\\')
        && !escaped_backslash(&before[..before.len() - letters])
}

/// Whether the trailing backslash of `s` is itself escaped (`\\`).
fn escaped_backslash(s: &str) -> bool {
    let n = s.bytes().rev().take_while(|&b| b == b'\\').count();
    n.is_multiple_of(2)
}

impl<'a> Doc<'a> {
    fn new(text: &'a str) -> Self {
        let (tokens, equations, blocks, front_matter) = tokenize(text);
        Self { text, tokens, equations, blocks, front_matter }
    }

    fn equation_at(&self, p: usize) -> Option<usize> {
        self.equations.iter().position(|e| e.content.start <= p && p <= e.content.end)
    }

    /// Innermost blocks containing `p`, innermost first.
    fn chain(&self, p: usize) -> Vec<usize> {
        let contains = |b: &Block| {
            b.range.start <= p
                && (p < b.range.end || (p == b.range.end && !self.text[..p].ends_with('\n') && p == self.text.len()))
        };
        let mut innermost = None;
        for (i, b) in self.blocks.iter().enumerate() {
            if contains(b) {
                innermost = Some(i);
            }
        }
        let mut out = Vec::new();
        let mut cur = innermost;
        while let Some(i) = cur {
            out.push(i);
            cur = self.blocks[i].parent;
        }
        out
    }

    /// The syntactic context typed text takes at `p`.
    fn context(&self, p: usize) -> Result<Context, EditRefusal> {
        let refuse = |reason: &str| Err(EditRefusal::NotEditable { reason: reason.into() });
        if let Some(fm) = &self.front_matter
            && fm.block.start <= p
            && p < fm.block.end
        {
            for key in ["title", "author", "date"] {
                if let Some(entry) = fm.get(key)
                    && let Some(s) = entry.values.iter().find(|s| s.range.start <= p && p <= s.range.end)
                {
                    return Ok(Context::FrontMatter(s.clone()));
                }
            }
            return refuse("front matter settings");
        }
        if let Some(i) = self.equation_at(p) {
            return Ok(Context::Math(i));
        }
        for t in &self.tokens {
            let inside = t.extent.start < p && p < t.extent.end;
            match &t.tok {
                Tok::Atom(a) if inside => {
                    let what = a.split(':').next().unwrap_or("generated");
                    return refuse(match what {
                        "code" => "code",
                        "html" => "raw HTML",
                        _ => "generated content",
                    });
                }
                Tok::Eq(..) if inside => return refuse("equation delimiters"),
                Tok::Ch(_) if inside && !t.plain(self.text) => return Err(EditRefusal::InvalidPosition),
                _ => {}
            }
        }
        for &b in &self.chain(p) {
            let block = &self.blocks[b];
            match block.kind {
                BlockKind::Code => return refuse("code"),
                BlockKind::Html => return refuse("raw HTML"),
                BlockKind::Inline => {
                    // Strictly inside an opening or closing delimiter run
                    // (`**`, `](url)`), not between content characters.
                    let children: Vec<&Token> = self
                        .tokens
                        .iter()
                        .filter(|t| {
                            t.extent.start >= block.range.start
                                && t.extent.end <= block.range.end
                                && t.extent != block.range
                        })
                        .collect();
                    let first = children.first().map_or(block.range.end, |t| t.extent.start);
                    let last = children.last().map_or(block.range.start, |t| t.extent.end);
                    if (block.range.start < p && p < first) || (last < p && p < block.range.end) {
                        return refuse("link or emphasis syntax");
                    }
                }
                _ => {}
            }
        }
        Ok(Context::Markup)
    }

    /// Whether only whitespace and container markers precede `p` on its line.
    fn at_line_start(&self, p: usize) -> bool {
        let line = self.text[..p].rsplit(['\n', '\r']).next().unwrap_or("");
        let rest = line.trim_start_matches([' ', '\t', '>']);
        if rest.is_empty() {
            return true;
        }
        // After a list marker (and task box).
        let after_marker = rest
            .strip_prefix(['-', '*', '+'])
            .or_else(|| {
                let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
                (digits > 0).then(|| rest[digits..].strip_prefix(['.', ')'])).flatten()
            })
            .map(str::trim_start);
        match after_marker {
            Some(r) => {
                let r = r
                    .strip_prefix("[ ]")
                    .or_else(|| r.strip_prefix("[x]"))
                    .or_else(|| r.strip_prefix("[X]"))
                    .unwrap_or(r);
                r.trim().is_empty() && line.ends_with([' ', '\t'])
            }
            None => false,
        }
    }

    fn in_table(&self, p: usize) -> bool {
        self.chain(p).iter().any(|&b| self.blocks[b].kind == BlockKind::Cell)
    }

    /// Expected tokens: old tokens minus `deletions`, with `inserted` placed
    /// at `at`. With `collapse`, equations compare by kind only.
    fn expected(&self, deletions: &[Range<usize>], at: usize, inserted: &[Tok], collapse: bool) -> Vec<Tok> {
        let mut out = Vec::with_capacity(self.tokens.len() + inserted.len());
        let mut placed = false;
        for t in &self.tokens {
            // Text typed at a block's first or last content position belongs
            // inside the block; at an inline element's edge, outside it.
            let before = match t.tok {
                Tok::Close(_, false) | Tok::Open(_, true) => t.pos <= at,
                _ => t.pos < at,
            };
            // Typing inside a run of whitespace splits it around the text.
            if !placed
                && t.tok == Tok::Space
                && t.extent.start < at
                && at < t.extent.end
                && !contained(&t.extent, deletions)
            {
                out.push(Tok::Space);
                out.extend(inserted.iter().cloned());
                out.push(Tok::Space);
                placed = true;
                continue;
            }
            if !placed && !before {
                out.extend(inserted.iter().cloned());
                placed = true;
            }
            if !contained(&t.extent, deletions) {
                out.push(if collapse { collapse_math(&t.tok) } else { t.tok.clone() });
            }
        }
        if !placed {
            out.extend(inserted.iter().cloned());
        }
        out
    }

    fn verify(&self, new_text: &str, expected: Vec<Tok>, collapse: bool) -> bool {
        let (tokens, ..) = tokenize(new_text);
        let actual = tokens.into_iter().map(|t| if collapse { collapse_math(&t.tok) } else { t.tok });
        normalize(expected) == normalize(actual)
    }

    /// Byte ranges to delete for a selection: rendered units inside it, and
    /// inline elements entirely inside it; delimiters of partly selected
    /// elements survive.
    fn selection_deletions(&self, sel: &Range<usize>) -> Result<Vec<Range<usize>>, EditRefusal> {
        let mut out = Vec::new();
        if let Some(fm) = &self.front_matter
            && sel.start < fm.block.end
            && sel.end > fm.block.start
        {
            return Err(EditRefusal::NotEditable { reason: "front matter".into() });
        }
        for t in &self.tokens {
            let inside = t.extent.start >= sel.start && t.extent.end <= sel.end;
            match &t.tok {
                Tok::Open(_, false) if inside => out.push(t.extent.clone()),
                Tok::Open(..) | Tok::Close(..) => {}
                Tok::Eq(..) => {
                    if inside {
                        out.push(t.extent.clone());
                    } else if t.extent.start < sel.end && sel.start < t.extent.end {
                        // Partly selected equation: delete inside its content only.
                        let eq = self.equations.iter().find(|e| e.range == t.extent).expect("equation");
                        if sel.start >= eq.content.start && sel.end <= eq.content.end {
                            out.extend(self.math_selection(eq, sel));
                        } else {
                            return Err(EditRefusal::WouldChangeStructure);
                        }
                    }
                }
                _ if inside => out.push(t.extent.clone()),
                _ => {}
            }
        }
        Ok(merge(out))
    }

    fn math_selection(&self, eq: &Equation, sel: &Range<usize>) -> Vec<Range<usize>> {
        math::units(self.text, eq.content.clone())
            .into_iter()
            .filter(|u| u.range.start >= sel.start && u.range.end <= sel.end)
            .map(|u| u.range)
            .collect()
    }

    /// Build patches for deleting `deletions` and inserting `source` at `p`;
    /// verify against `expected`; the caret goes after the inserted text
    /// minus `back` bytes.
    #[allow(clippy::too_many_arguments)]
    fn try_plan(
        &self,
        p: usize,
        deletions: &[Range<usize>],
        source: &str,
        expected: Vec<Tok>,
        collapse: bool,
        mode: EditMode,
        back: usize,
        extra_check: &dyn Fn(&str) -> bool,
    ) -> Result<Option<EditPlan>, EditRefusal> {
        let mut byte_patches: Vec<(Range<usize>, String)> = Vec::new();
        let mut inserted = false;
        for d in deletions {
            if !inserted && d.start >= p {
                if d.start == p {
                    byte_patches.push((d.clone(), source.to_owned()));
                    inserted = true;
                    continue;
                }
                if !source.is_empty() {
                    byte_patches.push((p..p, source.to_owned()));
                }
                inserted = true;
            }
            byte_patches.push((d.clone(), String::new()));
        }
        if !inserted && !source.is_empty() {
            byte_patches.push((p..p, source.to_owned()));
            byte_patches.sort_by_key(|(r, _)| r.start);
        }
        if byte_patches.is_empty() {
            return Err(EditRefusal::NoChange);
        }
        let new_text = apply(self.text, &byte_patches);
        if !self.verify(&new_text, expected, collapse) || !extra_check(&new_text) {
            return Ok(None);
        }
        let removed_before: usize = deletions.iter().filter(|d| d.end <= p).map(|d| d.len()).sum();
        let caret_byte = p - removed_before + source.len() - back;
        let caret = to_u16(&new_text, caret_byte)?;
        let patches = byte_patches
            .into_iter()
            .map(|(range, text)| {
                Ok(TextPatch {
                    range: TextRange::new(to_u16(self.text, range.start)?, to_u16(self.text, range.end)?),
                    text,
                })
            })
            .collect::<Result<Vec<_>, EditRefusal>>()?;
        Ok(Some(EditPlan { patches, caret, mode }))
    }

    // ----------------------------------------------------------- insert --

    fn insert(&self, sel: Range<usize>, typed: &str, line_ending: &str) -> Result<EditPlan, EditRefusal> {
        let p = sel.start;
        let context = self.context(p)?;
        let typed = typed.replace("\r\n", "\n").replace('\r', "\n").replace('\t', " ");
        match context {
            Context::Math(eq) => {
                let deletions = if sel.is_empty() {
                    Vec::new()
                } else {
                    let e = &self.equations[eq];
                    if sel.end > e.content.end {
                        return Err(EditRefusal::WouldChangeStructure);
                    }
                    merge(self.math_selection(e, &sel))
                };
                self.insert_math(eq, p, &deletions, &typed)
            }
            Context::FrontMatter(scalar) => self.edit_front_matter(&scalar, sel, &typed),
            Context::Markup => {
                let deletions = if sel.is_empty() { Vec::new() } else { self.selection_deletions(&sel)? };
                let first = self.insert_markup(p, &deletions, &typed, line_ending);
                if !matches!(first, Err(EditRefusal::WouldChangeStructure)) || !deletions.is_empty() {
                    return first;
                }
                // Next to emphasis delimiters the text may only fit inside or
                // outside them (e.g. a space just after an opening `*`).
                for q in self.delimiter_neighbours(p) {
                    if let Ok(plan) = self.insert_markup(q, &deletions, &typed, line_ending) {
                        return Ok(plan);
                    }
                }
                first
            }
        }
    }

    /// Positions across inline delimiter runs (`*`, `_`, `~`) adjacent to `p`.
    fn delimiter_neighbours(&self, p: usize) -> Vec<usize> {
        let is_delim = |c: char| matches!(c, '*' | '_' | '~');
        let rendered = |q: usize| {
            self.tokens
                .iter()
                .any(|t| !matches!(t.tok, Tok::Open(..) | Tok::Close(..)) && t.extent.start <= q && q < t.extent.end)
        };
        let mut out = Vec::new();
        let mut q = p;
        while let Some(c) = self.text[q..].chars().next() {
            if !is_delim(c) || rendered(q) {
                break;
            }
            q += c.len_utf8();
            out.push(q);
        }
        let mut q = p;
        while let Some(c) = self.text[..q].chars().next_back() {
            if !is_delim(c) || rendered(q - c.len_utf8()) {
                break;
            }
            q -= c.len_utf8();
            out.push(q);
        }
        out
    }

    fn insert_markup(
        &self,
        p: usize,
        deletions: &[Range<usize>],
        typed: &str,
        line_ending: &str,
    ) -> Result<EditPlan, EditRefusal> {
        if typed.is_empty() && deletions.is_empty() {
            return Err(EditRefusal::NoChange);
        }
        let chain = self.chain(p);
        let top_paragraph = chain.first().is_some_and(|&b| {
            self.blocks[b].kind == BlockKind::Paragraph
                && self.blocks[b].parent.is_none_or(|q| self.blocks[q].kind == BlockKind::Quote)
        });
        // Newlines become paragraph breaks in paragraphs, spaces elsewhere.
        let typed: String = if top_paragraph { typed.to_owned() } else { typed.replace('\n', " ") };
        let deleted_end = deletions.iter().map(|d| d.end).max().unwrap_or(p).max(p);
        let prev = self.text[..p].chars().next_back();
        let next = self.text[deleted_end..].chars().next();
        let table = self.in_table(p);
        let prefix = chain.first().map(|&b| self.container_prefix(b)).unwrap_or_default();
        let parts: Vec<&str> = typed.split('\n').collect();
        let mut rendered: Vec<Tok> = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            if i > 0 {
                rendered.push(Tok::Close("Paragraph".into(), true));
                rendered.push(Tok::Open("Paragraph".into(), true));
            }
            rendered.extend(part.chars().map(|c| if c == ' ' { Tok::Space } else { Tok::Ch(c) }));
        }
        let expected = self.expected(deletions, p, &rendered, false);
        let blank = prefix.trim_end().to_owned();
        for level in 0..3u8 {
            let mut source = String::new();
            let mut line_start = self.at_line_start(p);
            let mut prev_char = prev;
            for (i, part) in parts.iter().enumerate() {
                if i > 0 {
                    source.push_str(line_ending);
                    source.push_str(&blank);
                    source.push_str(line_ending);
                    source.push_str(&prefix);
                    line_start = true;
                    prev_char = None;
                }
                let following = if i + 1 == parts.len() { next } else { None };
                source.push_str(&escape_markdown(part, prev_char, following, line_start, table, level));
                if let Some(last) = part.chars().last() {
                    prev_char = Some(last);
                    line_start = false;
                }
            }
            if let Some(plan) =
                self.try_plan(p, deletions, &source, expected.clone(), false, EditMode::Markup, 0, &|_| true)?
            {
                return Ok(plan);
            }
        }
        Err(EditRefusal::WouldChangeStructure)
    }

    /// Container prefix for continuation lines of block `b` (e.g. `> `).
    fn container_prefix(&self, b: usize) -> String {
        let start = self.blocks[b].range.start;
        let line_begin = self.text[..start].rfind('\n').map_or(0, |i| i + 1);
        self.text[line_begin..start].chars().map(|c| if c == '>' || c == ' ' || c == '\t' { c } else { ' ' }).collect()
    }

    // ------------------------------------------------------------- math --

    fn math_ok(&self, eq: usize, delta: isize) -> impl Fn(&str) -> bool + '_ {
        let e = self.equations[eq].clone();
        let before = math::check(self.text, e.content.clone()).iter().filter(|p| p.marker).count();
        move |new_text: &str| {
            let end = (e.content.end as isize + delta) as usize;
            if end < e.content.start || end > new_text.len() || !new_text.is_char_boundary(end) {
                return false;
            }
            let after = math::check(new_text, e.content.start..end).iter().filter(|p| p.marker).count();
            after <= before
        }
    }

    /// Whether `p` is inside a `\text{...}` argument of equation `eq`.
    fn in_text_arg(&self, eq: usize, p: usize) -> bool {
        let e = &self.equations[eq];
        let before = &self.text[e.content.start..p];
        let mut depth = 0i32;
        for (i, c) in before.char_indices().rev() {
            match c {
                '}' => depth += 1,
                '{' => {
                    if depth == 0 {
                        let head = before[..i].trim_end();
                        return ["\\text", "\\textrm", "\\textit", "\\textbf", "\\mbox", "\\textnormal"]
                            .iter()
                            .any(|cmd| head.ends_with(cmd));
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }
        false
    }

    fn insert_math(
        &self,
        eq: usize,
        p: usize,
        deletions: &[Range<usize>],
        typed: &str,
    ) -> Result<EditPlan, EditRefusal> {
        let typed: String = typed.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
        if typed.is_empty() && deletions.is_empty() {
            return Err(EditRefusal::NoChange);
        }
        let deleted_end = deletions.iter().map(|d| d.end).max().unwrap_or(p).max(p);
        let next = self.text[deleted_end..].chars().next();
        let mut back = 0usize;
        let source = if self.in_text_arg(eq, p) {
            if typed.chars().any(|c| "{}\\$&%#_^~".contains(c)) {
                return Err(EditRefusal::WouldChangeStructure);
            }
            typed.clone()
        } else {
            if deletions.is_empty() && typed.chars().all(|c| c == ' ') {
                return Err(EditRefusal::NoChange);
            }
            let mut s = String::new();
            let before = &self.text[..p];
            // After a control word, letters need a separating space.
            let mut word_end = is_control_word_end(before);
            let single = typed.chars().count() == 1;
            for c in typed.chars() {
                let piece: String = match c {
                    '^' | '_' if single => {
                        back = 1;
                        format!("{c}{{}}")
                    }
                    '{' | '}' | '$' | '#' | '%' | '&' => format!("\\{c}"),
                    '\\' => "\\backslash".into(),
                    '~' => "\\sim".into(),
                    c => c.to_string(),
                };
                if word_end && piece.starts_with(|ch: char| ch.is_ascii_alphabetic()) {
                    s.push(' ');
                }
                s.push_str(&piece);
                word_end = is_control_word_end(&s) || (s.is_empty() && word_end);
            }
            if word_end && next.is_some_and(|n| n.is_ascii_alphabetic()) && back == 0 {
                s.push(' ');
                back = 1;
            }
            s
        };
        let delta = source.len() as isize - deletions.iter().map(|d| d.len() as isize).sum::<isize>();
        let expected = self.expected(deletions, p, &[], true);
        let check = self.math_ok(eq, delta);
        match self.try_plan(p, deletions, &source, expected, true, EditMode::Math, back, &check)? {
            Some(plan) => Ok(plan),
            None => Err(EditRefusal::WouldChangeStructure),
        }
    }

    fn delete_math(&self, eq: usize, p: usize, forward: bool) -> Result<EditPlan, EditRefusal> {
        let e = self.equations[eq].clone();
        let units = math::units(self.text, e.content.clone());
        let candidates: Vec<&math::MathUnit> = if forward {
            units.iter().filter(|u| u.range.start >= p).collect()
        } else {
            units.iter().filter(|u| u.range.end <= p).rev().collect()
        };
        let mut target: Option<Range<usize>> = None;
        for u in candidates {
            match u.kind {
                UnitKind::Space | UnitKind::Brace => continue,
                UnitKind::Char | UnitKind::Symbol => {
                    let mut range = u.range.clone();
                    // A one-token script argument goes with its `^`/`_`.
                    if let Some(op) = units.iter().find(|s| s.range.end == range.start && s.kind == UnitKind::Script) {
                        range.start = op.range.start;
                    }
                    target = Some(range);
                    break;
                }
                UnitKind::Script => {
                    target = Some(u.range.clone());
                    break;
                }
                UnitKind::Structure => {
                    return Err(EditRefusal::NotEditable { reason: "equation structure".into() });
                }
            }
        }
        let Some(mut range) = target else {
            // Nothing left on this side inside the equation: continue in prose.
            let q = if forward { e.range.end } else { e.range.start };
            return self.delete_markup(q, forward);
        };
        // Inline mathematics may not start or end with whitespace: remove the
        // whitespace the deletion would leave next to a delimiter.
        let tail = &self.text[range.end..e.content.end];
        if tail.trim().is_empty() {
            let lead = self.text[e.content.start..range.start].trim_end().len();
            range.start = e.content.start + lead;
            range.end = e.content.end;
        }
        let head = &self.text[e.content.start..range.start];
        if head.trim().is_empty() {
            let rest = &self.text[range.end..e.content.end];
            range.end += rest.len() - rest.trim_start().len();
            range.start = e.content.start;
        }
        // Deleting the last rendered token deletes the whole equation.
        let rest_empty = {
            let mut rest = self.text[e.content.start..range.start].to_owned();
            rest.push_str(&self.text[range.end..e.content.end]);
            rest.trim().is_empty()
        };
        if rest_empty {
            let deletions = vec![e.range.clone()];
            let expected = self.expected(&deletions, e.range.start, &[], false);
            return match self
                .try_plan(e.range.start, &deletions, "", expected, false, EditMode::Markup, 0, &|_| true)?
            {
                Some(plan) => Ok(plan),
                None => Err(EditRefusal::WouldChangeStructure),
            };
        }
        let deletions = vec![range.clone()];
        let delta = -(range.len() as isize);
        let expected = self.expected(&deletions, range.start, &[], true);
        let check = self.math_ok(eq, delta);
        match self.try_plan(range.start, &deletions, "", expected, true, EditMode::Math, 0, &check)? {
            Some(plan) => Ok(plan),
            None => Err(EditRefusal::WouldChangeStructure),
        }
    }

    // ----------------------------------------------------------- delete --

    /// Replace a non-empty selection with nothing.
    fn replace(&self, sel: Range<usize>) -> Result<EditPlan, EditRefusal> {
        if let Some(fm) = &self.front_matter
            && sel.start < fm.block.end
        {
            return match self.context(sel.start)? {
                Context::FrontMatter(s) if sel.end <= s.range.end => self.edit_front_matter(&s, sel, ""),
                _ => Err(EditRefusal::NotEditable { reason: "front matter".into() }),
            };
        }
        if let Some(eq) = self.equation_at(sel.start)
            && sel.end <= self.equations[eq].content.end
        {
            let deletions = merge(self.math_selection(&self.equations[eq], &sel));
            if deletions.is_empty() {
                return Err(EditRefusal::NoChange);
            }
            let delta = -(deletions.iter().map(|d| d.len() as isize).sum::<isize>());
            let expected = self.expected(&deletions, sel.start, &[], true);
            let check = self.math_ok(eq, delta);
            return match self.try_plan(sel.start, &deletions, "", expected, true, EditMode::Math, 0, &check)? {
                Some(plan) => Ok(plan),
                None => Err(EditRefusal::WouldChangeStructure),
            };
        }
        let deletions = self.selection_deletions(&sel)?;
        if deletions.is_empty() {
            return Err(EditRefusal::NoChange);
        }
        let expected = self.expected(&deletions, sel.start, &[], false);
        match self.try_plan(sel.start, &deletions, "", expected, false, EditMode::Markup, 0, &|_| true)? {
            Some(plan) => Ok(plan),
            None => Err(EditRefusal::WouldChangeStructure),
        }
    }

    fn delete_unit(&self, p: usize, forward: bool) -> Result<EditPlan, EditRefusal> {
        match self.context(p) {
            Ok(Context::Math(eq)) => self.delete_math(eq, p, forward),
            Ok(Context::FrontMatter(s)) => self.delete_front_matter(&s, p, forward),
            Ok(Context::Markup) => self.delete_markup(p, forward),
            Err(e) => Err(e),
        }
    }

    fn delete_markup(&self, p: usize, forward: bool) -> Result<EditPlan, EditRefusal> {
        let order: Vec<usize> = if forward {
            (0..self.tokens.len()).filter(|&i| self.tokens[i].fwd_key() >= p).collect()
        } else {
            (0..self.tokens.len()).rev().filter(|&i| self.tokens[i].back_key() <= p).collect()
        };
        for (k, &i) in order.iter().enumerate() {
            let t = &self.tokens[i];
            match &t.tok {
                Tok::Open(_, false) | Tok::Close(_, false) => continue,
                Tok::Open(key, true) | Tok::Close(key, true) => {
                    return self.join_blocks(&order[k..], key, forward);
                }
                Tok::Ch(_) => {
                    let range = if t.plain(self.text) { self.grapheme_at(i, p, forward) } else { t.extent.clone() };
                    return self.delete_range(range);
                }
                Tok::Space => return self.delete_range(t.extent.clone()),
                Tok::Eq(..) => {
                    let eq = self.equations.iter().position(|e| e.range == t.extent).expect("equation");
                    let q = if forward { self.equations[eq].content.start } else { self.equations[eq].content.end };
                    return self.delete_math(eq, q, forward);
                }
                Tok::Atom(a) if a == "br" => {
                    // A hard line break becomes a soft one (rendered as a space).
                    let deletions = vec![t.extent.clone()];
                    let expected = self.expected(&deletions, t.extent.start, &[Tok::Space], false);
                    let newline = if self.text[t.extent.clone()].ends_with("\r\n") { "\r\n" } else { "\n" };
                    return match self.try_plan(
                        t.extent.start,
                        &deletions,
                        newline,
                        expected,
                        false,
                        EditMode::Markup,
                        0,
                        &|_| true,
                    )? {
                        Some(plan) => Ok(plan),
                        None => Err(EditRefusal::WouldChangeStructure),
                    };
                }
                Tok::Atom(a) => {
                    let reason = match a.split(':').next() {
                        Some("code") => "code",
                        Some("html") => "raw HTML",
                        Some("meta") => "front matter",
                        _ => "generated content",
                    };
                    return Err(EditRefusal::NotEditable { reason: reason.into() });
                }
            }
        }
        Err(EditRefusal::NoChange)
    }

    /// The grapheme next to `p` within the run of plain characters around
    /// token `i`.
    fn grapheme_at(&self, i: usize, p: usize, forward: bool) -> Range<usize> {
        let mut a = i;
        while a > 0
            && self.tokens[a - 1].plain(self.text)
            && self.tokens[a - 1].extent.end == self.tokens[a].extent.start
        {
            a -= 1;
        }
        let mut b = i;
        while b + 1 < self.tokens.len()
            && self.tokens[b + 1].plain(self.text)
            && self.tokens[b + 1].extent.start == self.tokens[b].extent.end
        {
            b += 1;
        }
        let run = self.tokens[a].extent.start..self.tokens[b].extent.end;
        let t = &self.tokens[i];
        if forward {
            let from = p.max(t.extent.start).min(run.end);
            let g = self.text[from..run.end].graphemes(true).next().map_or(0, str::len);
            from..from + g.max(t.extent.end - from)
        } else {
            let to = p.min(t.extent.end).max(run.start);
            let g = self.text[run.start..to].graphemes(true).next_back().map_or(0, str::len);
            to - g.max(to - t.extent.start)..to
        }
    }

    fn delete_range(&self, range: Range<usize>) -> Result<EditPlan, EditRefusal> {
        let deletions = vec![range.clone()];
        let expected = self.expected(&deletions, range.start, &[], false);
        match self.try_plan(range.start, &deletions, "", expected, false, EditMode::Markup, 0, &|_| true)? {
            Some(plan) => Ok(plan),
            None => Err(EditRefusal::WouldChangeStructure),
        }
    }

    /// Backspace at the start (Delete at the end) of a paragraph joins it
    /// with the neighbouring sibling paragraph.
    fn join_blocks(&self, order: &[usize], _key: &str, forward: bool) -> Result<EditPlan, EditRefusal> {
        // Expect: (backward) Open(P) of this paragraph, then Close(P) of the
        // previous one; (forward) Close(P), then Open(P) of the next.
        let (a, b) = match order {
            [x, y, ..] => (*x, *y),
            _ => return Err(EditRefusal::NoChange),
        };
        let (first, second) = if forward { (a, b) } else { (b, a) };
        let (Tok::Close(k1, true), Tok::Open(k2, true)) = (&self.tokens[first].tok, &self.tokens[second].tok) else {
            return Err(EditRefusal::WouldChangeStructure);
        };
        if k1 != "Paragraph" || k2 != "Paragraph" {
            return Err(EditRefusal::WouldChangeStructure);
        }
        // Content end of the first paragraph and start of the second.
        let end = self.tokens[..first].iter().rev().find(|t| !matches!(t.tok, Tok::Open(..))).map(|t| t.back_key());
        let start = self.tokens[second + 1..].iter().find(|t| !matches!(t.tok, Tok::Close(..))).map(|t| t.fwd_key());
        let (Some(end), Some(start)) = (end, start) else { return Err(EditRefusal::NoChange) };
        if start < end {
            return Err(EditRefusal::WouldChangeStructure);
        }
        // Spaces typed before the break (not rendered at a paragraph end)
        // separate the joined words again, so Enter + Backspace round-trips.
        let kept = self.text[end..start].len() - self.text[end..start].trim_start_matches([' ', '\t']).len();
        let gap = end + kept..start;
        // Expected: everything except the Close/Open pair (a space when kept).
        let mut expected = Vec::new();
        for (i, t) in self.tokens.iter().enumerate() {
            if i == second && kept > 0 {
                expected.push(Tok::Space);
            }
            if i != first && i != second {
                expected.push(t.tok.clone());
            }
        }
        match self
            .try_plan(gap.start, std::slice::from_ref(&gap), "", expected, false, EditMode::Markup, 0, &|_| true)?
        {
            Some(plan) => Ok(plan),
            None => Err(EditRefusal::WouldChangeStructure),
        }
    }

    // ---------------------------------------------------- paragraph break --

    fn paragraph_break(&self, sel: Range<usize>, line_ending: &str) -> Result<EditPlan, EditRefusal> {
        let p = sel.start;
        match self.context(p)? {
            Context::Math(_) => return Err(EditRefusal::NotEditable { reason: "line breaks inside equations".into() }),
            Context::FrontMatter(_) => return Err(EditRefusal::NotEditable { reason: "front matter".into() }),
            Context::Markup => {}
        }
        let deletions = if sel.is_empty() { Vec::new() } else { self.selection_deletions(&sel)? };
        let chain = self.chain(p);
        let Some(&inner) = chain.first() else {
            return Err(EditRefusal::NoChange);
        };
        // The block to split: a list item (new item), a heading (rest becomes
        // a paragraph) or a paragraph.
        let split = chain
            .iter()
            .copied()
            .find(|&b| matches!(self.blocks[b].kind, BlockKind::Item | BlockKind::Heading | BlockKind::Paragraph));
        let Some(split) = split else {
            return Err(EditRefusal::NotEditable { reason: "paragraph breaks here".into() });
        };
        // An item contains the paragraph directly.
        let split = match self.blocks[split].parent {
            Some(parent)
                if self.blocks[split].kind == BlockKind::Paragraph && self.blocks[parent].kind == BlockKind::Item =>
            {
                parent
            }
            _ => split,
        };
        let refuse = |reason: &str| Err(EditRefusal::NotEditable { reason: reason.into() });
        // Blocks between `inner` and `split` (exclusive of split) close and reopen.
        let mut nested: Vec<usize> = Vec::new();
        for &b in &chain {
            if b == split {
                break;
            }
            match self.blocks[b].kind {
                BlockKind::Inline => return Err(EditRefusal::WouldChangeStructure),
                BlockKind::Paragraph => nested.push(b),
                BlockKind::Cell | BlockKind::Table => return refuse("table cells"),
                _ => return Err(EditRefusal::WouldChangeStructure),
            }
        }
        let _ = inner;
        let block = &self.blocks[split];
        let mut inserted: Vec<Tok> = Vec::new();
        for &b in &nested {
            inserted.push(Tok::Close(self.blocks[b].key.clone(), true));
        }
        let source = match block.kind {
            BlockKind::Paragraph => {
                if block.parent.is_some_and(|q| self.blocks[q].kind == BlockKind::Footnote) {
                    return refuse("footnote definitions");
                }
                inserted.push(Tok::Close(block.key.clone(), true));
                inserted.push(Tok::Open(block.key.clone(), true));
                let prefix = self.container_prefix(split);
                format!("{line_ending}{}{line_ending}{prefix}", prefix.trim_end())
            }
            BlockKind::Heading => {
                if !self.text[block.range.clone()].trim_start().starts_with('#') {
                    return refuse("setext headings");
                }
                inserted.push(Tok::Close(block.key.clone(), true));
                inserted.push(Tok::Open("Paragraph".into(), true));
                let prefix = self.container_prefix(split);
                format!("{line_ending}{}{line_ending}{prefix}", prefix.trim_end())
            }
            BlockKind::Item => {
                let list = block.parent.expect("item in list");
                let loose = !nested.is_empty();
                inserted.push(Tok::Close(block.key.clone(), true));
                inserted.push(Tok::Open(block.key.clone(), true));
                let marker_text = &self.text[block.range.start..];
                let marker = match self.blocks[list].kind {
                    BlockKind::List { ordered: true } => {
                        let digits: String = marker_text.chars().take_while(char::is_ascii_digit).collect();
                        let delim = marker_text[digits.len()..].chars().next().unwrap_or('.');
                        let n: u64 = digits.parse().unwrap_or(1);
                        format!("{}{delim}", n + 1)
                    }
                    _ => marker_text.chars().next().unwrap_or('-').to_string(),
                };
                let task = self.tokens.iter().any(|t| {
                    matches!(&t.tok, Tok::Atom(a) if a.starts_with("task:"))
                        && t.extent.start >= block.range.start
                        && t.extent.end <= block.range.end
                        && t.extent.start < p
                });
                if task {
                    inserted.push(Tok::Atom("task:false".into()));
                }
                for &b in nested.iter().rev() {
                    inserted.push(Tok::Open(self.blocks[b].key.clone(), true));
                }
                let indent = self.container_prefix(split);
                let blank = if loose { format!("{}{line_ending}", indent.trim_end()) } else { String::new() };
                format!("{line_ending}{blank}{indent}{marker} {}", if task { "[ ] " } else { "" })
            }
            _ => return refuse("paragraph breaks here"),
        };
        if !matches!(block.kind, BlockKind::Item) {
            for &b in nested.iter().rev() {
                inserted.push(Tok::Open(self.blocks[b].key.clone(), true));
            }
        }
        let mut expected = self.expected(&deletions, p, &inserted, false);
        if block.kind == BlockKind::Heading {
            // The heading's end now closes the new paragraph.
            if let Some(pos) = expected.iter().rposition(|t| *t == Tok::Close(block.key.clone(), true)) {
                expected[pos] = Tok::Close("Paragraph".into(), true);
            }
        }
        match self.try_plan(p, &deletions, &source, expected, false, EditMode::Markup, 0, &|_| true)? {
            Some(plan) => Ok(plan),
            None => Err(EditRefusal::WouldChangeStructure),
        }
    }

    // ------------------------------------------------------ front matter --

    fn edit_front_matter(&self, s: &Scalar, sel: Range<usize>, typed: &str) -> Result<EditPlan, EditRefusal> {
        if sel.end > s.range.end || typed.contains('\n') {
            return Err(EditRefusal::WouldChangeStructure);
        }
        let raw_before = &self.text[s.range.start..sel.start];
        let raw_sel = &self.text[sel.clone()];
        let value_before = decode(s.style, raw_before);
        let value_sel = decode(s.style, raw_sel);
        if !s.value.starts_with(&value_before) {
            return Err(EditRefusal::InvalidPosition);
        }
        let rest = &s.value[value_before.len() + value_sel.len().min(s.value.len() - value_before.len())..];
        let new_value = format!("{value_before}{typed}{rest}");
        if new_value == s.value {
            return Err(EditRefusal::NoChange);
        }
        let encoded = encode(s.style, typed);
        // A plain scalar that would stop being plain becomes double-quoted.
        let (range, source, caret_back) = if s.style == ScalarStyle::Plain && !plain_ok(&new_value) {
            let before = encode(ScalarStyle::DoubleQuoted, &format!("{value_before}{typed}"));
            let after = encode(ScalarStyle::DoubleQuoted, rest);
            (s.range.clone(), format!("\"{before}{after}\""), after.len() + 1)
        } else {
            (sel.clone(), encoded, 0)
        };
        self.front_matter_plan(s, range, &source, &new_value, caret_back)
    }

    fn delete_front_matter(&self, s: &Scalar, p: usize, forward: bool) -> Result<EditPlan, EditRefusal> {
        let raw = &self.text[s.range.clone()];
        let local = p - s.range.start;
        let unit = if forward {
            let g = raw[local..].graphemes(true).next().ok_or(EditRefusal::NoChange)?;
            let mut r = local..local + g.len();
            if (s.style == ScalarStyle::DoubleQuoted && g == "\\")
                || (s.style == ScalarStyle::SingleQuoted && raw[local..].starts_with("''"))
            {
                r.end = (r.end + 1).min(raw.len());
            }
            r
        } else {
            let g = raw[..local].graphemes(true).next_back().ok_or(EditRefusal::NoChange)?;
            let mut r = local - g.len()..local;
            if s.style == ScalarStyle::DoubleQuoted
                && raw[..r.start].ends_with('\\')
                && !escaped_backslash(&raw[..r.start - 1])
            {
                r.start -= 1;
            }
            if s.style == ScalarStyle::SingleQuoted && g == "'" && raw[..r.start].ends_with('\'') {
                r.start -= 1;
            }
            r
        };
        let sel = s.range.start + unit.start..s.range.start + unit.end;
        self.edit_front_matter(s, sel, "")
    }

    fn front_matter_plan(
        &self,
        s: &Scalar,
        range: Range<usize>,
        source: &str,
        new_value: &str,
        caret_back: usize,
    ) -> Result<EditPlan, EditRefusal> {
        let patches = vec![(range.clone(), source.to_owned())];
        let new_text = apply(self.text, &patches);
        // Verify: the same scalar now decodes to the new value, and the
        // rest of the document renders the same.
        let fm_ok = {
            let (tokens, _, _, fm) = tokenize(&new_text);
            let old: Vec<Tok> = self
                .tokens
                .iter()
                .filter(|t| !matches!(&t.tok, Tok::Atom(a) if a.starts_with("meta:")))
                .map(|t| t.tok.clone())
                .collect();
            let new: Vec<Tok> = tokens
                .into_iter()
                .filter(|t| !matches!(&t.tok, Tok::Atom(a) if a.starts_with("meta:")))
                .map(|t| t.tok)
                .collect();
            let value_ok = fm.as_ref().is_some_and(|fm| {
                let entry = self.front_matter.as_ref().and_then(|old| {
                    old.entries
                        .iter()
                        .find_map(|e| e.values.iter().position(|v| v.range == s.range).map(|i| (e.key.clone(), i)))
                });
                entry.is_some_and(|(key, i)| {
                    fm.get(&key).and_then(|e| e.values.get(i)).is_some_and(|v| v.value == new_value)
                })
            });
            value_ok && normalize(old) == normalize(new)
        };
        if !fm_ok {
            return Err(EditRefusal::WouldChangeStructure);
        }
        let caret_byte = range.start + source.len() - caret_back;
        let caret = to_u16(&new_text, caret_byte)?;
        let patch = TextPatch {
            range: TextRange::new(to_u16(self.text, range.start)?, to_u16(self.text, range.end)?),
            text: source.to_owned(),
        };
        Ok(EditPlan { patches: vec![patch], caret, mode: EditMode::Markup })
    }
}

fn decode(style: ScalarStyle, raw: &str) -> String {
    match style {
        ScalarStyle::Plain => raw.to_owned(),
        ScalarStyle::SingleQuoted => raw.replace("''", "'"),
        ScalarStyle::DoubleQuoted => {
            let mut out = String::new();
            let mut it = raw.chars();
            while let Some(c) = it.next() {
                if c == '\\' {
                    match it.next() {
                        Some('n') => out.push('\n'),
                        Some('t') => out.push('\t'),
                        Some(e) => out.push(e),
                        None => {}
                    }
                } else {
                    out.push(c);
                }
            }
            out
        }
    }
}

fn encode(style: ScalarStyle, value: &str) -> String {
    match style {
        ScalarStyle::Plain => value.to_owned(),
        ScalarStyle::SingleQuoted => value.replace('\'', "''"),
        ScalarStyle::DoubleQuoted => value.replace('\\', "\\\\").replace('"', "\\\""),
    }
}

/// Whether `value` can stay a plain YAML scalar.
fn plain_ok(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with([
            '-', '?', ':', ',', '[', ']', '{', '}', '#', '&', '*', '!', '|', '>', '\'', '"', '%', '@', '`', ' ',
        ])
        && !value.ends_with(' ')
        && !value.contains(": ")
        && !value.contains(" #")
        && !value.ends_with(':')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str, at: usize, op: PageEditOp) -> Result<(String, usize), EditRefusal> {
        run_sel(text, at, at, op)
    }

    fn run_sel(text: &str, a: usize, b: usize, op: PageEditOp) -> Result<(String, usize), EditRefusal> {
        // Tests use ASCII-only offsets except where noted (UTF-16 == bytes).
        let plan = MarkdownInput.plan(text, TextRange::new(a as u32, b as u32), &op, "\n")?;
        let mut out = text.to_owned();
        for patch in plan.patches.iter().rev() {
            let r = utf16_range_to_bytes(&out, patch.range).unwrap();
            out.replace_range(r, &patch.text);
        }
        Ok((out, plan.caret as usize))
    }

    fn ins(t: &str) -> PageEditOp {
        PageEditOp::Insert { text: t.into() }
    }

    #[test]
    fn plain_typing_is_inserted_verbatim() {
        assert_eq!(run("Hello world", 5, ins("X")).unwrap(), ("HelloX world".into(), 6));
        assert_eq!(run("Hello world", 5, ins(", dear")).unwrap(), ("Hello, dear world".into(), 11));
        // Characters that keep their meaning in context are not escaped.
        assert_eq!(run("a b", 2, ins("* ")).unwrap().0, "a * b");
        assert_eq!(run("snakecase", 5, ins("_")).unwrap().0, "snake_case");
        assert_eq!(run("one two", 3, ins(" [1]")).unwrap().0, "one [1] two");
    }

    #[test]
    fn markdown_specials_are_escaped_where_they_change_meaning() {
        for (typed, expected) in
            [("\\", "a\\\\b"), ("`", "a\\`b"), ("$", "a\\$b"), ("<i", "a\\<ib"), ("&amp", "a\\&ampb")]
        {
            assert_eq!(run("ab", 1, ins(typed)).unwrap().0, expected, "typed {typed}");
        }
        // Emphasis that would form is escaped.
        assert_eq!(run("a b*", 2, ins("*")).unwrap().0, "a \\*b*");
        // Line-start markers.
        assert_eq!(run("x\n\nab", 3, ins("# ")).unwrap().0, "x\n\n\\# ab");
        assert_eq!(run("x\n\nab", 3, ins("- ")).unwrap().0, "x\n\n\\- ab");
        assert_eq!(run("x\n\nab", 3, ins("+ ")).unwrap().0, "x\n\n\\+ ab");
        assert_eq!(run("x\n\nab", 3, ins("> ")).unwrap().0, "x\n\n\\> ab");
        assert_eq!(run("x\n\nab", 3, ins("1. ")).unwrap().0, "x\n\n1\\. ab");
        // In a table cell a pipe is escaped.
        let table = "| a | b |\n|---|---|\n| c | d |\n";
        let (text, _) = run(table, 22, ins("|")).unwrap();
        assert_eq!(text, "| a | b |\n|---|---|\n| \\|c | d |\n");
    }

    #[test]
    fn typing_inside_emphasis_and_links_stays_inside() {
        assert_eq!(run("a *bold* c", 5, ins("X")).unwrap(), ("a *boXld* c".into(), 6));
        assert_eq!(run("see [the link](u) now", 8, ins("X")).unwrap().0, "see [theX link](u) now");
        // Inside the destination nothing can be typed.
        assert!(matches!(run("see [the link](url) now", 16, ins("X")), Err(EditRefusal::NotEditable { .. })));
    }

    #[test]
    fn deletion_removes_whole_escapes_entities_and_graphemes() {
        assert_eq!(run("a\\*b", 3, PageEditOp::DeleteBackward).unwrap(), ("ab".into(), 1));
        assert_eq!(run("a\\*b", 1, PageEditOp::DeleteForward).unwrap(), ("ab".into(), 1));
        assert_eq!(run("a&amp;b", 6, PageEditOp::DeleteBackward).unwrap(), ("ab".into(), 1));
        assert_eq!(run("ae\u{301}b", 3, PageEditOp::DeleteBackward).unwrap(), ("ab".into(), 1));
        // Deleting over an emphasis delimiter deletes the character inside.
        assert_eq!(run("a *bc* d", 6, PageEditOp::DeleteBackward).unwrap().0, "a *b* d");
        // Code is not editable on the page.
        assert!(matches!(run("a `code` b", 8, PageEditOp::DeleteBackward), Err(EditRefusal::NotEditable { .. })));
    }

    #[test]
    fn paragraph_breaks_split_paragraphs_items_and_headings() {
        assert_eq!(run("ab cd", 3, PageEditOp::ParagraphBreak).unwrap(), ("ab \n\ncd".into(), 5));
        assert_eq!(run("> ab cd\n", 5, PageEditOp::ParagraphBreak).unwrap().0, "> ab \n>\n> cd\n");
        assert_eq!(run("- ab\n- cd\n", 3, PageEditOp::ParagraphBreak).unwrap(), ("- a\n- b\n- cd\n".into(), 6));
        assert_eq!(run("1. ab\n2. cd\n", 5, PageEditOp::ParagraphBreak).unwrap().0, "1. ab\n2. \n2. cd\n");
        assert_eq!(run("- [x] done\n", 8, PageEditOp::ParagraphBreak).unwrap().0, "- [x] do\n- [ ] ne\n");
        assert_eq!(run("# Title\n", 4, PageEditOp::ParagraphBreak).unwrap().0, "# Ti\n\ntle\n");
        assert!(matches!(run("$x + 1$", 2, PageEditOp::ParagraphBreak), Err(EditRefusal::NotEditable { .. })));
        // Backspace at a paragraph start joins it with the previous one.
        assert_eq!(run("ab\n\ncd\n", 4, PageEditOp::DeleteBackward).unwrap(), ("abcd\n".into(), 2));
        assert_eq!(run("ab\n\ncd\n", 2, PageEditOp::DeleteForward).unwrap(), ("abcd\n".into(), 2));
        // Enter then Backspace restores the original bytes.
        let (split, caret) = run("ab cd\n", 3, PageEditOp::ParagraphBreak).unwrap();
        assert_eq!(run(&split, caret, PageEditOp::DeleteBackward).unwrap(), ("ab cd\n".into(), 3));
    }

    #[test]
    fn math_typing_and_deletion() {
        // Change an exponent: select it and type.
        assert_eq!(run_sel("$x^2$", 3, 4, ins("3")).unwrap(), ("$x^3$".into(), 4));
        // A letter after a control word gets a separating space.
        assert_eq!(run("$\\alpha$", 7, ins("x")).unwrap().0, "$\\alpha x$");
        // `^` opens an empty group with the caret inside.
        assert_eq!(run("$x$", 2, ins("^")).unwrap(), ("$x^{}$".into(), 4));
        // LaTeX specials are escaped.
        assert_eq!(run("$a$", 2, ins("{")).unwrap().0, "$a\\{$");
        // Backspace removes a whole command, and a one-token script with its operator.
        assert_eq!(run("$a + \\beta$", 10, PageEditOp::DeleteBackward).unwrap().0, "$a +$");
        assert_eq!(run("$\\beta + a$", 6, PageEditOp::DeleteBackward).unwrap().0, "$+ a$");
        assert_eq!(run("$x^2 + y$", 4, PageEditOp::DeleteBackward).unwrap().0, "$x + y$");
        // Deleting the last token removes the equation.
        assert_eq!(run("a $x$ b", 4, PageEditOp::DeleteBackward).unwrap().0, "a  b");
        // Structure commands are refused.
        assert!(matches!(run("$\\frac{a}{b}$", 6, PageEditOp::DeleteBackward), Err(EditRefusal::NotEditable { .. })));
        // Typing that would break the mathematics is refused.
        assert_eq!(run("$a$", 2, ins("\\")).unwrap().0, "$a\\backslash$");
    }

    #[test]
    fn front_matter_values_are_edited_as_yaml() {
        let doc = "---\ntitle: My Paper\n---\n\nBody\n";
        assert_eq!(run(doc, 19, ins("s")).unwrap().0, "---\ntitle: My Papers\n---\n\nBody\n");
        // A value that can no longer be plain is quoted.
        assert_eq!(run(doc, 13, ins(": A")).unwrap().0, "---\ntitle: \"My: A Paper\"\n---\n\nBody\n");
        assert_eq!(run(doc, 19, PageEditOp::DeleteBackward).unwrap().0, "---\ntitle: My Pape\n---\n\nBody\n");
        let quoted = "---\ntitle: \"Say \\\"hi\\\"\"\n---\n";
        let (text, _) = run(quoted, 16, ins("\"")).unwrap();
        assert_eq!(text, "---\ntitle: \"Say \\\"\\\"hi\\\"\"\n---\n");
        assert_eq!(MarkdownInput.mode_at(doc, 14), Some(EditMode::Markup));
        assert!(run("---\nfontsize: 11pt\n---\n", 15, ins("2")).is_err());
    }

    #[test]
    fn modes() {
        let text = "a `c` $x$ [l](u)";
        assert_eq!(MarkdownInput.mode_at(text, 1), Some(EditMode::Markup));
        assert_eq!(MarkdownInput.mode_at(text, 3), None);
        assert_eq!(MarkdownInput.mode_at(text, 7), Some(EditMode::Math));
        assert_eq!(MarkdownInput.mode_at(text, 11), Some(EditMode::Markup));
        assert_eq!(MarkdownInput.mode_at(text, 14), None);
    }
}
