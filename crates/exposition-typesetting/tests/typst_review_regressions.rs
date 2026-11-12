//! Regressions for the independent R-m23 review (M3, Typst page editing).
//! Each test reproduced an open finding before its fix:
//! - F1: words appended at the end of a paragraph were refused after the space.
//! - F3: text produced by a `#let` macro or a `#show` rule was editable and a
//!   page edit rewrote the definition (changing every use).
//! - F9: right-to-left runs: selection/copy text was reversed and hit testing
//!   was mirrored.
mod common;

use common::Fixture;
use exposition_core::text::TextRange;
use exposition_page::map::ClusterKind;
use exposition_page::{PageEditOp, PageEditOutcome};

const SOURCE: &str = "#set page(width: 12cm, height: 12cm)
#let product = [Exposition Studio]
#show \"secret\": [classified]

The macro names #product here and #product there.

The plan is secret today.
";

fn fixture() -> (Fixture, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("main.typ"), SOURCE).unwrap();
    (Fixture::open_dir(dir.path(), "main.typ"), dir)
}

/// Clusters whose rendered text is `needle`'s first char, in reading order,
/// with their kind and source range.
fn clusters_spelling(fx: &Fixture, needle: &str) -> Vec<(String, ClusterKind, Option<TextRange>)> {
    let record = fx.build();
    let map = record.map.as_ref().expect("map");
    let mut out = Vec::new();
    for page in &map.pages {
        for line in &page.lines {
            let text: String = line.clusters.iter().map(|c| c.text.as_str()).collect();
            if let Some(byte) = text.find(needle) {
                // Walk clusters to the ones covering `needle`.
                let mut pos = 0;
                for c in &line.clusters {
                    let end = pos + c.text.len();
                    if end > byte && pos < byte + needle.len() {
                        out.push((c.text.clone(), c.kind, c.source.map(|s| s.range)));
                    }
                    pos = end;
                }
                return out;
            }
        }
    }
    panic!("{needle:?} not rendered");
}

fn definition_range(text: &str, start: &str) -> std::ops::Range<u32> {
    let s = text.find(start).unwrap();
    let e = s + text[s..].find('\n').unwrap();
    s as u32..e as u32
}

/// the macro output is rendered (sanity for the tests below).
#[test]
fn macro_and_show_rule_output_is_rendered() {
    let (fx, _dir) = fixture();
    assert!(!clusters_spelling(&fx, "Exposition").is_empty());
    assert!(!clusters_spelling(&fx, "classified").is_empty());
}

/// R-m23 F3 (fixed): glyphs of `#product` output map into the `#let` definition
/// and are editable; a page edit there rewrites the definition, changing
/// every use of the macro, with no affordance.
#[test]
fn f3_macro_output_is_not_editable_and_never_edits_the_definition() {
    let (fx, _dir) = fixture();
    let text = fx.text();
    let def = definition_range(&text, "#let product");
    let clusters = clusters_spelling(&fx, "Exposition");
    let (_, kind, range) = clusters[0].clone();
    let range = range.expect("source range");
    assert!(def.contains(&range.start), "the macro output maps into the definition: {range:?} in {def:?}");
    // The defect: it is editable, and typing there edits the definition.
    assert!(
        !matches!(kind, ClusterKind::Editable(_)),
        "macro output is editable on the page ({kind:?}) — typing would edit the #let definition"
    );
    let outcome = fx.edit(TextRange::empty(range.start + 1), PageEditOp::Insert { text: "Z".into() }, 1);
    assert!(
        !matches!(outcome, PageEditOutcome::Applied { .. }),
        "a page edit at macro output was applied to the definition: {:?}",
        &fx.text()[def.start as usize..def.end as usize + 1]
    );
}

/// R-m23 F3 (fixed): text substituted by a `#show` rule is editable and maps
/// into the rule's replacement content.
#[test]
fn f3_show_rule_output_is_not_editable() {
    let (fx, _dir) = fixture();
    let text = fx.text();
    let rule = definition_range(&text, "#show");
    let clusters = clusters_spelling(&fx, "classified");
    let (_, kind, range) = clusters[0].clone();
    let range = range.expect("source range");
    assert!(rule.contains(&range.start), "show output maps into the rule: {range:?} in {rule:?}");
    assert!(
        !matches!(kind, ClusterKind::Editable(_)),
        "show-rule output is editable on the page ({kind:?}) — typing would edit the #show rule"
    );
}

// ---- Typing sequences through the Typst input rules -------------------------

fn apply(text: &str, patches: &[exposition_core::document::TextPatch]) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut out: Vec<u16> = Vec::new();
    let mut at = 0usize;
    for p in patches {
        out.extend_from_slice(&units[at..p.range.start as usize]);
        out.extend(p.text.encode_utf16());
        at = p.range.end as usize;
    }
    out.extend_from_slice(&units[at..]);
    String::from_utf16(&out).unwrap()
}

/// Type `typed` one keystroke at a time (each plan applied before the next).
fn type_keys(text: &str, at: u32, typed: &str) -> Result<String, (exposition_page::EditRefusal, String, usize)> {
    use exposition_page::{PageInputRules, TypstInput};
    let mut text = text.to_owned();
    let mut caret = at;
    for (i, ch) in typed.chars().enumerate() {
        match TypstInput.plan(&text, TextRange::empty(caret), &PageEditOp::Insert { text: ch.to_string() }, "\n") {
            Ok(plan) => {
                text = apply(&text, &plan.patches);
                caret = plan.caret;
            }
            Err(refusal) => return Err((refusal, text, i)),
        }
    }
    Ok(text)
}

const PARAS: &str = "First paragraph ends here.\n\nSecond paragraph.\n";

/// typing letters directly after the last word of a paragraph.
#[test]
fn typing_letters_at_paragraph_end_is_accepted() {
    let at = PARAS.find("here.").unwrap() as u32 + 5;
    let out = type_keys(PARAS, at, "Yes").expect("letters at the paragraph end");
    assert!(out.contains("here.Yes\n\n"), "{out:?}");
}

/// R-m23 (fixed; also checked in the real app): after a space typed at the end of a
/// paragraph, every further keystroke is refused ("would change the document
/// structure"), so a word cannot be appended to a paragraph on the page.
#[test]
fn f1_appending_a_word_at_paragraph_end() {
    let at = PARAS.find("here.").unwrap() as u32 + 5;
    match type_keys(PARAS, at, " More words.") {
        Ok(out) => assert!(out.contains("here. More words.\n\n"), "{out:?}"),
        Err((refusal, so_far, i)) => panic!("keystroke {i} refused ({refusal:?}); text so far: {so_far:?}"),
    }
}

// ---- Right-to-left text ------------------------------------------------------

/// R-m23 (fixed; also checked in the real app): the rendered text of a selection
/// (what Copy puts on the clipboard and what the caret line reports to
/// assistive technology) is assembled in visual left-to-right order, so
/// Hebrew/Arabic come out reversed.
#[test]
fn f9_rtl_selection_text_is_in_logical_order() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("main.typ"), "#set page(width: 12cm, height: 6cm)\n\nWord שלום end.\n").unwrap();
    let fx = Fixture::open_dir(dir.path(), "main.typ");
    let record = fx.build();
    let map = record.map.as_ref().expect("map");
    let start = fx.offset_of("שלום", 0);
    let selection = map.selection(&fx.path, start, start + 4);
    assert_eq!(selection.text, "שלום", "selection text of a Hebrew word");
}

/// R-m23 (fixed; also checked in the real app): hit testing treats RTL glyphs as
/// left-to-right: the right half of a Hebrew letter maps to the offset after
/// it (logically), so a click at the right edge of a Hebrew word places the
/// caret after its first letter.
#[test]
fn f9_rtl_hit_test_is_not_mirrored() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("main.typ"), "#set page(width: 12cm, height: 6cm)\n\nWord שלום end.\n").unwrap();
    let fx = Fixture::open_dir(dir.path(), "main.typ");
    let record = fx.build();
    let map = record.map.as_ref().expect("map");
    let start = fx.offset_of("שלום", 0);
    let first = map.selection(&fx.path, start, start + 1).rects[0];
    // The first (rightmost) letter: its right half is logically before it.
    let hit = map.hit_test(first.page, first.x + first.width * 0.85, first.y + first.height / 2.0);
    assert_eq!(hit.caret.expect("caret").offset, start, "right half of the first Hebrew letter");
}
