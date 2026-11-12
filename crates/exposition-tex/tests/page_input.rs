//! Typing on the page, one keystroke at a time as the page editor sends it,
//! through the LaTeX input rules (regressions from the R-m23 review).

use exposition_core::text::{TextRange, utf16_len, utf16_to_byte};
use exposition_page::{PageEditOp, PageInputRules};
use exposition_tex::TexInput;

/// Type `typed` one keystroke at a time from `at` (a byte offset), as the
/// page editor sends it, failing with the refused keystroke.
fn type_keys(text: &str, at: usize, typed: &str) -> Result<String, String> {
    let mut text = text.to_owned();
    let mut caret = utf16_len(&text[..at]) as u32;
    for (i, ch) in typed.chars().enumerate() {
        let op = PageEditOp::Insert { text: ch.to_string() };
        let plan = TexInput
            .plan(&text, TextRange::empty(caret), &op, "\n")
            .map_err(|e| format!("keystroke {i} ({ch:?}) refused: {e:?}; so far {text:?}"))?;
        for patch in plan.patches.iter().rev() {
            let s = utf16_to_byte(&text, patch.range.start as usize).unwrap();
            let e = utf16_to_byte(&text, patch.range.end as usize).unwrap();
            text.replace_range(s..e, &patch.text);
        }
        caret = plan.caret;
    }
    Ok(text)
}

const PARAGRAPHS: &str = "\\documentclass{article}\n\\begin{document}\nThe first paragraph ends here.\n\nThe macro line ends here. % trailing comment\nThe next line continues.\n\nA baseline for words.\n\\end{document}\n";

fn byte_at(needle: &str, delta: usize) -> usize {
    PARAGRAPHS.find(needle).unwrap_or_else(|| panic!("{needle}")) + delta
}

/// R-m23 F1: a word can be appended at the end of a paragraph, also
/// before a paragraph break or a `%` comment, like mid-paragraph typing.
#[test]
fn words_can_be_appended_at_a_paragraph_end_and_before_a_comment() {
    let end = byte_at("ends here.\n\nThe macro", 10);
    let out = type_keys(PARAGRAPHS, end, " More words.").unwrap();
    assert!(out.contains("ends here. More words.\n\nThe macro"), "{out:?}");
    let before_comment = byte_at("ends here. %", 10);
    let out = type_keys(PARAGRAPHS, before_comment, " more").unwrap();
    assert!(out.contains("ends here. more % trailing comment"), "{out:?}");
    // Mid-paragraph typing is unchanged.
    let mid = byte_at("baseline for", 8);
    assert!(type_keys(PARAGRAPHS, mid, " and more").unwrap().contains("A baseline and more for words."));
    // A blank line between paragraphs (or inside a comment) is not text:
    // typing there would join the paragraphs.
    let blank = byte_at("here.\n\nThe macro", 6);
    assert!(type_keys(PARAGRAPHS, blank, "x").is_err());
    let in_comment = byte_at("trailing comment", 4);
    assert!(type_keys(PARAGRAPHS, in_comment, "x").is_err());
}

/// R-m23 F2: after a typed `~`, `^`, or `\` (inserted as an escape with a
/// terminating `{}`), typing continues.
#[test]
fn typing_continues_after_an_inserted_escape() {
    let mid = byte_at("baseline for", 8);
    for (typed, escape, next) in
        [(" x~y", "\\textasciitilde{}", "y"), (" 2^3", "\\textasciicircum{}", "3"), (" a\\b", "\\textbackslash{}", "b")]
    {
        let out = type_keys(PARAGRAPHS, mid, typed).unwrap();
        let expected = format!("baseline {}{escape}{next} for", &typed[1..2]);
        assert!(out.contains(&expected), "{typed:?}: {out:?}");
        // Backspace right after the escape removes it as one character.
        let after = out.find(escape).unwrap() + escape.len();
        let caret = TextRange::empty(utf16_len(&out[..after]) as u32);
        let plan = TexInput.plan(&out, caret, &PageEditOp::DeleteBackward, "\n").unwrap();
        let range = plan.patches[0].range;
        assert_eq!((range.end - range.start) as usize, escape.len(), "{typed:?}");
        assert_eq!(plan.patches[0].text, "");
    }
}

/// R-m23 F8: `<`, `>`, `|`, and `"` are inserted in forms that typeset as
/// typed with pdfLaTeX's default OT1 encoding (and every other).
#[test]
fn specials_are_inserted_in_encoding_safe_forms() {
    let mid = byte_at("baseline for", 8);
    let out = type_keys(PARAGRAPHS, mid, " a<b>c|d \"q\"").unwrap();
    assert!(out.contains("baseline a\\textless{}b\\textgreater{}c\\textbar{}d ``q'' for"), "{out:?}");
    // In mathematics they stay as typed.
    let math = "\\documentclass{article}\n\\begin{document}\n$x$\n\\end{document}\n";
    let at = math.find("x$").unwrap() + 1;
    assert!(type_keys(math, at, "<y").unwrap().contains("$x<y$"));
}
