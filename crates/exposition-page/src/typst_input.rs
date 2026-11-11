//! Typst page input: turns typing, deletion, and paragraph breaks at a caret
//! or selection into source patches.
//!
//! Inserted text is escaped as Typst markup (or written as math inside
//! equations). Every planned edit is verified by re-parsing: the rendered-text
//! token stream of the new source must equal the old stream with exactly the
//! intended characters inserted or removed, and all other structure unchanged.
//! Edits that would change structure (open a strong span, break out of an
//! equation, turn text into code, ...) are refused rather than applied.

use std::ops::Range;

use exposition_core::document::TextPatch;
use exposition_core::text::{TextRange, byte_to_utf16, utf16_len, utf16_range_to_bytes};
use typst_syntax::{SyntaxKind, SyntaxNode, parse};
use unicode_segmentation::UnicodeSegmentation;

use crate::input::{EditPlan, EditRefusal, PageEditOp, PageInputRules};
use crate::map::EditMode;

/// Typst input rules.
#[derive(Debug, Default, Clone, Copy)]
pub struct TypstInput;

impl PageInputRules for TypstInput {
    fn plan(
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
            PageEditOp::ParagraphBreak => doc.insert(bytes, "\n", line_ending),
            PageEditOp::DeleteBackward | PageEditOp::DeleteForward if !bytes.is_empty() => doc.replace(bytes, ""),
            PageEditOp::DeleteBackward => doc.delete_unit(bytes.start, false),
            PageEditOp::DeleteForward => doc.delete_unit(bytes.start, true),
        }
    }

    fn mode_at(&self, text: &str, offset: u32) -> Option<EditMode> {
        let byte = utf16_range_to_bytes(text, TextRange::empty(offset)).ok()?.start;
        let doc = Doc::new(text);
        match doc.insertion_mode(byte) {
            Ok(Mode::Markup) => Some(EditMode::Markup),
            Ok(Mode::Math) => Some(EditMode::Math),
            _ => None,
        }
    }
}

/// Syntactic context of a leaf.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Markup,
    Math,
    Code,
    Raw,
    Comment,
}

#[derive(Clone, Debug)]
struct Leaf {
    kind: SyntaxKind,
    range: Range<usize>,
    mode: Mode,
    /// Range of a unit that must be deleted as a whole (e.g. `plus.minus`).
    unit: Option<Range<usize>>,
    /// Whether the leaf is its parent's first child (an opening delimiter).
    first: bool,
}

struct Doc<'a> {
    text: &'a str,
    root: SyntaxNode,
    leaves: Vec<Leaf>,
}

fn flatten(node: &SyntaxNode, offset: usize, mode: Mode, unit: Option<Range<usize>>, first: bool, out: &mut Vec<Leaf>) {
    let kind = node.kind();
    let range = offset..offset + node.len();
    if node.children().len() == 0 {
        let mode = match kind {
            SyntaxKind::LineComment | SyntaxKind::BlockComment | SyntaxKind::Shebang => Mode::Comment,
            _ => mode,
        };
        if !range.is_empty() {
            out.push(Leaf { kind, range, mode, unit, first });
        }
        return;
    }
    let inner_mode = match kind {
        SyntaxKind::Markup => Mode::Markup,
        SyntaxKind::Math | SyntaxKind::Equation => Mode::Math,
        SyntaxKind::Raw => Mode::Raw,
        SyntaxKind::ContentBlock => Mode::Markup,
        SyntaxKind::Code | SyntaxKind::CodeBlock => Mode::Code,
        _ => mode,
    };
    let unit = unit.or_else(|| (kind == SyntaxKind::MathFieldAccess).then(|| range.clone()));
    let mut child_offset = offset;
    let mut after_hash = false;
    for (i, child) in node.children().enumerate() {
        let child_mode = if after_hash { Mode::Code } else { inner_mode };
        flatten(child, child_offset, child_mode, unit.clone(), i == 0, out);
        after_hash = child.kind() == SyntaxKind::Hash && matches!(inner_mode, Mode::Markup | Mode::Math);
        child_offset += child.len();
    }
}

/// One element of the rendered-text token stream used to verify edits.
#[derive(Clone, Debug, PartialEq)]
enum Tok {
    /// A literal rendered character (text, escapes).
    Ch(char),
    Space,
    Par,
    Open(SyntaxKind),
    Close(SyntaxKind),
    /// An equation collapsed to one token (`block` flag).
    Eq(bool),
    /// Any other leaf, compared verbatim.
    Leaf(SyntaxKind, String),
}

#[derive(Clone, Debug)]
struct Token {
    tok: Tok,
    /// Position used to place insertions (start; end for `Close`).
    pos: usize,
    /// Source range the token occupies (whole node for `Open`/`Close`/`Eq`).
    extent: Range<usize>,
}

fn unescape(text: &str) -> Option<char> {
    let rest = text.strip_prefix('\\')?;
    if let Some(hex) = rest.strip_prefix("u{").and_then(|r| r.strip_suffix('}')) {
        return u32::from_str_radix(hex, 16).ok().and_then(char::from_u32);
    }
    let mut chars = rest.chars();
    let c = chars.next()?;
    chars.next().is_none().then_some(c)
}

fn is_block_equation(node: &SyntaxNode) -> bool {
    let children: Vec<_> = node.children().collect();
    children.len() >= 3
        && children.get(1).is_some_and(|c| c.kind() == SyntaxKind::Space)
        && children.get(children.len() - 2).is_some_and(|c| c.kind() == SyntaxKind::Space)
}

fn tokenize(node: &SyntaxNode, offset: usize, collapse_math: bool, in_raw: bool, out: &mut Vec<Token>) {
    let kind = node.kind();
    let range = offset..offset + node.len();
    if range.is_empty() {
        return;
    }
    if kind == SyntaxKind::Equation && collapse_math {
        out.push(Token { tok: Tok::Eq(is_block_equation(node)), pos: range.start, extent: range });
        return;
    }
    if node.children().len() == 0 {
        let text = node.leaf_text().as_str();
        match kind {
            SyntaxKind::Text if !in_raw => {
                for (i, ch) in text.char_indices() {
                    let start = range.start + i;
                    out.push(Token { tok: Tok::Ch(ch), pos: start, extent: start..start + ch.len_utf8() });
                }
            }
            SyntaxKind::Escape => match unescape(text) {
                Some(ch) => out.push(Token { tok: Tok::Ch(ch), pos: range.start, extent: range }),
                None => out.push(Token { tok: Tok::Leaf(kind, text.to_owned()), pos: range.start, extent: range }),
            },
            SyntaxKind::Space => out.push(Token { tok: Tok::Space, pos: range.start, extent: range }),
            SyntaxKind::Parbreak => out.push(Token { tok: Tok::Par, pos: range.start, extent: range }),
            _ => out.push(Token { tok: Tok::Leaf(kind, text.to_owned()), pos: range.start, extent: range }),
        }
        return;
    }
    out.push(Token { tok: Tok::Open(kind), pos: range.start, extent: range.clone() });
    let mut child_offset = offset;
    for child in node.children() {
        tokenize(child, child_offset, collapse_math, in_raw || kind == SyntaxKind::Raw, out);
        child_offset += child.len();
    }
    out.push(Token { tok: Tok::Close(kind), pos: range.end, extent: range });
}

/// Collapse whitespace runs: Typst renders consecutive spaces as one, and a
/// run containing a paragraph break as a paragraph break.
fn normalize(tokens: impl IntoIterator<Item = Tok>) -> Vec<Tok> {
    let mut out: Vec<Tok> = Vec::new();
    for tok in tokens {
        // Spaces inside text runs render like separate space nodes.
        let tok = match tok {
            Tok::Ch(' ' | '\t') => Tok::Space,
            other => other,
        };
        match tok {
            Tok::Space | Tok::Par => match out.last_mut() {
                Some(last @ (Tok::Space | Tok::Par)) => {
                    if tok == Tok::Par {
                        *last = Tok::Par;
                    }
                }
                _ => out.push(tok),
            },
            other => out.push(other),
        }
    }
    out
}

fn tokens_of(root: &SyntaxNode, collapse_math: bool) -> Vec<Token> {
    // The root markup node itself has no delimiters to compare.
    let mut out = Vec::new();
    let mut offset = 0;
    for child in root.children() {
        tokenize(child, offset, collapse_math, false, &mut out);
        offset += child.len();
    }
    out
}

/// Whether a token lies before an insertion at `p`.
fn before(token: &Token, p: usize) -> bool {
    match token.tok {
        Tok::Close(_) => token.pos <= p,
        _ => token.pos < p,
    }
}

fn contained(token: &Token, deletions: &[Range<usize>]) -> bool {
    deletions.iter().any(|d| token.extent.start >= d.start && token.extent.end <= d.end && !d.is_empty())
}

fn rendered_tokens(rendered: &str) -> Vec<Tok> {
    rendered
        .chars()
        .map(|c| match c {
            '\n' => Tok::Par,
            c if c == ' ' || c == '\t' => Tok::Space,
            c => Tok::Ch(c),
        })
        .collect()
}

const ALWAYS_ESCAPE: &[char] = &['\\', '#', '$', '[', ']', '`', '<', '@', '*', '_', '~', '\'', '"'];

/// Escape `typed` (no newlines) for insertion into markup between `prev` and
/// `next` source characters. `line_start` is true when only whitespace
/// precedes the insertion point on its line.
fn escape_markup(typed: &str, mut prev: Option<char>, next: Option<char>, mut line_start: bool, level: u8) -> String {
    let chars: Vec<char> = typed.chars().collect();
    let mut out = String::with_capacity(typed.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        let following = chars.get(i + 1).copied().or(next);
        let escape = if c.is_whitespace() {
            false
        } else {
            match level {
                0 => {
                    ALWAYS_ESCAPE.contains(&c)
                        || match c {
                            '-' => {
                                line_start
                                    || prev == Some('-')
                                    || matches!(following, Some('-' | '?'))
                                    || following.is_some_and(|f| f.is_numeric())
                            }
                            '/' => line_start || prev == Some('/') || matches!(following, Some('/' | '*')),
                            '=' | '+' => line_start,
                            '.' => prev == Some('.') || following == Some('.'),
                            _ => false,
                        }
                }
                1 => c.is_ascii_punctuation(),
                _ => true,
            }
        };
        if escape {
            out.push('\\');
        }
        out.push(c);
        if !c.is_whitespace() {
            line_start = false;
        }
        prev = Some(c);
    }
    out
}

fn is_math_word(c: char) -> bool {
    c.is_alphanumeric()
}

/// Whether two adjacent math characters would merge into one identifier.
fn math_needs_space(a: char, b: char) -> bool {
    is_math_word(a) && is_math_word(b) && !a.is_numeric()
}

impl<'a> Doc<'a> {
    fn new(text: &'a str) -> Self {
        let root = parse(text);
        let mut leaves = Vec::new();
        flatten(&root, 0, Mode::Markup, None, false, &mut leaves);
        Self { text, root, leaves }
    }

    fn prev_char(&self, p: usize) -> Option<char> {
        self.text[..p].chars().next_back()
    }

    fn next_char(&self, p: usize) -> Option<char> {
        self.text[p..].chars().next()
    }

    fn at_line_start(&self, p: usize) -> bool {
        let line = self.text[..p].rsplit(['\n', '\r']).next().unwrap_or("");
        line.chars().all(char::is_whitespace)
    }

    /// The leaf containing the character before `p`.
    fn leaf_before(&self, p: usize) -> Option<usize> {
        if p == 0 {
            return None;
        }
        self.leaves.iter().rposition(|l| l.range.start < p && !l.range.is_empty())
    }

    /// The leaf containing the character at `p`.
    fn leaf_after(&self, p: usize) -> Option<usize> {
        self.leaves.iter().position(|l| l.range.end > p && !l.range.is_empty())
    }

    /// The mode typed text takes at `p`.
    fn insertion_mode(&self, p: usize) -> Result<Mode, EditRefusal> {
        let refuse = |mode: Mode| match mode {
            Mode::Code => Err(EditRefusal::NotEditable { reason: "code".into() }),
            Mode::Raw => Err(EditRefusal::NotEditable { reason: "raw".into() }),
            Mode::Comment => Err(EditRefusal::NotEditable { reason: "comment".into() }),
            mode => Ok(mode),
        };
        let before = self.leaf_before(p).map(|i| &self.leaves[i]);
        let after = self.leaf_after(p).map(|i| &self.leaves[i]);
        // Strictly inside a leaf: its mode decides.
        if let Some(leaf) = before
            && leaf.range.end > p
        {
            return refuse(leaf.mode);
        }
        let math_content = |l: Option<&Leaf>| l.is_some_and(|l| l.mode == Mode::Math && l.kind != SyntaxKind::Dollar);
        if math_content(before) || math_content(after) {
            return Ok(Mode::Math);
        }
        // Between the dollar signs of an empty equation.
        if before.is_some_and(|l| l.kind == SyntaxKind::Dollar && l.first)
            && after.is_some_and(|l| l.kind == SyntaxKind::Dollar && !l.first)
        {
            return Ok(Mode::Math);
        }
        if [before, after].into_iter().flatten().any(|l| l.mode == Mode::Markup) {
            return Ok(Mode::Markup);
        }
        match after.or(before) {
            None => Ok(Mode::Markup),
            Some(l) => refuse(l.mode),
        }
    }

    /// Verify that `new_text` renders as `self` with `deletions` removed and
    /// `inserted` (rendered characters) placed at `at`.
    fn verify(&self, new_text: &str, deletions: &[Range<usize>], at: usize, inserted: &str, math: bool) -> bool {
        let new_root = parse(new_text);
        let old = tokens_of(&self.root, math);
        let mut expected: Vec<Tok> = Vec::with_capacity(old.len() + inserted.len());
        let mut placed = false;
        let insert_toks = if math { Vec::new() } else { rendered_tokens(inserted) };
        for token in &old {
            if !placed && !before(token, at) {
                expected.extend(insert_toks.iter().cloned());
                placed = true;
            }
            if !contained(token, deletions) {
                expected.push(token.tok.clone());
            }
        }
        if !placed {
            expected.extend(insert_toks);
        }
        let actual = tokens_of(&new_root, math).into_iter().map(|t| t.tok);
        normalize(expected) == normalize(actual)
    }

    /// Byte ranges to delete for a selection, keeping the delimiters of
    /// structures that are only partly selected.
    fn selection_deletions(&self, sel: Range<usize>) -> Result<Vec<Range<usize>>, EditRefusal> {
        let mut out: Vec<Range<usize>> = Vec::new();
        collect_deletions(&self.root, 0, &sel, &mut out)?;
        // Merge adjacent ranges.
        let mut merged: Vec<Range<usize>> = Vec::new();
        for r in out {
            match merged.last_mut() {
                Some(last) if last.end >= r.start => last.end = last.end.max(r.end),
                _ => merged.push(r),
            }
        }
        Ok(merged)
    }

    /// Replace a selection (possibly empty) with typed text.
    fn insert(&self, sel: Range<usize>, typed: &str, line_ending: &str) -> Result<EditPlan, EditRefusal> {
        let deletions = if sel.is_empty() { Vec::new() } else { self.selection_deletions(sel.clone())? };
        let p = sel.start;
        let mode = self.insertion_mode(p)?;
        let typed: String = typed.replace("\r\n", "\n").replace('\r', "\n").replace('\t', " ");
        match mode {
            Mode::Math => self.insert_math(p, &deletions, &typed),
            _ => self.insert_markup(p, &deletions, &typed, line_ending),
        }
    }

    fn insert_markup(
        &self,
        p: usize,
        deletions: &[Range<usize>],
        typed: &str,
        line_ending: &str,
    ) -> Result<EditPlan, EditRefusal> {
        let deleted_end = deletions.iter().map(|d| d.end).max().unwrap_or(p).max(p);
        let prev = self.prev_char(p);
        let next = self.next_char(deleted_end);
        // Whitespace runs collapse when rendered: drop typed spaces next to
        // existing whitespace (typing a space before a space moves past it).
        let mut rendered = typed.to_owned();
        if deletions.is_empty() && !rendered.is_empty() && rendered.chars().all(|c| c == ' ') {
            if next.is_some_and(char::is_whitespace) {
                let skip =
                    self.text[p..].chars().take_while(|c| *c == ' ' || *c == '\t').map(char::len_utf8).sum::<usize>();
                let caret = to_u16(self.text, p + skip.max(next.map_or(0, char::len_utf8)))?;
                return Ok(EditPlan { patches: Vec::new(), caret, mode: EditMode::Markup });
            }
            if prev.is_none_or(char::is_whitespace) {
                return Err(EditRefusal::NoChange);
            }
        }
        if prev.is_some_and(char::is_whitespace) {
            rendered = rendered.trim_start_matches(' ').to_owned();
        }
        if next.is_some_and(char::is_whitespace) && !rendered.ends_with('\n') {
            rendered = rendered.trim_end_matches(' ').to_owned();
        }
        if rendered.is_empty() && deletions.is_empty() {
            return Err(EditRefusal::NoChange);
        }
        // Paragraph breaks: runs of newlines become one blank line.
        let parts: Vec<&str> = rendered.split('\n').collect();
        let break_source = format!("{line_ending}{line_ending}");
        for level in 0..3u8 {
            let mut source = String::new();
            let mut prev_char = prev;
            let mut line_start = self.at_line_start(p);
            for (i, part) in parts.iter().enumerate() {
                if i > 0 {
                    if !source.ends_with(&break_source) {
                        source.push_str(&break_source);
                    }
                    line_start = true;
                    prev_char = Some('\n');
                }
                let following = if i + 1 == parts.len() { next } else { Some('\n') };
                source.push_str(&escape_markup(part, prev_char, following, line_start, level));
                if let Some(last) = part.chars().last() {
                    prev_char = Some(last);
                    line_start = false;
                }
            }
            if let Some(plan) = self.try_plan(p, deletions, &source, &rendered, false, EditMode::Markup, 0)? {
                return Ok(plan);
            }
        }
        Err(EditRefusal::WouldChangeStructure)
    }

    fn insert_math(&self, p: usize, deletions: &[Range<usize>], typed: &str) -> Result<EditPlan, EditRefusal> {
        let deleted_end = deletions.iter().map(|d| d.end).max().unwrap_or(p).max(p);
        let typed: String = typed.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
        let prev = self.prev_char(p);
        let next = self.next_char(deleted_end);
        if deletions.is_empty() && typed.chars().all(|c| c == ' ') {
            if prev.is_none_or(char::is_whitespace) || next.is_none_or(char::is_whitespace) {
                return Err(EditRefusal::NoChange);
            }
        }
        let mut source = String::new();
        let mut last = prev;
        for c in typed.chars() {
            if let Some(a) = last
                && math_needs_space(a, c)
            {
                source.push(' ');
            }
            match c {
                '$' | '#' | '"' | '\\' => {
                    source.push('\\');
                    source.push(c);
                }
                c => source.push(c),
            }
            last = Some(c);
        }
        let mut trailing = 0;
        if let (Some(a), Some(b)) = (last, next)
            && math_needs_space(a, b)
        {
            source.push(' ');
            trailing = 1;
        }
        match self.try_plan(p, deletions, &source, "", true, EditMode::Math, trailing)? {
            Some(plan) => Ok(plan),
            None => Err(EditRefusal::WouldChangeStructure),
        }
    }

    /// Build the patches for deleting `deletions` and inserting `source` at
    /// `p`, verify them, and compute the caret (after the inserted text minus
    /// `trailing` bytes).
    #[allow(clippy::too_many_arguments)]
    fn try_plan(
        &self,
        p: usize,
        deletions: &[Range<usize>],
        source: &str,
        rendered: &str,
        math: bool,
        mode: EditMode,
        trailing: usize,
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
        if !self.verify(&new_text, deletions, p, rendered, math) {
            return Ok(None);
        }
        // Caret: deletions before `p` shift it left.
        let removed_before: usize = deletions.iter().filter(|d| d.end <= p).map(|d| d.len()).sum();
        let caret_byte = p - removed_before + source.len() - trailing;
        let caret = to_u16(&new_text, caret_byte)?;
        let patches = byte_patches
            .into_iter()
            .map(|(range, text)| {
                let start = to_u16(self.text, range.start)?;
                let end = to_u16(self.text, range.end)?;
                Ok(TextPatch { range: TextRange::new(start, end), text })
            })
            .collect::<Result<Vec<_>, EditRefusal>>()?;
        Ok(Some(EditPlan { patches, caret, mode }))
    }

    fn inside_equation(&self, range: &Range<usize>) -> bool {
        self.leaves
            .iter()
            .filter(|l| l.range.start < range.end && l.range.end > range.start)
            .all(|l| l.mode == Mode::Math && l.kind != SyntaxKind::Dollar)
    }

    /// Replace a non-empty selection with `with` (used for deletion).
    fn replace(&self, sel: Range<usize>, with: &str) -> Result<EditPlan, EditRefusal> {
        let deletions = self.selection_deletions(sel.clone())?;
        if deletions.is_empty() {
            return Err(EditRefusal::NoChange);
        }
        let math = deletions.iter().all(|d| self.inside_equation(d));
        let mode = if math { EditMode::Math } else { EditMode::Markup };
        match self.try_plan(sel.start, &deletions, with, with, math, mode, 0)? {
            Some(plan) => Ok(plan),
            None => Err(EditRefusal::WouldChangeStructure),
        }
    }

    /// Delete the rendered unit before (or after) `p`.
    fn delete_unit(&self, p: usize, forward: bool) -> Result<EditPlan, EditRefusal> {
        let mut index = if forward { self.leaf_after(p) } else { self.leaf_before(p) };
        let mut guard = 0;
        while let Some(i) = index {
            guard += 1;
            if guard > 64 {
                break;
            }
            let leaf = &self.leaves[i];
            let next = |i: usize| if forward { (i + 1 < self.leaves.len()).then_some(i + 1) } else { i.checked_sub(1) };
            let step = classify(leaf);
            match step {
                Step::Skip => {
                    index = next(i);
                    continue;
                }
                Step::Refuse(reason) => return Err(EditRefusal::NotEditable { reason: reason.into() }),
                Step::Grapheme => {
                    let local = if forward {
                        p.max(leaf.range.start)..leaf.range.end
                    } else {
                        leaf.range.start..p.min(leaf.range.end)
                    };
                    let segment = &self.text[local.clone()];
                    let g = if forward { segment.graphemes(true).next() } else { segment.graphemes(true).next_back() };
                    let Some(g) = g else {
                        index = next(i);
                        continue;
                    };
                    let range =
                        if forward { local.start..local.start + g.len() } else { local.end - g.len()..local.end };
                    return self.delete_range(range, leaf.mode == Mode::Math);
                }
                Step::Whole => {
                    let range = leaf.unit.clone().unwrap_or_else(|| leaf.range.clone());
                    return self.delete_range(range, leaf.mode == Mode::Math);
                }
            }
        }
        Err(EditRefusal::NoChange)
    }

    fn delete_range(&self, range: Range<usize>, math: bool) -> Result<EditPlan, EditRefusal> {
        let deletions = vec![range.clone()];
        let mode = if math { EditMode::Math } else { EditMode::Markup };
        match self.try_plan(range.start, &deletions, "", "", math, mode, 0)? {
            Some(plan) => Ok(plan),
            None => Err(EditRefusal::WouldChangeStructure),
        }
    }
}

enum Step {
    Skip,
    Grapheme,
    Whole,
    Refuse(&'static str),
}

fn classify(leaf: &Leaf) -> Step {
    use SyntaxKind as K;
    match leaf.mode {
        Mode::Code => Step::Refuse("code"),
        Mode::Raw => Step::Refuse("raw"),
        Mode::Comment => Step::Skip,
        Mode::Markup => match leaf.kind {
            K::Text => Step::Grapheme,
            K::Escape | K::Shorthand | K::SmartQuote | K::Linebreak | K::Space | K::Parbreak => Step::Whole,
            K::Star
            | K::Underscore
            | K::HeadingMarker
            | K::ListMarker
            | K::EnumMarker
            | K::TermMarker
            | K::Colon
            | K::LeftBracket
            | K::RightBracket
            | K::Dollar => Step::Skip,
            _ => Step::Refuse("generated"),
        },
        Mode::Math => match leaf.kind {
            K::MathText => Step::Grapheme,
            K::MathIdent
            | K::MathShorthand
            | K::MathPrimes
            | K::Escape
            | K::Hat
            | K::Underscore
            | K::Slash
            | K::Root
            | K::MathAlignPoint
            | K::Bang
            | K::Linebreak => Step::Whole,
            K::Space
            | K::LeftParen
            | K::RightParen
            | K::LeftBrace
            | K::RightBrace
            | K::Comma
            | K::Semicolon
            | K::Dollar
            | K::Dot => Step::Skip,
            _ => Step::Refuse("generated"),
        },
    }
}

/// Collect deletion ranges for a selection: whole nodes inside it, the
/// selected part of text, and the content (not the delimiters) of partly
/// selected structures.
fn collect_deletions(
    node: &SyntaxNode,
    offset: usize,
    sel: &Range<usize>,
    out: &mut Vec<Range<usize>>,
) -> Result<(), EditRefusal> {
    let range = offset..offset + node.len();
    if range.end <= sel.start || range.start >= sel.end || range.is_empty() {
        return Ok(());
    }
    let is_root = offset == 0 && node.kind() == SyntaxKind::Markup;
    let inside = range.start >= sel.start && range.end <= sel.end;
    if node.children().len() == 0 {
        use SyntaxKind as K;
        match node.kind() {
            // Text can be partly deleted.
            K::Text | K::MathText => out.push(range.start.max(sel.start)..range.end.min(sel.end)),
            // Rendered atoms are deleted when fully selected.
            K::Escape
            | K::Shorthand
            | K::SmartQuote
            | K::Space
            | K::Parbreak
            | K::Linebreak
            | K::MathIdent
            | K::MathShorthand
            | K::MathPrimes
                if inside =>
            {
                out.push(range)
            }
            // Delimiters, markers, and code survive unless their whole
            // structure is selected (handled by the parent).
            _ => {}
        }
        return Ok(());
    }
    if !is_root && inside {
        out.push(range);
        return Ok(());
    }
    let mut child_offset = offset;
    for child in node.children() {
        collect_deletions(child, child_offset, sel, out)?;
        child_offset += child.len();
    }
    Ok(())
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

/// UTF-16 length helper for callers.
pub fn len16(text: &str) -> u32 {
    utf16_len(text) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str, at: u32, op: PageEditOp) -> Result<(String, u32), EditRefusal> {
        run_sel(text, at, at, op)
    }

    fn run_sel(text: &str, a: u32, b: u32, op: PageEditOp) -> Result<(String, u32), EditRefusal> {
        let plan = TypstInput.plan(text, TextRange::new(a, b), &op, "\n")?;
        let mut out = text.to_owned();
        for patch in plan.patches.iter().rev() {
            let r = utf16_range_to_bytes(&out, patch.range).unwrap();
            out.replace_range(r, &patch.text);
        }
        Ok((out, plan.caret))
    }

    fn ins(t: &str) -> PageEditOp {
        PageEditOp::Insert { text: t.into() }
    }

    #[test]
    fn plain_typing_is_inserted_verbatim() {
        assert_eq!(run("Hello world", 5, ins("X")).unwrap(), ("HelloX world".into(), 6));
        assert_eq!(run("Hello world", 5, ins(",")).unwrap(), ("Hello, world".into(), 6));
    }

    #[test]
    fn markup_specials_are_escaped() {
        for (typed, expected) in [
            ("*", "a\\*b"),
            ("_", "a\\_b"),
            ("#", "a\\#b"),
            ("$", "a\\$b"),
            ("[", "a\\[b"),
            ("]", "a\\]b"),
            ("@", "a\\@b"),
            ("<", "a\\<b"),
            ("`", "a\\`b"),
            ("~", "a\\~b"),
            ("\\", "a\\\\b"),
            ("'", "a\\'b"),
        ] {
            assert_eq!(run("ab", 1, ins(typed)).unwrap().0, expected, "typed {typed}");
        }
        // Line-start markers.
        assert_eq!(run("x\n\nab", 3, ins("= ")).unwrap().0, "x\n\n\\= ab");
        assert_eq!(run("x\n\nab", 3, ins("- ")).unwrap().0, "x\n\n\\- ab");
        assert_eq!(run("x\n\nab", 3, ins("+ ")).unwrap().0, "x\n\n\\+ ab");
        // Shorthands.
        assert_eq!(run("a-b", 2, ins("-")).unwrap().0, "a-\\-b");
        assert_eq!(run("a.b", 2, ins(".")).unwrap().0, "a.\\.b");
    }

    #[test]
    fn typing_inside_strong_stays_strong() {
        let (text, caret) = run("a *bold* c", 5, ins("X")).unwrap();
        assert_eq!(text, "a *boXld* c");
        assert_eq!(caret, 6);
    }

    #[test]
    fn math_insertions_avoid_identifier_merging() {
        // After `x` in `$x + 1$` typing `y` must not produce the identifier `xy`.
        let (text, caret) = run("$x + 1$", 2, ins("y")).unwrap();
        assert_eq!(text, "$x y + 1$");
        assert_eq!(caret, 4);
        let (text, _) = run("$x + 1$", 6, ins("2")).unwrap();
        assert_eq!(text, "$x + 12$");
        let (text, _) = run("$a$", 2, ins("$")).unwrap();
        assert_eq!(text, "$a\\$$");
    }

    #[test]
    fn deletion_removes_whole_escapes_and_graphemes() {
        assert_eq!(run("a\\*b", 3, PageEditOp::DeleteBackward).unwrap(), ("ab".into(), 1));
        assert_eq!(run("a\\*b", 1, PageEditOp::DeleteForward).unwrap(), ("ab".into(), 1));
        // A combining sequence is one grapheme.
        assert_eq!(run("ae\u{301}b", 3, PageEditOp::DeleteBackward).unwrap(), ("ab".into(), 1));
        // An astral character is one grapheme (two UTF-16 units).
        assert_eq!(run("a😀b", 3, PageEditOp::DeleteBackward).unwrap(), ("ab".into(), 1));
        // Deleting over a strong delimiter deletes the character inside.
        assert_eq!(run("a *b* c", 5, PageEditOp::DeleteBackward).unwrap().0, "a ** c");
    }

    #[test]
    fn deletion_refuses_structure_changes() {
        // Removing the space before `*b*` would turn the delimiters into text.
        assert!(matches!(run("a *b*", 2, PageEditOp::DeleteBackward), Err(EditRefusal::WouldChangeStructure)));
        // Code is not editable on the page.
        assert!(matches!(run("#x y", 2, PageEditOp::DeleteBackward), Err(EditRefusal::NotEditable { .. })));
    }

    #[test]
    fn math_deletion_inside_equation() {
        assert_eq!(run("$x^2$", 4, PageEditOp::DeleteBackward).unwrap().0, "$x^$");
        assert_eq!(run("$alpha + 1$", 6, PageEditOp::DeleteBackward).unwrap().0, "$ + 1$");
    }

    #[test]
    fn selection_delete_keeps_partial_delimiters() {
        // Select "ld" plus the closing delimiter: the delimiter survives.
        let (text, caret) = run_sel("a *bold* cd", 5, 8, PageEditOp::DeleteBackward).unwrap();
        assert_eq!(text, "a *bo* cd");
        assert_eq!(caret, 5);
        // Joining the strong text to the next word would make `*` part of a
        // word (no longer a delimiter): refused rather than corrupting.
        assert_eq!(run_sel("a *bold* cd", 5, 10, PageEditOp::DeleteBackward), Err(EditRefusal::WouldChangeStructure));
        // Whole structures inside the selection are removed.
        let (text, _) = run_sel("a *b* c", 1, 6, PageEditOp::DeleteBackward).unwrap();
        assert_eq!(text, "ac");
    }

    #[test]
    fn paragraph_break_and_paste() {
        assert_eq!(run("ab cd", 3, PageEditOp::ParagraphBreak).unwrap(), ("ab \n\ncd".into(), 5));
        let (text, _) = run("ab", 1, ins("x*y\n- z")).unwrap();
        assert_eq!(text, "ax\\*y\n\n\\- zb");
    }

    #[test]
    fn space_next_to_space_moves_caret() {
        let plan = TypstInput.plan("a b", TextRange::empty(1), &ins(" "), "\n").unwrap();
        assert!(plan.patches.is_empty());
        assert_eq!(plan.caret, 2);
    }
}
