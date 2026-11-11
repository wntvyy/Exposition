//! Glyph maps from Typst layout.
//!
//! Walks the frames of a laid-out document (groups with their transforms,
//! text items with per-glyph spans and advances) and produces page clusters
//! linked to UTF-16 source ranges of the build snapshot:
//!
//! - prose glyphs map character-exactly through their span and span offset
//!   (verified against the source text; a mismatch means a show rule or
//!   function transformed the text, which is then not editable);
//! - glyphs are merged into grapheme clusters, and a glyph covering several
//!   graphemes (a ligature) gets proportional caret stops;
//! - escapes, shorthands, smart quotes, math identifiers and math shorthands
//!   are atomic (one source unit, one rendered unit);
//! - math text maps glyph-by-grapheme within its syntax node;
//! - generated content (numbering, markers, supplements, hyphens), code
//!   results, raw text, and content from packages are marked non-editable.

use std::collections::HashMap;
use std::ops::Range;

use exposition_core::path::WorkspacePath;
use exposition_core::text::TextRange;
use exposition_page::map::{
    Cluster, ClusterKind, ClusterSource, EditMode, EquationInfo, NonEditableReason, PageMap, RawPage, Stop,
};
use typst::World;
use typst::layout::{Abs, Frame, FrameItem, Point, Transform};
use typst::syntax::{FileId, LinkedNode, Source, Span, SyntaxKind, VirtualRoot, ast};
use typst::text::TextItem;
use typst_layout::PagedDocument;
use unicode_segmentation::UnicodeSegmentation;

/// How a span resolves to source.
#[derive(Clone, Debug)]
enum Resolved {
    /// No source (compiler-generated) or not in a known file.
    Generated(Option<(u32, Range<usize>)>),
    NonEditable(NonEditableReason, Option<(u32, Range<usize>)>),
    /// A markup text node: glyphs map through span offsets. `boundaries`
    /// are the node's grapheme boundaries (absolute byte offsets, sorted).
    Text {
        file: u32,
        node: Range<usize>,
        boundaries: std::sync::Arc<Vec<usize>>,
    },
    /// A math text node: glyphs map to its graphemes in order.
    MathText {
        file: u32,
        graphemes: Vec<Range<usize>>,
        node: Range<usize>,
        equation: Option<u32>,
    },
    /// One source unit rendered as one unit.
    Atomic {
        file: u32,
        range: Range<usize>,
        mode: EditMode,
        equation: Option<u32>,
    },
}

struct Extractor<'a> {
    world: &'a dyn World,
    files: Vec<WorkspacePath>,
    file_index: HashMap<FileId, Option<u32>>,
    sources: HashMap<u32, Source>,
    equations: Vec<EquationInfo>,
    equation_index: HashMap<(u32, usize), u32>,
    resolved: HashMap<Span, Resolved>,
    span_totals: HashMap<Span, usize>,
    span_seen: HashMap<Span, usize>,
}

/// A text item with its absolute transform.
struct Placed<'a> {
    ts: Transform,
    item: &'a TextItem,
}

fn collect<'a>(frame: &'a Frame, ts: Transform, out: &mut Vec<Placed<'a>>) {
    for (pos, item) in frame.items() {
        let local = ts.pre_concat(Transform::translate(pos.x, pos.y));
        match item {
            FrameItem::Group(group) => collect(&group.frame, local.pre_concat(group.transform), out),
            FrameItem::Text(text) => out.push(Placed { ts: local, item: text }),
            _ => {}
        }
    }
}

/// Build the page map of a document.
pub fn extract(world: &dyn World, document: &PagedDocument) -> PageMap {
    let mut ex = Extractor {
        world,
        files: Vec::new(),
        file_index: HashMap::new(),
        sources: HashMap::new(),
        equations: Vec::new(),
        equation_index: HashMap::new(),
        resolved: HashMap::new(),
        span_totals: HashMap::new(),
        span_seen: HashMap::new(),
    };
    let mut placed_pages = Vec::with_capacity(document.pages().len());
    for page in document.pages() {
        let mut placed = Vec::new();
        collect(&page.frame, Transform::identity(), &mut placed);
        for p in &placed {
            for glyph in &p.item.glyphs {
                *ex.span_totals.entry(glyph.span.0).or_default() += 1;
            }
        }
        placed_pages.push((page.frame.width().to_pt(), page.frame.height().to_pt(), placed));
    }
    let mut pages = Vec::with_capacity(placed_pages.len());
    for (width, height, placed) in &placed_pages {
        let mut clusters = Vec::new();
        for p in placed {
            ex.text_item(p, &mut clusters);
        }
        pages.push(RawPage { width: *width, height: *height, clusters });
    }
    PageMap::build(ex.files, ex.equations, pages)
}

struct GlyphBox {
    x0: f64,
    x1: f64,
    /// Local baseline (y down).
    baseline: f64,
}

impl Extractor<'_> {
    fn file(&mut self, id: FileId) -> Option<u32> {
        if let Some(&index) = self.file_index.get(&id) {
            return index;
        }
        let index = match id.root() {
            VirtualRoot::Project => WorkspacePath::parse(id.vpath().get_without_slash()).ok().map(|path| {
                self.files.push(path);
                (self.files.len() - 1) as u32
            }),
            VirtualRoot::Package(_) => None,
        };
        if let Some(i) = index
            && let Ok(source) = self.world.source(id)
        {
            self.sources.insert(i, source);
        }
        self.file_index.insert(id, index);
        index
    }

    fn utf16(&self, file: u32, range: &Range<usize>) -> Option<TextRange> {
        let lines = self.sources.get(&file)?.lines();
        let start = lines.byte_to_utf16(range.start)?;
        let end = lines.byte_to_utf16(range.end)?;
        Some(TextRange::new(start as u32, end as u32))
    }

    fn equation_of(&mut self, file: u32, node: &LinkedNode) -> Option<u32> {
        let mut current = node.parent().cloned();
        while let Some(n) = current {
            match n.kind() {
                SyntaxKind::Equation => {
                    let key = (file, n.offset());
                    if let Some(&i) = self.equation_index.get(&key) {
                        return Some(i);
                    }
                    let range = self.utf16(file, &n.range())?;
                    let block = n.get().cast::<ast::Equation>().is_some_and(|e| e.block());
                    self.equations.push(EquationInfo { file, range, block });
                    let i = (self.equations.len() - 1) as u32;
                    self.equation_index.insert(key, i);
                    return Some(i);
                }
                SyntaxKind::Markup | SyntaxKind::ContentBlock => return None,
                _ => current = n.parent().cloned(),
            }
        }
        None
    }

    fn resolve(&mut self, span: Span) -> Resolved {
        if let Some(r) = self.resolved.get(&span) {
            return r.clone();
        }
        let resolved = self.resolve_uncached(span);
        self.resolved.insert(span, resolved.clone());
        resolved
    }

    fn resolve_uncached(&mut self, span: Span) -> Resolved {
        let Some(id) = span.id() else { return Resolved::Generated(None) };
        let Some(file) = self.file(id) else {
            return Resolved::NonEditable(NonEditableReason::External, None);
        };
        let Some(source) = self.sources.get(&file).cloned() else { return Resolved::Generated(None) };
        let Some(node) = source.find(span) else { return Resolved::Generated(None) };
        let range = node.range();
        let in_raw = |n: &LinkedNode| {
            let mut p = n.parent().cloned();
            while let Some(x) = p {
                if x.kind() == SyntaxKind::Raw {
                    return true;
                }
                p = x.parent().cloned();
            }
            false
        };
        match node.kind() {
            SyntaxKind::Text if in_raw(&node) => Resolved::NonEditable(NonEditableReason::Raw, Some((file, range))),
            SyntaxKind::Text => {
                let text = &source.text()[range.clone()];
                let mut boundaries: Vec<usize> = text.grapheme_indices(true).map(|(i, _)| range.start + i).collect();
                boundaries.push(range.end);
                Resolved::Text { file, node: range, boundaries: std::sync::Arc::new(boundaries) }
            }
            SyntaxKind::MathText => {
                let text = &source.text()[range.clone()];
                let graphemes =
                    text.grapheme_indices(true).map(|(i, g)| range.start + i..range.start + i + g.len()).collect();
                let equation = self.equation_of(file, &node);
                Resolved::MathText { file, graphemes, node: range, equation }
            }
            SyntaxKind::Escape | SyntaxKind::Linebreak => {
                let equation = self.equation_of(file, &node);
                let mode = if equation.is_some() { EditMode::Math } else { EditMode::Markup };
                Resolved::Atomic { file, range, mode, equation }
            }
            SyntaxKind::Shorthand | SyntaxKind::SmartQuote | SyntaxKind::Space => {
                Resolved::Atomic { file, range, mode: EditMode::Markup, equation: None }
            }
            SyntaxKind::MathIdent
            | SyntaxKind::MathShorthand
            | SyntaxKind::MathPrimes
            | SyntaxKind::MathFieldAccess => {
                // A field access (`plus.minus`) is one unit.
                let unit = match node.parent() {
                    Some(parent) if parent.kind() == SyntaxKind::MathFieldAccess => parent.clone(),
                    _ => node.clone(),
                };
                let equation = self.equation_of(file, &unit);
                Resolved::Atomic { file, range: unit.range(), mode: EditMode::Math, equation }
            }
            SyntaxKind::Raw | SyntaxKind::RawTrimmed | SyntaxKind::RawLang | SyntaxKind::RawDelim => {
                Resolved::NonEditable(NonEditableReason::Raw, Some((file, range)))
            }
            kind if is_code(kind) => Resolved::NonEditable(NonEditableReason::Code, Some((file, range))),
            _ => Resolved::Generated(Some((file, range))),
        }
    }

    fn text_item(&mut self, placed: &Placed, out: &mut Vec<Cluster>) {
        let item = placed.item;
        let size = item.size;
        let metrics = item.font.metrics();
        let ascent = metrics.ascender.at(size).to_pt();
        let descent = (-metrics.descender.at(size).to_pt()).max(0.0);
        let rotated = placed.ts.kx.get().abs() > 1e-9 || placed.ts.ky.get().abs() > 1e-9;

        let mut x = 0.0;
        let mut boxes = Vec::with_capacity(item.glyphs.len());
        for glyph in &item.glyphs {
            let advance = glyph.x_advance.at(size).to_pt();
            boxes.push(GlyphBox { x0: x, x1: x + advance, baseline: -glyph.y_offset.at(size).to_pt() });
            x += advance;
        }

        let mut start = 0;
        while start < item.glyphs.len() {
            let span = item.glyphs[start].span.0;
            let mut end = start + 1;
            while end < item.glyphs.len() && item.glyphs[end].span.0 == span {
                end += 1;
            }
            let resolved =
                if rotated { Resolved::NonEditable(NonEditableReason::Transformed, None) } else { self.resolve(span) };
            self.run(placed, &boxes, start..end, &resolved, ascent, descent, out);
            start = end;
        }
    }

    /// Make a cluster in page coordinates from local glyph geometry.
    #[allow(clippy::too_many_arguments)]
    fn make(
        &self,
        placed: &Placed,
        x0: f64,
        x1: f64,
        baseline: f64,
        ascent: f64,
        descent: f64,
        text: String,
        source: Option<ClusterSource>,
        kind: ClusterKind,
        stops: Vec<(f64, u32, u32)>,
        equation: Option<u32>,
    ) -> Cluster {
        let ts = placed.ts;
        let pt = |x: f64, y: f64| Point::new(Abs::pt(x), Abs::pt(y)).transform(ts);
        let a = pt(x0, baseline - ascent);
        let b = pt(x1, baseline + descent);
        let base = pt(x0, baseline);
        Cluster {
            x0: a.x.to_pt().min(b.x.to_pt()),
            x1: a.x.to_pt().max(b.x.to_pt()),
            top: a.y.to_pt().min(b.y.to_pt()),
            bottom: a.y.to_pt().max(b.y.to_pt()),
            baseline: base.y.to_pt(),
            text,
            source,
            kind,
            stops: stops
                .into_iter()
                .map(|(x, offset, text_byte)| Stop { x: pt(x, baseline).x.to_pt(), offset, text_byte })
                .collect(),
            equation,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn run(
        &mut self,
        placed: &Placed,
        boxes: &[GlyphBox],
        glyphs: Range<usize>,
        resolved: &Resolved,
        ascent: f64,
        descent: f64,
        out: &mut Vec<Cluster>,
    ) {
        let item = placed.item;
        let rendered = |r: Range<usize>| -> String {
            let range = item.glyphs[r.start].range().start..item.glyphs[r.end - 1].range().end;
            item.text.get(range.start.min(range.end)..range.end.max(range.start)).unwrap_or_default().to_owned()
        };
        let baseline = boxes[glyphs.start].baseline;
        match resolved {
            Resolved::Generated(src) | Resolved::NonEditable(_, src) => {
                let reason = match resolved {
                    Resolved::NonEditable(reason, _) => *reason,
                    _ => NonEditableReason::Generated,
                };
                let source = src
                    .as_ref()
                    .and_then(|(file, r)| self.utf16(*file, r).map(|range| ClusterSource { file: *file, range }));
                let c = self.make(
                    placed,
                    boxes[glyphs.start].x0,
                    boxes[glyphs.end - 1].x1,
                    baseline,
                    ascent,
                    descent,
                    rendered(glyphs.clone()),
                    source,
                    ClusterKind::NonEditable(reason),
                    Vec::new(),
                    None,
                );
                out.push(c);
            }
            Resolved::Atomic { file, range, mode, equation } => {
                let Some(r16) = self.utf16(*file, range) else { return };
                let text = rendered(glyphs.clone());
                let len = text.len() as u32;
                let x0 = boxes[glyphs.start].x0;
                let x1 = boxes[glyphs.end - 1].x1;
                let c = self.make(
                    placed,
                    x0,
                    x1,
                    baseline,
                    ascent,
                    descent,
                    text,
                    Some(ClusterSource { file: *file, range: r16 }),
                    ClusterKind::Editable(*mode),
                    vec![(x0, r16.start, 0), (x1, r16.end, len)],
                    *equation,
                );
                out.push(c);
            }
            Resolved::MathText { file, graphemes, node, equation } => {
                let span = item.glyphs[glyphs.start].span.0;
                let total = self.span_totals.get(&span).copied().unwrap_or(0);
                for g in glyphs.clone() {
                    let seen = self.span_seen.entry(span).or_default();
                    let k = *seen;
                    *seen += 1;
                    let target = if total == graphemes.len() {
                        graphemes.get(k).cloned()
                    } else if k == 0 {
                        Some(node.clone())
                    } else {
                        None
                    };
                    let text = rendered(g..g + 1);
                    let b = &boxes[g];
                    let c = match target.and_then(|r| self.utf16(*file, &r)) {
                        Some(r16) => {
                            let len = text.len() as u32;
                            self.make(
                                placed,
                                b.x0,
                                b.x1,
                                b.baseline,
                                ascent,
                                descent,
                                text,
                                Some(ClusterSource { file: *file, range: r16 }),
                                ClusterKind::Editable(EditMode::Math),
                                vec![(b.x0, r16.start, 0), (b.x1, r16.end, len)],
                                *equation,
                            )
                        }
                        None => self.make(
                            placed,
                            b.x0,
                            b.x1,
                            b.baseline,
                            ascent,
                            descent,
                            text,
                            None,
                            ClusterKind::NonEditable(NonEditableReason::Generated),
                            Vec::new(),
                            *equation,
                        ),
                    };
                    out.push(c);
                }
            }
            Resolved::Text { file, node, boundaries } => {
                self.text_run(placed, boxes, glyphs, *file, node.clone(), boundaries, ascent, descent, out)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn text_run(
        &mut self,
        placed: &Placed,
        boxes: &[GlyphBox],
        glyphs: Range<usize>,
        file: u32,
        node: Range<usize>,
        boundaries: &[usize],
        ascent: f64,
        descent: f64,
        out: &mut Vec<Cluster>,
    ) {
        let item = placed.item;
        let Some(source) = self.sources.get(&file).cloned() else { return };
        let text = source.text();
        let is_boundary =
            |byte: usize| byte <= node.start || byte >= node.end || boundaries.binary_search(&byte).is_ok();

        // Per glyph: its source byte range if it maps exactly.
        struct Pending {
            x0: f64,
            x1: f64,
            baseline: f64,
            src: Range<usize>,
        }
        let mut current: Option<Pending> = None;
        let flush = |this: &mut Self, p: Pending, out: &mut Vec<Cluster>| {
            let src_text = &text[p.src.clone()];
            let starts: Vec<usize> = src_text.grapheme_indices(true).map(|(i, _)| i).collect();
            let n = starts.len().max(1);
            let mut stops = Vec::with_capacity(n + 1);
            for (k, &i) in starts.iter().enumerate() {
                let x = p.x0 + (p.x1 - p.x0) * k as f64 / n as f64;
                let Some(o) = this.utf16(file, &(p.src.start + i..p.src.start + i)) else { return };
                stops.push((x, o.start, i as u32));
            }
            let Some(end) = this.utf16(file, &(p.src.end..p.src.end)) else { return };
            stops.push((p.x1, end.start, src_text.len() as u32));
            let Some(range) = this.utf16(file, &p.src) else { return };
            let c = this.make(
                placed,
                p.x0,
                p.x1,
                p.baseline,
                ascent,
                descent,
                src_text.to_owned(),
                Some(ClusterSource { file, range }),
                ClusterKind::Editable(EditMode::Markup),
                stops,
                None,
            );
            out.push(c);
        };

        for g in glyphs {
            let glyph = &item.glyphs[g];
            let rendered_range = glyph.range();
            let b = &boxes[g];
            let start = node.start + usize::from(glyph.span.1);
            let end = start + rendered_range.len();
            let rendered = item.text.get(rendered_range.clone()).unwrap_or_default();
            let exact = !rendered_range.is_empty()
                && end <= node.end
                && text.is_char_boundary(start)
                && text.is_char_boundary(end)
                && &text[start..end] == rendered;
            if !exact {
                if let Some(p) = current.take() {
                    flush(self, p, out);
                }
                if rendered.is_empty() && b.x1 - b.x0 <= 0.0 {
                    continue;
                }
                // Hyphenation and other glyphs without a source counterpart.
                let reason = if matches!(rendered, "\u{ad}" | "\u{2010}" | "-") && start <= node.end {
                    NonEditableReason::Generated
                } else {
                    NonEditableReason::Transformed
                };
                let c = self.make(
                    placed,
                    b.x0,
                    b.x1,
                    b.baseline,
                    ascent,
                    descent,
                    if reason == NonEditableReason::Generated { "-".into() } else { rendered.to_owned() },
                    None,
                    ClusterKind::NonEditable(reason),
                    Vec::new(),
                    None,
                );
                out.push(c);
                continue;
            }
            match current.as_mut() {
                // Continue a grapheme (combining marks, multi-glyph clusters).
                Some(p) if p.src.end == start && !is_boundary(start) => {
                    p.x1 = b.x1;
                    p.src.end = end;
                }
                // Several glyphs for the same characters (decomposition).
                Some(p) if start >= p.src.start && end <= p.src.end => {
                    p.x1 = p.x1.max(b.x1);
                }
                _ => {
                    if let Some(p) = current.take() {
                        flush(self, p, out);
                    }
                    current = Some(Pending { x0: b.x0, x1: b.x1, baseline: b.baseline, src: start..end });
                }
            }
        }
        if let Some(p) = current.take() {
            flush(self, p, out);
        }
    }
}

fn is_code(kind: SyntaxKind) -> bool {
    use SyntaxKind as K;
    matches!(
        kind,
        K::Code
            | K::CodeBlock
            | K::Ident
            | K::Bool
            | K::Int
            | K::Float
            | K::Numeric
            | K::Str
            | K::FuncCall
            | K::Args
            | K::FieldAccess
            | K::Array
            | K::Dict
            | K::Named
            | K::Keyed
            | K::Unary
            | K::Binary
            | K::Closure
            | K::LetBinding
            | K::SetRule
            | K::ShowRule
            | K::Contextual
            | K::Conditional
            | K::WhileLoop
            | K::ForLoop
            | K::ModuleImport
            | K::ModuleInclude
            | K::Parenthesized
            | K::Hash
            | K::MathCall
    )
}
