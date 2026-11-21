//! Character-level page ↔ source mapping for LaTeX output.
//!
//! Inputs per page: the glyphs extracted from the PDF (drawing order = TeX
//! shipout order), the SyncTeX sheet for the page, and the [`SourceSet`] of
//! the build snapshot.
//!
//! 1. **Line windows (SyncTeX).** Every glyph is located in the innermost
//!    SyncTeX hbox containing it; the input lines recorded by that box and
//!    its glue/kern/math records form the glyph's *window* (file tag + line
//!    range). Consecutive glyphs with overlapping windows form a *block*
//!    (typically a paragraph, a display, a footnote, a caption).
//! 2. **Source model.** [`crate::latex`] predicts, for the window's lines,
//!    the visible characters and the exact source range of each (accent
//!    macros, TeX ligature input, math macros, styled arguments, …).
//! 3. **Alignment.** The block's normalized glyph text is aligned (LCS) with
//!    the normalized expected text of each candidate stream (main flow,
//!    footnotes, captions, title block); the best stream wins, leftovers are
//!    re-aligned against the remaining streams, and math glyphs that TeX
//!    emits out of source order (limits) are matched when the candidate is
//!    unique.
//! 4. **Classification.** Every glyph ends up mapped to a UTF-16 source
//!    range (with caret boundaries) or `Unmapped` with a reason: generated
//!    text (numbers, citation labels, macro output), discretionary hyphens,
//!    glyphs without Unicode, or glyphs outside any source line.
//!
//! Round-trip verification ([`MappingStats::roundtrip_exact`]) checks that
//! the expected text of the mapped source equals the glyph text after
//! normalization.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use exposition_core::path::WorkspacePath;
use exposition_core::text::{TextRange, utf16_len, utf16_to_byte};
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::latex::{self, Construct, Mode, SourceModel};
use crate::normalize::{comparison_chars, spacing_accent_to_combining};
use crate::pdf::{PageGlyph, PageRect};
use crate::synctex::{NodeKind, SyncSheet, SyncTex};

/// One source file of a build snapshot together with its source model.
pub struct SourceFile {
    pub path: WorkspacePath,
    pub text: Arc<str>,
    pub model: SourceModel,
    /// Lookups over `text` and `model` that mapping a page needs for every
    /// glyph cluster, prepared once per file: a page is mapped in time
    /// proportional to the page, not to the file.
    index: SourceIndex,
}

/// Bytes between two marks of a [`SourceIndex`].
const OFFSET_MARK_BYTES: usize = 1024;

struct SourceIndex {
    /// (byte offset, UTF-16 offset) of the first character boundary at or
    /// after every [`OFFSET_MARK_BYTES`] bytes, starting with (0, 0).
    marks: Vec<(usize, usize)>,
    /// The units' lines never decrease in unit order (units are in source
    /// order): the units of a line range are then one run.
    lines_ordered: bool,
    /// The units' byte ranges never go backwards in unit order.
    ranges_ordered: bool,
    /// The units of each deferred stream, in unit order.
    deferred: HashMap<u32, Vec<usize>>,
}

impl SourceIndex {
    fn new(text: &str, model: &SourceModel) -> Self {
        let mut marks = vec![(0, 0)];
        let mut next = OFFSET_MARK_BYTES;
        let mut units = 0;
        for (byte, ch) in text.char_indices() {
            if byte >= next {
                marks.push((byte, units));
                next = byte + OFFSET_MARK_BYTES;
            }
            units += ch.len_utf16();
        }
        let lines_ordered = model.units.windows(2).all(|pair| pair[0].line <= pair[1].line);
        let ranges_ordered = model
            .units
            .windows(2)
            .all(|pair| pair[0].range.start <= pair[1].range.start && pair[0].range.end <= pair[1].range.end);
        let mut deferred: HashMap<u32, Vec<usize>> =
            model.deferred_streams.iter().map(|stream| (*stream, Vec::new())).collect();
        for (index, unit) in model.units.iter().enumerate() {
            if let Some(list) = deferred.get_mut(&unit.stream) {
                list.push(index);
            }
        }
        Self { marks, lines_ordered, ranges_ordered, deferred }
    }
}

impl SourceFile {
    fn new(path: WorkspacePath, text: Arc<str>) -> Self {
        let model = latex::parse(&text);
        let index = SourceIndex::new(&text, &model);
        Self { path, text, model, index }
    }

    /// UTF-16 offset of a byte offset (`None` beyond the text or inside a
    /// character), as `exposition_core::text::byte_to_utf16`.
    fn byte_to_utf16(&self, byte: usize) -> Option<usize> {
        if byte > self.text.len() || !self.text.is_char_boundary(byte) {
            return None;
        }
        let after = self.index.marks.partition_point(|mark| mark.0 <= byte);
        let (mark_byte, mark_units) = self.index.marks[after - 1];
        Some(mark_units + utf16_len(&self.text[mark_byte..byte]))
    }

    /// Byte offset of a UTF-16 offset (`None` beyond the text or inside a
    /// surrogate pair), as `exposition_core::text::utf16_to_byte`.
    fn utf16_to_byte(&self, offset: usize) -> Option<usize> {
        let after = self.index.marks.partition_point(|mark| mark.1 <= offset);
        let (mark_byte, mark_units) = self.index.marks[after - 1];
        utf16_to_byte(&self.text[mark_byte..], offset - mark_units).ok().map(|byte| mark_byte + byte)
    }

    /// Indexes of the units on lines `first..=last`, in unit order.
    fn units_on_lines(&self, first: u32, last: u32) -> Vec<usize> {
        let units = &self.model.units;
        if self.index.lines_ordered {
            let start = units.partition_point(|unit| unit.line < first);
            let end = units.partition_point(|unit| unit.line <= last);
            (start..end.max(start)).collect()
        } else {
            (0..units.len()).filter(|&i| units[i].line >= first && units[i].line <= last).collect()
        }
    }

    /// The units whose byte range lies within `start..end`, in unit order.
    fn units_within(&self, start: usize, end: usize) -> Vec<&latex::SourceUnit> {
        let units = &self.model.units;
        if self.index.ranges_ordered {
            let from = units.partition_point(|unit| unit.range.start < start);
            units[from..].iter().take_while(|unit| unit.range.end <= end).collect()
        } else {
            units.iter().filter(|unit| unit.range.start >= start && unit.range.end <= end).collect()
        }
    }
}

/// The source files of a build snapshot, bound to SyncTeX input tags.
#[derive(Default)]
pub struct SourceSet {
    files: Vec<SourceFile>,
    by_tag: HashMap<u32, usize>,
}

impl SourceSet {
    /// `files`: snapshot texts by workspace path. `resolve` maps a SyncTeX
    /// input name to a workspace path (or `None` for files outside the
    /// project, such as packages and generated `.aux`/`.bbl`).
    pub fn new(
        files: impl IntoIterator<Item = (WorkspacePath, Arc<str>)>,
        synctex: &SyncTex,
        resolve: impl Fn(&str) -> Option<WorkspacePath>,
    ) -> Self {
        let files: Vec<SourceFile> = files.into_iter().map(|(path, text)| SourceFile::new(path, text)).collect();
        let mut by_tag = HashMap::new();
        for (tag, name) in &synctex.inputs {
            if let Some(path) = resolve(name)
                && let Some(index) = files.iter().position(|f| f.path == path)
            {
                by_tag.insert(*tag, index);
            }
        }
        Self { files, by_tag }
    }

    pub fn file(&self, index: usize) -> Option<&SourceFile> {
        self.files.get(index)
    }

    pub fn files(&self) -> &[SourceFile] {
        &self.files
    }

    pub fn file_for_tag(&self, tag: u32) -> Option<usize> {
        self.by_tag.get(&tag).copied()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, rename = "TexUnmappedReason")]
#[serde(rename_all = "camelCase")]
pub enum UnmappedReason {
    /// The PDF gives no Unicode for the glyph.
    NoUnicode,
    /// No SyncTeX box with a project source line contains the glyph
    /// (page furniture, generated bibliography, package output).
    NoSourceLine,
    /// A hyphen inserted by line breaking.
    Hyphenation,
    /// Text generated by macros or counters (section/equation numbers,
    /// citation labels, footnote marks, `\LaTeX`, user macro bodies).
    Generated,
}

/// Where a glyph came from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, rename = "TexGlyphTarget")]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum GlyphTarget {
    Source {
        path: WorkspacePath,
        /// UTF-16 range over the snapshot text of `path`.
        range: TextRange,
        /// UTF-16 caret positions across the glyph, left to right (at least
        /// `[range.start, range.end]`; ligatures add inner boundaries).
        boundaries: Vec<u32>,
        mode: Mode,
        construct: Construct,
        /// Every source unit behind the glyph is literal text (editable
        /// character by character); otherwise it is an atomic macro such as
        /// `\'e`, `--`, or `\alpha`.
        literal: bool,
        /// UTF-16 range of the enclosing math construct, delimiters included.
        equation: Option<TextRange>,
    },
    Unmapped {
        reason: UnmappedReason,
    },
}

impl GlyphTarget {
    pub fn is_mapped(&self) -> bool {
        matches!(self, GlyphTarget::Source { .. })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, rename = "TexMappingStats")]
#[serde(rename_all = "camelCase")]
pub struct MappingStats {
    pub glyphs: u32,
    pub mapped: u32,
    pub mapped_text: u32,
    pub mapped_math: u32,
    pub unmapped: BTreeMap<String, u32>,
    /// Mapped glyph groups whose normalized source text equals the glyph text.
    pub roundtrip_exact: u32,
    pub roundtrip_checked: u32,
}

/// The mapping of one page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, rename = "TexPageMapping")]
#[serde(rename_all = "camelCase")]
pub struct PageMapping {
    /// 0-based page index.
    pub page: u32,
    pub glyphs: Vec<PageGlyph>,
    /// Parallel to `glyphs`.
    pub targets: Vec<GlyphTarget>,
    pub stats: MappingStats,
    /// Source units (per file index) expected in this page's windows, and
    /// those matched — for document-level coverage.
    #[serde(skip)]
    #[ts(skip)]
    pub expected_units: BTreeSet<(usize, usize)>,
    #[serde(skip)]
    #[ts(skip)]
    pub matched_units: BTreeSet<(usize, usize)>,
}

/// A caret position on the page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, rename = "TexCaretHit")]
#[serde(rename_all = "camelCase")]
pub struct CaretHit {
    pub glyph: u32,
    pub path: WorkspacePath,
    /// UTF-16 offset in the snapshot text of `path`.
    pub offset: u32,
    pub x: f64,
    pub y0: f64,
    pub y1: f64,
}

impl PageMapping {
    /// Caret at a page point: the mapped glyph under (or nearest to, within
    /// `tolerance` points) the point, and the caret boundary closest to `x`.
    pub fn hit_test(&self, x: f64, y: f64, tolerance: f64) -> Option<CaretHit> {
        // A glyph whose ink (outline bounds, slightly inflated) contains the
        // point wins — stacked math (limits over a large operator) has
        // overlapping layout boxes. Otherwise the nearest layout box; among
        // overlapping boxes (kerning), the glyph whose centre is closest.
        let mut best: Option<(u8, f64, f64, usize)> = None;
        for (i, (glyph, target)) in self.glyphs.iter().zip(&self.targets).enumerate() {
            if !target.is_mapped() || self.is_attached_accent(i) {
                continue;
            }
            let in_ink =
                glyph.ink.is_some_and(|r| PageRect::new(r.x0 - 0.5, r.y0 - 0.5, r.x1 + 0.5, r.y1 + 0.5).contains(x, y));
            let rank = u8::from(!in_ink);
            let d = glyph.bbox.distance_sq(x, y);
            let centre = (x - (glyph.origin_x + glyph.advance / 2.0)).abs();
            let better = best.is_none_or(|(r, b, c, _)| (rank, d, centre) < (r, b, c));
            if (in_ink || d <= tolerance * tolerance) && better {
                best = Some((rank, d, centre, i));
            }
        }
        let (_, _, _, index) = best?;
        let glyph = &self.glyphs[index];
        let GlyphTarget::Source { path, boundaries, .. } = &self.targets[index] else { return None };
        let steps = boundaries.len().saturating_sub(1).max(1);
        let fraction = if glyph.advance > 0.0 { ((x - glyph.origin_x) / glyph.advance).clamp(0.0, 1.0) } else { 0.0 };
        let slot = (fraction * steps as f64).round() as usize;
        let slot = slot.min(boundaries.len() - 1);
        Some(CaretHit {
            glyph: index as u32,
            path: path.clone(),
            offset: boundaries[slot],
            x: glyph.origin_x + glyph.advance * slot as f64 / steps as f64,
            y0: glyph.bbox.y0,
            y1: glyph.bbox.y1,
        })
    }

    /// A spacing accent glyph drawn over a neighbouring base glyph with the
    /// same target (TeX `\accent` construction): the base is the hit target.
    fn is_attached_accent(&self, i: usize) -> bool {
        let is_accent = self.glyphs[i]
            .text
            .as_deref()
            .and_then(|t| t.chars().next().filter(|_| t.chars().count() == 1))
            .and_then(spacing_accent_to_combining)
            .is_some();
        is_accent
            && [i.checked_sub(1), Some(i + 1)]
                .into_iter()
                .flatten()
                .any(|j| self.targets.get(j).is_some_and(|t| t == &self.targets[i]))
    }

    /// Page position of a caret at `offset` in `path`, if a mapped glyph
    /// has that boundary.
    pub fn caret_for_offset(&self, path: &WorkspacePath, offset: u32) -> Option<CaretHit> {
        let mut fallback = None;
        for (i, (glyph, target)) in self.glyphs.iter().zip(&self.targets).enumerate() {
            let GlyphTarget::Source { path: p, boundaries, .. } = target else { continue };
            if p != path {
                continue;
            }
            let steps = boundaries.len().saturating_sub(1).max(1);
            if let Some(slot) = boundaries.iter().position(|&b| b == offset) {
                let hit = CaretHit {
                    glyph: i as u32,
                    path: p.clone(),
                    offset,
                    x: glyph.origin_x + glyph.advance * slot as f64 / steps as f64,
                    y0: glyph.bbox.y0,
                    y1: glyph.bbox.y1,
                };
                if slot == 0 {
                    return Some(hit);
                }
                fallback.get_or_insert(hit);
            }
        }
        fallback
    }

    /// The glyph drawn at a page point (ink first, then layout box), mapped
    /// or not — lets a page view show generated/unsupported regions (and
    /// their reason) instead of placing a caret in them.
    pub fn glyph_at(&self, x: f64, y: f64) -> Option<u32> {
        let ink = self.glyphs.iter().position(|g| {
            g.ink.is_some_and(|r| PageRect::new(r.x0 - 0.5, r.y0 - 0.5, r.x1 + 0.5, r.y1 + 0.5).contains(x, y))
        });
        ink.or_else(|| self.glyphs.iter().position(|g| g.bbox.contains(x, y))).map(|i| i as u32)
    }

    /// Visible text of the page in drawing order (debugging/tests).
    pub fn text(&self) -> String {
        self.glyphs.iter().map(|g| g.text.as_deref().unwrap_or("\u{FFFD}")).collect()
    }
}

/// A run of glyphs forming one logical character cluster (an accent + its
/// base, or a single glyph).
struct Cluster {
    glyphs: Vec<usize>,
    chars: Vec<char>,
}

/// File tag → inclusive line range.
type Window = BTreeMap<u32, (u32, u32)>;
/// (file index, stream id).
type StreamKey = (usize, u32);
/// (cluster index, unit index) pairs.
type Matches = Vec<(usize, usize)>;

fn windows_overlap(a: &Window, b: &Window) -> bool {
    b.iter().any(|(tag, (b0, b1))| a.get(tag).is_some_and(|(a0, a1)| *b0 <= a1 + 1 && *a0 <= b1 + 1))
}

fn merge_window(into: &mut Window, from: &Window) {
    for (tag, (l0, l1)) in from {
        let entry = into.entry(*tag).or_insert((*l0, *l1));
        entry.0 = entry.0.min(*l0);
        entry.1 = entry.1.max(*l1);
    }
}

/// Map one page. `page_index` is 0-based.
pub fn map_page(page_index: u32, mut glyphs: Vec<PageGlyph>, synctex: &SyncTex, sources: &SourceSet) -> PageMapping {
    let mut targets = vec![GlyphTarget::Unmapped { reason: UnmappedReason::NoSourceLine }; glyphs.len()];
    let mut expected_units = BTreeSet::new();
    let mut matched_units = BTreeSet::new();
    let mut clusters = build_clusters(&glyphs);

    if let Some(sheet) = synctex.sheet(page_index + 1) {
        let index = SheetIndex::new(sheet, sources);
        // Window per cluster.
        let windows: Vec<Window> = clusters.iter().map(|c| index.window_for(&glyphs[c.glyphs[0]])).collect();
        // Blocks of consecutive clusters with overlapping windows.
        let mut blocks: Vec<(Window, Vec<usize>)> = Vec::new();
        for (ci, window) in windows.iter().enumerate() {
            if window.is_empty() {
                continue;
            }
            match blocks.last_mut() {
                Some((bw, members)) if windows_overlap(bw, window) => {
                    merge_window(bw, window);
                    members.push(ci);
                }
                _ => blocks.push((window.clone(), vec![ci])),
            }
        }
        let mut cluster_units: Vec<Option<(usize, Vec<usize>)>> = vec![None; clusters.len()];
        for (window, members) in &blocks {
            let window = extend_with_spans(window, sources);
            align_block(
                &window,
                members,
                &clusters,
                sources,
                &mut cluster_units,
                &mut expected_units,
                &mut matched_units,
            );
        }
        assign_targets(&clusters, &cluster_units, &glyphs, sources, &windows, &mut targets);
        // Mapped glyphs whose Unicode the PDF does not reveal (or an OT1
        // accent glyph standing for a typed `~`/`^`) take the text of their
        // source unit, so copying gives what was typed.
        let unknown = |g: &PageGlyph| g.text.as_deref().is_none_or(|t| t.is_empty() || t == "\u{FFFD}");
        for (ci, cluster) in clusters.iter_mut().enumerate() {
            let Some((file, units)) = &cluster_units[ci] else { continue };
            let text: String = units.iter().map(|&u| sources.files[*file].model.units[u].text.as_str()).collect();
            let accent_for_ascii = cluster.glyphs.len() == 1
                && matches!(
                    (glyphs[cluster.glyphs[0]].text.as_deref(), text.as_str()),
                    (Some("˜"), "~") | (Some("ˆ"), "^")
                );
            if accent_for_ascii || cluster.glyphs.iter().all(|&g| unknown(&glyphs[g])) {
                for (k, &g) in cluster.glyphs.iter().enumerate() {
                    glyphs[g].text = Some(if k == 0 { text.clone() } else { String::new() });
                }
                cluster.chars = comparison_chars(&text);
            }
        }
    }
    // Rules recorded as glyphs (see `pdf::RULE_FONT`) stay only where they
    // typeset a source character.
    if glyphs.iter().any(|g| g.font == crate::pdf::RULE_FONT) {
        let keep: Vec<bool> =
            glyphs.iter().zip(&targets).map(|(g, t)| g.font != crate::pdf::RULE_FONT || t.is_mapped()).collect();
        let mut renumbered = vec![usize::MAX; glyphs.len()];
        let mut next = 0;
        for (i, kept) in keep.iter().enumerate() {
            if *kept {
                renumbered[i] = next;
                next += 1;
            }
        }
        clusters.retain_mut(|c| {
            c.glyphs.retain(|&g| keep[g]);
            c.glyphs.iter_mut().for_each(|g| *g = renumbered[*g]);
            !c.glyphs.is_empty()
        });
        let kept: Vec<(PageGlyph, GlyphTarget)> =
            glyphs.into_iter().zip(targets).zip(&keep).filter(|(_, k)| **k).map(|(pair, _)| pair).collect();
        (glyphs, targets) = kept.into_iter().unzip();
        for (i, g) in glyphs.iter_mut().enumerate() {
            g.index = i as u32;
        }
    }

    // Glyphs without Unicode are never mappable.
    for (glyph, target) in glyphs.iter().zip(targets.iter_mut()) {
        if glyph.text.as_deref().is_none_or(str::is_empty) && !target.is_mapped() {
            *target = GlyphTarget::Unmapped { reason: UnmappedReason::NoUnicode };
        }
    }
    let stats = compute_stats(&clusters, &glyphs, &targets, sources);
    PageMapping { page: page_index, glyphs, targets, stats, expected_units, matched_units }
}

/// Group accent glyphs with the base glyph they sit on.
fn build_clusters(glyphs: &[PageGlyph]) -> Vec<Cluster> {
    let mut clusters: Vec<Cluster> = Vec::new();
    let mut i = 0;
    while i < glyphs.len() {
        let text = glyphs[i].text.clone().unwrap_or_default();
        // Spacing accents (TeX `\accent`) and overlay marks (e.g. the U+0338
        // solidus cm fonts draw over "=" for `\neq`) combine with the glyph
        // they are drawn over.
        let accent = single_char(&text).and_then(|c| {
            spacing_accent_to_combining(c).or(unicode_normalization::char::is_combining_mark(c).then_some(c))
        });
        if let Some(mark) = accent {
            // Accent followed by its base (\accent order) or preceded by it (cedilla).
            if let Some(next) = glyphs.get(i + 1)
                && overlaps(&glyphs[i], next)
                && let Some(base) = next.text.as_deref().and_then(single_char)
            {
                let composed: String = [base, mark].iter().collect::<String>().nfc().collect();
                clusters.push(Cluster { glyphs: vec![i, i + 1], chars: comparison_chars(&composed) });
                i += 2;
                continue;
            }
            if let Some(prev) = clusters.last_mut()
                && prev.glyphs.len() == 1
                && overlaps(&glyphs[prev.glyphs[0]], &glyphs[i])
                && prev.chars.len() == 1
            {
                let composed: String = [prev.chars[0], mark].iter().collect::<String>().nfc().collect();
                prev.glyphs.push(i);
                prev.chars = comparison_chars(&composed);
                i += 1;
                continue;
            }
        }
        // A glyph without Unicode still takes part in the alignment as an
        // unknown character (see `lcs_align`).
        let chars = if text.is_empty() { vec!['\u{FFFD}'] } else { comparison_chars(&text) };
        clusters.push(Cluster { glyphs: vec![i], chars });
        i += 1;
    }
    clusters
}

fn single_char(text: &str) -> Option<char> {
    let mut chars = text.chars();
    let c = chars.next()?;
    chars.next().is_none().then_some(c)
}

/// Whether two glyphs are drawn over each other horizontally (ink extents
/// when known: overlay marks such as cm's negation slash have no advance).
fn overlaps(a: &PageGlyph, b: &PageGlyph) -> bool {
    let span = |g: &PageGlyph| match g.ink {
        Some(r) if r.width() > 0.0 => (r.x0, r.x1),
        _ => (g.origin_x, g.origin_x + g.advance),
    };
    let ((a0, a1), (b0, b1)) = (span(a), span(b));
    let overlap = a1.min(b1) - a0.max(b0);
    overlap > 0.3 * (a1 - a0).min(b1 - b0).max(0.1)
}

struct SheetIndex<'a> {
    sheet: &'a SyncSheet,
    hboxes: Vec<usize>,
    /// Lines recorded inside each box (own line + point records), by tag.
    box_windows: HashMap<usize, Window>,
}

impl<'a> SheetIndex<'a> {
    fn new(sheet: &'a SyncSheet, sources: &SourceSet) -> Self {
        let hboxes: Vec<usize> = sheet
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.kind == NodeKind::HBox && n.width > 0.0)
            .map(|(i, _)| i)
            .collect();
        let mut box_windows: HashMap<usize, Window> = HashMap::new();
        for (i, node) in sheet.nodes.iter().enumerate() {
            if sources.file_for_tag(node.tag).is_none() || node.line == 0 {
                continue;
            }
            // A record contributes to its own box (if a box) and its direct parent box.
            let mut targets = Vec::new();
            if node.kind == NodeKind::HBox {
                targets.push(i);
            }
            if let Some(p) = node.parent {
                targets.push(p as usize);
            }
            for t in targets {
                let w = box_windows.entry(t).or_default();
                let e = w.entry(node.tag).or_insert((node.line, node.line));
                e.0 = e.0.min(node.line);
                e.1 = e.1.max(node.line);
            }
        }
        Self { sheet, hboxes, box_windows }
    }

    fn window_for(&self, glyph: &PageGlyph) -> Window {
        let px = glyph.origin_x + glyph.advance * 0.5;
        let py = glyph.origin_y - 0.25 * glyph.size;
        let mut best: Option<(f64, usize)> = None;
        for &i in &self.hboxes {
            let node = &self.sheet.nodes[i];
            let r = node.rect();
            let inflated = PageRect::new(r.x0 - 0.5, r.y0 - 0.5, r.x1 + 0.5, r.y1 + 0.5);
            if inflated.contains(px, py) {
                let area = r.width() * r.height().max(0.1);
                if best.is_none_or(|(a, _)| area < a) {
                    best = Some((area, i));
                }
            }
        }
        // Use the innermost box with recorded project lines, walking outwards
        // through enclosing hboxes only (vboxes span whole pages/floats).
        let mut current = best.map(|(_, i)| i);
        while let Some(i) = current {
            if let Some(w) = self.box_windows.get(&i)
                && !w.is_empty()
            {
                return w.clone();
            }
            current =
                self.sheet.nodes[i].parent.map(|p| p as usize).filter(|&p| self.sheet.nodes[p].kind == NodeKind::HBox);
        }
        Window::new()
    }
}

/// Widen a window over multi-line constructs it touches: SyncTeX attributes
/// display math and multi-line arguments to their last line.
fn extend_with_spans(window: &Window, sources: &SourceSet) -> Window {
    let mut out = window.clone();
    for (tag, range) in out.iter_mut() {
        let Some(file) = sources.file_for_tag(*tag).and_then(|i| sources.file(i)) else { continue };
        loop {
            let mut changed = false;
            for &(s0, s1) in &file.model.spans {
                // Only when the window reaches into the construct past its
                // first line (where SyncTeX puts the construct's content).
                let inside = range.1 > s0 && range.0 <= s1;
                if inside && (s0 < range.0 || s1 > range.1) {
                    range.0 = range.0.min(s0);
                    range.1 = range.1.max(s1);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }
    out
}

/// Align the clusters of one block against the source units of its window.
/// `taken` holds units already matched on this page (never matched twice).
fn align_block(
    window: &Window,
    members: &[usize],
    clusters: &[Cluster],
    sources: &SourceSet,
    cluster_units: &mut [Option<(usize, Vec<usize>)>],
    expected: &mut BTreeSet<(usize, usize)>,
    taken: &mut BTreeSet<(usize, usize)>,
) {
    // Candidate streams: (file, stream id) → unit indices in source order.
    let mut streams: BTreeMap<(usize, u32), Vec<usize>> = BTreeMap::new();
    for (tag, (l0, l1)) in window {
        let Some(file_index) = sources.file_for_tag(*tag) else { continue };
        let file = &sources.files[file_index];
        for ui in file.units_on_lines(*l0, *l1) {
            if !taken.contains(&(file_index, ui)) {
                streams.entry((file_index, file.model.units[ui].stream)).or_default().push(ui);
            }
        }
        // Deferred streams (title block) may be typeset anywhere.
        for stream in &file.model.deferred_streams {
            streams.entry((file_index, *stream)).or_insert_with(|| {
                let units = file.index.deferred.get(stream).map(Vec::as_slice).unwrap_or(&[]);
                units.iter().copied().filter(|i| !taken.contains(&(file_index, *i))).collect()
            });
        }
    }
    let mut remaining: Vec<usize> = members.to_vec();
    let mut used: BTreeSet<(usize, u32)> = BTreeSet::new();
    for _round in 0..4 {
        let pending: Vec<usize> = remaining
            .iter()
            .copied()
            .filter(|&c| cluster_units[c].is_none() && !clusters[c].chars.is_empty())
            .collect();
        if pending.is_empty() {
            break;
        }
        let pending_chars: usize = pending.iter().map(|&c| clusters[c].chars.len()).sum();
        let mut best: Option<(StreamKey, Matches)> = None;
        for (key, units) in &streams {
            if used.contains(key) {
                continue;
            }
            let file = &sources.files[key.0];
            let matches = lcs_align(&pending, clusters, units, file);
            let score = matches.len();
            // A stream must explain a substantial share of the glyphs or of
            // its own text; stray letter matches are not an alignment.
            let stream_chars: usize = units.iter().map(|&u| file.model.units[u].text.chars().count()).sum();
            let substantial =
                score * 2 >= pending_chars.min(stream_chars).max(1) || (key.1 == 0 && used.is_empty() && score >= 1);
            if substantial && best.as_ref().is_none_or(|(_, m)| score > m.len()) {
                best = Some((*key, matches));
            }
        }
        let Some((key, matches)) = best else { break };
        if matches.len() < if used.is_empty() { 1 } else { 3 } {
            break;
        }
        used.insert(key);
        for &u in &streams[&key] {
            expected.insert((key.0, u));
        }
        // matches: (cluster index, unit index)
        let mut per_cluster: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for (c, u) in matches {
            per_cluster.entry(c).or_default().push(u);
        }
        for (c, mut units) in per_cluster {
            units.sort_unstable();
            units.dedup();
            for &u in &units {
                taken.insert((key.0, u));
            }
            cluster_units[c] = Some((key.0, units));
        }
        remaining.retain(|c| cluster_units[*c].is_none());
    }

    // Out-of-order math (limits drawn above/below): an unmatched glyph next
    // to matched math takes the unique unmatched math unit with the same text
    // close to that neighbour in the source.
    for (k, &c) in members.iter().enumerate() {
        if cluster_units[c].is_some() || clusters[c].chars.is_empty() {
            continue;
        }
        let neighbour = |i: Option<usize>| -> Option<(usize, usize)> {
            let &n = members.get(i?)?;
            let (f, units) = cluster_units[n].as_ref()?;
            let u = *units.first()?;
            (sources.files[*f].model.units[u].mode == Mode::Math).then_some((*f, u))
        };
        let anchors: Vec<(usize, usize)> =
            [neighbour(k.checked_sub(1)), neighbour(Some(k + 1))].into_iter().flatten().collect();
        if anchors.is_empty() {
            continue;
        }
        let mut candidates = Vec::new();
        for ((file_index, _), units) in streams.iter().filter(|(k, _)| used.contains(k)) {
            let file = &sources.files[*file_index];
            for &u in units {
                let unit = &file.model.units[u];
                let near = anchors.iter().any(|&(af, au)| af == *file_index && au.abs_diff(u) <= 24);
                if near
                    && unit.mode == Mode::Math
                    && !taken.contains(&(*file_index, u))
                    && comparison_chars(&unit.text) == clusters[c].chars
                {
                    let distance =
                        anchors.iter().filter(|(af, _)| af == file_index).map(|(_, au)| au.abs_diff(u)).min();
                    candidates.push((distance.unwrap_or(usize::MAX), *file_index, u));
                }
            }
        }
        // The unique candidate, or the one strictly closest to the anchor.
        candidates.sort_unstable();
        let unique = match candidates.as_slice() {
            [only] => Some(*only),
            [first, second, ..] if first.0 < second.0 => Some(*first),
            _ => None,
        };
        if let Some((_, f, u)) = unique {
            taken.insert((f, u));
            cluster_units[c] = Some((f, vec![u]));
        }
    }
}

/// LCS alignment of cluster characters against unit characters. Returns
/// (cluster index, unit index) pairs, filtered for isolated stray matches.
fn lcs_align(pending: &[usize], clusters: &[Cluster], units: &[usize], file: &SourceFile) -> Vec<(usize, usize)> {
    let a: Vec<(char, usize)> =
        pending.iter().flat_map(|&c| clusters[c].chars.iter().map(move |&ch| (ch, c))).collect();
    let b: Vec<(char, usize)> = units
        .iter()
        .flat_map(|&u| comparison_chars(&file.model.units[u].text).into_iter().map(move |ch| (ch, u)))
        .collect();
    let (n, m) = (a.len(), b.len());
    if n == 0 || m == 0 || n.saturating_mul(m) > 40_000_000 {
        return Vec::new();
    }
    // dp[i][j] = LCS of a[i..], b[j..]
    let width = m + 1;
    let mut dp = vec![0u16; (n + 1) * width];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i * width + j] = if a[i].0 == b[j].0 {
                dp[(i + 1) * width + j + 1].saturating_add(1)
            } else {
                dp[(i + 1) * width + j].max(dp[i * width + j + 1])
            };
        }
    }
    let mut pairs: Vec<(usize, usize)> = Vec::new(); // (a index, b index)
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i].0 == b[j].0 && dp[i * width + j] == dp[(i + 1) * width + j + 1] + 1 {
            pairs.push((i, j));
            i += 1;
            j += 1;
        } else if dp[(i + 1) * width + j] >= dp[i * width + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    // Glyphs whose Unicode the PDF does not reveal (U+FFFD, e.g. pdfLaTeX's
    // OT1 `\$`, characters a font lacks) between two matches pair one to one
    // with an equally long run of unmatched source characters there.
    let mut filled: Vec<(usize, usize)> = Vec::with_capacity(pairs.len());
    for (k, &(ai, bj)) in pairs.iter().enumerate() {
        filled.push((ai, bj));
        if let Some(&(na, nb)) = pairs.get(k + 1) {
            let gap = na - ai - 1;
            if gap > 0 && nb - bj - 1 == gap && a[ai + 1..na].iter().all(|(ch, _)| *ch == '\u{FFFD}') {
                filled.extend((1..=gap).map(|d| (ai + d, bj + d)));
            }
        }
    }
    let pairs = filled;
    // Drop isolated single-character matches surrounded by gaps on both sides
    // (typically generated digits matching a stray source digit).
    let keep: Vec<bool> = pairs
        .iter()
        .enumerate()
        .map(|(k, &(ai, bj))| {
            let prev = k.checked_sub(1).map(|p| pairs[p]);
            let next = pairs.get(k + 1).copied();
            let joined_prev = prev.is_some_and(|(pa, pb)| pa + 1 == ai && pb + 1 == bj);
            let joined_next = next.is_some_and(|(na, nb)| na == ai + 1 && nb == bj + 1);
            joined_prev || joined_next || !a[ai].0.is_ascii_digit()
        })
        .collect();
    pairs.iter().zip(keep).filter(|(_, k)| *k).map(|(&(ai, bj), _)| (a[ai].1, b[bj].1)).collect()
}

fn assign_targets(
    clusters: &[Cluster],
    cluster_units: &[Option<(usize, Vec<usize>)>],
    glyphs: &[PageGlyph],
    sources: &SourceSet,
    windows: &[Window],
    targets: &mut [GlyphTarget],
) {
    for (ci, cluster) in clusters.iter().enumerate() {
        let target = match &cluster_units[ci] {
            Some((file_index, units)) => {
                let file = &sources.files[*file_index];
                let model_units: Vec<&latex::SourceUnit> = units.iter().map(|&u| &file.model.units[u]).collect();
                let start = model_units.iter().map(|u| u.range.start).min().unwrap_or(0);
                let end = model_units.iter().map(|u| u.range.end).max().unwrap_or(0);
                let to16 = |b: usize| file.byte_to_utf16(b).unwrap_or(0) as u32;
                let mut boundaries: Vec<u32> = model_units.iter().map(|u| to16(u.range.start)).collect();
                boundaries.push(to16(end));
                boundaries.dedup();
                if boundaries.len() < 2 {
                    boundaries = vec![to16(start), to16(end)];
                }
                let first = model_units[0];
                let equation = (first.mode == Mode::Math)
                    .then(|| file.model.math_region_at(first.range.start))
                    .flatten()
                    .map(|(r, _)| TextRange::new(to16(r.start), to16(r.end)));
                GlyphTarget::Source {
                    path: file.path.clone(),
                    range: TextRange::new(to16(start), to16(end)),
                    boundaries,
                    mode: first.mode,
                    construct: first.construct,
                    literal: model_units.iter().all(|u| u.is_literal(&file.text)),
                    equation,
                }
            }
            None => {
                let text = cluster.chars.iter().collect::<String>();
                let last = *cluster.glyphs.last().unwrap_or(&0);
                let line_end = glyphs.get(last + 1).is_none_or(|next| {
                    (next.origin_y - glyphs[last].origin_y).abs() > 0.5 * glyphs[last].size
                        || next.origin_x < glyphs[last].origin_x
                });
                let reason = if windows[ci].is_empty() {
                    UnmappedReason::NoSourceLine
                } else if (text == "-") && line_end {
                    UnmappedReason::Hyphenation
                } else {
                    UnmappedReason::Generated
                };
                GlyphTarget::Unmapped { reason }
            }
        };
        for &g in &cluster.glyphs {
            targets[g] = target.clone();
        }
    }
}

fn compute_stats(
    clusters: &[Cluster],
    glyphs: &[PageGlyph],
    targets: &[GlyphTarget],
    sources: &SourceSet,
) -> MappingStats {
    let mut stats = MappingStats { glyphs: glyphs.len() as u32, ..Default::default() };
    for target in targets {
        match target {
            GlyphTarget::Source { mode, .. } => {
                stats.mapped += 1;
                match mode {
                    Mode::Text => stats.mapped_text += 1,
                    Mode::Math => stats.mapped_math += 1,
                }
            }
            GlyphTarget::Unmapped { reason } => {
                *stats.unmapped.entry(format!("{reason:?}")).or_default() += 1;
            }
        }
    }
    // Round trip: group clusters by identical target range; compare the
    // normalized glyph text with the normalized expected text of the units.
    let mut groups: BTreeMap<(String, u32, u32), Vec<usize>> = BTreeMap::new();
    for (ci, cluster) in clusters.iter().enumerate() {
        if let GlyphTarget::Source { path, range, .. } = &targets[cluster.glyphs[0]] {
            groups.entry((path.as_str().to_owned(), range.start, range.end)).or_default().push(ci);
        }
    }
    for ((path, start, end), members) in groups {
        let Some(file) = sources.files.iter().find(|f| f.path.as_str() == path) else { continue };
        let (Some(b0), Some(b1)) = (file.utf16_to_byte(start as usize), file.utf16_to_byte(end as usize)) else {
            continue;
        };
        let expected: String = file.units_within(b0, b1).into_iter().map(|u| u.text.as_str()).collect();
        let actual: Vec<char> = members.iter().flat_map(|&c| clusters[c].chars.iter().copied()).collect();
        stats.roundtrip_checked += 1;
        if comparison_chars(&expected) == actual {
            stats.roundtrip_exact += 1;
        }
    }
    stats
}

/// Document-level source coverage: of the visible source units expected in
/// the mapped windows, how many were matched to a glyph (by mode).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceCoverage {
    pub text_expected: u32,
    pub text_matched: u32,
    pub math_expected: u32,
    pub math_matched: u32,
    /// Expected units that were not matched, as (path, line, text).
    pub missed: Vec<(String, u32, String)>,
}

pub fn source_coverage(pages: &[PageMapping], sources: &SourceSet) -> SourceCoverage {
    let expected: BTreeSet<(usize, usize)> = pages.iter().flat_map(|p| p.expected_units.iter().copied()).collect();
    let matched: BTreeSet<(usize, usize)> = pages.iter().flat_map(|p| p.matched_units.iter().copied()).collect();
    let mut coverage = SourceCoverage::default();
    for key in &expected {
        let Some(file) = sources.file(key.0) else { continue };
        let unit = &file.model.units[key.1];
        let hit = matched.contains(key);
        match unit.mode {
            Mode::Text => {
                coverage.text_expected += 1;
                coverage.text_matched += hit as u32;
            }
            Mode::Math => {
                coverage.math_expected += 1;
                coverage.math_matched += hit as u32;
            }
        }
        if !hit {
            coverage.missed.push((file.path.to_string(), unit.line, unit.text.clone()));
        }
    }
    coverage
}

#[cfg(test)]
mod index_tests {
    use exposition_core::text::byte_to_utf16;

    use super::*;

    fn file(text: &str) -> SourceFile {
        SourceFile::new(WorkspacePath::parse("main.tex").unwrap(), Arc::from(text))
    }

    /// A long source with characters of every UTF-8 and UTF-16 width.
    fn long_text() -> String {
        let mut text = String::from("\\documentclass{article}\n\\begin{document}\n");
        for i in 0..400 {
            text.push_str(&format!(
                "Line {i}: caf\u{e9} na\u{ef}ve \u{1d538} $a_{i} + \\alpha$ \u{4e2d}\u{6587} end.\n"
            ));
        }
        text.push_str("\\end{document}\n");
        text
    }

    #[test]
    fn indexed_offsets_equal_a_scan_from_the_start() {
        let text = long_text();
        let file = file(&text);
        assert!(file.index.marks.len() > 10, "several marks: {}", file.index.marks.len());
        for byte in 0..=text.len() + 2 {
            assert_eq!(file.byte_to_utf16(byte), byte_to_utf16(&text, byte).ok(), "byte {byte}");
        }
        let units = utf16_len(&text);
        for offset in 0..=units + 2 {
            assert_eq!(file.utf16_to_byte(offset), utf16_to_byte(&text, offset).ok(), "offset {offset}");
        }
    }

    #[test]
    fn indexed_unit_lookups_equal_a_scan_of_every_unit() {
        let text = long_text();
        let file = file(&text);
        let units = &file.model.units;
        assert!(units.len() > 5000);
        assert!(file.index.lines_ordered && file.index.ranges_ordered, "units are in source order");
        for (first, last) in [(1, 1), (3, 3), (10, 12), (200, 199), (399, 500), (0, 2), (900, 950)] {
            let scanned: Vec<usize> =
                (0..units.len()).filter(|&i| units[i].line >= first && units[i].line <= last).collect();
            assert_eq!(file.units_on_lines(first, last), scanned, "lines {first}..={last}");
        }
        for (start, end) in [(0, 0), (0, 60), (57, 300), (1000, 1500), (text.len() - 40, text.len()), (500, 400)] {
            let scanned: Vec<&str> = units
                .iter()
                .filter(|u| u.range.start >= start && u.range.end <= end)
                .map(|u| u.text.as_str())
                .collect();
            let indexed: Vec<&str> = file.units_within(start, end).into_iter().map(|u| u.text.as_str()).collect();
            assert_eq!(indexed, scanned, "bytes {start}..{end}");
        }
    }
}

#[cfg(test)]
#[path = "mapping_review_tests.rs"]
mod review_tests;
