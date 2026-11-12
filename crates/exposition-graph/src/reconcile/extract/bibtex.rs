//! BibTeX extractor (`bibtex/1`).
//!
//! Every `@type{key, field = value, …}` entry (`{}` or `()` delimiters,
//! nested braces, quoted strings, `#` concatenation) becomes a `Reference`
//! declaration named by its key. `@string`, `@preamble`, and `@comment` are
//! skipped; text outside entries is a comment. Duplicate keys are all
//! returned and reported; a malformed entry is reported and skipped.

use std::collections::{BTreeMap, HashSet};
use std::ops::Range;

use exposition_core::path::WorkspacePath;

use super::Extraction;
use super::latex::common::{Budget, Builder, DeclSpec, normalize};
use crate::vocabulary::EntityKind;

pub const EXTRACTOR: &str = "bibtex/1";

/// Largest entry read (larger ones are reported as malformed).
const MAX_ENTRY: usize = 1 << 20;

/// Commands dropped (with their braces) from titles.
const FORMATTING: &[&str] = &[
    "emph",
    "textit",
    "textbf",
    "textsc",
    "textrm",
    "textsf",
    "texttt",
    "textup",
    "textsl",
    "textnormal",
    "mathrm",
    "mathbf",
    "mathit",
    "mathcal",
    "mathbb",
    "mathsf",
    "mathtt",
    "ensuremath",
    "mbox",
    "hbox",
    "text",
    "bf",
    "it",
    "em",
    "sc",
    "rm",
    "sf",
    "tt",
    "sl",
    "relax",
    "protect",
    "NoCaseChange",
    "MakeUppercase",
    "MakeLowercase",
    "uppercase",
    "lowercase",
];

pub fn extract(_path: &WorkspacePath, text: &str) -> Extraction {
    let mut out = Builder::new(EXTRACTOR, text);
    let mut budget = Budget::new(text.len());
    let mut keys: HashSet<String> = HashSet::new();
    let b = text.as_bytes();
    let mut i = 0;
    while let Some(k) = b.get(i..).and_then(|r| r.iter().position(|&c| c == b'@')) {
        let at = i + k;
        let limit = at.saturating_add(budget.allow(MAX_ENTRY)).min(b.len());
        let parsed = parse(text, at, limit);
        match parsed {
            Parsed::NotEntry => i = at + 1,
            Parsed::Skipped(end) => {
                budget.spend(end - at);
                i = end;
            }
            Parsed::Malformed { key, scanned } => {
                budget.spend(scanned - at);
                match key {
                    Some(key) => out.partial(format!("malformed entry {} was skipped", normalize(&key, 60))),
                    None => out.partial("a malformed entry was skipped"),
                }
                i = at + 1;
            }
            Parsed::Entry(entry) => {
                budget.spend(entry.end - at);
                i = entry.end;
                declare(&mut out, text, at, entry, &mut keys);
            }
        }
        if budget.exhausted() {
            out.partial("the bibliography is too irregular to read completely; later entries were not read");
            break;
        }
    }
    out.finish()
}

fn declare(out: &mut Builder<'_>, text: &str, at: usize, entry: Entry, keys: &mut HashSet<String>) {
    let name = out.checked_name(&entry.key);
    if let Some(name) = &name
        && !keys.insert(name.clone())
    {
        out.partial(format!("duplicate key {name}"));
    }
    let field = |wanted: &str| entry.fields.iter().find(|(n, _)| n == wanted).map(|(_, v)| value(text, v));
    let mut attributes = BTreeMap::new();
    attributes.insert("bibtex.type".to_owned(), entry.ty.clone());
    if let Some((year, true)) = field("year") {
        let year = clean_tex(&year);
        if !year.is_empty() && year.chars().count() <= 16 {
            attributes.insert("bibtex.year".to_owned(), year);
        }
    }
    if let Some((doi, true)) = field("doi") {
        let doi = clean_tex(&doi);
        if !doi.is_empty() && doi.chars().count() <= 200 {
            attributes.insert("bibtex.doi".to_owned(), doi);
        }
    }
    let title = field("title").map(|(t, _)| clean_tex(&t)).filter(|t| !t.is_empty());
    let header = format!("@{}{{{}", entry.ty, entry.key);
    out.declare(
        DeclSpec {
            kind: EntityKind::Reference,
            syntax: "bibtex.entry".to_owned(),
            name,
            title,
            start: at,
            header: &header,
            context: None,
            attributes,
        },
        entry.end,
    );
}

enum Piece {
    /// `{…}` or `"…"`: the inner range.
    Delimited(Range<usize>),
    /// A number or a `@string` macro name.
    Token(Range<usize>),
}

struct Entry {
    ty: String,
    key: String,
    fields: Vec<(String, Vec<Piece>)>,
    end: usize,
}

enum Parsed {
    /// An `@` that does not start an entry (comment text).
    NotEntry,
    /// `@string`, `@preamble`, `@comment`: skipped through `end`.
    Skipped(usize),
    Malformed {
        key: Option<String>,
        scanned: usize,
    },
    Entry(Entry),
}

fn skip_ws(b: &[u8], mut i: usize, limit: usize) -> usize {
    while i < limit && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

fn name_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b':' | b'.' | b'+' | b'/' | b'\'' | b'*' | b'!' | b'?')
}

/// End (after the closing brace) of the balanced `{…}` group at `open`.
fn braced_end(b: &[u8], open: usize, limit: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (k, &c) in b.get(open..limit)?.iter().enumerate() {
        match c {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(open + k + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// End (after the closing quote) of the `"…"` string at `open`; quotes
/// inside braces do not end it.
fn quoted_end(b: &[u8], open: usize, limit: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (k, &c) in b.get(open + 1..limit)?.iter().enumerate() {
        match c {
            b'{' => depth += 1,
            b'}' => depth = depth.checked_sub(1)?,
            b'"' if depth == 0 => return Some(open + 1 + k + 1),
            _ => {}
        }
    }
    None
}

fn parse(text: &str, at: usize, limit: usize) -> Parsed {
    let b = text.as_bytes();
    let mut j = skip_ws(b, at + 1, limit);
    let ts = j;
    while j < limit && (b[j].is_ascii_alphanumeric() || b[j] == b'_' || b[j] == b'-') {
        j += 1;
    }
    if j == ts {
        return Parsed::NotEntry;
    }
    let ty = text[ts..j].to_ascii_lowercase();
    j = skip_ws(b, j, limit);
    let close = match b.get(j) {
        Some(b'{') if j < limit => b'}',
        Some(b'(') if j < limit => b')',
        _ => return Parsed::NotEntry,
    };
    let open = j;
    let malformed =
        |key: Option<String>, scanned: usize| Parsed::Malformed { key, scanned: scanned.min(limit).max(at + 1) };
    if matches!(ty.as_str(), "comment" | "string" | "preamble") {
        let end = if close == b'}' {
            braced_end(b, open, limit)
        } else {
            // `( … )`: skip to the matching parenthesis outside braces.
            let mut depth = 0usize;
            let mut found = None;
            for (k, &c) in b[open..limit].iter().enumerate() {
                match c {
                    b'{' | b'(' => depth += 1,
                    b'}' | b')' => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 && c == b')' {
                            found = Some(open + k + 1);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            found
        };
        return match end {
            Some(end) => Parsed::Skipped(end),
            None => malformed(None, limit),
        };
    }
    j = skip_ws(b, open + 1, limit);
    let ks = j;
    while j < limit
        && !b[j].is_ascii_whitespace()
        && !matches!(b[j], b',' | b'{' | b'}' | b'(' | b')' | b'"' | b'#' | b'=')
    {
        j += 1;
    }
    let key = text[ks..j].to_owned();
    j = skip_ws(b, j, limit);
    let mut entry = Entry { ty, key, fields: Vec::new(), end: 0 };
    match b.get(j) {
        Some(&c) if c == close && j < limit => {
            entry.end = j + 1;
            return Parsed::Entry(entry);
        }
        Some(b',') if j < limit => j += 1,
        _ => return malformed(Some(entry.key), j),
    }
    loop {
        j = skip_ws(b, j, limit);
        if j >= limit {
            return malformed(Some(entry.key), j);
        }
        if b[j] == close {
            entry.end = j + 1;
            return Parsed::Entry(entry);
        }
        if b[j] == b',' {
            j += 1;
            continue;
        }
        let fs = j;
        while j < limit && name_char(b[j]) {
            j += 1;
        }
        if j == fs {
            return malformed(Some(entry.key), j);
        }
        let field = text[fs..j].to_ascii_lowercase();
        j = skip_ws(b, j, limit);
        if b.get(j) != Some(&b'=') || j >= limit {
            return malformed(Some(entry.key), j);
        }
        j += 1;
        let mut pieces = Vec::new();
        loop {
            j = skip_ws(b, j, limit);
            let piece_end = match b.get(j) {
                Some(b'{') if j < limit => braced_end(b, j, limit).map(|e| (Piece::Delimited(j + 1..e - 1), e)),
                Some(b'"') if j < limit => quoted_end(b, j, limit).map(|e| (Piece::Delimited(j + 1..e - 1), e)),
                Some(&c) if j < limit && name_char(c) => {
                    let s = j;
                    let mut e = j;
                    while e < limit && name_char(b[e]) {
                        e += 1;
                    }
                    Some((Piece::Token(s..e), e))
                }
                _ => None,
            };
            let Some((piece, e)) = piece_end else {
                // An unterminated group was scanned to the limit.
                let scanned = if matches!(b.get(j), Some(b'{' | b'"')) { limit } else { j };
                return malformed(Some(entry.key), scanned);
            };
            pieces.push(piece);
            j = skip_ws(b, e, limit);
            if b.get(j) == Some(&b'#') && j < limit {
                j += 1;
                continue;
            }
            break;
        }
        entry.fields.push((field, pieces));
        j = skip_ws(b, j, limit);
        match b.get(j) {
            Some(b',') if j < limit => j += 1,
            Some(&c) if c == close && j < limit => {
                entry.end = j + 1;
                return Parsed::Entry(entry);
            }
            _ => return malformed(Some(entry.key), j),
        }
    }
}

/// A field's literal text (delimited pieces and numbers joined) and whether
/// it was fully literal (no `@string` macro).
fn value(text: &str, pieces: &[Piece]) -> (String, bool) {
    let mut out = String::new();
    let mut literal = true;
    for piece in pieces {
        match piece {
            Piece::Delimited(r) => out.push_str(&text[r.clone()]),
            Piece::Token(r) => {
                let token = &text[r.clone()];
                if token.bytes().all(|c| c.is_ascii_digit()) {
                    out.push_str(token);
                } else {
                    literal = false;
                }
            }
        }
    }
    (out, literal)
}

/// Braces, math shifts, and TeX escapes removed; whitespace collapsed;
/// at most 200 characters.
fn clean_tex(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' | '}' | '$' => {}
            '~' => out.push(' '),
            '\\' => match chars.peek().copied() {
                Some(n) if n.is_ascii_alphabetic() => {
                    let mut name = String::new();
                    while let Some(&n) = chars.peek()
                        && n.is_ascii_alphabetic()
                    {
                        name.push(n);
                        chars.next();
                    }
                    match name.as_str() {
                        "o" | "O" | "l" | "L" | "i" | "j" => out.push_str(&name),
                        n if n.len() == 1 || FORMATTING.contains(&n) => {}
                        n => out.push_str(n),
                    }
                    // TeX consumes the space after a control word.
                    if chars.peek() == Some(&' ') {
                        chars.next();
                    }
                }
                Some(n) if "'\"`^~=.".contains(n) => {
                    chars.next();
                }
                Some(n) => {
                    out.push(n);
                    chars.next();
                }
                None => {}
            },
            c => out.push(c),
        }
        if out.len() > 4096 {
            break;
        }
    }
    normalize(&out, 200)
}

#[cfg(test)]
mod tests {
    use super::super::latex::common::{check_invariants, garbage, slice, truncations};
    use super::*;

    fn run(text: &str) -> Extraction {
        let x = extract(&WorkspacePath::parse("refs.bib").unwrap(), text);
        check_invariants(text, &x);
        x
    }

    const SAMPLE: &str = r#"Comments outside entries are ignored, even e-mail: someone@example.org.
@string{jgt = "Journal of Graph Theory"}
@preamble{"\newcommand{\noop}[1]{}"}
@comment{ @article{commented, title={No}} }
@Article{euler1736,
  author = {Leonhard Euler},
  title  = "Solutio problematis ad geometriam {situs} pertinentis",
  journal = jgt # " (reprint)",
  year = 1736,
  doi = {10.1000/xyz\_1},
}
@book(knuth1984,
  title = {The {\TeX}book},
  year = {1984}
)
@misc{caf\'e, title = {Caf{\'e} {\"o}ber {\emph{alles}}~now}, year = jan}
@misc{euler1736, title={Again}}
@article{broken, title = {unclosed
@misc{after, title = {After the break}}
"#;

    #[test]
    fn entries_titles_and_attributes() {
        let x = run(SAMPLE);
        let names: Vec<_> = x.declarations.iter().map(|d| d.name.as_deref().unwrap_or("?")).collect();
        assert_eq!(names, ["euler1736", "knuth1984", "caf\\'e", "euler1736", "after"]);
        assert!(x.declarations.iter().all(|d| d.kind == EntityKind::Reference && d.syntax == "bibtex.entry"));
        let euler = &x.declarations[0];
        assert_eq!(euler.title.as_deref(), Some("Solutio problematis ad geometriam situs pertinentis"));
        assert_eq!(euler.attributes["bibtex.type"], "article");
        assert_eq!(euler.attributes["bibtex.year"], "1736");
        assert_eq!(euler.attributes["bibtex.doi"], "10.1000/xyz_1");
        let text = slice(SAMPLE, euler.range);
        assert!(text.starts_with("@Article{euler1736,") && text.ends_with("}"));
        let knuth = &x.declarations[1];
        assert_eq!(knuth.title.as_deref(), Some("The TeXbook"));
        assert_eq!(knuth.attributes["bibtex.type"], "book");
        assert!(slice(SAMPLE, knuth.range).ends_with(")"));
        let cafe = &x.declarations[2];
        assert_eq!(cafe.title.as_deref(), Some("Cafe ober alles now"));
        assert!(!cafe.attributes.contains_key("bibtex.year"), "a macro year is not literal");
        assert!(x.partial.contains(&"duplicate key euler1736".to_owned()));
        assert!(x.partial.contains(&"malformed entry broken was skipped".to_owned()), "{:?}", x.partial);
        assert_eq!(x.partial.len(), 2, "{:?}", x.partial);
        assert!(x.links.is_empty() && x.labels.is_empty());
    }

    #[test]
    fn real_fixture() {
        let bib = include_str!("../../../../../tests/fixtures/latex-basic/refs.bib");
        let x = run(bib);
        assert_eq!(x.declarations.len(), 1);
        let d = &x.declarations[0];
        assert_eq!(d.name.as_deref(), Some("knuth1984"));
        assert_eq!(d.title.as_deref(), Some("The TeXbook"));
        assert_eq!(d.attributes["bibtex.year"], "1984");
        assert_eq!(slice(bib, d.range), bib.trim_end());
        assert!(x.partial.is_empty());
    }

    #[test]
    fn fingerprints_track_entry_edits() {
        let a = run("@book{k, title={A}}");
        let b = run("@book{k, title={B}}");
        assert_eq!(a.declarations[0].context_fingerprint, b.declarations[0].context_fingerprint);
        assert_ne!(a.declarations[0].body_fingerprint, b.declarations[0].body_fingerprint);
    }

    #[test]
    fn robustness() {
        for text in [
            "",
            "@",
            "@{",
            "@a",
            "@a{",
            "@a{k",
            "@a{k,",
            "@a{k, t",
            "@a{k, t=",
            "@a{k, t={",
            "@a{k, t=\"",
            "@a(k, t=x)",
            "@string(",
            "@comment{",
            "@a{k, t = a # }",
            "@é{k}",
            "@a{é, t={é}}",
        ] {
            run(text);
        }
        truncations(SAMPLE, 1, |t| extract(&WorkspacePath::parse("r.bib").unwrap(), t));
        let alphabet = ["@", "article", "{", "}", "(", ")", ",", "=", "\"", "#", "key", " ", "\n", "é", "title", "\\"];
        for seed in 0..40 {
            run(&garbage(seed, 400, &alphabet));
        }
        run(&"@a{k, t={".repeat(20_000));
        run(&"@a{".repeat(20_000));
    }
}
