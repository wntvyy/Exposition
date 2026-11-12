//! Regressions for the R-m23 independent review (M3, Markdown print
//! profile): typing sequences through the page input rules one keystroke at a
//! time, as the page editor sends them (each plan applied before the next
//! keystroke). The reviewer's `review_defect_*` reproductions failed before
//! the fixes (keystrokes refused as structure changes) and now pass with the
//! exact Markdown checked.

use exposition_core::text::TextRange;
use exposition_markdown::{EditRefusal, MarkdownInput, PageEditOp, PageInputRules, TranslateOptions, translate};

/// Apply UTF-16 patches (sorted, non-overlapping) to `text`.
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

/// Type `typed` char by char at `at`; returns the final text and caret, or
/// the refusal with the text reached so far.
fn type_keys(text: &str, at: u32, typed: &str) -> Result<(String, u32), (EditRefusal, String, usize)> {
    let mut text = text.to_owned();
    let mut caret = at;
    for (i, ch) in typed.chars().enumerate() {
        match MarkdownInput.plan(&text, TextRange::empty(caret), &PageEditOp::Insert { text: ch.to_string() }, "\n") {
            Ok(plan) => {
                text = apply(&text, &plan.patches);
                caret = plan.caret;
            }
            Err(refusal) => return Err((refusal, text, i)),
        }
    }
    Ok((text, caret))
}

fn typed(text: &str, at: u32, keys: &str) -> (String, u32) {
    type_keys(text, at, keys).unwrap_or_else(|(refusal, so_far, i)| {
        panic!("typing {keys:?}: keystroke {i} refused ({refusal:?}); so far {so_far:?}")
    })
}

const DOC: &str = "First paragraph ends here.\n\nSecond paragraph.\n";

/// Typing a word at the end of a paragraph without a leading space.
#[test]
fn typing_letters_at_paragraph_end_is_accepted() {
    let at = DOC.find("here.").unwrap() as u32 + 5;
    let (out, _) = typed(DOC, at, "Yes");
    assert_eq!(out, "First paragraph ends here.Yes\n\nSecond paragraph.\n");
}

/// R-m23 F1 (was `review_defect_appending_a_word_at_paragraph_end_is_refused`):
/// after a space typed at the end of a paragraph every further keystroke was
/// refused, so a word could not be appended on the page.
#[test]
fn appending_a_word_at_paragraph_end_types_every_keystroke() {
    let at = DOC.find("here.").unwrap() as u32 + 5;
    let (out, caret) = typed(DOC, at, " More words.");
    assert_eq!(out, "First paragraph ends here. More words.\n\nSecond paragraph.\n");
    assert_eq!(caret, at + 12);
    // The same at the end of the document without a final newline, and at
    // the end of a line inside a paragraph (before a soft break).
    let (out, _) = typed("Last words.", 11, " And more.");
    assert_eq!(out, "Last words. And more.");
    let soft = "Line one\nline two.\n";
    let (out, _) = typed(soft, 8, " continues");
    assert_eq!(out, "Line one continues\nline two.\n");
    // The rendered paragraph has the words separated by one space.
    let tr = translate(&out, &TranslateOptions::default());
    assert!(tr.typst.contains("Line one continues line two."), "{}", tr.typst);
}

/// R-m23 F11 (was `review_defect_line_start_list_markers_are_refused_not_escaped`):
/// typing a list marker and a space at the start of a paragraph was refused;
/// the profile documents that line-start markers are escaped and print
/// literally.
#[test]
fn line_start_list_markers_are_escaped() {
    let at = DOC.find("Second").unwrap() as u32;
    for (keys, written) in [("- ", "\\- "), ("+ ", "\\+ "), ("* ", "\\* "), ("1. ", "1\\. "), ("12) ", "12\\) ")] {
        let (out, caret) = typed(DOC, at, keys);
        assert_eq!(out, format!("First paragraph ends here.\n\n{written}Second paragraph.\n"), "typing {keys:?}");
        assert_eq!(caret, at + written.len() as u32, "caret after {keys:?}");
        // Still one paragraph, printing the marker literally.
        let tr = translate(&out, &TranslateOptions::default());
        assert!(!tr.typst.contains("#list(") && !tr.typst.contains("#enum("), "{keys:?}: {}", tr.typst);
    }
    // Inside a list item and in a block quote the same holds.
    let (out, _) = typed("- item\n", 2, "- ");
    assert_eq!(out, "- \\- item\n");
    let (out, _) = typed("> quoted\n", 2, "1. ");
    assert_eq!(out, "> 1\\. quoted\n");
    // Mid-line markers need no escape.
    let (out, _) = typed(DOC, 5, " -");
    assert_eq!(out, "First - paragraph ends here.\n\nSecond paragraph.\n");
}
