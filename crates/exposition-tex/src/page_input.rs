//! LaTeX input rules for the shared page editor.
//!
//! Page input is translated into source patches that keep the document's
//! structure intact:
//!
//! * **Typing** inserts [`latex::escape_insertion`] text for the mode at the
//!   caret (prose escapes TeX specials; math turns typed Greek/operators into
//!   macros). A control word just before the caret is terminated (`{}` in
//!   prose, a space in math) so `\ldots` + `x` never becomes `\ldotsx`.
//!   Newlines become a paragraph break in body prose and spaces elsewhere.
//! * **Backspace/Delete** removes one rendered character: the source unit
//!   behind it as a whole (`\'e`, `--`, `\alpha`, a single letter of a
//!   ligature), or the whitespace/tie between two words. Deleting an
//!   unbraced script (`c^2`) leaves an empty group (`c^{}`) with the caret
//!   inside it, so typing replaces the exponent.
//! * **Selections** are deleted or replaced only when they consist of whole
//!   rendered characters and whitespace; anything that would remove markup
//!   (braces, commands, math delimiters, environment boundaries) is refused
//!   with [`EditRefusal::WouldChangeStructure`].
//! * **Enter** inserts a paragraph break in body prose only.

use exposition_core::text::{TextRange, byte_to_utf16, utf16_len, utf16_to_byte};
use exposition_page::{EditMode, EditPlan, EditRefusal, PageEditOp, PageInputRules};

use crate::latex::{self, Construct, Mode, SourceModel, SourceUnit};

/// LaTeX page input rules (`.tex` documents).
pub struct TexInput;

/// Control words whose following token is their delimiter argument.
const DELIMITER_COMMANDS: &[&str] = &[
    "left", "right", "middle", "big", "Big", "bigg", "Bigg", "bigl", "bigr", "Bigl", "Bigr", "biggl", "biggr", "Biggl",
    "Biggr",
];

struct Context<'a> {
    text: &'a str,
    model: SourceModel,
}

impl<'a> Context<'a> {
    fn new(text: &'a str) -> Self {
        Self { text, model: latex::parse(text) }
    }

    fn units(&self) -> &[SourceUnit] {
        &self.model.units
    }

    /// A unit whose source strictly contains byte `b` (inside an atomic macro).
    fn unit_containing(&self, b: usize) -> Option<&SourceUnit> {
        self.units().iter().find(|u| u.range.start < b && b < u.range.end)
    }

    fn previous_unit(&self, b: usize) -> Option<&SourceUnit> {
        self.units().iter().filter(|u| u.range.end <= b).max_by_key(|u| u.range.end)
    }

    fn next_unit(&self, b: usize) -> Option<&SourceUnit> {
        self.units().iter().filter(|u| u.range.start >= b).min_by_key(|u| u.range.start)
    }

    /// Inside a math construct, between its delimiters.
    fn in_math(&self, b: usize) -> bool {
        self.model.math_region_at(b).is_some_and(|(r, _)| {
            let inner_start = delimiter_len(&self.text[r.clone()], true) + r.start;
            let inner_end = r.end - delimiter_len(&self.text[r.clone()], false);
            b >= inner_start && b <= inner_end
        })
    }

    /// The input mode and construct at byte `b`, or `None` where page input
    /// is not accepted.
    fn mode_at(&self, b: usize) -> Option<(Mode, Construct)> {
        if self.unit_containing(b).is_some() || ends_inside_control_word(self.text, b) {
            return None;
        }
        if self.in_math(b) {
            let construct = self
                .model
                .math_region_at(b)
                .map(|(_, block)| if *block { Construct::DisplayMath } else { Construct::InlineMath })
                .unwrap_or(Construct::InlineMath);
            return Some((Mode::Math, construct));
        }
        let ending = self.units().iter().find(|u| u.range.end == b && u.mode == Mode::Text);
        let starting = self.units().iter().find(|u| u.range.start == b && u.mode == Mode::Text);
        let unit = match (ending, starting) {
            (Some(u), _) | (None, Some(u)) => u,
            (None, None) => {
                // Between words: only whitespace/ties to both neighbouring units.
                let prev = self.previous_unit(b)?;
                let next = self.next_unit(b)?;
                let quiet = |s: &str| s.chars().all(|c| c.is_whitespace() || c == '~') && !has_paragraph_break(s);
                if prev.mode != Mode::Text
                    || next.mode != Mode::Text
                    || !quiet(&self.text[prev.range.end..b])
                    || !quiet(&self.text[b..next.range.start])
                {
                    return None;
                }
                prev
            }
        };
        (unit.construct != Construct::Verbatim).then_some((Mode::Text, unit.construct))
    }

    /// Whether `[s, e)` consists only of whole visible units and whitespace/ties.
    fn only_text(&self, s: usize, e: usize) -> bool {
        let mut p = s;
        while p < e {
            if let Some(unit) = self.units().iter().find(|u| u.range.start == p && u.range.end <= e) {
                if unit.construct == Construct::Verbatim {
                    return false;
                }
                p = unit.range.end;
                continue;
            }
            let c = self.text[p..].chars().next().expect("in range");
            if !(c.is_whitespace() || c == '~') {
                return false;
            }
            p += c.len_utf8();
        }
        true
    }

    /// Expand a byte range so it never cuts an atomic unit.
    fn expand(&self, s: usize, e: usize) -> (usize, usize) {
        let s = self.unit_containing(s).map_or(s, |u| u.range.start);
        let e = self.unit_containing(e).map_or(e, |u| u.range.end);
        (s, e)
    }
}

/// Length of the opening (or closing) delimiter of a math construct.
fn delimiter_len(construct: &str, opening: bool) -> usize {
    let begins = ["$$", "\\[", "\\(", "$"];
    if construct.starts_with("\\begin{") {
        let close = construct.find('}').map(|i| i + 1).unwrap_or(0);
        return if opening { close } else { construct.len() - construct.rfind("\\end{").unwrap_or(construct.len()) };
    }
    let ends = ["$$", "\\]", "\\)", "$"];
    let candidates = if opening { &begins } else { &ends };
    candidates
        .iter()
        .find(|d| if opening { construct.starts_with(**d) } else { construct.ends_with(**d) })
        .map_or(0, |d| d.len())
}

fn has_paragraph_break(s: &str) -> bool {
    s.matches('\n').count() >= 2 && s.split('\n').skip(1).any(|line| line.trim().is_empty())
}

/// The control word ending exactly at byte `b`, e.g. `ldots` for `…\ldots|`.
fn control_word_before(text: &str, b: usize) -> Option<&str> {
    let before = &text[..b];
    let letters = before.chars().rev().take_while(|c| c.is_ascii_alphabetic()).count();
    if letters == 0 {
        return None;
    }
    let start = b - letters;
    let backslashes = before[..start].chars().rev().take_while(|&c| c == '\\').count();
    (backslashes % 2 == 1).then(|| &text[start..b])
}

/// `b` splits the letters of a control word (`\al|pha`).
fn ends_inside_control_word(text: &str, b: usize) -> bool {
    text[b..].starts_with(|c: char| c.is_ascii_alphabetic()) && control_word_before(text, b).is_some()
}

fn to16(text: &str, b: usize) -> u32 {
    byte_to_utf16(text, b).unwrap_or(0) as u32
}

impl TexInput {
    fn insert(
        &self,
        cx: &Context<'_>,
        s: usize,
        e: usize,
        typed: &str,
        line_ending: &str,
    ) -> Result<EditPlan, EditRefusal> {
        let (s, e) = cx.expand(s, e);
        if s != e && !cx.only_text(s, e) {
            return Err(EditRefusal::WouldChangeStructure);
        }
        let (mode, construct) = cx.mode_at(s).or_else(|| cx.mode_at(e)).ok_or_else(not_editable)?;
        let mut replacement = String::new();
        for (i, part) in typed.split('\n').enumerate() {
            if i > 0 {
                if mode == Mode::Text && construct == Construct::Prose {
                    replacement.push_str(line_ending);
                    replacement.push_str(line_ending);
                } else {
                    replacement.push(' ');
                }
            }
            replacement.push_str(&latex::escape_insertion(part.trim_end_matches('\r'), mode));
        }
        if replacement.is_empty() {
            return if s == e { Err(EditRefusal::NoChange) } else { self.delete_range(cx, s, e) };
        }
        // Terminate a control word right before the caret.
        if control_word_before(cx.text, s).is_some()
            && replacement.starts_with(|c: char| c.is_ascii_alphabetic() || c.is_whitespace())
        {
            replacement.insert_str(0, if mode == Mode::Math { " " } else { "{}" });
        }
        let start = to16(cx.text, s);
        Ok(EditPlan {
            patches: vec![exposition_core::document::TextPatch::replace(start, to16(cx.text, e), replacement.clone())],
            caret: start + utf16_len(&replacement) as u32,
            mode: edit_mode(mode),
        })
    }

    fn delete_range(&self, cx: &Context<'_>, s: usize, e: usize) -> Result<EditPlan, EditRefusal> {
        let (s, e) = cx.expand(s, e);
        if !cx.only_text(s, e) {
            return Err(EditRefusal::WouldChangeStructure);
        }
        let (mode, _) = cx.mode_at(s).or_else(|| cx.mode_at(e)).ok_or_else(not_editable)?;
        let start = to16(cx.text, s);
        Ok(EditPlan {
            patches: vec![exposition_core::document::TextPatch::delete(start, to16(cx.text, e))],
            caret: start,
            mode: edit_mode(mode),
        })
    }

    /// Delete one rendered character before (`backward`) or after the caret.
    fn delete_one(&self, cx: &Context<'_>, b: usize, backward: bool) -> Result<EditPlan, EditRefusal> {
        if let Some(unit) = cx.unit_containing(b) {
            return self.delete_unit(cx, unit);
        }
        let neighbour = if backward { cx.previous_unit(b) } else { cx.next_unit(b) };
        let Some(unit) = neighbour else { return Err(EditRefusal::NoChange) };
        let gap = if backward { &cx.text[unit.range.end..b] } else { &cx.text[b..unit.range.start] };
        // Whitespace/ties adjacent to the caret render as the space between
        // words: delete them (a blank line becomes a single space, joining
        // the paragraphs). Whitespace after a control word is not rendered.
        let is_space = |c: char| c.is_whitespace() || c == '~';
        let run_len: usize = if backward {
            gap.chars().rev().take_while(|&c| is_space(c)).map(char::len_utf8).sum()
        } else {
            gap.chars().take_while(|&c| is_space(c)).map(char::len_utf8).sum()
        };
        let (rs, re) = if backward { (b - run_len, b) } else { (b, b + run_len) };
        if run_len > 0 && control_word_before(cx.text, rs).is_none() {
            let structural = gap.len() != run_len;
            let run = &cx.text[rs..re];
            if structural && has_paragraph_break(run) {
                return Err(EditRefusal::WouldChangeStructure);
            }
            let (mode, _) = cx.mode_at(b).ok_or_else(not_editable)?;
            if mode == Mode::Math {
                // Spaces are invisible in mathematics: delete the character.
                return self.delete_unit(cx, unit);
            }
            let replacement = if has_paragraph_break(run) { " " } else { "" };
            let start = to16(cx.text, rs);
            return Ok(EditPlan {
                patches: vec![exposition_core::document::TextPatch::replace(start, to16(cx.text, re), replacement)],
                caret: start + replacement.len() as u32,
                mode: EditMode::Markup,
            });
        }
        // Only group braces and inline-math delimiters may lie between the
        // caret and the character; anything else is structure.
        let invisible_ok = gap.chars().all(|c| matches!(c, '{' | '}' | '$') || is_space(c));
        if !invisible_ok || has_paragraph_break(gap) {
            return Err(EditRefusal::WouldChangeStructure);
        }
        self.delete_unit(cx, unit)
    }

    fn delete_unit(&self, cx: &Context<'_>, unit: &SourceUnit) -> Result<EditPlan, EditRefusal> {
        if unit.construct == Construct::Verbatim {
            return Err(not_editable());
        }
        let (s, e) = (unit.range.start, unit.range.end);
        // A delimiter after \left, \right, \big…: removing it breaks the pair.
        let before = cx.text[..s].trim_end();
        if control_word_before(before, before.len()).is_some_and(|w| DELIMITER_COMMANDS.contains(&w)) {
            return Err(EditRefusal::WouldChangeStructure);
        }
        let mode = edit_mode(unit.mode);
        let start = to16(cx.text, s);
        // An unbraced script argument (`^2`, `_i`) keeps an empty group.
        if unit.mode == Mode::Math && cx.text[..s].ends_with(['^', '_']) {
            return Ok(EditPlan {
                patches: vec![exposition_core::document::TextPatch::replace(start, to16(cx.text, e), "{}")],
                caret: start + 1,
                mode,
            });
        }
        Ok(EditPlan {
            patches: vec![exposition_core::document::TextPatch::delete(start, to16(cx.text, e))],
            caret: start,
            mode,
        })
    }
}

fn edit_mode(mode: Mode) -> EditMode {
    match mode {
        Mode::Text => EditMode::Markup,
        Mode::Math => EditMode::Math,
    }
}

fn not_editable() -> EditRefusal {
    EditRefusal::NotEditable { reason: "LaTeX markup or generated content".into() }
}

impl PageInputRules for TexInput {
    fn plan(
        &self,
        text: &str,
        selection: TextRange,
        op: &PageEditOp,
        line_ending: &str,
    ) -> Result<EditPlan, EditRefusal> {
        let (a, b) = (selection.start.min(selection.end), selection.start.max(selection.end));
        let s = utf16_to_byte(text, a as usize).map_err(|_| EditRefusal::InvalidPosition)?;
        let e = utf16_to_byte(text, b as usize).map_err(|_| EditRefusal::InvalidPosition)?;
        let cx = Context::new(text);
        match op {
            PageEditOp::Insert { text: typed } => self.insert(&cx, s, e, typed, line_ending),
            PageEditOp::DeleteBackward | PageEditOp::DeleteForward if s != e => self.delete_range(&cx, s, e),
            PageEditOp::DeleteBackward => self.delete_one(&cx, s, true),
            PageEditOp::DeleteForward => self.delete_one(&cx, s, false),
            PageEditOp::ParagraphBreak => match cx.mode_at(s) {
                Some((Mode::Text, Construct::Prose)) => self.insert(&cx, s, e, "\n", line_ending),
                Some(_) => Err(EditRefusal::WouldChangeStructure),
                None => Err(not_editable()),
            },
        }
    }

    fn mode_at(&self, text: &str, offset: u32) -> Option<EditMode> {
        let b = utf16_to_byte(text, offset as usize).ok()?;
        Context::new(text).mode_at(b).map(|(mode, _)| edit_mode(mode))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "\\documentclass{article}\n\\begin{document}\nThe caf\\'e baseline -- here.\nSee \\emph{bold words} now.\nMath $c^2 + \\alpha$ and \\ldots x.\n\\[ E = mc^2 \\]\nNext para.\n\n\\section{Title}\n\\end{document}\n";

    fn at(needle: &str) -> u32 {
        utf16_len(&DOC[..DOC.find(needle).unwrap_or_else(|| panic!("{needle}"))]) as u32
    }

    fn apply(plan: &EditPlan) -> String {
        let mut text = DOC.to_owned();
        for patch in plan.patches.iter().rev() {
            let s = utf16_to_byte(&text, patch.range.start as usize).unwrap();
            let e = utf16_to_byte(&text, patch.range.end as usize).unwrap();
            text.replace_range(s..e, &patch.text);
        }
        text
    }

    fn run(selection: (u32, u32), op: PageEditOp) -> Result<String, EditRefusal> {
        TexInput.plan(DOC, TextRange::new(selection.0, selection.1), &op, "\n").map(|p| apply(&p))
    }

    fn insert(text: &str) -> PageEditOp {
        PageEditOp::Insert { text: text.into() }
    }

    #[test]
    fn typing_in_prose_and_math_escapes_for_the_mode() {
        let c = at("line --"); // inside "baseline": base|line
        assert!(run((c, c), insert("50% {x}")).unwrap().contains("base50\\% \\{x\\}line"));
        let m = at("+ \\alpha");
        assert!(run((m, m), insert("α")).unwrap().contains("$c^2 \\alpha + \\alpha$"));
        assert_eq!(TexInput.mode_at(DOC, c), Some(EditMode::Markup));
        assert_eq!(TexInput.mode_at(DOC, m), Some(EditMode::Math));
        // A control word before the caret is terminated.
        let after_ldots = at(" x.");
        assert!(run((after_ldots, after_ldots), insert("y")).unwrap().contains("\\ldots{}y x."));
        // No input inside markup.
        assert_eq!(TexInput.mode_at(DOC, at("documentclass")), None);
        assert_eq!(TexInput.mode_at(DOC, at("emph") + 2), None);
        assert!(matches!(run((at("emph"), at("emph")), insert("x")), Err(EditRefusal::NotEditable { .. })));
    }

    #[test]
    fn backspace_deletes_one_rendered_character() {
        // The accent macro as a whole.
        let after_e = at(" baseline");
        assert!(run((after_e, after_e), PageEditOp::DeleteBackward).unwrap().contains("The caf baseline"));
        // TeX ligature input as a whole.
        let after_dash = at(" here.");
        assert!(run((after_dash, after_dash), PageEditOp::DeleteBackward).unwrap().contains("baseline  here."));
        // The space between words.
        let word = at("baseline");
        assert!(run((word, word), PageEditOp::DeleteBackward).unwrap().contains("caf\\'ebaseline"));
        // Inside styled text, braces stay.
        let bold_end = at("} now");
        assert!(run((bold_end, bold_end), PageEditOp::DeleteBackward).unwrap().contains("\\emph{bold word} now"));
        // An unbraced exponent keeps an empty group with the caret inside.
        let after_two = at(" + \\alpha");
        let plan = TexInput.plan(DOC, TextRange::empty(after_two), &PageEditOp::DeleteBackward, "\n").unwrap();
        assert!(apply(&plan).contains("$c^{} + \\alpha$"));
        assert_eq!(plan.caret, after_two);
        // Delete forward of a macro.
        let before_alpha = at("\\alpha$");
        assert!(run((before_alpha, before_alpha), PageEditOp::DeleteForward).unwrap().contains("$c^2 + $"));
    }

    #[test]
    fn structure_is_never_removed() {
        // Selection spanning markup.
        let s = at("See") + 4;
        let e = at("words}") + 6;
        assert_eq!(run((s, e), PageEditOp::DeleteBackward), Err(EditRefusal::WouldChangeStructure));
        // Backspace at the start of a heading would reach into the previous paragraph.
        let title = at("Title");
        assert_eq!(run((title, title), PageEditOp::DeleteBackward), Err(EditRefusal::WouldChangeStructure));
        // Paragraph breaks only in body prose.
        let in_title = title + 2;
        assert_eq!(run((in_title, in_title), PageEditOp::ParagraphBreak), Err(EditRefusal::WouldChangeStructure));
        let in_body = at("para.");
        assert!(run((in_body, in_body), PageEditOp::ParagraphBreak).unwrap().contains("Next \n\npara."));
        // Whole words and spaces can be selected and replaced.
        let (ws, we) = (at("bold words"), at("bold words") + 4);
        assert!(run((ws, we), insert("big")).unwrap().contains("\\emph{big words}"));
        // A selection that cuts an atomic macro grows to cover it.
        let (cs, ce) = (at("\\'e") - 1, at("\\'e") + 1);
        assert!(run((cs, ce), PageEditOp::DeleteBackward).unwrap().contains("The ca baseline"));
    }
}
