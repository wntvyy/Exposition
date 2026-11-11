//! Coverage and failure classes beyond the basic fixture: lists, tables,
//! user macros, fractions/roots/delimiters, align, `\text`, `\operatorname`,
//! `\verb`, and a LuaLaTeX `fontspec` + `unicode-math` variant.

mod common;

use exposition_core::build::{BuildStatus, Engine};
use exposition_core::text::utf16_to_byte;
use exposition_tex::mapping::{GlyphTarget, PageMapping, UnmappedReason};
use exposition_tex::{BuildMode, TexBuild, TexPageSession};

fn find_word(mapping: &PageMapping, word: &str) -> Vec<u32> {
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

fn source_of(build: &TexBuild, mapping: &PageMapping, glyph: u32) -> String {
    match &mapping.targets[glyph as usize] {
        GlyphTarget::Source { path, range, .. } => {
            let text = build.snapshot.as_ref().unwrap().text(path).unwrap();
            let (s, e) =
                (utf16_to_byte(text, range.start as usize).unwrap(), utf16_to_byte(text, range.end as usize).unwrap());
            text[s..e].to_owned()
        }
        other => panic!("glyph {glyph} {:?} unmapped: {other:?}", mapping.glyphs[glyph as usize].text),
    }
}

fn map(fixture: &str, engine: Engine) -> Option<(TexBuild, Vec<PageMapping>, exposition_tex::mapping::SourceCoverage)> {
    let h = common::Harness::new(fixture, engine)?;
    let build = h.builder.build(&h.request(BuildMode::Full));
    assert_eq!(build.result.status, BuildStatus::Succeeded, "{fixture} {engine:?}: {:?}", build.result.diagnostics);
    let session = TexPageSession::open(&build).unwrap();
    let pages = session.map_all().unwrap();
    let coverage = session.coverage(&pages);
    let s = &pages[0].stats;
    eprintln!(
        "{fixture} {engine:?}: glyphs {} mapped {} (text {}, math {}) unmapped {:?} round trip {}/{}; source text {}/{} math {}/{}",
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
    for page in &pages {
        assert_eq!(page.stats.roundtrip_exact, page.stats.roundtrip_checked, "{fixture} {engine:?}");
    }
    assert_eq!(coverage.text_matched, coverage.text_expected, "{fixture} {engine:?}: {:?}", coverage.missed);
    assert_eq!(coverage.math_matched, coverage.math_expected, "{fixture} {engine:?}: {:?}", coverage.missed);
    Some((build, pages, coverage))
}

#[test]
fn constructs_fixture_maps_declared_constructs_and_classifies_generated_text() {
    for engine in [Engine::PdfLatex, Engine::LuaLatex] {
        let Some((build, pages, coverage)) = map("latex-constructs", engine) else { continue };
        assert!(coverage.text_expected > 240 && coverage.math_expected > 50);
        let p = &pages[0];
        // User macro bodies and macro-produced symbols are generated text.
        for g in find_word(p, "Exposition") {
            assert_eq!(p.targets[g as usize], GlyphTarget::Unmapped { reason: UnmappedReason::Generated });
        }
        let r = find_word(p, "∈R")[1];
        assert_eq!(p.targets[r as usize], GlyphTarget::Unmapped { reason: UnmappedReason::Generated });
        // List labels: the bullet is generated; an explicit label is source.
        let first = find_word(p, "Firstitem")[0];
        assert!(!p.targets[first as usize - 1].is_mapped());
        assert_eq!(source_of(&build, p, find_word(p, "–Custom")[0]), "--");
        // Tables, verbatim, small caps, typewriter.
        assert_eq!(source_of(&build, p, find_word(p, "alpha1.5")[5]), "1");
        assert_eq!(source_of(&build, p, find_word(p, "verb{x}")[4]), "{");
        assert_eq!(source_of(&build, p, find_word(p, "SmallCaps")[1]), "m");
        // Math: roots, overlays, limits drawn out of order, \text, \operatorname.
        assert_eq!(source_of(&build, p, find_word(p, "√x2")[0]), "\\sqrt");
        let neq = find_word(p, "ρ̸=0");
        assert_eq!(source_of(&build, p, neq[1]), "\\neq");
        let int = find_word(p, "∫︂10");
        assert_eq!(source_of(&build, p, int[1]), "1");
        assert_eq!(source_of(&build, p, int[2]), "0");
        assert_eq!(source_of(&build, p, find_word(p, "xify")[1]), "i");
        assert_eq!(source_of(&build, p, find_word(p, "tr(A)")[0]), "t");
        assert_eq!(source_of(&build, p, find_word(p, "sinθ")[0]), "\\sin");
        // Equation numbers are generated.
        let eq2 = find_word(p, "(2)");
        assert!(!p.targets[eq2[1] as usize].is_mapped());
    }
}

#[test]
fn lualatex_unicode_math_variant_maps_opentype_math() {
    let Some((build, pages, _)) = map("latex-unicode-math", Engine::LuaLatex) else { return };
    let p = &pages[0];
    // Direct UTF-8 input is literal source.
    assert_eq!(source_of(&build, p, find_word(p, "café")[3]), "é");
    assert_eq!(source_of(&build, p, find_word(p, "Grüße")[2]), "ü");
    // unicode-math draws Mathematical Alphanumeric code points.
    let a = find_word(p, "𝑎2+𝑏𝑖≤𝛼")[0];
    assert_eq!(p.glyphs[a as usize].text.as_deref(), Some("\u{1D44E}"));
    assert_eq!(source_of(&build, p, a), "a");
    assert_eq!(source_of(&build, p, find_word(p, "𝑎2+𝑏𝑖≤𝛼")[5]), "\\leq");
    // Greek typed directly in math.
    assert_eq!(source_of(&build, p, find_word(p, "𝛼+𝛽≤𝛾")[0]), "α");
    // OpenType math has a precomposed ≠ glyph.
    assert_eq!(source_of(&build, p, find_word(p, "≠0")[0]), "\\neq");
    assert_eq!(source_of(&build, p, find_word(p, "ℝ")[0]), "R");
}
