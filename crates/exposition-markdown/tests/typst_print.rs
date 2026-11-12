//! The Markdown print profile against the real Typst 0.15.1 compiler.
//!
//! Fixture: tests/fixtures/markdown-basic/main.md (front matter, headings,
//! paragraphs with emphasis/links/inline code, lists with a task item, a
//! table, a footnote, inline and display mathematics, a figure, a code block,
//! a quote, a rule, a hard line break).

mod common;

use std::time::Instant;

use common::*;
use exposition_core::document::{
    ConflictPolicy, DocumentEdit, EditOrigin, TextPatch, TransactionId, TransactionRequest, UndoBehavior,
};
use exposition_core::path::WorkspacePath;
use exposition_core::text::{TextRange, utf16_len, utf16_range_to_bytes};
use exposition_markdown::{EditMode, MarkdownInput, PageEditOp, SegmentKind, TranslateOptions, Translation, translate};
use exposition_workspace::{DocumentService, OpenDocument};

const FIXTURE: &str = "markdown-basic";

fn options() -> TranslateOptions {
    TranslateOptions { document: Some(WorkspacePath::parse("main.md").unwrap()) }
}

struct Built {
    tr: Translation,
    doc: typst_layout::PagedDocument,
    glyphs: Vec<PlacedGlyph>,
}

fn build(compiler: &Compiler, md: &str) -> Built {
    let tr = translate(md, &options());
    let out = compiler.compile(&tr.typst);
    assert!(out.errors.is_empty(), "Typst errors: {:#?}\n--- generated ---\n{}", out.errors, tr.typst);
    let doc = out.document.expect("document");
    let glyphs = glyphs(&doc, &compiler.source());
    Built { tr, doc, glyphs }
}

/// The page glyph whose Markdown source starts at UTF-16 offset `at`.
fn glyph_at_markdown<'a>(b: &'a Built, md: &str, at: u32, math: bool) -> &'a PlacedGlyph {
    b.glyphs
        .iter()
        .find(|g| {
            g.in_math == math
                && generated_range(g, &b.tr)
                    .and_then(|r| b.tr.map.generated_range_to_markdown(r))
                    .is_some_and(|m| m.markdown.start == at && m.mode.is_some())
        })
        .unwrap_or_else(|| panic!("no glyph for markdown offset {at} ({:?})", &md[at as usize..]))
}

/// Generated UTF-16 range of a glyph (MathText glyphs: the node; prose: exact).
fn generated_range(g: &PlacedGlyph, tr: &Translation) -> Option<TextRange> {
    match &g.source {
        GlyphSource::Text(r) | GlyphSource::Node(r, _) | GlyphSource::MathText(r) => Some(gen16(&tr.typst, r.clone())),
        _ => None,
    }
}

fn apply_plan(md: &str, patches: &[TextPatch]) -> String {
    apply_patches(md, patches)
}

/// Assert that `new` equals `old` except for replacing UTF-16 `range` of
/// `old` with `with`.
fn assert_only_changed(old: &str, new: &str, range: TextRange, with: &str) {
    let r = utf16_range_to_bytes(old, range).unwrap();
    let mut expected = old.to_owned();
    expected.replace_range(r.clone(), with);
    assert_eq!(new, expected, "only the intended bytes change");
    assert_eq!(&new.as_bytes()[..r.start], &old.as_bytes()[..r.start]);
    assert_eq!(&new.as_bytes()[r.start + with.len()..], &old.as_bytes()[r.end..]);
}

#[test]
fn basic_fixture_compiles_and_every_prose_and_math_glyph_maps() {
    let md = read_fixture(FIXTURE);
    let compiler = Compiler::new(fixture_dir(FIXTURE));
    let tr = translate(&md, &options());
    assert_eq!(tr.profile, "exposition-markdown-print/1");
    assert!(tr.diagnostics.is_empty(), "translation diagnostics: {:#?}", tr.diagnostics);
    let out = compiler.compile(&tr.typst);
    assert!(out.errors.is_empty(), "{:#?}", out.errors);
    assert!(out.warnings.is_empty(), "{:#?}", out.warnings);
    let doc = out.document.unwrap();
    let glyphs = glyphs(&doc, &compiler.source());
    let cov = coverage(&md, &tr, &glyphs);
    println!(
        "markdown-basic: {} page(s); {}/{} prose glyphs mapped, {}/{} math glyphs mapped; {} template/generated, {} code/raw",
        doc.pages().len(),
        cov.prose_mapped,
        cov.prose,
        cov.math_mapped,
        cov.math,
        cov.generated,
        cov.code
    );
    assert!(cov.failures.is_empty(), "{:#?}", cov.failures);
    assert!(cov.prose > 400 && cov.math > 25, "fixture should exercise prose and math: {cov:?}");
    assert_eq!(cov.prose_mapped, cov.prose);
    assert_eq!(cov.math_mapped, cov.math);

    // Every rendered construct of the fixture is present.
    let text = page_texts(&glyphs).concat();
    for needle in [
        "A Markdown Print Profile",
        "Ada Lovelace and Alan Turing",
        "Introduction",
        "strong text",
        "link to Typst",
        "*stars*",
        "50% & #hash, snake_case, 1 < 2 and a [bracket]",
        "– with dashes… and “quotes”",
        "circle constant",
        "A small test figure",
        "println!(\"hello\");",
        "The footnote text with",
        "Final paragraph with a hard",
    ] {
        assert!(text.contains(needle), "rendered text lacks {needle:?}:\n{text}");
    }
    assert!(tr.equations.len() >= 6, "{:?}", tr.equations);
    assert_eq!(tr.images.len(), 1);
    assert_eq!(tr.images[0].path.as_ref().map(|p| p.as_str()), Some("figure.png"));
}

#[test]
fn source_map_invariants_hold() {
    let md = read_fixture(FIXTURE);
    let tr = translate(&md, &options());
    let segs = tr.map.segments();
    // Contiguous, complete cover of the generated text.
    assert_eq!(segs.first().unwrap().generated.start, 0);
    for w in segs.windows(2) {
        assert_eq!(w[0].generated.end, w[1].generated.start);
    }
    assert_eq!(segs.last().unwrap().generated.end as usize, utf16_len(&tr.typst));
    // Rendered segments: disjoint Markdown ranges; linear ones copy exactly.
    let mut rendered: Vec<_> = segs.iter().filter(|s| !s.kind.is_syntax()).collect();
    rendered.sort_by_key(|s| s.markdown.start);
    for w in rendered.windows(2) {
        assert!(w[0].markdown.end <= w[1].markdown.start, "overlap {:?} {:?}", w[0], w[1]);
    }
    for s in segs.iter().filter(|s| s.is_linear() && s.kind != SegmentKind::Raw) {
        assert_eq!(md_slice(&tr.typst, s.generated), md_slice(&md, s.markdown), "{s:?}");
    }
    // Every generated offset maps to Markdown, and every Markdown offset of
    // rendered text maps back to it.
    let mut checked = 0;
    for s in segs.iter().filter(|s| s.is_linear() && s.is_editable()) {
        for k in 0..=s.generated.len() {
            let g = s.generated.start + k;
            let m = tr.map.generated_to_markdown(g).unwrap();
            assert!(m.exact);
            assert!(tr.map.markdown_to_generated(m.markdown).contains(&g), "{s:?} at {g}");
            checked += 1;
        }
    }
    for s in segs.iter().filter(|s| s.is_editable() && !s.is_linear()) {
        assert_eq!(tr.map.generated_to_markdown(s.generated.start).unwrap().markdown, s.markdown.start);
        assert_eq!(tr.map.markdown_range_to_generated(s.markdown), vec![s.generated]);
        checked += 1;
    }
    println!("source map: {} segments, {checked} editable offsets round-tripped", segs.len());
}

#[test]
fn page_edit_inserts_prose_reflows_and_preserves_other_bytes() {
    let md = read_fixture(FIXTURE);
    let compiler = Compiler::new(fixture_dir(FIXTURE));
    let before = build(&compiler, &md);
    let pages_before = page_texts(&before.glyphs);

    // The user clicks before "cites" on the page: the glyph's generated
    // range maps to the Markdown caret.
    let at = md.find("cites a footnote").unwrap() as u32;
    let glyph = glyph_at_markdown(&before, &md, at, false);
    assert_eq!(glyph.text, "c");
    let caret = before.tr.map.generated_to_markdown(generated_range(glyph, &before.tr).unwrap().start).unwrap();
    assert_eq!((caret.markdown, caret.mode, caret.exact), (at, Some(EditMode::Markup), true));
    assert_eq!(MarkdownInput.mode_at(&md, caret.markdown), Some(EditMode::Markup));

    // Type a long phrase (it contains Markdown/Typst specials) so the
    // paragraph reflows.
    let typed = "repeatedly & carefully (with *care*, #1 and $5), as every long sentence that wraps does, ";
    let plan = MarkdownInput
        .plan(&md, TextRange::empty(caret.markdown), &PageEditOp::Insert { text: typed.into() }, "\n")
        .unwrap();
    assert_eq!(plan.mode, EditMode::Markup);
    assert_eq!(plan.patches.len(), 1);
    let edited = apply_plan(&md, &plan.patches);
    let written = &plan.patches[0].text;
    assert_eq!(
        written,
        "repeatedly & carefully (with \\*care\\*, #1 and \\$5), as every long sentence that wraps does, "
    );
    assert_only_changed(&md, &edited, TextRange::empty(at), written);
    assert_eq!(plan.caret, at + utf16_len(written) as u32);

    let after = build(&compiler, &edited);
    let pages_after = page_texts(&after.glyphs);
    let joined = pages_after.concat();
    assert!(
        joined.contains("cites a footnote")
            && joined.contains("repeatedly & carefully (with *care*, #1 and $5), as every long"),
        "{joined}"
    );
    // The inserted text maps back to exactly the inserted Markdown bytes.
    let inserted = TextRange::new(at, at + utf16_len(written) as u32);
    let mapped: String = after
        .glyphs
        .iter()
        .filter(|g| {
            generated_range(g, &after.tr)
                .and_then(|r| after.tr.map.generated_range_to_markdown(r))
                .is_some_and(|m| m.markdown.start >= inserted.start && m.markdown.end <= inserted.end)
        })
        .map(|g| g.text.as_str())
        .collect();
    assert_eq!(mapped.replace(' ', ""), typed.replace(' ', ""));
    // Reflow: the paragraph now takes more lines, later content moved down,
    // and every glyph still maps.
    // Lines of the edited paragraph: distinct baselines of its prose glyphs.
    let para_start = md.find("The second paragraph").unwrap() as u32;
    let para_end = md.find("- First item").unwrap() as u32;
    let lines = |b: &Built, shift: u32| {
        let mut ys: Vec<i64> = b
            .glyphs
            .iter()
            .filter(|g| !g.in_math && g.text.chars().all(char::is_alphabetic))
            .filter(|g| {
                generated_range(g, &b.tr)
                    .and_then(|r| b.tr.map.generated_range_to_markdown(r))
                    .is_some_and(|m| m.markdown.start >= para_start && m.markdown.end <= para_end + shift)
            })
            .map(|g| (g.y * 10.0).round() as i64)
            .collect();
        ys.sort_unstable();
        ys.dedup();
        ys.len()
    };
    let shift = utf16_len(written) as u32;
    let (lines_before, lines_after) = (lines(&before, 0), lines(&after, shift));
    assert!(lines_after > lines_before, "the paragraph reflows: {lines_before} -> {lines_after} lines");
    // Content after the paragraph moved down by the added line(s).
    let first_item_y = |b: &Built, md: &str| glyph_at_markdown(b, md, md.find("First item").unwrap() as u32, false).y;
    assert!(first_item_y(&after, &edited) > first_item_y(&before, &md) + 5.0, "later content moves down");
    let footnote_y = |b: &Built| {
        b.glyphs.iter().filter(|g| g.page == 0 && g.text == "1" && !g.in_math).map(|g| g.y).fold(f64::MIN, f64::max)
    };
    assert!(footnote_y(&after) >= footnote_y(&before));
    let cov = coverage(&edited, &after.tr, &after.glyphs);
    assert!(cov.failures.is_empty(), "{:#?}", cov.failures);
    println!(
        "after prose edit: paragraph {lines_before} -> {lines_after} lines; {}/{} prose, {}/{} math glyphs mapped; pages {} -> {}",
        cov.prose_mapped,
        cov.prose,
        cov.math_mapped,
        cov.math,
        pages_before.len(),
        pages_after.len()
    );
}

#[test]
fn page_edit_changes_an_exponent_in_place() {
    let md = read_fixture(FIXTURE);
    let compiler = Compiler::new(fixture_dir(FIXTURE));
    let before = build(&compiler, &md);
    // `\alpha^2` in the second paragraph: the user selects the exponent glyph.
    let eq_start = md.find("$\\alpha^2").unwrap();
    let two = (eq_start + "$\\alpha^".len()) as u32;
    let glyph = glyph_at_markdown(&before, &md, two, true);
    assert_eq!(glyph.text, "2");
    let alpha = glyph_at_markdown(&before, &md, eq_start as u32 + 1, true);
    assert_eq!(alpha.text, "𝛼");
    assert!(glyph.y < alpha.y, "the exponent is raised");
    let selected = before.tr.map.generated_range_to_markdown(generated_range(glyph, &before.tr).unwrap()).unwrap();
    assert_eq!((selected.markdown, selected.mode), (TextRange::new(two, two + 1), Some(EditMode::Math)));
    assert_eq!(MarkdownInput.mode_at(&md, two), Some(EditMode::Math));

    let plan = MarkdownInput.plan(&md, selected.markdown, &PageEditOp::Insert { text: "3".into() }, "\n").unwrap();
    assert_eq!(plan.mode, EditMode::Math);
    let edited = apply_plan(&md, &plan.patches);
    assert_only_changed(&md, &edited, TextRange::new(two, two + 1), "3");
    assert_eq!(plan.caret, two + 1);

    let after = build(&compiler, &edited);
    let new = glyph_at_markdown(&after, &edited, two, true);
    assert_eq!(new.text, "3");
    let new_alpha = glyph_at_markdown(&after, &edited, eq_start as u32 + 1, true);
    assert!(new.y < new_alpha.y, "still a superscript");
    assert!((new.x - glyph.x).abs() < 0.5 && (new.y - glyph.y).abs() < 0.01, "same place: {new:?} vs {glyph:?}");
    // The generated Typst differs only inside that equation.
    let (a, b) = (&before.tr.typst, &after.tr.typst);
    let prefix = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
    let suffix = a.bytes().rev().zip(b.bytes().rev()).take_while(|(x, y)| x == y).count();
    assert_eq!(&a[prefix..a.len() - suffix], "2");
    assert_eq!(&b[prefix..b.len() - suffix], "3");

    // A second math edit: delete the exponent (with its `^`) by Backspace.
    let plan = MarkdownInput.plan(&edited, TextRange::empty(two + 1), &PageEditOp::DeleteBackward, "\n").unwrap();
    let removed = apply_plan(&edited, &plan.patches);
    assert_only_changed(&edited, &removed, TextRange::new(two - 1, two + 1), "");
    let gone = build(&compiler, &removed);
    assert!(page_texts(&gone.glyphs).concat().contains("𝛼+𝛽1≤√𝑥"), "{}", page_texts(&gone.glyphs).concat());
}

#[test]
fn shared_undo_restores_markdown_and_layout() {
    let md = read_fixture(FIXTURE);
    let compiler = Compiler::new(fixture_dir(FIXTURE));
    let documents = DocumentService::new();
    let info = documents.open(OpenDocument {
        path: Some(WorkspacePath::parse("main.md").unwrap()),
        text: md.clone(),
        language: "markdown".into(),
        encoding: exposition_core::document::TextEncoding::Utf8,
        read_only: None,
    });
    let original = build(&compiler, &md);
    let at = md.find("Final paragraph").unwrap() as u32 + "Final".len() as u32;
    // Two typed characters merge into one undo group, as on the page.
    let mut revision = info.revision;
    for ch in ["!", "?"] {
        let text = documents.snapshot(&info.id).unwrap().text.to_string();
        let caret = at + (revision - info.revision) as u32;
        let plan =
            MarkdownInput.plan(&text, TextRange::empty(caret), &PageEditOp::Insert { text: ch.into() }, "\n").unwrap();
        let outcome = documents
            .apply(TransactionRequest {
                id: TransactionId(format!("t{revision}")),
                origin: EditOrigin::Page,
                edits: vec![DocumentEdit { document: info.id.clone(), base_revision: revision, patches: plan.patches }],
                undo: UndoBehavior::Merge { key: "page-type-1".into() },
                conflict: ConflictPolicy::RebaseDisjoint,
                label: Some("Typing".into()),
                selection: None,
            })
            .unwrap();
        revision = outcome.applied[0].revision;
    }
    let typed = documents.snapshot(&info.id).unwrap().text.to_string();
    assert!(typed.contains("Final!? paragraph"));
    let edited = build(&compiler, &typed);
    assert!(page_texts(&edited.glyphs).concat().contains("Final!? paragraph"));
    // One undo (from the source editor: undo is shared) restores the exact
    // Markdown and the identical layout.
    documents.undo(&info.id, EditOrigin::Source).unwrap();
    let restored = documents.snapshot(&info.id).unwrap().text.to_string();
    assert_eq!(restored, md);
    let rebuilt = build(&compiler, &restored);
    assert_eq!(rebuilt.tr.typst, original.tr.typst);
    assert_eq!(page_texts(&rebuilt.glyphs), page_texts(&original.glyphs));
    documents.redo(&info.id, EditOrigin::Page).unwrap();
    assert_eq!(documents.snapshot(&info.id).unwrap().text.to_string(), typed);
}

#[test]
fn crlf_documents_map_and_edit_with_their_line_endings() {
    let md = read_fixture(FIXTURE).replace('\n', "\r\n");
    let compiler = Compiler::new(fixture_dir(FIXTURE));
    let built = build(&compiler, &md);
    assert!(built.tr.diagnostics.is_empty(), "{:#?}", built.tr.diagnostics);
    let cov = coverage(&md, &built.tr, &built.glyphs);
    assert!(cov.failures.is_empty(), "{:#?}", cov.failures);
    assert_eq!((cov.prose_mapped, cov.math_mapped), (535, 30));
    // The code block keeps its lines without carriage returns.
    assert!(page_texts(&built.glyphs).concat().contains("fn main() {    println!(\"hello\");}"));

    // Enter splits the paragraph with the document's line ending; Backspace
    // at the new paragraph's start joins it back to the original bytes.
    let at = md.find("uses the inline formula").unwrap() as u32;
    let plan = MarkdownInput.plan(&md, TextRange::empty(at), &PageEditOp::ParagraphBreak, "\r\n").unwrap();
    assert_eq!(plan.patches[0].text, "\r\n\r\n");
    let split = apply_plan(&md, &plan.patches);
    let rebuilt = build(&compiler, &split);
    assert!(rebuilt.tr.typst.contains("<exmd-fn-1> and\n\nuses the inline formula"), "{}", rebuilt.tr.typst);
    let joined = MarkdownInput.plan(&split, TextRange::empty(plan.caret), &PageEditOp::DeleteBackward, "\r\n").unwrap();
    let back = apply_plan(&split, &joined.patches);
    assert_eq!(back, md);
    assert_eq!(joined.caret, at);
}

#[test]
fn unsupported_math_is_reported_and_visibly_marked() {
    let md = "# Limits of the profile\n\nSupported $x^2$ and unsupported $\\color{red}{x} + \\unknowncmd + y$.\n\n\
              $$\n\\begin{array}{cc} a & b \\end{array} = \\frac{1}{} + \\left( x\n$$\n";
    let compiler = Compiler::new(fixture_dir(FIXTURE));
    let tr = translate(md, &options());
    let messages: Vec<(String, &str)> =
        tr.diagnostics.iter().map(|d| (d.message.clone(), md_slice(md, d.range))).collect();
    println!("{messages:#?}");
    let find =
        |src: &str| messages.iter().find(|(_, s)| *s == src).unwrap_or_else(|| panic!("no diagnostic for {src}"));
    assert!(find("\\color{red}{x}").0.contains("unsupported command `\\color`"));
    assert!(find("\\unknowncmd").0.contains("unsupported command"));
    assert!(find("\\begin{array}{cc} a & b \\end{array}").0.contains("unsupported environment `array`"));
    assert!(find("\\left( x").0.contains("`\\left` without `\\right`"));
    for d in &tr.diagnostics {
        assert_eq!(d.severity, exposition_core::build::Severity::Warning);
        let diag = d.to_diagnostic(&WorkspacePath::parse("main.md").unwrap());
        assert_eq!(diag.location.unwrap().range, d.range);
    }
    let out = compiler.compile(&tr.typst);
    assert!(out.errors.is_empty(), "{:#?}", out.errors);
    let doc = out.document.unwrap();
    let glyphs = glyphs(&doc, &compiler.source());
    let text = page_texts(&glyphs).concat();
    for marker in [
        "unsupported: \\color{red}{x}",
        "unsupported: \\unknowncmd",
        "unsupported: \\begin{array}{cc} a & b \\end{array}",
        "unsupported: \\left( x",
    ] {
        assert!(text.contains(marker), "missing marker {marker:?} in {text}");
    }
    // Supported mathematics around the markers still renders and maps.
    assert!(text.contains("𝑥2"));
    let cov = coverage(md, &tr, &glyphs);
    assert!(cov.failures.is_empty(), "{:#?}", cov.failures);
    // Marker glyphs are not editable and navigate to the unsupported source.
    let unsupported: Vec<_> = tr.map.segments().iter().filter(|s| s.kind == SegmentKind::Unsupported).collect();
    assert_eq!(unsupported.len(), 4);
    for s in unsupported {
        let m = tr.map.generated_to_markdown(s.generated.start + 1).unwrap();
        assert_eq!((m.mode, m.markdown), (None, s.markdown.start));
    }
}

#[test]
fn pdf_export_has_the_same_pages() {
    let md = read_fixture(FIXTURE);
    let compiler = Compiler::new(fixture_dir(FIXTURE));
    let built = build(&compiler, &md);
    let pdf = typst_pdf::pdf(&built.doc, &typst_pdf::PdfOptions::default()).expect("pdf export");
    let dir = std::env::temp_dir().join("exposition-markdown-test");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("markdown-basic.pdf"), &pdf).unwrap();
    let parsed = exposition_tex::pdf::PdfDocument::open(pdf).expect("valid PDF");
    assert_eq!(parsed.page_count(), built.doc.pages().len());
    for (i, page) in built.doc.pages().iter().enumerate() {
        let (w, h) = parsed.page_size(i).unwrap();
        let (tw, th) = (page.frame.width().to_pt(), page.frame.height().to_pt());
        assert!((w - tw).abs() < 0.01 && (h - th).abs() < 0.01, "page {i}: pdf {w}x{h} vs layout {tw}x{th}");
        // A4 from the front matter.
        assert!((tw - 595.28).abs() < 0.1 && (th - 841.89).abs() < 0.1);
        // The PDF carries the same text as the laid-out page.
        let pdf_text: String = parsed.extract_glyphs(i).unwrap().into_iter().filter_map(|g| g.text).collect();
        let layout_text: String = built
            .glyphs
            .iter()
            .filter(|g| g.page == i)
            .map(|g| g.text.as_str())
            .filter(|t| !t.trim().is_empty())
            .collect();
        let strip = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
        assert_eq!(strip(&pdf_text).chars().count(), strip(&layout_text).chars().count(), "page {i} glyph text");
    }
    println!("PDF: {} page(s), A4, text matches layout", built.doc.pages().len());
}

#[test]
fn declared_math_subset_compiles_command_by_command() {
    let compiler = Compiler::new(fixture_dir(FIXTURE));
    let mut lines = String::new();
    let samples = |name: &str| -> String {
        match name {
            "frac" | "dfrac" | "tfrac" | "cfrac" | "binom" | "dbinom" | "tbinom" | "overset" | "underset"
            | "stackrel" => {
                format!("\\{name}{{a}}{{b}}")
            }
            "left" => "\\left( x \\right)".into(),
            "right" | "end" | "nolimits" | "middle" => String::new(),
            "begin" => "\\begin{pmatrix} 1 & 2 \\\\ 3 & 4 \\end{pmatrix}".into(),
            "limits" => "\\sum\\limits_{i}".into(),
            "not" => "a \\not= b".into(),
            "operatorname" => "\\operatorname{rank} A".into(),
            "big" | "Big" | "bigg" | "Bigg" => format!("\\{name}( x \\{name})"),
            "displaystyle" | "textstyle" => format!("\\{name} x"),
            "pmod" => "a \\pmod{n}".into(),
            "nonumber" => "x \\nonumber".into(),
            n if n.starts_with("text") || n == "mbox" => format!("\\{n}{{if }} x"),
            n => format!("x \\{n}{{y}}"),
        }
    };
    let mut count = 0;
    for name in exposition_markdown::math::supported_commands() {
        let sample = samples(name);
        if sample.is_empty() {
            continue;
        }
        lines.push_str(&format!("$${sample}$$\n\n"));
        count += 1;
    }
    for extra in [
        "\\begin{aligned} a &= b + c \\\\ &= d \\end{aligned}",
        "f(x) = \\begin{cases} 1 & x > 0 \\\\ 0 & \\text{otherwise} \\end{cases}",
        "\\begin{bmatrix} a & b \\\\ c & d \\end{bmatrix} \\begin{vmatrix} 1 \\end{vmatrix} \\begin{matrix} x \\end{matrix}",
        "\\sqrt[3]{x} + \\left[ \\frac{a}{b} \\right) + \\left. y \\middle| z \\right| + \\left\\{ x \\right\\}",
        "\\lim_{x \\to 0} \\frac{\\sin x}{x} = 1, \\int_0^\\infty e^{-x^2} \\, dx = \\frac{\\sqrt{\\pi}}{2}",
        "\\sum_{i=1}^{n} i = \\frac{n(n+1)}{2}, \\prod_{k} a_k, x'' + {(a+b)}^2 + {}^{14}C",
        "\\underbrace{a + b}_{n} + \\overbrace{x}^{m} + \\mathbf{v} \\cdot \\mathbb{R}^n, \\quad \\{1, 2\\} \\setminus \\emptyset",
        "a,b;c:d.e / f * g | h ! 3.14 \\, \\; \\: \\! \\quad \\qquad \\ x ~ y % comment",
    ] {
        lines.push_str(&format!("$${extra}$$\n\n"));
        count += 1;
    }
    let tr = translate(&lines, &options());
    let marked: Vec<_> =
        tr.diagnostics.iter().filter(|d| d.severity != exposition_core::build::Severity::Info).collect();
    assert!(marked.is_empty(), "supported commands produced diagnostics: {marked:#?}");
    let out = compiler.compile(&tr.typst);
    assert!(out.errors.is_empty(), "{:#?}", out.errors);
    let doc = out.document.unwrap();
    let glyphs = glyphs(&doc, &compiler.source());
    let cov = coverage(&lines, &tr, &glyphs);
    assert!(cov.failures.is_empty(), "{:#?}", cov.failures);
    println!(
        "math subset: {count} equations, {} commands; {}/{} math glyphs mapped",
        exposition_markdown::math::supported_commands().len(),
        cov.math_mapped,
        cov.math
    );
}

#[test]
fn escaping_corpus_compiles_and_maps() {
    let compiler = Compiler::new(fixture_dir(FIXTURE));
    let corpus = [
        "Typst specials: #let x = 1; [a] {b} $5 @ref <label> * _ ~ ` \\ // /* */ and https://example.com/x",
        "Line-start markers:\n\n= not a heading\n\n\\- not a list\n\n\\+ not an enum\n\n1\\. not numbered\n\n/ not a term: x",
        "Numbers and dashes: 3-5, -2, a-?b, x--y, x---y, ... and ....; e.g. *word*.g and *word*(x)",
        "Entities: &amp; &lt;tag&gt; &copy; &#35; &#x41; &nbsp;x and escapes \\* \\_ \\# \\[ \\] \\` \\\\",
        "Quotes 'single' \"double\" it's and unicode: café, naïve, 数学, 😀, e\u{301}",
        "**strong**word, word**strong**, *a*b*c*, ~~gone~~ and <https://example.com/auto> or <me@example.com>",
        "A [link](https://e.com/(x)) and [ref link][r] and ![inline](figure.png \"t\") icon.\n\n[r]: https://e.com",
        "| a | b |\n|---|---|\n| `x\\|y` | 1 \\| 2 |\n| *e* | $x$ |",
        "Hard\\\nbreak and soft\nbreak; html <b>bold</b> inline.",
        "Footnotes[^a] twice[^a] and nested[^b].\n\n[^a]: First *note*.\n\n[^b]: Second with $y$.\n\n    Indented paragraph.",
        "1) first\n2) second\n\n3. third\n\n- [ ] open\n- [x] done\n\n> quote\n>\n> > nested\n\n***",
        "```\nplain \"code\" \\ with `ticks` and $\n```\n\n    indented code\n\n<div>\nhtml block\n</div>",
    ];
    for (i, md) in corpus.iter().enumerate() {
        let tr = translate(md, &options());
        let out = compiler.compile(&tr.typst);
        assert!(out.errors.is_empty(), "corpus {i}: {:#?}\n{}", out.errors, tr.typst);
        let doc = out.document.unwrap();
        let glyphs = glyphs(&doc, &compiler.source());
        let cov = coverage(md, &tr, &glyphs);
        assert!(cov.failures.is_empty(), "corpus {i}: {:#?}\n{}", cov.failures, tr.typst);
        let text = page_texts(&glyphs).concat();
        match i {
            0 => assert!(
                text.contains("#let x = 1; [a] {b} $5 @ref <label> * _ ~ ` \\ // /* */ and https://example.com/x"),
                "{text}"
            ),
            1 => assert!(
                text.contains("= not a heading") && text.contains("- not a list") && text.contains("1. not numbered"),
                "{text}"
            ),
            2 => assert!(text.contains("3-5, -2, a-?b, x–y, x—y, … and …."), "{text}"),
            3 => assert!(text.contains("& <tag> © # A"), "{text}"),
            4 => assert!(text.contains("数学") && text.contains("it’s"), "{text}"),
            _ => {}
        }
    }
}

#[test]
fn translation_and_compile_latency() {
    let md = read_fixture(FIXTURE);
    let compiler = Compiler::new(fixture_dir(FIXTURE));
    let cold = Instant::now();
    let tr = translate(&md, &options());
    let out = compiler.compile(&tr.typst);
    assert!(out.errors.is_empty());
    let cold_ms = cold.elapsed().as_secs_f64() * 1000.0;
    // Warm, incremental: alternate a one-character page edit in a paragraph.
    let at = md.find("Final paragraph").unwrap() as u32;
    let mut text = md.clone();
    let mut translate_ms = Vec::new();
    let mut compile_ms = Vec::new();
    let mut plan_ms = Vec::new();
    for i in 0..20 {
        let t0 = Instant::now();
        let plan = MarkdownInput
            .plan(&text, TextRange::empty(at + i), &PageEditOp::Insert { text: "x".into() }, "\n")
            .unwrap();
        plan_ms.push(t0.elapsed().as_secs_f64() * 1000.0);
        text = apply_patches(&text, &plan.patches);
        let t1 = Instant::now();
        let tr = translate(&text, &options());
        translate_ms.push(t1.elapsed().as_secs_f64() * 1000.0);
        let t2 = Instant::now();
        let out = compiler.compile(&tr.typst);
        compile_ms.push(t2.elapsed().as_secs_f64() * 1000.0);
        assert!(out.errors.is_empty());
    }
    let median = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    println!(
        "latency ({} build): cold translate+compile {cold_ms:.1} ms; warm median: input plan {:.2} ms, translate {:.2} ms, compile {:.1} ms",
        if cfg!(debug_assertions) { "debug crate, optimized deps" } else { "release" },
        median(&mut plan_ms),
        median(&mut translate_ms),
        median(&mut compile_ms)
    );
}
