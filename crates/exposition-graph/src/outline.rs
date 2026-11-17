//! Document outline: a transient, navigation-only projection of one text.
//!
//! The outline runs the same versioned source extractor the reconciler uses
//! ([`crate::reconcile::extract`]) on a text (usually an open document's live
//! buffer) and arranges what it states literally — headings and recognised
//! declarations (theorems, lemmas, definitions, proofs, Lean declarations) —
//! into a tree in source order. Nothing is stored: outline rows are not graph
//! entities, and nesting is navigation structure, never a dependency.
//!
//! Coverage is reported with the extractor's own words (`partial` reasons),
//! plus heading forms the outline does not list, so an incomplete outline
//! says so instead of implying that absent items do not exist.

use std::collections::HashMap;

use exposition_core::path::WorkspacePath;
use exposition_core::text::TextRange;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::model::CoverageState;
use crate::reconcile::extract::{self, Declaration, Extraction, Heading};
use crate::vocabulary::EntityKind;

/// Items listed per outline (more are reported as partial coverage).
pub const MAX_OUTLINE_ITEMS: usize = 5_000;
/// Longest header text returned per item (characters).
const HEADER_CHARS: usize = 200;

/// The outline of one text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct DocumentOutline {
    pub path: WorkspacePath,
    /// Document revision of the text (set by the caller).
    pub revision: u64,
    /// The text is an unsaved buffer (set by the caller).
    pub unsaved: bool,
    /// Extractor name and version (`latex/1`), when one applies.
    pub extractor: Option<String>,
    /// `complete`, `partial`, `unsupported` (no outline for this kind of
    /// file), or `failed` (too large to read).
    pub coverage: CoverageState,
    /// What was not understood or is not listed, in plain words.
    pub detail: Option<String>,
    /// Top-level items in source order.
    pub items: Vec<OutlineItem>,
    /// Items in the whole tree.
    pub item_count: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum OutlineItemKind {
    /// A part, chapter, section, or Markdown/Typst heading.
    Heading,
    /// A recognised declaration (theorem-like environment, Lean declaration).
    Declaration,
}

/// One outline row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct OutlineItem {
    /// Key within this outline: the construct, its written name, and its
    /// occurrence among items with the same construct and name
    /// (`latex.env:theorem|Main result|0`). Not an identity across edits.
    pub id: String,
    pub kind: OutlineItemKind,
    /// The kind of a declaration (never set for headings).
    pub entity_kind: Option<EntityKind>,
    /// The construct (`latex.heading:section`, `typst.call:lemma`, …).
    pub syntax: String,
    /// What the row is, in words: "Section", "Heading 2", "Theorem".
    pub type_label: String,
    /// Readable title written in the source (heading text, optional
    /// theorem title, Lean name); empty when none is written.
    pub title: String,
    /// The label/anchor written for it (`thm:main`, `<eq:one>`, `#sec:x`).
    pub label: Option<String>,
    /// Heading level (1 outermost) or 0 for declarations.
    pub level: u8,
    /// UTF-16 range of the whole construct (a heading's runs to the next
    /// heading of the same or a higher level).
    pub range: TextRange,
    /// UTF-16 range to select when navigating: the construct's first line.
    pub selection: TextRange,
    /// The text of `selection` (whitespace normalized, at most 200
    /// characters), used to check a navigation target before moving to it.
    pub header: String,
    pub children: Vec<OutlineItem>,
}

/// Formats the outline lists (the others are `unsupported`).
fn outline_extractor(path: &WorkspacePath) -> Option<&'static str> {
    let ext = path.extension()?.to_ascii_lowercase();
    match ext.as_str() {
        "tex" | "ltx" | "typ" | "md" | "markdown" | "lean" => extract::extractor_for(path),
        _ => None,
    }
}

/// The outline of `text`, read as the file `path`. `revision`/`unsaved`
/// describe the text and are returned unchanged.
pub fn document_outline(path: &WorkspacePath, text: &str, revision: u64, unsaved: bool) -> DocumentOutline {
    let mut outline = DocumentOutline {
        path: path.clone(),
        revision,
        unsaved,
        extractor: None,
        coverage: CoverageState::Unsupported,
        detail: None,
        items: Vec::new(),
        item_count: 0,
    };
    let Some(name) = outline_extractor(path) else {
        outline.detail = Some("No outline for this kind of file.".to_owned());
        return outline;
    };
    outline.extractor = Some(name.to_owned());
    if text.len() > extract::MAX_EXTRACT_BYTES {
        outline.coverage = CoverageState::Failed;
        outline.detail = Some(format!(
            "The text is larger than {} MiB and was not read.",
            extract::MAX_EXTRACT_BYTES / (1024 * 1024)
        ));
        return outline;
    }
    let Some(extraction) = extract::extract(path, text) else {
        outline.coverage = CoverageState::Unsupported;
        return outline;
    };
    let (items, dropped) = build(text, &extraction);
    let mut reasons: Vec<String> = extraction.partial.clone();
    reasons.extend(extraction.heading_partial.iter().cloned());
    if dropped > 0 {
        reasons.push(format!("{dropped} items beyond the first {MAX_OUTLINE_ITEMS} are not listed"));
    }
    outline.coverage = if reasons.is_empty() { CoverageState::Complete } else { CoverageState::Partial };
    if !reasons.is_empty() {
        outline.detail = Some(format!("Not interpreted: {}", list(&reasons, 5)));
    }
    outline.item_count = count(&items) as u32;
    outline.items = items;
    outline
}

/// The first `n` reasons joined, with how many more there are.
fn list(reasons: &[String], n: usize) -> String {
    let shown: Vec<&str> = reasons.iter().take(n).map(String::as_str).collect();
    let more = reasons.len().saturating_sub(n);
    if more > 0 { format!("{}; and {more} more", shown.join("; ")) } else { shown.join("; ") }
}

fn count(items: &[OutlineItem]) -> usize {
    items.iter().map(|i| 1 + count(&i.children)).sum()
}

/// A flat entry before nesting.
struct Flat {
    item: OutlineItem,
    /// Headings sort before a declaration that starts at the same offset.
    heading: bool,
}

/// Arrange headings and declarations into a tree; returns the dropped count.
fn build(text: &str, x: &Extraction) -> (Vec<OutlineItem>, usize) {
    let total = crate::reconcile::extract::Utf16Index::new(text).utf16(text.len());
    let mut flat: Vec<Flat> = Vec::new();
    // Heading extents: to the next heading of the same or a higher level.
    for (i, h) in x.headings.iter().enumerate() {
        let end =
            x.headings[i + 1..].iter().find(|n| n.level <= h.level).map_or(total, |n| n.range.start).max(h.range.end);
        flat.push(Flat { item: heading_item(h, end, x.extractor.as_str()), heading: true });
    }
    for d in &x.declarations {
        flat.push(Flat { item: declaration_item(d), heading: false });
    }
    let dropped = flat.len().saturating_sub(MAX_OUTLINE_ITEMS);
    // Source order; at one offset headings first, then the longer range.
    flat.sort_by(|a, b| {
        a.item
            .range
            .start
            .cmp(&b.item.range.start)
            .then(b.heading.cmp(&a.heading))
            .then(b.item.range.end.cmp(&a.item.range.end))
    });
    flat.truncate(MAX_OUTLINE_ITEMS);
    let starts = byte_offsets(text, flat.iter().map(|f| f.item.range.start));
    // Keys: construct + name + occurrence.
    let mut seen: HashMap<String, usize> = HashMap::new();
    for f in &mut flat {
        let (header, units) = header_at(text, &starts, f.item.range.start);
        let start = f.item.selection.start;
        // The first line, within the heading's own syntax or the declaration.
        let limit = if f.heading { f.item.selection.end } else { f.item.range.end };
        let end = start.saturating_add(units).min(limit).max(start);
        f.item.selection = TextRange::new(start, end);
        f.item.header = header;
        let name =
            if f.item.title.is_empty() { f.item.label.clone().unwrap_or_default() } else { f.item.title.clone() };
        let base = format!("{}|{}", f.item.syntax, name);
        let n = seen.entry(base.clone()).or_insert(0);
        f.item.id = format!("{base}|{n}");
        *n += 1;
    }
    (nest(flat.into_iter().map(|f| f.item).collect()), dropped)
}

/// Nest items by range containment (items are in source order).
fn nest(items: Vec<OutlineItem>) -> Vec<OutlineItem> {
    let mut roots: Vec<OutlineItem> = Vec::new();
    let mut stack: Vec<OutlineItem> = Vec::new();
    for item in items {
        while let Some(top) = stack.last() {
            let contains = top.range.start <= item.range.start && item.range.end <= top.range.end;
            if contains {
                break;
            }
            let done = stack.pop().expect("non-empty");
            attach(&mut stack, &mut roots, done);
        }
        stack.push(item);
    }
    while let Some(done) = stack.pop() {
        attach(&mut stack, &mut roots, done);
    }
    roots
}

fn attach(stack: &mut [OutlineItem], roots: &mut Vec<OutlineItem>, item: OutlineItem) {
    match stack.last_mut() {
        Some(parent) => parent.children.push(item),
        None => roots.push(item),
    }
}

fn heading_item(h: &Heading, end: u32, extractor: &str) -> OutlineItem {
    let latex = extractor.starts_with("latex/");
    let type_label = match h.syntax.strip_prefix("latex.heading:") {
        Some(cmd) => capitalize(cmd),
        None => format!("Heading {}", h.level),
    };
    let title = if latex { readable_latex(&h.title) } else { h.title.clone() };
    OutlineItem {
        id: String::new(),
        kind: OutlineItemKind::Heading,
        entity_kind: None,
        syntax: h.syntax.clone(),
        type_label,
        title,
        label: None,
        level: h.level,
        range: TextRange::new(h.range.start, end),
        selection: h.range,
        header: String::new(),
        children: Vec::new(),
    }
}

fn declaration_item(d: &Declaration) -> OutlineItem {
    let latex = d.syntax.starts_with("latex.");
    let lean = d.syntax.starts_with("lean.");
    let type_label = if lean {
        d.attributes.get("lean.kind").map_or_else(|| "Declaration".to_owned(), |k| capitalize(k))
    } else {
        d.kind.title().to_owned()
    };
    let title = match (&d.title, lean) {
        (_, true) => d.name.clone().unwrap_or_default(),
        (Some(t), _) if latex => readable_latex(t),
        (Some(t), _) => t.clone(),
        (None, _) => String::new(),
    };
    OutlineItem {
        id: String::new(),
        kind: OutlineItemKind::Declaration,
        entity_kind: Some(d.kind),
        syntax: d.syntax.clone(),
        type_label,
        title,
        label: if lean { None } else { d.name.clone() },
        level: 0,
        range: d.range,
        selection: d.range,
        header: String::new(),
        children: Vec::new(),
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// A LaTeX title as readers see it, roughly: `\label{…}` removed, command
/// names dropped (their arguments kept), escaped characters unescaped,
/// braces removed. Math between `$` is kept as written (minus braces).
pub fn readable_latex(title: &str) -> String {
    let mut out = String::new();
    let mut chars = title.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let mut name = String::new();
                while let Some(&n) = chars.peek() {
                    if n.is_ascii_alphabetic() {
                        name.push(n);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if name.is_empty() {
                    match chars.next() {
                        Some('\\') | None => out.push(' '),
                        Some(e) => out.push(e),
                    }
                    continue;
                }
                if matches!(name.as_str(), "label" | "index" | "footnote") {
                    // Skip the argument.
                    while chars.peek().is_some_and(|c| c.is_whitespace()) {
                        chars.next();
                    }
                    if chars.peek() == Some(&'{') {
                        let mut depth = 0usize;
                        for n in chars.by_ref() {
                            match n {
                                '{' => depth += 1,
                                '}' => {
                                    depth -= 1;
                                    if depth == 0 {
                                        break;
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                } else if chars.peek().is_some_and(|c| c.is_whitespace()) {
                    out.push(' ');
                }
            }
            '{' | '}' => {}
            '~' => out.push(' '),
            c => out.push(c),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Byte offsets of the given UTF-16 offsets (any order), in one pass.
fn byte_offsets(text: &str, offsets: impl Iterator<Item = u32>) -> HashMap<u32, usize> {
    let mut wanted: Vec<u32> = offsets.collect();
    wanted.sort_unstable();
    wanted.dedup();
    let mut out = HashMap::with_capacity(wanted.len());
    let mut next = wanted.into_iter().peekable();
    let mut units = 0u32;
    for (byte, ch) in text.char_indices() {
        while next.peek().is_some_and(|&w| w <= units) {
            out.insert(next.next().expect("peeked"), byte);
        }
        if next.peek().is_none() {
            return out;
        }
        units += ch.len_utf16() as u32;
    }
    for w in next {
        out.insert(w, text.len());
    }
    out
}

/// The first line of the construct at UTF-16 offset `start`: (normalized
/// text, its length in UTF-16 units up to the line end, bounded).
fn header_at(text: &str, starts: &HashMap<u32, usize>, start: u32) -> (String, u32) {
    let byte = starts.get(&start).copied().unwrap_or(text.len()).min(text.len());
    let rest = &text[byte..];
    let mut line = rest.split('\n').next().unwrap_or("");
    line = line.strip_suffix('\r').unwrap_or(line);
    // Bound the scanned line (long lines stay cheap).
    let mut cut = line.len().min(4 * 1024);
    while !line.is_char_boundary(cut) {
        cut -= 1;
    }
    let line = line[..cut].trim_end();
    let units = line.encode_utf16().count() as u32;
    let normalized: String = line.split_whitespace().collect::<Vec<_>>().join(" ");
    let header: String = normalized.chars().take(HEADER_CHARS).collect();
    (header, units)
}

#[cfg(test)]
mod tests {
    use exposition_core::text::utf16_range_to_bytes;

    use super::*;

    fn wp(s: &str) -> WorkspacePath {
        WorkspacePath::parse(s).unwrap()
    }

    fn slice(text: &str, r: TextRange) -> &str {
        &text[utf16_range_to_bytes(text, r).unwrap()]
    }

    /// (depth, type label, title) of every item, depth first.
    fn rows(items: &[OutlineItem]) -> Vec<(usize, String, String)> {
        fn walk(items: &[OutlineItem], depth: usize, out: &mut Vec<(usize, String, String)>) {
            for i in items {
                out.push((depth, i.type_label.clone(), i.title.clone()));
                walk(&i.children, depth + 1, out);
            }
        }
        let mut out = Vec::new();
        walk(items, 0, &mut out);
        out
    }

    fn r(depth: usize, label: &str, title: &str) -> (usize, String, String) {
        (depth, label.to_owned(), title.to_owned())
    }

    const TEX: &str = r"\documentclass{article}
\newtheorem{theorem}{Theorem}
\begin{document}
\section{Introduction}\label{sec:intro}
Text.
% \section{Commented out}
% \begin{theorem}ghost\end{theorem}
\begin{definition}[Group]\label{def:group}
A group.
\end{definition}
\subsection{On \emph{finite} sets}
\begin{theorem}[Main result]\label{thm:main}
Statement.
\end{theorem}
\begin{proof}
Proof.
\end{proof}
\begin{verbatim}
\section{Not a section}
\end{verbatim}
\section*{Appendix}
\paragraph{Details}
\begin{lemma}
Aux é.
\end{lemma}
\end{document}
\section{After the end}
";

    #[test]
    fn latex_sections_and_declarations_nest_in_source_order() {
        let o = document_outline(&wp("paper/main.tex"), TEX, 7, true);
        assert_eq!(o.revision, 7);
        assert!(o.unsaved);
        assert_eq!(o.extractor.as_deref(), Some("latex/1"));
        assert_eq!(o.coverage, CoverageState::Complete, "{:?}", o.detail);
        assert_eq!(
            rows(&o.items),
            [
                r(0, "Section", "Introduction"),
                r(1, "Definition", "Group"),
                r(1, "Subsection", "On finite sets"),
                r(2, "Theorem", "Main result"),
                r(2, "Proof", ""),
                r(0, "Section", "Appendix"),
                r(1, "Paragraph", "Details"),
                r(2, "Lemma", ""),
            ]
        );
        assert_eq!(o.item_count, 8);
        let intro = &o.items[0];
        assert_eq!(slice(TEX, intro.selection), r"\section{Introduction}");
        assert_eq!(intro.header, r"\section{Introduction}\label{sec:intro}");
        let extent = slice(TEX, intro.range);
        assert!(extent.starts_with(r"\section{Introduction}"), "{extent}");
        assert!(extent.contains(r"\end{verbatim}") && !extent.contains("Appendix"), "{extent}");
        let thm = &intro.children[1].children[0];
        assert_eq!(thm.entity_kind, Some(EntityKind::Theorem));
        assert_eq!(thm.label.as_deref(), Some("thm:main"));
        assert_eq!(thm.header, r"\begin{theorem}[Main result]\label{thm:main}");
        assert_eq!(slice(TEX, thm.selection), thm.header);
        assert!(slice(TEX, thm.range).ends_with(r"\end{theorem}"));
        assert_eq!(thm.id, "latex.env:theorem|Main result|0");
        // Nothing commented out, inside verbatim, or after \end{document}.
        let titles: Vec<_> = rows(&o.items).into_iter().map(|(_, _, t)| t).collect();
        assert!(!titles.iter().any(|t| t.contains("Commented") || t.contains("Not a section") || t.contains("After")));
    }

    #[test]
    fn typst_headings_and_theorem_calls() {
        let text = "#import \"@preview/ctheorems:1.1.3\": *\n#let theorem = thmbox(\"theorem\", \"Theorem\")\n#set heading(numbering: \"1.\")\n= Intro <sec:intro>\n// = Not a heading\n#theorem(\"Main\")[Body] <thm:main>\n== Details\n#proof[By @thm:main.]\n```\n= raw\n```\n= Second\n";
        let o = document_outline(&wp("main.typ"), text, 3, false);
        assert_eq!(o.coverage, CoverageState::Complete, "{:?}", o.detail);
        assert_eq!(
            rows(&o.items),
            [
                r(0, "Heading 1", "Intro"),
                r(1, "Theorem", "Main"),
                r(1, "Heading 2", "Details"),
                r(2, "Proof", ""),
                r(0, "Heading 1", "Second")
            ]
        );
        assert_eq!(slice(text, o.items[0].selection), "= Intro <sec:intro>");
    }

    #[test]
    fn typst_heading_calls_make_the_outline_partial() {
        let o = document_outline(&wp("main.typ"), "= A\n#heading(level: 2)[B]\n", 1, false);
        assert_eq!(o.coverage, CoverageState::Partial);
        assert!(o.detail.unwrap().contains("#heading("));
        assert_eq!(rows(&o.items), [r(0, "Heading 1", "A")]);
    }

    #[test]
    fn markdown_headings_and_fenced_divs() {
        let text = "---\ntitle: x\n---\n# Intro {#sec:intro}\n\n```\n# not a heading\n```\n<!-- ## hidden -->\n::: {.theorem #thm:a title=\"Main\"}\nBody\n:::\n\nSetext\n------\n\n::: lemma\nAux\n:::\n";
        let o = document_outline(&wp("notes.md"), text, 0, false);
        assert_eq!(o.coverage, CoverageState::Complete, "{:?}", o.detail);
        assert_eq!(
            rows(&o.items),
            [r(0, "Heading 1", "Intro"), r(1, "Theorem", "Main"), r(1, "Heading 2", "Setext"), r(2, "Lemma", "")]
        );
        let setext = &o.items[0].children[1];
        assert_eq!(slice(text, setext.selection), "Setext");
    }

    #[test]
    fn unsupported_constructs_are_reported_not_hidden() {
        let text = "\\section{A}\n\\begin{mytheorem}\nx\n\\end{mytheorem}\n\\newcommand{\\thm}{\\begin{theorem}}\n";
        let o = document_outline(&wp("a.tex"), text, 0, false);
        assert_eq!(o.coverage, CoverageState::Partial);
        let detail = o.detail.unwrap();
        assert!(detail.starts_with("Not interpreted: "), "{detail}");
        assert!(detail.contains("mytheorem"), "{detail}");
    }

    #[test]
    fn other_files_are_unsupported_and_large_texts_fail() {
        let o = document_outline(&wp("data.csv"), "a,b\n", 0, false);
        assert_eq!(o.coverage, CoverageState::Unsupported);
        assert!(o.items.is_empty());
        let o = document_outline(&wp("refs.bib"), "@book{a, title={x}}", 0, false);
        assert_eq!(o.coverage, CoverageState::Unsupported);
        let big = "a".repeat(extract::MAX_EXTRACT_BYTES + 1);
        let o = document_outline(&wp("big.typ"), &big, 0, false);
        assert_eq!(o.coverage, CoverageState::Failed);
    }

    #[test]
    fn edits_change_the_outline_of_the_unsaved_text() {
        let saved = "= A\n#lemma[x]\n";
        let buffer = "= A renamed\n= B\n#lemma[x]\n";
        let before = document_outline(&wp("m.typ"), saved, 1, false);
        let after = document_outline(&wp("m.typ"), buffer, 2, true);
        assert_eq!(rows(&before.items), [r(0, "Heading 1", "A"), r(1, "Lemma", "")]);
        assert_eq!(rows(&after.items), [r(0, "Heading 1", "A renamed"), r(0, "Heading 1", "B"), r(1, "Lemma", "")]);
        assert_eq!(after.revision, 2);
        assert!(after.unsaved);
    }

    #[test]
    fn ids_count_occurrences_and_ranges_use_utf16() {
        let text = "= é😀\n#lemma[a]\n#lemma[b]\n";
        let o = document_outline(&wp("m.typ"), text, 0, false);
        let lemmas = &o.items[0].children;
        assert_eq!(lemmas[0].id, "typst.call:lemma||0");
        assert_eq!(lemmas[1].id, "typst.call:lemma||1");
        assert_eq!(slice(text, lemmas[1].selection), "#lemma[b]");
        assert_eq!(slice(text, o.items[0].selection), "= é😀");
    }

    #[test]
    fn lean_declarations_are_listed_by_name() {
        let text = "namespace Paper\ntheorem gauss (n : Nat) : True := trivial\ndef f := 1\nend Paper\n";
        let o = document_outline(&wp("Paper/Gauss.lean"), text, 0, false);
        let titles: Vec<_> = rows(&o.items).into_iter().map(|(_, _, t)| t).collect();
        assert!(titles.iter().any(|t| t.ends_with("gauss")), "{titles:?}");
        assert!(o.items.iter().all(|i| i.kind == OutlineItemKind::Declaration));
    }

    #[test]
    fn readable_latex_titles() {
        assert_eq!(readable_latex(r"On \emph{finite} groups\label{x}"), "On finite groups");
        assert_eq!(readable_latex(r"A \& B~C"), "A & B C");
        assert_eq!(readable_latex(r"$L^{2}$ bounds"), "$L^2$ bounds");
    }

    #[test]
    fn garbage_never_panics() {
        for text in ["\\section{", "= ", "#heading(", "::: {", "\\begin{theorem}\\section{x}\\end{theorem}\\section{y}"]
        {
            for p in ["a.tex", "a.typ", "a.md"] {
                let o = document_outline(&wp(p), text, 0, false);
                for item in &o.items {
                    utf16_range_to_bytes(text, item.range).unwrap();
                    utf16_range_to_bytes(text, item.selection).unwrap();
                }
            }
        }
    }
}
