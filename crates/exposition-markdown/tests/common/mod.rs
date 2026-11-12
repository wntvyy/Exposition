//! Test harness: compiles generated Typst with the real Typst 0.15.1 compiler
//! (embedded fonts only), extracts glyphs with their source spans, and maps
//! them back to Markdown through the translation's source map.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use exposition_core::text::{TextRange, byte_range_to_utf16, utf16_range_to_bytes};
use exposition_markdown::{SegmentKind, Translation};
use typst::diag::{FileError, FileResult, Warned};
use typst::foundations::{Bytes, Datetime};
use typst::layout::{Frame, FrameItem, Transform};
use typst::syntax::{FileId, RootedPath, Source, Span, SyntaxKind, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook, TextItem};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World, WorldExt};
use typst_kit::fonts::{FontStore, embedded};
use typst_layout::PagedDocument;

pub fn fixture_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures").join(name)
}

pub fn read_fixture(name: &str) -> String {
    std::fs::read_to_string(fixture_dir(name).join("main.md")).expect("fixture")
}

fn fonts() -> &'static FontStore {
    static FONTS: OnceLock<FontStore> = OnceLock::new();
    FONTS.get_or_init(|| {
        let mut store = FontStore::new();
        for entry in embedded() {
            store.push(entry);
        }
        store
    })
}

/// A long-lived compiler context (library, fonts, incrementally reparsed
/// sources), like the application's Typst adapter.
pub struct Compiler {
    library: LazyHash<Library>,
    root: PathBuf,
    sources: Mutex<HashMap<FileId, Source>>,
}

pub struct Compiled {
    pub document: Option<PagedDocument>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

pub const MAIN: &str = "main.typ";

pub fn main_id() -> FileId {
    RootedPath::new(VirtualRoot::Project, VirtualPath::new(MAIN).unwrap()).intern()
}

impl Compiler {
    pub fn new(root: PathBuf) -> Self {
        Self { library: LazyHash::new(Library::default()), root, sources: Mutex::new(HashMap::new()) }
    }

    pub fn compile(&self, typst: &str) -> Compiled {
        let world = TestWorld { compiler: self, main: main_id(), text: typst };
        let Warned { output, warnings } = typst::compile::<PagedDocument>(&world);
        let warnings = warnings.iter().map(|w| w.message.to_string()).collect();
        comemo::evict(10);
        match output {
            Ok(document) => Compiled { document: Some(document), errors: Vec::new(), warnings },
            Err(errors) => Compiled {
                document: None,
                errors: errors
                    .iter()
                    .map(|e| {
                        let at = world.range(e.span).map(|r| {
                            let start = r.start.saturating_sub(40);
                            let end = (r.end + 40).min(typst.len());
                            let mut a = start;
                            while !typst.is_char_boundary(a) {
                                a -= 1;
                            }
                            let mut b = end;
                            while !typst.is_char_boundary(b) {
                                b += 1;
                            }
                            typst[a..b].to_owned()
                        });
                        format!("{} (at: {:?})", e.message, at)
                    })
                    .collect(),
                warnings,
            },
        }
    }

    /// The parsed generated source (after a compile).
    pub fn source(&self) -> Source {
        self.sources.lock().unwrap().get(&main_id()).cloned().expect("compiled source")
    }
}

struct TestWorld<'a> {
    compiler: &'a Compiler,
    main: FileId,
    text: &'a str,
}

impl World for TestWorld<'_> {
    fn library(&self) -> &LazyHash<Library> {
        &self.compiler.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        fonts().book()
    }

    fn main(&self) -> FileId {
        self.main
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if id != self.main {
            return Err(FileError::NotFound(PathBuf::from(id.vpath().get_with_slash())));
        }
        let mut sources = self.compiler.sources.lock().unwrap();
        match sources.get_mut(&id) {
            Some(source) => {
                if source.text() != self.text {
                    source.replace(self.text);
                }
                Ok(source.clone())
            }
            None => {
                let source = Source::new(id, self.text.to_owned());
                sources.insert(id, source.clone());
                Ok(source)
            }
        }
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        let path = id.vpath().get_without_slash();
        let full = self.compiler.root.join(path);
        let canonical = full.canonicalize().map_err(|e| FileError::from_io(e, &full))?;
        let root = self.compiler.root.canonicalize().map_err(|e| FileError::from_io(e, &full))?;
        if !canonical.starts_with(&root) {
            return Err(FileError::AccessDenied);
        }
        std::fs::read(&canonical).map(Bytes::new).map_err(|e| FileError::from_io(e, &full))
    }

    fn font(&self, index: usize) -> Option<Font> {
        fonts().font(index)
    }

    fn today(&self, _offset: Option<typst::foundations::Duration>) -> Option<Datetime> {
        Datetime::from_ymd(2026, 11, 11)
    }
}

// ------------------------------------------------------------- glyphs --

/// Where a glyph's span points in the generated Typst source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GlyphSource {
    /// Markup text: exact generated byte range of the glyph's characters.
    Text(std::ops::Range<usize>),
    /// A whole syntax node rendered as a unit (escape, shorthand, smart quote,
    /// space, math identifier, ...): its byte range and kind.
    Node(std::ops::Range<usize>, SyntaxKind),
    /// A math text node: byte range of the node.
    MathText(std::ops::Range<usize>),
    /// Code, strings, raw: not page text.
    Code(SyntaxKind),
    /// No span (template-generated) or a span outside the main file.
    Generated,
}

#[derive(Clone, Debug)]
pub struct PlacedGlyph {
    pub page: usize,
    pub x: f64,
    pub y: f64,
    /// Rendered text of the glyph.
    pub text: String,
    pub source: GlyphSource,
    /// Whether the glyph is inside an equation.
    pub in_math: bool,
}

fn collect<'a>(frame: &'a Frame, ts: Transform, out: &mut Vec<(Transform, &'a TextItem)>) {
    for (pos, item) in frame.items() {
        let local = ts.pre_concat(Transform::translate(pos.x, pos.y));
        match item {
            FrameItem::Group(group) => collect(&group.frame, local.pre_concat(group.transform), out),
            FrameItem::Text(text) => out.push((local, text)),
            _ => {}
        }
    }
}

fn in_kind(node: &typst::syntax::LinkedNode, kind: SyntaxKind) -> bool {
    let mut p = node.parent().cloned();
    while let Some(n) = p {
        if n.kind() == kind {
            return true;
        }
        p = n.parent().cloned();
    }
    false
}

/// Every glyph of the document with its generated-source origin.
pub fn glyphs(document: &PagedDocument, source: &Source) -> Vec<PlacedGlyph> {
    let mut out = Vec::new();
    for (page_index, page) in document.pages().iter().enumerate() {
        let mut items = Vec::new();
        collect(&page.frame, Transform::identity(), &mut items);
        for (ts, item) in items {
            let mut x = 0.0;
            for glyph in &item.glyphs {
                let text = item.text.get(glyph.range()).unwrap_or_default().to_owned();
                let advance = glyph.x_advance.at(item.size).to_pt();
                let gx = ts.tx.to_pt() + x;
                let gy = ts.ty.to_pt();
                x += advance;
                let (span, offset): (Span, u16) = glyph.span;
                let (src, in_math) = resolve(source, span, offset, text.len());
                out.push(PlacedGlyph { page: page_index, x: gx, y: gy, text, source: src, in_math });
            }
        }
    }
    out
}

fn resolve(source: &Source, span: Span, offset: u16, len: usize) -> (GlyphSource, bool) {
    if span.id() != Some(source.id()) {
        return (GlyphSource::Generated, false);
    }
    let Some(node) = source.find(span) else { return (GlyphSource::Generated, false) };
    let in_math = in_kind(&node, SyntaxKind::Equation);
    let range = node.range();
    let kind = node.kind();
    let src = match kind {
        SyntaxKind::Text if in_kind(&node, SyntaxKind::Raw) => GlyphSource::Code(SyntaxKind::Raw),
        SyntaxKind::Text => {
            let start = range.start + offset as usize;
            GlyphSource::Text(start..start + len)
        }
        SyntaxKind::MathText => GlyphSource::MathText(range),
        SyntaxKind::Escape
        | SyntaxKind::Shorthand
        | SyntaxKind::SmartQuote
        | SyntaxKind::Space
        | SyntaxKind::MathIdent
        | SyntaxKind::MathShorthand
        | SyntaxKind::MathPrimes
        | SyntaxKind::Linebreak => GlyphSource::Node(range, kind),
        other => GlyphSource::Code(other),
    };
    (src, in_math)
}

/// Generated byte range → generated UTF-16 range.
pub fn gen16(typst: &str, range: std::ops::Range<usize>) -> TextRange {
    byte_range_to_utf16(typst, range).expect("generated byte range on char boundaries")
}

pub fn md_slice(md: &str, range: TextRange) -> &str {
    &md[utf16_range_to_bytes(md, range).expect("markdown range")]
}

/// Mapping statistics for the glyph coverage proof.
#[derive(Debug, Default)]
pub struct Coverage {
    pub prose: usize,
    pub prose_mapped: usize,
    pub math: usize,
    pub math_mapped: usize,
    pub generated: usize,
    pub code: usize,
    pub failures: Vec<String>,
}

/// Map every prose/math glyph to Markdown and check that the Markdown
/// characters correspond to the glyph text and that the offsets round-trip.
pub fn coverage(md: &str, tr: &Translation, glyphs: &[PlacedGlyph]) -> Coverage {
    let mut cov = Coverage::default();
    // MathText nodes: glyphs map to their graphemes in order.
    let mut math_seen: HashMap<std::ops::Range<usize>, usize> = HashMap::new();
    for g in glyphs {
        let generated = match &g.source {
            GlyphSource::Generated => {
                cov.generated += 1;
                continue;
            }
            GlyphSource::Code(_) => {
                cov.code += 1;
                continue;
            }
            GlyphSource::Text(r) => r.clone(),
            GlyphSource::Node(r, SyntaxKind::Space) if g.text.trim().is_empty() => r.clone(),
            GlyphSource::Node(r, _) => r.clone(),
            GlyphSource::MathText(r) => {
                let text = &tr.typst[r.clone()];
                let graphemes: Vec<std::ops::Range<usize>> = {
                    use unicode_segmentation::UnicodeSegmentation;
                    text.grapheme_indices(true).map(|(i, s)| r.start + i..r.start + i + s.len()).collect()
                };
                let k = math_seen.entry(r.clone()).or_default();
                let target = graphemes.get(*k).cloned().unwrap_or(r.clone());
                *k += 1;
                target
            }
        };
        let blank = g.text.trim().is_empty();
        if blank {
            // Inter-word spaces are not counted as glyphs.
            continue;
        }
        let is_math = g.in_math;
        if is_math {
            cov.math += 1;
        } else {
            cov.prose += 1;
        }
        let r16 = gen16(&tr.typst, generated.clone());
        let Some(mapped) = tr.map.generated_range_to_markdown(r16) else {
            cov.failures.push(format!("unmapped glyph {:?} at generated {:?}", g.text, generated));
            continue;
        };
        let editable_kind = if is_math {
            matches!(mapped.kind, SegmentKind::MathText | SegmentKind::MathAtom)
        } else {
            matches!(mapped.kind, SegmentKind::Text | SegmentKind::Atom)
        };
        let md_text = md_slice(md, mapped.markdown);
        let ok = editable_kind && corresponds(md_text, &g.text, is_math);
        // Round trip: the Markdown start maps back to the generated start.
        let back = tr.map.markdown_to_generated(mapped.markdown.start);
        let round_trip = back.contains(&r16.start);
        if ok && round_trip {
            if is_math {
                cov.math_mapped += 1;
            } else {
                cov.prose_mapped += 1;
            }
        } else {
            cov.failures.push(format!(
                "glyph {:?} ({}) → markdown {:?} {:?} kind {:?} round_trip={round_trip}",
                g.text,
                if is_math { "math" } else { "prose" },
                md_text,
                mapped.markdown,
                mapped.kind
            ));
        }
    }
    cov
}

/// Whether Markdown text `md` legitimately renders as glyph text `glyph`.
pub fn corresponds(md: &str, glyph: &str, math: bool) -> bool {
    if md == glyph {
        return true;
    }
    // Markdown backslash escapes and entities.
    if md.strip_prefix('\\') == Some(glyph) {
        return true;
    }
    let entities = [("&amp;", "&"), ("&lt;", "<"), ("&gt;", ">"), ("&quot;", "\""), ("&#35;", "#")];
    if entities.iter().any(|(e, g)| *e == md && *g == glyph) {
        return true;
    }
    // Typographic punctuation (Typst shorthands and smart quotes).
    let typographic =
        [("--", "–"), ("---", "—"), ("...", "…"), ("'", "’"), ("'", "‘"), ("\"", "“"), ("\"", "”"), ("~", "\u{a0}")];
    if typographic.iter().any(|(m, g)| *m == md && *g == glyph) {
        return true;
    }
    // Soft line break rendered as a space.
    if md.trim().is_empty() && glyph.trim().is_empty() {
        return true;
    }
    if math {
        if let Some(name) = md.strip_prefix('\\') {
            if let Some(sym) = exposition_markdown::math::symbol_text(name) {
                let mut chars = sym.chars();
                let italic = match (chars.next(), chars.next()) {
                    (Some(c), None) => math_italic(c).map(String::from),
                    _ => None,
                };
                if sym == glyph || italic.as_deref() == Some(glyph) {
                    return true;
                }
            }
            // Operator names (`\sin` → "sin" glyphs).
            if name.contains(glyph) && glyph.chars().all(|c| c.is_ascii_alphabetic()) {
                return true;
            }
            // Escaped delimiters `\{` → "{", spacing commands render nothing.
            if name == glyph {
                return true;
            }
        }
        let math_forms = [("-", "−"), ("*", "∗"), ("'", "′"), ("''", "″"), ("'", "'")];
        if math_forms.iter().any(|(m, g)| *m == md && *g == glyph) {
            return true;
        }
        // Math alphanumerics: Typst renders letters in math italic code points.
        if md.chars().count() == 1 && glyph.chars().count() == 1 {
            let (m, g) = (md.chars().next().unwrap(), glyph.chars().next().unwrap());
            if math_italic(m) == Some(g) {
                return true;
            }
        }
    }
    false
}

/// Mathematical italic code point of an ASCII or Greek letter.
fn math_italic(c: char) -> Option<char> {
    let code = match c {
        'h' => return Some('ℎ'),
        'A'..='Z' => 0x1D434 + (c as u32 - 'A' as u32),
        'a'..='z' => 0x1D44E + (c as u32 - 'a' as u32),
        'Α'..='Ω' => 0x1D6E2 + (c as u32 - 'Α' as u32),
        'α'..='ω' => 0x1D6FC + (c as u32 - 'α' as u32),
        'ϵ' => 0x1D716,
        'ϑ' => 0x1D717,
        'ϰ' => 0x1D718,
        'ϕ' => 0x1D719,
        'ϱ' => 0x1D71A,
        'ϖ' => 0x1D71B,
        _ => return None,
    };
    char::from_u32(code)
}

/// The concatenated rendered text of a document's glyphs (per page).
pub fn page_texts(glyphs: &[PlacedGlyph]) -> Vec<String> {
    let mut pages: Vec<String> = Vec::new();
    for g in glyphs {
        while pages.len() <= g.page {
            pages.push(String::new());
        }
        pages[g.page].push_str(&g.text);
    }
    pages
}

/// Apply UTF-16 patches (sorted, non-overlapping) to `text`.
pub fn apply_patches(text: &str, patches: &[exposition_core::document::TextPatch]) -> String {
    let mut out = text.to_owned();
    for patch in patches.iter().rev() {
        let r = utf16_range_to_bytes(&out, patch.range).expect("patch range");
        out.replace_range(r, &patch.text);
    }
    out
}
