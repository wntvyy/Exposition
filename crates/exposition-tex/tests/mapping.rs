//! Character-level mapping and SyncTeX queries on real engine output.

mod common;

use exposition_core::build::Engine;
use exposition_core::path::WorkspacePath;
use exposition_core::text::utf16_to_byte;
use exposition_tex::latex::{Construct, Mode};
use exposition_tex::mapping::{GlyphTarget, PageMapping, UnmappedReason};
use exposition_tex::{BuildMode, TexBuild, TexPageSession};

const ENGINES: [Engine; 2] = [Engine::PdfLatex, Engine::LuaLatex];

pub fn find_word(mapping: &PageMapping, word: &str) -> Vec<u32> {
    let mut text = String::new();
    let mut owners = Vec::new();
    for (i, g) in mapping.glyphs.iter().enumerate() {
        for c in g.text.as_deref().unwrap_or("\u{FFFD}").chars() {
            text.push(c);
            owners.push(i as u32);
        }
    }
    let chars: Vec<char> = text.chars().collect();
    let needle: Vec<char> = word.chars().collect();
    let start = chars.windows(needle.len()).position(|w| w == needle.as_slice()).unwrap_or_else(|| panic!("{word}"));
    let mut glyphs: Vec<u32> = owners[start..start + needle.len()].to_vec();
    glyphs.dedup();
    glyphs
}

fn source<'a>(build: &'a TexBuild, path: &str) -> &'a str {
    build.snapshot.as_ref().unwrap().text(&WorkspacePath::parse(path).unwrap()).unwrap()
}

fn slice(text: &str, start: u32, end: u32) -> &str {
    &text[utf16_to_byte(text, start as usize).unwrap()..utf16_to_byte(text, end as usize).unwrap()]
}

fn target(mapping: &PageMapping, glyph: u32) -> &GlyphTarget {
    &mapping.targets[glyph as usize]
}

fn mapped_slice<'a>(build: &'a TexBuild, mapping: &PageMapping, glyph: u32) -> (&'a str, Mode, Construct, bool) {
    match target(mapping, glyph) {
        GlyphTarget::Source { path, range, mode, construct, literal, .. } => {
            (slice(source(build, path.as_str()), range.start, range.end), *mode, *construct, *literal)
        }
        other => panic!("glyph {glyph} ({:?}) unmapped: {other:?}", mapping.glyphs[glyph as usize].text),
    }
}

fn unmapped(mapping: &PageMapping, glyph: u32) -> UnmappedReason {
    match target(mapping, glyph) {
        GlyphTarget::Unmapped { reason } => *reason,
        other => panic!("glyph {glyph} mapped: {other:?}"),
    }
}

#[test]
fn fixture_maps_prose_and_math_at_character_level() {
    for engine in ENGINES {
        let Some(h) = common::Harness::new("latex-basic", engine) else { continue };
        let build = h.builder.build(&h.request(BuildMode::Full));
        let session = TexPageSession::open(&build).unwrap();
        let pages = session.map_all().unwrap();
        let coverage = session.coverage(&pages);
        let p = &pages[0];
        let s = &p.stats;
        eprintln!(
            "{engine:?}: page 1 glyphs {} mapped {} (text {}, math {}), unmapped {:?}, round trip {}/{}; \
             document source coverage text {}/{} math {}/{}",
            s.glyphs,
            s.mapped,
            s.mapped_text,
            s.mapped_math,
            s.unmapped,
            s.roundtrip_exact,
            s.roundtrip_checked,
            coverage.text_matched,
            coverage.text_expected,
            coverage.math_matched,
            coverage.math_expected
        );
        assert!(coverage.text_expected > 650 && coverage.math_expected >= 40, "{coverage:?}");
        assert_eq!(coverage.text_matched, coverage.text_expected, "{engine:?} missed {:?}", coverage.missed);
        assert_eq!(coverage.math_matched, coverage.math_expected, "{engine:?} missed {:?}", coverage.missed);
        for page in &pages {
            assert_eq!(page.stats.roundtrip_exact, page.stats.roundtrip_checked, "{engine:?} page {}", page.page);
        }

        // Ligatures: one glyph, three source characters, inner caret stops.
        let official = find_word(p, "official");
        let ffi = official[1];
        assert_eq!(p.glyphs[ffi as usize].text.as_deref(), Some("ffi"));
        let (text, mode, construct, literal) = mapped_slice(&build, p, ffi);
        assert_eq!((text, mode, construct, literal), ("ffi", Mode::Text, Construct::Prose, true));
        let GlyphTarget::Source { boundaries, .. } = target(p, ffi) else { unreachable!() };
        assert_eq!(boundaries.len(), 4);
        // Accents: pdfLaTeX (OT1) draws accent + base, LuaLaTeX (TU) one glyph;
        // both map to the whole `\'e` macro, which is atomic.
        let cafe = find_word(p, if engine == Engine::PdfLatex { "caf´e" } else { "café" });
        let e_acute = *cafe.last().unwrap();
        assert_eq!(mapped_slice(&build, p, e_acute).0, "\\'e");
        assert!(!mapped_slice(&build, p, e_acute).3);
        // TeX ligature input and quotes.
        assert_eq!(mapped_slice(&build, p, find_word(p, "1–2")[1]).0, "--");
        assert_eq!(mapped_slice(&build, p, find_word(p, "dash—like")[4]).0, "---");
        assert_eq!(mapped_slice(&build, p, find_word(p, "“double")[0]).0, "``");
        // Styled text and footnote text.
        assert_eq!(mapped_slice(&build, p, find_word(p, "emphasized")[0]).2, Construct::Styled);
        let note = find_word(p, "Ashortfootnote");
        assert_eq!(mapped_slice(&build, p, note[0]).2, Construct::Footnote);
        // Inline math: macros and scripts.
        let alpha = find_word(p, "α+β≤γ");
        assert_eq!(mapped_slice(&build, p, alpha[0]).0, "\\alpha");
        assert_eq!(mapped_slice(&build, p, alpha[3]).0, "\\leq");
        assert_eq!(mapped_slice(&build, p, alpha[0]).1, Mode::Math);
        // Display math: exponent, limits drawn out of source order.
        let display = find_word(p, "E=mc2+n");
        assert_eq!(mapped_slice(&build, p, display[4]).0, "2");
        assert_eq!(mapped_slice(&build, p, display[4]).2, Construct::DisplayMath);
        assert_eq!(mapped_slice(&build, p, display[6]).0, "n");
        // Generated text stays unmapped.
        let intro = find_word(p, "1Introduction");
        assert_eq!(unmapped(p, intro[0]), UnmappedReason::Generated);
        assert_eq!(unmapped(p, find_word(p, "Section2")[7]), UnmappedReason::Generated);
        assert_eq!(unmapped(p, find_word(p, "detail[1]")[7]), UnmappedReason::Generated);
        let eqno = find_word(p, "xi(1)");
        assert_eq!(unmapped(p, eqno[3]), UnmappedReason::Generated);
        let fig = find_word(p, "Figure1:A");
        assert_eq!(unmapped(p, fig[0]), UnmappedReason::Generated);
        assert_eq!(mapped_slice(&build, p, fig[fig.len() - 1]).2, Construct::Caption);
        // Discretionary hyphens are not source characters.
        let hyphen = find_word(p, "characteristi-")[13];
        assert_eq!(unmapped(p, hyphen), UnmappedReason::Hyphenation);
        // The word continues on the next line from the same source characters.
        let cally = find_word(p, "cally");
        let (text, ..) = mapped_slice(&build, p, cally[0]);
        assert_eq!(text, "c");
        // Text from an \input file.
        let display_word = find_word(p, "Display");
        let GlyphTarget::Source { path, .. } = target(p, display_word[0]) else { panic!() };
        assert_eq!(path.as_str(), "sections/results.tex");
        // Bibliography (generated from refs.bib) is not editable page text.
        let bib = find_word(&pages[1], "Knuth.");
        assert!(!target(&pages[1], bib[0]).is_mapped());
    }
}

#[test]
fn hit_testing_and_caret_positions_round_trip() {
    for engine in ENGINES {
        let Some(h) = common::Harness::new("latex-basic", engine) else { continue };
        let build = h.builder.build(&h.request(BuildMode::Full));
        let session = TexPageSession::open(&build).unwrap();
        let p = session.map_page(0).unwrap();
        let mut checked = 0;
        for (i, (glyph, t)) in p.glyphs.iter().zip(&p.targets).enumerate() {
            let GlyphTarget::Source { path, boundaries, literal: true, .. } = t else { continue };
            if glyph.advance < 1.0 {
                continue;
            }
            let (_, y) = glyph.bbox.center();
            // Just inside the left edge → caret before the glyph.
            // (Kerned neighbours may overlap; the caret offset is what matters.)
            let left = p.hit_test(glyph.origin_x + 0.05 * glyph.advance, y, 0.5).unwrap();
            assert_eq!(left.offset, boundaries[0], "{engine:?} glyph {i}");
            assert!(left.glyph.abs_diff(i as u32) <= 1);
            // Just inside the right edge → caret after the glyph.
            let right = p.hit_test(glyph.origin_x + 0.95 * glyph.advance, y, 0.5).unwrap();
            assert_eq!(right.offset, *boundaries.last().unwrap());
            // The caret for the start offset sits at the glyph's left edge.
            let caret = p.caret_for_offset(path, boundaries[0]).unwrap();
            assert!((caret.x - glyph.origin_x).abs() < 0.01, "{engine:?} glyph {i}");
            checked += 1;
        }
        assert!(checked > 600, "{checked}");
        // Points in the margin hit nothing.
        assert!(p.hit_test(5.0, 5.0, 2.0).is_none());
        assert!(p.glyph_at(5.0, 5.0).is_none());
        // A click on generated text (the section number) identifies it as such.
        let number = find_word(&p, "1Introduction")[0];
        let (x, y) = p.glyphs[number as usize].bbox.center();
        let at = p.glyph_at(x, y).unwrap();
        assert_eq!(at, number);
        assert_eq!(p.targets[at as usize], GlyphTarget::Unmapped { reason: UnmappedReason::Generated });
    }
}

#[test]
fn synctex_forward_and_backward_search_on_real_output() {
    for engine in ENGINES {
        let Some(h) = common::Harness::new("latex-basic", engine) else { continue };
        let build = h.builder.build(&h.request(BuildMode::Full));
        let session = TexPageSession::open(&build).unwrap();
        let snapshot = build.snapshot.as_ref().unwrap();
        let tag = |path: &str| {
            *session
                .synctex
                .tags_where(|name| snapshot.resolve(name).is_some_and(|p| p.as_str() == path))
                .first()
                .unwrap()
        };
        let (main, results) = (tag("main.tex"), tag("sections/results.tex"));
        let p = session.map_page(0).unwrap();
        // Backward: glyph centres of known words → their source lines.
        for (word, file_tag, line) in
            [("official", main, 14), ("affine", main, 15), ("Incomprehensibilities", main, 20), ("Display", results, 3)]
        {
            let g = &p.glyphs[find_word(&p, word)[1] as usize];
            let (x, y) = g.bbox.center();
            let hit = session.synctex.backward(1, x, y).unwrap();
            assert_eq!((hit.tag, hit.line), (file_tag, line), "{engine:?} {word}");
        }
        // Forward: line 14 → a rectangle on page 1 containing "first".
        let hits = session.synctex.forward(main, 14);
        let first = &p.glyphs[find_word(&p, "first")[2] as usize];
        assert!(
            hits.iter().any(|h| h.page == 1 && h.rect.contains(first.origin_x, first.origin_y - 1.0)),
            "{engine:?} {hits:?}"
        );
        // SyncTeX alone is line-level: compare its backward answer with the
        // character mapping for every mapped prose glyph.
        let (mut agree, mut total) = (0, 0);
        for (g, t) in p.glyphs.iter().zip(&p.targets) {
            let GlyphTarget::Source { path, range, mode: Mode::Text, .. } = t else { continue };
            let text = snapshot.text(path).unwrap();
            let line = text[..utf16_to_byte(text, range.start as usize).unwrap()].matches('\n').count() as u32 + 1;
            let (x, y) = g.bbox.center();
            if let Some(hit) = session.synctex.backward(1, x, y) {
                total += 1;
                agree += u32::from(hit.line == line && hit.tag == tag(path.as_str()));
            }
        }
        // Words that start a source line in the middle of a typeset line, or
        // end a typeset line, are ambiguous at line level (~15% here).
        eprintln!("{engine:?}: SyncTeX backward line == mapped line for {agree}/{total} prose glyphs");
        assert!(agree * 10 >= total * 8, "{agree}/{total}");
    }
}

#[test]
fn glyphtounicode_keeps_layout_identical() {
    let Some(h) = common::Harness::new("latex-basic", Engine::PdfLatex) else { return };
    let plain = h.builder.build(&h.request(BuildMode::Full));
    let mut request = h.request(BuildMode::Full);
    request.tounicode = true;
    let tounicode = h.builder.build(&request);
    assert_eq!(tounicode.result.configuration.as_ref().unwrap().profile.as_deref(), Some("glyphtounicode"));
    let a = TexPageSession::open(&plain).unwrap();
    let b = TexPageSession::open(&tounicode).unwrap();
    assert_eq!(a.page_count(), b.page_count());
    let (mut differing_text, mut total) = (0, 0);
    for page in 0..a.page_count() {
        let ga = a.pdf.extract_glyphs(page).unwrap();
        let gb = b.pdf.extract_glyphs(page).unwrap();
        assert_eq!(ga.len(), gb.len());
        for (x, y) in ga.iter().zip(&gb) {
            assert!((x.origin_x - y.origin_x).abs() < 1e-6 && (x.origin_y - y.origin_y).abs() < 1e-6);
            assert_eq!(x.char_code, y.char_code);
            total += 1;
            if x.text != y.text {
                differing_text += 1;
                eprintln!("unicode differs: {:?} vs {:?}", x.text, y.text);
            }
        }
    }
    eprintln!("glyphtounicode: {total} glyphs, identical positions, {differing_text} differ in recovered Unicode");
    assert!(a.pdf.extract_glyphs(0).unwrap().iter().all(|g| g.text.is_some()));
}

#[test]
fn page_rendering_speed_and_size() {
    for engine in ENGINES {
        let Some(h) = common::Harness::new("latex-basic", engine) else { continue };
        let build = h.builder.build(&h.request(BuildMode::Draft));
        let session = TexPageSession::open(&build).unwrap();
        for scale in [1.0f32, 2.0, 3.0] {
            let started = std::time::Instant::now();
            let page = session.render_page(0, scale).unwrap();
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(page.width_px, (612.0 * scale) as u32);
            assert!(page.png.starts_with(b"\x89PNG"));
            eprintln!(
                "{engine:?}: render page 1 at {scale}x ({}x{} px): {ms:.1} ms, png {} KiB",
                page.width_px,
                page.height_px,
                page.png.len() / 1024
            );
        }
        let started = std::time::Instant::now();
        let glyphs = session.pdf.extract_glyphs(0).unwrap();
        let extract = started.elapsed().as_secs_f64() * 1000.0;
        let started = std::time::Instant::now();
        let mapping = session.map_page(0).unwrap();
        let map = started.elapsed().as_secs_f64() * 1000.0;
        eprintln!("{engine:?}: extract {} glyphs {extract:.1} ms; extract+map {map:.1} ms", glyphs.len());
        assert_eq!(mapping.glyphs.len(), glyphs.len());
    }
}
