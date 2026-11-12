//! P3 evidence: Unicode on the page — combining sequences and astral
//! (surrogate-pair) characters are single caret units mapped to the right
//! UTF-16 ranges; deleting removes whole graphemes; ligatures subdivide.
mod common;

use common::Fixture;
use exposition_core::build::BuildStatus;
use exposition_core::text::TextRange;
use exposition_page::map::Affinity;
use exposition_page::{Motion, PageEditOp, PageEditOutcome};

fn right(fx: &Fixture, map: &exposition_page::PageMap, offset: u32) -> u32 {
    map.navigate(&fx.path, offset, Affinity::Downstream, Motion::Right, None).unwrap().caret.offset
}

#[test]
fn combining_sequence_is_one_caret_unit() {
    let fx = Fixture::open("typst-unicode", "main.typ");
    let record = fx.build();
    assert_eq!(record.result.status, BuildStatus::Succeeded, "{:?}", record.result.diagnostics);
    let map = record.map.as_ref().unwrap();
    // "cafe\u{301}": e + combining acute (2 UTF-16 units, one grapheme).
    let e = fx.offset_of("e\u{301} au", 0);
    assert_eq!(right(&fx, map, e), e + 2, "Right steps over the whole grapheme");
    assert_eq!(map.navigate(&fx.path, e + 2, Affinity::Downstream, Motion::Left, None).unwrap().caret.offset, e);
    // No caret stop inside the grapheme.
    let inside = map.caret_geometry(&fx.path, e + 1, Affinity::Downstream).unwrap();
    assert!(!inside.exact);
    assert_eq!(map.selection(&fx.path, e, e + 2).text, "e\u{301}");
    // Backspace after it removes both code points.
    let out = fx.edit(TextRange::empty(e + 2), PageEditOp::DeleteBackward, 1);
    assert!(matches!(out, PageEditOutcome::Applied { caret, .. } if caret == e), "{out:?}");
    assert!(fx.text().contains("Combining: caf au lait."));
}

#[test]
fn astral_characters_map_to_surrogate_pairs() {
    let fx = Fixture::open("typst-unicode", "main.typ");
    let record = fx.build();
    let map = record.map.as_ref().unwrap();
    let a = fx.offset_of("\u{1D538} letter", 0);
    assert_eq!(right(&fx, map, a), a + 2, "one press crosses the surrogate pair");
    let geometry = map.caret_geometry(&fx.path, a + 2, Affinity::Upstream).unwrap();
    assert!(geometry.exact);
    // A click on the right half of the glyph lands after the pair.
    let start = map.caret_geometry(&fx.path, a, Affinity::Downstream).unwrap();
    let hit = map.hit_test(0, start.x + (geometry.x - start.x) * 0.8, (start.top + start.bottom) / 2.0);
    assert_eq!(hit.caret.unwrap().offset, a + 2);
    assert_eq!(map.selection(&fx.path, a, a + 2).text, "\u{1D538}");
    // Typing after it inserts at the UTF-16 offset after the pair.
    let caret = fx.type_text(a + 2, "x");
    assert_eq!(caret, a + 3);
    assert!(fx.text().contains("\u{1D538}x letter"));
    // Backspace twice: removes x, then the whole astral character.
    fx.edit(TextRange::empty(a + 3), PageEditOp::DeleteBackward, 2);
    let out = fx.edit(TextRange::empty(a + 2), PageEditOp::DeleteBackward, 2);
    assert!(matches!(out, PageEditOutcome::Applied { caret, .. } if caret == a), "{out:?}");
    assert!(fx.text().contains("double-struck  letter"));
    let rebuilt = fx.build();
    assert_eq!(rebuilt.result.status, BuildStatus::Succeeded);
}

#[test]
fn astral_math_identifier_and_ligatures() {
    let fx = Fixture::open("typst-unicode", "main.typ");
    let record = fx.build();
    let map = record.map.as_ref().unwrap();
    // In math, 𝔸 is one math text atom of two UTF-16 units.
    let a = fx.offset_of("\u{1D538} + x", 0);
    let g = map.caret_geometry(&fx.path, a, Affinity::Downstream).unwrap();
    assert!(g.exact && g.mode == exposition_page::EditMode::Math);
    assert_eq!(right(&fx, map, a), a + 2);
    // "waffled": the ffl ligature has one stop per character.
    let w = fx.offset_of("waffled", 1);
    let mut offset = w;
    for _ in 0..4 {
        let next = right(&fx, map, offset);
        assert_eq!(next, offset + 1);
        offset = next;
    }
    // Inserting inside the ligature splits it in the source and rendering.
    fx.type_text(w + 2, "-");
    assert!(fx.text().contains("waf-fled"));
    let rebuilt = fx.build();
    let m = rebuilt.map.as_ref().unwrap();
    assert_eq!(m.selection(&fx.path, w, w + 5).text, "af-fl");
}
