//! Removing a declared item from its source text ("Delete from Source").
//!
//! A theorem environment, a Lean declaration, a BibTeX entry … exists because
//! its source says so: deleting it from the graph alone would bring it back
//! at the next reindex. Deleting it therefore removes its text from the
//! source — as an unsaved, undoable edit the person reviews and saves (the
//! application applies it through the document service).
//!
//! This module is the pure part: it locates the item in the CURRENT text (an
//! open buffer may differ from what the graph last read), computes the whole
//! block to remove, and finds literal mentions of a Lean declaration's name.
//! It never guesses: an item that cannot be located exactly is not removed.

use exposition_core::path::WorkspacePath;
use exposition_core::text::{LineIndex, TextRange, byte_range_to_utf16, utf16_range_to_bytes};
use sha2::{Digest, Sha256};

use crate::model::{SourceBinding, SourceSelector};
use crate::reconcile::extract::{self, Declaration, Extraction};
use crate::reconcile::matching::{decode_fingerprint, encode_fingerprint};

/// Why an item could not be located in the current text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotLocated {
    /// The binding does not name a declaration (a whole file, a region).
    NotDeclared,
    /// No extractor reads this kind of file.
    NoExtractor,
    /// The declaration is no longer in the text.
    Missing,
    /// Its name is declared more than once.
    Ambiguous,
    /// An unnamed construct whose text changed since the graph read it.
    Changed,
}

/// SHA-256 (hex) of the exact UTF-8 text, as recorded in `SourceVersion`.
pub fn content_hash(text: &str) -> String {
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// Locate the declaration `binding` refers to in `text` (the source's current
/// text). Named declarations are found by syntax and name; unnamed ones (a
/// proof) by their recorded fingerprint, or by their recorded range when the
/// text is exactly the one the graph read.
pub fn locate(path: &WorkspacePath, text: &str, binding: &SourceBinding) -> Result<Declaration, NotLocated> {
    let SourceSelector::Declaration { syntax, name, .. } = &binding.selector else {
        return Err(NotLocated::NotDeclared);
    };
    let extraction = extract::extract(path, text).ok_or(NotLocated::NoExtractor)?;
    let candidates = extraction.declarations.iter().filter(|d| &d.syntax == syntax && d.range.end > d.range.start);
    if let Some(name) = name {
        let mut named = candidates.filter(|d| d.name.as_deref() == Some(name.as_str()));
        return match (named.next(), named.next()) {
            (Some(d), None) => Ok(d.clone()),
            (Some(_), Some(_)) => Err(NotLocated::Ambiguous),
            (None, _) => Err(NotLocated::Missing),
        };
    }
    let unnamed: Vec<&Declaration> = candidates.filter(|d| d.name.is_none()).collect();
    if let Some(fp) = &binding.fingerprint {
        // The same construct in the same place (header, context, body)…
        let mut same = unnamed.iter().filter(|d| &encode_fingerprint(d) == fp);
        if let (Some(d), None) = (same.next(), same.next()) {
            return Ok((*d).clone());
        }
        // …or the only construct with exactly its text (edits nearby changed
        // its surroundings, not the construct).
        if let Some((_, body, _)) = decode_fingerprint(fp) {
            let mut same = unnamed.iter().filter(|d| d.body_fingerprint == body);
            if let (Some(d), None) = (same.next(), same.next()) {
                return Ok((*d).clone());
            }
        }
    }
    let unchanged = binding.version.as_ref().is_some_and(|v| v.content_hash == content_hash(text));
    match binding.range {
        Some(range) if unchanged => unnamed.into_iter().find(|d| d.range == range).cloned().ok_or(NotLocated::Missing),
        _ => Err(NotLocated::Changed),
    }
}

fn blank(s: &str) -> bool {
    s.chars().all(|c| c == ' ' || c == '\t' || c == '\r')
}

/// The text to remove for a construct at `construct` (UTF-16): the construct
/// itself; when it stands on lines of its own, those whole lines, plus one
/// blank line when it was separated by blank lines on both sides (so no
/// double gap is left behind).
pub fn removal_range(text: &str, construct: TextRange) -> Option<TextRange> {
    let bytes = utf16_range_to_bytes(text, construct).ok()?;
    let (s, e) = (bytes.start, bytes.end);
    let line_start = text[..s].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[e..].find('\n').map_or(text.len(), |i| e + i);
    if !(blank(&text[line_start..s]) && blank(&text[e..line_end])) {
        return byte_range_to_utf16(text, s..e).ok();
    }
    let mut end = if line_end < text.len() { line_end + 1 } else { line_end };
    let mut start = line_start;
    // Blank line before (or the start of the text) and a blank line after:
    // take the one after too.
    let before_blank = start == 0 || {
        let prev_end = start - 1; // the '\n' ending the previous line
        let prev_start = text[..prev_end].rfind('\n').map_or(0, |i| i + 1);
        blank(&text[prev_start..prev_end])
    };
    if before_blank && end < text.len() {
        let next_end = text[end..].find('\n').map_or(text.len(), |i| end + i);
        if blank(&text[end..next_end]) {
            end = if next_end < text.len() { next_end + 1 } else { next_end };
        }
    } else if end == text.len() && start > 0 && before_blank {
        // Last block of the text: drop the blank line before it instead.
        let prev_end = start - 1;
        start = text[..prev_end].rfind('\n').map_or(0, |i| i + 1);
    }
    byte_range_to_utf16(text, start..end).ok()
}

/// The ranges to remove for an item (sorted, merged) and its 1-based first
/// and last line.
pub type ItemRemoval = (Vec<TextRange>, Option<(usize, usize)>);

/// What removing `declaration` takes out of `text`: the construct (whole
/// lines when it stands alone, see [`removal_range`]) and the text outside
/// it that belongs to it. A Typst element in markup is labelled by the label
/// that FOLLOWS it (`#theorem[…] <thm:x>`); left behind, that label would
/// attach to the preceding element and references to the removed statement
/// would silently resolve to another one. So the label goes with it: in one
/// range when only whitespace separates them, else as a second range (a
/// comment between them stays). LaTeX and Markdown labels of a construct are
/// inside it (`\begin{theorem}\label{…}`, `::: {#id .theorem}`); a LaTeX
/// `\label` after `\end{theorem}` names the enclosing counter, not the
/// theorem, so it keeps its meaning and stays. Returns `(ranges, lines)`:
/// ranges sorted and merged, lines the 1-based first and last line of what
/// belongs to the item.
pub fn removal_ranges(text: &str, declaration: &Declaration) -> Option<ItemRemoval> {
    let construct = declaration.range;
    let label = typst_attached_label(text, declaration)?;
    let ranges = match label {
        None => vec![removal_range(text, construct)?],
        Some(label) => {
            let gap = utf16_range_to_bytes(text, TextRange::new(construct.end, label.start)).ok()?;
            if text[gap].chars().all(char::is_whitespace) {
                vec![removal_range(text, TextRange::new(construct.start, label.end))?]
            } else {
                merge(vec![removal_range(text, construct)?, removal_range(text, label)?])
            }
        }
    };
    let whole = TextRange::new(construct.start, label.map_or(construct.end, |l| l.end));
    Some((ranges, lines_of(text, whole)))
}

/// The label a Typst markup call (`#theorem[…]`) carries after it, if any.
/// `None` when the text cannot be measured.
fn typst_attached_label(text: &str, declaration: &Declaration) -> Option<Option<TextRange>> {
    let bytes = utf16_range_to_bytes(text, declaration.range).ok()?;
    if !declaration.syntax.starts_with("typst.") || !text[bytes.clone()].starts_with('#') {
        return Some(None);
    }
    Some(extract::typst::attached_label(text, bytes.end).and_then(|label| byte_range_to_utf16(text, label).ok()))
}

/// Sort and merge overlapping or touching ranges (a proof removed with its
/// theorem, an item inside another removed item).
pub fn merge(mut ranges: Vec<TextRange>) -> Vec<TextRange> {
    ranges.sort_by_key(|r| (r.start, r.end));
    let mut out: Vec<TextRange> = Vec::with_capacity(ranges.len());
    for r in ranges {
        match out.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => out.push(r),
        }
    }
    out
}

/// 1-based first and last line of `range` in `text`.
pub fn lines_of(text: &str, range: TextRange) -> Option<(usize, usize)> {
    let index = LineIndex::new(text);
    let first = index.line_column(range.start as usize).ok()?.line;
    let last_offset = if range.end > range.start { range.end as usize - 1 } else { range.start as usize };
    let last = index.line_column(last_offset).ok()?.line;
    Some((first as usize + 1, last as usize + 1))
}

/// A literal mention of a Lean name outside the removed text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mention {
    pub range: TextRange,
    /// 1-based line.
    pub line: usize,
    /// The declaration the mention is in, when it is in one.
    pub declaration: Option<String>,
}

/// Mentions per file are capped (the decision lists names, not every use).
pub const MAX_MENTIONS: usize = 50;

fn id_first(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn id_rest(c: char) -> bool {
    id_first(c)
        || c.is_ascii_digit()
        || matches!(c, '\'' | '!' | '?')
        || ('\u{2080}'..='\u{209c}').contains(&c)
        || ('\u{1d62}'..='\u{1d6a}').contains(&c)
        || c == '\u{2c7c}'
}

/// Whether the dotted identifier `token` names `full` (a suffix of its
/// components, as written inside its namespace or after `open`), possibly
/// followed by more components (`basel.symm`, generalized field notation).
fn names(token: &str, full: &[&str]) -> bool {
    let mut parts: Vec<&str> = token.split('.').collect();
    if parts.first() == Some(&"_root_") {
        parts.remove(0);
    }
    (1..=full.len().min(parts.len())).any(|k| parts[..k] == full[full.len() - k..])
}

/// Literal uses of the Lean declaration `full_name` in `text` (comments and
/// strings excluded), outside `exclude` (the removed text). This is a
/// mention, not a resolved dependency: `open` and local names are not
/// interpreted, so a person reviews the list.
pub fn lean_mentions(path: &WorkspacePath, text: &str, full_name: &str, exclude: &[TextRange]) -> Vec<Mention> {
    let full: Vec<&str> = full_name.split('.').filter(|p| !p.is_empty()).collect();
    let Some(last) = full.last() else { return Vec::new() };
    if !text.contains(last) {
        return Vec::new();
    }
    let (masked, _, _) = extract::lean::mask(text);
    let extraction: Option<Extraction> = extract::extract(path, text);
    let index = LineIndex::new(text);
    let mut out = Vec::new();
    let mut chars = masked.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if !id_first(c) {
            continue;
        }
        // The character before must not continue an identifier (a token starts here).
        if masked[..i].chars().next_back().is_some_and(|p| id_rest(p) || p == '.') {
            continue;
        }
        let mut end = i + c.len_utf8();
        while let Some(&(j, d)) = chars.peek() {
            let dot_then_id = d == '.' && masked[j + 1..].chars().next().is_some_and(id_first);
            if id_rest(d) || dot_then_id {
                end = j + d.len_utf8();
                chars.next();
            } else {
                break;
            }
        }
        let token = &masked[i..end];
        if !token.contains(last) || !names(token, &full) {
            continue;
        }
        let Ok(range) = byte_range_to_utf16(text, i..end) else { continue };
        if exclude.iter().any(|x| x.start <= range.start && range.end <= x.end) {
            continue;
        }
        let line = index.line_column(range.start as usize).map_or(0, |p| p.line as usize + 1);
        let declaration = extraction.as_ref().and_then(|x| {
            x.declarations
                .iter()
                .filter(|d| d.range.start <= range.start && range.end <= d.range.end)
                .min_by_key(|d| d.range.end - d.range.start)
                .and_then(|d| d.name.clone())
        });
        out.push(Mention { range, line, declaration });
        if out.len() >= MAX_MENTIONS {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{BindingState, SourceVersion};
    use crate::reconcile::extract::extract;

    fn wp(s: &str) -> WorkspacePath {
        WorkspacePath::parse(s).unwrap()
    }

    fn binding_for(path: &str, text: &str, syntax: &str, name: Option<&str>) -> SourceBinding {
        let x = extract(&wp(path), text).unwrap();
        let d = x
            .declarations
            .iter()
            .find(|d| d.syntax == syntax && d.name.as_deref() == name)
            .unwrap_or_else(|| panic!("{syntax} {name:?} in {:?}", x.declarations));
        SourceBinding {
            entity: crate::ids::EntityId::mint(),
            path: wp(path),
            selector: SourceSelector::Declaration { syntax: syntax.into(), name: name.map(Into::into), ordinal: None },
            range: Some(d.range),
            version: Some(SourceVersion { content_hash: content_hash(text), revision: None, saved: true }),
            fingerprint: Some(encode_fingerprint(d)),
            state: BindingState::Resolved,
            detail: None,
        }
    }

    fn cut(text: &str, range: TextRange) -> String {
        let b = utf16_range_to_bytes(text, range).unwrap();
        format!("{}{}", &text[..b.start], &text[b.end..])
    }

    const PAPER: &str = "\\begin{document}\n\n\\begin{lemma}\\label{lem:closed}\nA closed subset is compact.\n\\end{lemma}\n\n\\begin{theorem}\\label{thm:main}\nThe image is compact.\n\\end{theorem}\n\n\\begin{proof}\nSee Lemma~\\ref{lem:closed}.\n\\end{proof}\n\n\\end{document}\n";

    #[test]
    fn a_named_theorem_is_removed_with_its_lines_and_one_separating_blank_line() {
        let b = binding_for("paper.tex", PAPER, "latex.env:theorem", Some("thm:main"));
        let d = locate(&wp("paper.tex"), PAPER, &b).unwrap();
        let removal = removal_range(PAPER, d.range).unwrap();
        assert_eq!(
            cut(PAPER, removal),
            "\\begin{document}\n\n\\begin{lemma}\\label{lem:closed}\nA closed subset is compact.\n\\end{lemma}\n\n\\begin{proof}\nSee Lemma~\\ref{lem:closed}.\n\\end{proof}\n\n\\end{document}\n"
        );
        assert_eq!(lines_of(PAPER, d.range), Some((7, 9)));
    }

    #[test]
    fn a_theorem_and_its_proof_merge_into_one_clean_removal() {
        let thm = binding_for("paper.tex", PAPER, "latex.env:theorem", Some("thm:main"));
        let proof = binding_for("paper.tex", PAPER, "latex.env:proof", None);
        let a = removal_range(PAPER, locate(&wp("paper.tex"), PAPER, &thm).unwrap().range).unwrap();
        let b = removal_range(PAPER, locate(&wp("paper.tex"), PAPER, &proof).unwrap().range).unwrap();
        let merged = merge(vec![b, a]);
        assert_eq!(merged.len(), 1, "adjacent removals merge: {merged:?}");
        assert_eq!(
            cut(PAPER, merged[0]),
            "\\begin{document}\n\n\\begin{lemma}\\label{lem:closed}\nA closed subset is compact.\n\\end{lemma}\n\n\\end{document}\n"
        );
    }

    #[test]
    fn the_item_is_found_again_after_unsaved_edits_above_it() {
        let b = binding_for("paper.tex", PAPER, "latex.env:theorem", Some("thm:main"));
        let edited = PAPER.replace("\\begin{document}\n", "\\begin{document}\n% a new comment line\n");
        let d = locate(&wp("paper.tex"), &edited, &b).unwrap();
        let text = &edited[utf16_range_to_bytes(&edited, d.range).unwrap()];
        assert!(text.starts_with("\\begin{theorem}") && text.ends_with("\\end{theorem}"), "{text}");
        // An unnamed proof is re-found by its fingerprint after edits elsewhere.
        let p = binding_for("paper.tex", PAPER, "latex.env:proof", None);
        let found = locate(&wp("paper.tex"), &edited, &p).unwrap();
        assert!(edited[utf16_range_to_bytes(&edited, found.range).unwrap()].starts_with("\\begin{proof}"));
    }

    #[test]
    fn an_edited_unnamed_proof_or_a_missing_or_duplicated_name_is_not_guessed() {
        let p = binding_for("paper.tex", PAPER, "latex.env:proof", None);
        let edited = PAPER.replace("See Lemma", "By Lemma");
        assert_eq!(locate(&wp("paper.tex"), &edited, &p).unwrap_err(), NotLocated::Changed);
        let t = binding_for("paper.tex", PAPER, "latex.env:theorem", Some("thm:main"));
        let gone = PAPER.replace("\\label{thm:main}", "");
        assert_eq!(locate(&wp("paper.tex"), &gone, &t).unwrap_err(), NotLocated::Missing);
        let twice = PAPER.replace("\\label{lem:closed}", "\\label{thm:main}");
        let t2 = SourceBinding {
            selector: SourceSelector::Declaration {
                syntax: "latex.env:theorem".into(),
                name: Some("thm:main".into()),
                ordinal: None,
            },
            ..t.clone()
        };
        // A lemma with the same label is another syntax: still unique among theorems.
        assert!(locate(&wp("paper.tex"), &twice, &t2).is_ok());
        // Two proofs with the same text and the file edited: neither is guessed.
        let doubled = PAPER
            .replace("\\end{document}", "\\begin{proof}\nSee Lemma~\\ref{lem:closed}.\n\\end{proof}\n\\end{document}");
        let edited_doubled = format!("% new\n{doubled}");
        assert_eq!(locate(&wp("paper.tex"), &edited_doubled, &p).unwrap_err(), NotLocated::Changed);
        let whole = SourceBinding { selector: SourceSelector::WholeFile, ..t };
        assert_eq!(locate(&wp("paper.tex"), PAPER, &whole).unwrap_err(), NotLocated::NotDeclared);
        assert_eq!(locate(&wp("data.csv"), "a,b\n", &p).unwrap_err(), NotLocated::NoExtractor);
    }

    #[test]
    fn an_inline_construct_is_removed_exactly_and_a_last_block_takes_the_gap_before_it() {
        let inline = "Text \\begin{claim}\\label{c:x}C.\\end{claim} more text.\n";
        let b = binding_for("a.tex", inline, "latex.env:claim", Some("c:x"));
        let r = removal_range(inline, locate(&wp("a.tex"), inline, &b).unwrap().range).unwrap();
        assert_eq!(cut(inline, r), "Text  more text.\n");
        let last = "\\begin{lemma}\\label{a}A.\\end{lemma}\n\n\\begin{lemma}\\label{b}B.\\end{lemma}";
        let b = binding_for("a.tex", last, "latex.env:lemma", Some("b"));
        let r = removal_range(last, locate(&wp("a.tex"), last, &b).unwrap().range).unwrap();
        assert_eq!(cut(last, r), "\\begin{lemma}\\label{a}A.\\end{lemma}\n");
    }

    #[test]
    fn crlf_and_non_ascii_text_keep_utf16_offsets_right() {
        let text = "% Größe ∑\r\n\\begin{lemma}\\label{l:ü}Für alle ε.\r\n\\end{lemma}\r\n\r\nTail\r\n";
        let b = binding_for("a.tex", text, "latex.env:lemma", Some("l:ü"));
        let r = removal_range(text, locate(&wp("a.tex"), text, &b).unwrap().range).unwrap();
        // The lemma's lines go; the gap before Tail stays (the line above it is not blank).
        assert_eq!(cut(text, r), "% Größe ∑\r\n\r\nTail\r\n");
        let spaced = "% Größe ∑\r\n\r\n\\begin{lemma}\\label{l:ü}Für alle ε.\r\n\\end{lemma}\r\n\r\nTail\r\n";
        let b = binding_for("a.tex", spaced, "latex.env:lemma", Some("l:ü"));
        let r = removal_range(spaced, locate(&wp("a.tex"), spaced, &b).unwrap().range).unwrap();
        assert_eq!(cut(spaced, r), "% Größe ∑\r\n\r\nTail\r\n", "one separating blank line is taken with it");
    }

    const BIB: &str =
        "@book{rudin,\n  title = {Principles},\n  year = {1976}\n}\n\n@article{euler,\n  title = {Sums}\n}\n";

    #[test]
    fn a_bibtex_entry_is_removed_whole() {
        let b = binding_for("refs.bib", BIB, "bibtex.entry", Some("rudin"));
        let r = removal_range(BIB, locate(&wp("refs.bib"), BIB, &b).unwrap().range).unwrap();
        assert_eq!(cut(BIB, r), "@article{euler,\n  title = {Sums}\n}\n");
    }

    const LEAN: &str = "namespace LeanBasic\n\n/-- The Basel sum. -/\n@[simp]\ntheorem basel : True := by\n  trivial\n\ntheorem uses_basel : True := basel\n\ntheorem field : True := basel.elim\n\n-- basel in a comment\ndef s := \"basel\"\n\ntheorem other : True := Foo.basel\n\nend LeanBasic\n\n#check LeanBasic.basel\n";

    #[test]
    fn a_lean_declaration_is_removed_with_its_doc_comment_and_attributes() {
        let b = binding_for("B.lean", LEAN, "lean.theorem", Some("LeanBasic.basel"));
        let d = locate(&wp("B.lean"), LEAN, &b).unwrap();
        let r = removal_range(LEAN, d.range).unwrap();
        let after = cut(LEAN, r);
        assert!(after.starts_with("namespace LeanBasic\n\ntheorem uses_basel"), "{after}");
        assert!(!after.contains("The Basel sum") && !after.contains("@[simp]"), "{after}");
    }

    #[test]
    fn lean_mentions_are_literal_uses_outside_comments_strings_and_the_removed_text() {
        let b = binding_for("B.lean", LEAN, "lean.theorem", Some("LeanBasic.basel"));
        let d = locate(&wp("B.lean"), LEAN, &b).unwrap();
        let removal = removal_range(LEAN, d.range).unwrap();
        let mentions = lean_mentions(&wp("B.lean"), LEAN, "LeanBasic.basel", &[removal]);
        let found: Vec<(Option<&str>, usize)> = mentions.iter().map(|m| (m.declaration.as_deref(), m.line)).collect();
        assert_eq!(
            found,
            vec![(Some("LeanBasic.uses_basel"), 8), (Some("LeanBasic.field"), 10), (None, 19)],
            "the header, the comment, the string and Foo.basel are not uses of LeanBasic.basel"
        );
    }

    /// Remove the declaration `syntax`/`name` of `text` the way Delete from Source does.
    fn remove(path: &str, text: &str, syntax: &str, name: Option<&str>) -> (String, Option<(usize, usize)>) {
        let b = binding_for(path, text, syntax, name);
        let d = locate(&wp(path), text, &b).unwrap();
        let (ranges, lines) = removal_ranges(text, &d).unwrap();
        let mut out = text.to_owned();
        for r in ranges.iter().rev() {
            out = cut(&out, *r);
        }
        (out, lines)
    }

    // XR-D-04: the review's fixture (Typst numbered theorem figures).
    const TYPST_REF: &str = "#set page(width: 150mm, height: 130mm, margin: 15mm)\n#let theorem(body) = figure(body, kind: \"theorem\", supplement: [Theorem], numbering: \"1\")\n#theorem[First surviving statement.] <keep-theorem>\n#theorem[Second target statement.] <delete-theorem>\n\nReference to target: @delete-theorem.\n";

    #[test]
    fn a_typst_theorem_is_removed_with_the_label_that_follows_it() {
        let (after, lines) = remove("d.typ", TYPST_REF, "typst.call:theorem", Some("delete-theorem"));
        assert_eq!(
            after,
            "#set page(width: 150mm, height: 130mm, margin: 15mm)\n#let theorem(body) = figure(body, kind: \"theorem\", supplement: [Theorem], numbering: \"1\")\n#theorem[First surviving statement.] <keep-theorem>\n\nReference to target: @delete-theorem.\n"
        );
        assert_eq!(lines, Some((4, 4)));
        // The reference cannot rebind: the label is defined nowhere, the
        // surviving theorem keeps its own name.
        let x = extract(&wp("d.typ"), &after).unwrap();
        assert!(!x.labels.contains(&"delete-theorem".to_owned()), "{:?}", x.labels);
        let names: Vec<&str> = x.declarations.iter().filter_map(|d| d.name.as_deref()).collect();
        assert_eq!(names, ["keep-theorem"], "{:?}", x.declarations);
    }

    #[test]
    fn a_typst_label_on_a_following_line_or_after_a_comment_goes_too() {
        let own_line = "#theorem[A.] <a>\n\n#theorem[B.]\n<b>\n\nSee @b.\n";
        let (after, lines) = remove("d.typ", own_line, "typst.call:theorem", Some("b"));
        assert_eq!(after, "#theorem[A.] <a>\n\nSee @b.\n");
        assert_eq!(lines, Some((3, 4)), "the preview names the label's line too");
        // A comment between the call and its label stays; the label goes.
        let commented = "#theorem[A.] <a>\n#theorem[B.] // note\n<b>\nSee @b.\n";
        let (after, _) = remove("d.typ", commented, "typst.call:theorem", Some("b"));
        assert_eq!(after, "#theorem[A.] <a>\n // note\nSee @b.\n");
        assert!(!extract(&wp("d.typ"), &after).unwrap().labels.contains(&"b".to_owned()));
        // Inline: exactly the call and its label.
        let inline = "Text #theorem[B.] <b> more.\n";
        let (after, _) = remove("d.typ", inline, "typst.call:theorem", Some("b"));
        assert_eq!(after, "Text  more.\n");
        // An unlabelled call takes no following text; a later label after
        // other content is not its label.
        let unlabelled = "#theorem[A.] <a>\n#theorem[B.]\nWords <w>\n";
        let x = extract(&wp("d.typ"), unlabelled).unwrap();
        let d = x.declarations.iter().find(|d| d.name.is_none()).unwrap().clone();
        let (ranges, _) = removal_ranges(unlabelled, &d).unwrap();
        assert_eq!(cut(unlabelled, ranges[0]), "#theorem[A.] <a>\nWords <w>\n");
    }

    #[test]
    fn latex_and_markdown_labels_are_inside_the_construct_and_a_label_after_end_stays() {
        // `\label` after `\end{theorem}` refers to the enclosing counter (the
        // section), before and after the removal: it is not the theorem's.
        let latex = "\\section{S}\\label{sec:s}\n\\begin{theorem}\\label{thm:a}\nA.\n\\end{theorem}\n\\label{after}\nSee \\ref{thm:a}.\n";
        let (after, _) = remove("a.tex", latex, "latex.env:theorem", Some("thm:a"));
        assert_eq!(after, "\\section{S}\\label{sec:s}\n\\label{after}\nSee \\ref{thm:a}.\n");
        let md = "Intro.\n\n::: {#thm:a .theorem}\nA.\n:::\n\nSee @thm:a.\n";
        let (after, _) = remove("a.md", md, "markdown.div:theorem", Some("thm:a"));
        assert_eq!(after, "Intro.\n\nSee @thm:a.\n");
        assert!(!extract(&wp("a.md"), &after).unwrap().labels.contains(&"thm:a".to_owned()));
    }

    #[test]
    fn merge_keeps_disjoint_ranges_apart() {
        let m =
            merge(vec![TextRange::new(10, 20), TextRange::new(0, 5), TextRange::new(15, 30), TextRange::new(30, 31)]);
        assert_eq!(m, vec![TextRange::new(0, 5), TextRange::new(10, 31)]);
    }
}
