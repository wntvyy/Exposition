//! Lean 4 extractor (`lean/1`).
//!
//! Comments, strings, and character literals are masked first; commands are
//! then recognised by their first token on a line. Declarations
//! (`theorem`, `lemma`, `def`, `abbrev`, `structure`, `inductive`, `class`,
//! named `instance`, `axiom`, `opaque`) are qualified by the literal
//! `namespace … end` scopes. A declaration extends (with its doc comment and
//! attributes) to the next command. Commands that generate declarations
//! (macros, elaborators, user commands) are not interpreted and are reported.

use std::collections::BTreeMap;

use exposition_core::path::WorkspacePath;

use super::latex::common::{Builder, DeclSpec, char_len_at, normalize, trim_end_pos};
use super::{Extraction, LinkFrom, LinkTarget};
use crate::vocabulary::{EntityKind, RelationKind};

pub const EXTRACTOR: &str = "lean/1";

const DECL_KEYWORDS: &[&str] =
    &["theorem", "lemma", "def", "abbrev", "structure", "inductive", "class", "instance", "axiom", "opaque"];
const MODIFIERS: &[&str] = &["private", "protected", "noncomputable", "partial", "unsafe", "nonrec", "scoped", "local"];
/// Commands that (may) generate declarations this extractor cannot see.
const GENERATORS: &[&str] = &[
    "macro",
    "macro_rules",
    "syntax",
    "elab",
    "elab_rules",
    "notation",
    "notation3",
    "infix",
    "infixl",
    "infixr",
    "prefix",
    "postfix",
    "initialize",
    "builtin_initialize",
    "declare_syntax_cat",
    "alias",
    "irreducible_def",
    "run_cmd",
    "run_elab",
    "run_meta",
];
/// Other commands that end the previous declaration (at column 0).
const OTHER_COMMANDS: &[&str] = &[
    "open",
    "export",
    "variable",
    "universe",
    "set_option",
    "attribute",
    "omit",
    "include",
    "import",
    "prelude",
    "module",
    "add_decl_doc",
    "library_note",
    "assert_not_exists",
    "suppress_compilation",
    "unseal",
    "seal",
    "deriving",
];
const MAX_SCOPES: usize = 256;
const HEADER_LIMIT: usize = 4096;

pub fn extract(_path: &WorkspacePath, text: &str) -> Extraction {
    let (masked, docs, unterminated) = mask(text);
    let mut lean = Lean {
        m: &masked,
        out: Builder::new(EXTRACTOR, text),
        scopes: Vec::new(),
        current: None,
        neutral: true,
        header_done: false,
        pending_doc: None,
        anonymous_instances: 0,
    };
    if unterminated {
        lean.out.partial("an unterminated comment or string hides the rest of the file");
    }
    lean.run(&docs);
    lean.out.finish()
}

fn ident_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '\'' | '!' | '?')
}

/// Keywords that stay escaped (`«theorem»`) when used as a name segment.
const NAME_KEYWORDS: &[&str] = &[
    "theorem",
    "lemma",
    "def",
    "abbrev",
    "structure",
    "inductive",
    "class",
    "instance",
    "axiom",
    "opaque",
    "example",
    "namespace",
    "section",
    "end",
    "where",
    "fun",
    "let",
    "have",
    "show",
    "by",
    "do",
    "then",
    "else",
    "if",
    "match",
    "with",
    "at",
    "from",
    "import",
    "open",
    "variable",
    "universe",
    "mutual",
    "private",
    "protected",
    "noncomputable",
    "partial",
    "unsafe",
    "in",
    "deriving",
    "extends",
    "Type",
    "Sort",
    "Prop",
];

/// The segments of a dotted Lean name, as Lean prints them: a dot inside
/// `«…»` belongs to the segment, and guillemets are kept only when the segment
/// needs them (`«plain»` prints as `plain`, `«weird name»` keeps them).
fn name_segments(name: &str) -> Vec<String> {
    let printed = |seg: &str| -> String {
        if let Some(inner) = seg.strip_prefix('«').and_then(|r| r.strip_suffix('»')) {
            let plain = inner.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')
                && inner.chars().all(ident_char)
                && !NAME_KEYWORDS.contains(&inner);
            if plain {
                return inner.to_owned();
            }
        }
        seg.to_owned()
    };
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in name.chars() {
        match c {
            '«' => {
                quoted = true;
                current.push(c);
            }
            '»' => {
                quoted = false;
                current.push(c);
            }
            '.' if !quoted => {
                if !current.is_empty() {
                    out.push(printed(&current));
                }
                current.clear();
            }
            _ => current.push(c),
        }
    }
    if !current.is_empty() {
        out.push(printed(&current));
    }
    out
}

/// A plausible qualified Lean name: bounded, no control characters, and
/// whitespace only inside `«…»` segments.
fn valid_lean_name(name: &str) -> bool {
    super::valid_name(name)
}

fn blank(m: &mut [u8], from: usize, to: usize) {
    let to = to.min(m.len());
    for byte in &mut m[from.min(to)..to] {
        if *byte != b'\n' && *byte != b'\r' {
            *byte = b' ';
        }
    }
}

/// Mask comments, strings, and character literals (keeping line breaks and
/// string quotes). Returns the masked text, the `/-- … -/` doc comment byte
/// ranges, and whether something ran to the end of the text unterminated.
fn mask(text: &str) -> (String, Vec<(usize, usize)>, bool) {
    let b = text.as_bytes();
    let mut m = b.to_vec();
    let mut docs = Vec::new();
    let mut unterminated = false;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'-' if b.get(i + 1) == Some(&b'-') => {
                let e = b[i..].iter().position(|&c| c == b'\n').map_or(b.len(), |k| i + k);
                blank(&mut m, i, e);
                i = e;
            }
            b'/' if b.get(i + 1) == Some(&b'-') => {
                let doc = b.get(i + 2) == Some(&b'-');
                let mut depth = 1usize;
                let mut j = i + 2;
                while j < b.len() {
                    if b[j] == b'/' && b.get(j + 1) == Some(&b'-') {
                        depth += 1;
                        j += 2;
                    } else if b[j] == b'-' && b.get(j + 1) == Some(&b'/') {
                        depth -= 1;
                        j += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        j += 1;
                    }
                }
                if depth > 0 {
                    unterminated = true;
                    j = b.len();
                }
                blank(&mut m, i, j);
                if doc {
                    docs.push((i, j));
                }
                i = j;
            }
            b'"' => {
                let mut j = i + 1;
                loop {
                    match b.get(j) {
                        None => {
                            unterminated = true;
                            break;
                        }
                        Some(b'\\') => j += 1 + char_len_at(text, j + 1),
                        Some(b'"') => break,
                        Some(_) => j += 1,
                    }
                }
                let close = j.min(b.len());
                blank(&mut m, i + 1, close);
                i = close + 1;
            }
            b'\'' => {
                let prime = text[..i].chars().next_back().is_some_and(ident_char);
                let close = if prime {
                    None
                } else if b.get(i + 1) == Some(&b'\\') {
                    b.get(i + 2..(i + 14).min(b.len()))
                        .and_then(|r| r.iter().position(|&c| c == b'\''))
                        .map(|k| i + 2 + k)
                } else {
                    let len = char_len_at(text, i + 1);
                    (b.get(i + 1 + len) == Some(&b'\'') && b.get(i + 1) != Some(&b'\n')).then_some(i + 1 + len)
                };
                match close {
                    Some(c) => {
                        blank(&mut m, i + 1, c);
                        i = c + 1;
                    }
                    None => i += 1,
                }
            }
            b'r' if !text[..i].chars().next_back().is_some_and(ident_char)
                && matches!(b.get(i + 1), Some(b'"') | Some(b'#')) =>
            {
                let hashes = b[i + 1..].iter().take_while(|&&c| c == b'#').count();
                let open = i + 1 + hashes;
                if b.get(open) != Some(&b'"') {
                    i += 1;
                    continue;
                }
                let closing = format!("\"{}", "#".repeat(hashes));
                match text[open + 1..].find(&closing) {
                    Some(k) => {
                        blank(&mut m, open + 1, open + 1 + k);
                        i = open + 1 + k + closing.len();
                    }
                    None => {
                        unterminated = true;
                        blank(&mut m, open + 1, b.len());
                        i = b.len();
                    }
                }
            }
            _ => i += 1,
        }
    }
    let masked = String::from_utf8(m).unwrap_or_else(|_| text.to_owned());
    (masked, docs, unterminated)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScopeKind {
    Namespace,
    Section,
    Mutual,
}

struct Scope {
    kind: ScopeKind,
    name: Option<String>,
}

/// A command's leading attributes, modifiers, and keyword.
struct Header {
    attrs: Vec<String>,
    modifiers: Vec<String>,
    keyword: Option<(usize, usize)>,
    /// Where reading stopped (lines before it are part of this header).
    end: usize,
}

struct Lean<'a> {
    m: &'a str,
    out: Builder<'a>,
    scopes: Vec<Scope>,
    /// The declaration being extended.
    current: Option<usize>,
    /// No command body is open (an unknown command here is not a
    /// continuation line).
    neutral: bool,
    header_done: bool,
    pending_doc: Option<usize>,
    anonymous_instances: usize,
}

impl<'a> Lean<'a> {
    fn run(&mut self, docs: &[(usize, usize)]) {
        let m = self.m;
        let mut doc_i = 0;
        let mut skip_until = 0;
        let mut pos = 0;
        for line in m.split_inclusive('\n') {
            let line_start = pos;
            pos += line.len();
            let Some(offset) = line.find(|c: char| !c.is_whitespace()) else { continue };
            let s = line_start + offset;
            if s < skip_until {
                continue;
            }
            while let Some(&(start, end)) = docs.get(doc_i)
                && start < s
            {
                // A doc comment prefixes the next command if one follows it.
                self.pending_doc = Some(self.pending_doc.unwrap_or(start)).filter(|_| end <= s);
                doc_i += 1;
            }
            skip_until = self.line(s, offset == 0);
        }
        self.close_current(m.len());
        if self.anonymous_instances > 0 {
            self.out.note(format!(
                "anonymous instances are not admitted as entities ({} in this file)",
                self.anonymous_instances
            ));
        }
    }

    fn word_at(&self, at: usize) -> (usize, usize) {
        let rest = self.m.get(at..).unwrap_or("");
        let len = rest
            .char_indices()
            .find(|&(k, c)| !(ident_char(c) || c == '.' || (k == 0 && c == '#') || (k > 0 && c == '%')))
            .map_or(rest.len(), |(k, _)| k);
        (at, at + len)
    }

    fn skip_space(&self, mut at: usize, newlines: bool) -> usize {
        let b = self.m.as_bytes();
        while at < b.len() && (b[at] == b' ' || b[at] == b'\t' || b[at] == b'\r' || (newlines && b[at] == b'\n')) {
            at += 1;
        }
        at
    }

    fn header(&self, s: usize) -> Header {
        let b = self.m.as_bytes();
        let limit = (s + HEADER_LIMIT).min(b.len());
        let mut h = Header { attrs: Vec::new(), modifiers: Vec::new(), keyword: None, end: s };
        let mut j = s;
        while j < limit {
            if b[j] == b'@' && b.get(j + 1) == Some(&b'[') {
                // An attribute list may continue on indented lines; a blank
                // line or a line starting a new command ends the search.
                let mut depth = 0usize;
                let mut k = j + 1;
                let close = loop {
                    if k >= limit {
                        break None;
                    }
                    match b[k] {
                        b'[' => depth += 1,
                        b']' => {
                            depth -= 1;
                            if depth == 0 {
                                break Some(k);
                            }
                        }
                        b'\n' => {
                            let next = self.skip_space(k + 1, false);
                            // A blank line, a line at column 0, or a new attribute list.
                            if next == k + 1
                                || matches!(b.get(next), None | Some(b'\n'))
                                || self.m[next..].starts_with("@[")
                            {
                                break None;
                            }
                        }
                        _ => {}
                    }
                    k += 1;
                };
                let Some(k) = close else { return h };
                h.attrs.push(normalize(&self.m[j + 2..k], 200));
                j = self.skip_space(k + 1, true);
                h.end = j;
                continue;
            }
            let (ws, we) = self.word_at(j);
            if we == ws {
                return h;
            }
            let word = &self.m[ws..we];
            if MODIFIERS.contains(&word) {
                h.modifiers.push(word.to_owned());
                j = self.skip_space(we, false);
                h.end = j;
                continue;
            }
            h.keyword = Some((ws, we));
            h.end = we;
            return h;
        }
        h
    }

    fn namespace(&self) -> String {
        let parts: Vec<&str> =
            self.scopes.iter().filter(|s| s.kind == ScopeKind::Namespace).filter_map(|s| s.name.as_deref()).collect();
        parts.join(".")
    }

    fn close_current(&mut self, before: usize) {
        if let Some(d) = self.current.take() {
            let end = trim_end_pos(self.m, before);
            self.out.close(d, end);
        }
    }

    /// Handle the line whose first token is at `s`; returns the offset up to
    /// which later lines are already consumed (by a multi-line header).
    fn line(&mut self, s: usize, col0: bool) -> usize {
        let h = self.header(s);
        let prefixed = !h.attrs.is_empty() || !h.modifiers.is_empty();
        let pending_doc = self.pending_doc.take();
        let Some((ks, ke)) = h.keyword else {
            if prefixed {
                // An attribute line before a declaration on a later line is
                // read with it (header() crosses lines), so this is a stray
                // attribute list: it ends the previous command.
                self.close_current(pending_doc.unwrap_or(s));
                self.neutral = true;
            }
            return h.end;
        };
        let keyword = &self.m[ks..ke];
        let start = pending_doc.unwrap_or(s);
        if DECL_KEYWORDS.contains(&keyword) {
            self.header_done = true;
            self.close_current(start);
            self.declaration(&h, keyword, start, ke);
            return ke;
        }
        match keyword {
            "example" => {
                self.close_current(start);
                self.neutral = false;
                self.header_done = true;
                return ke;
            }
            "namespace" | "section" => {
                self.close_current(start);
                self.neutral = true;
                self.header_done = true;
                let kind = if keyword == "namespace" { ScopeKind::Namespace } else { ScopeKind::Section };
                let at = self.skip_space(ke, false);
                match self.read_name(at) {
                    None => self.push_scope(kind, None),
                    Some(name) => {
                        for part in name_segments(&name) {
                            self.push_scope(kind, Some(part));
                        }
                        return at + name.len();
                    }
                }
                return ke;
            }
            "mutual" => {
                self.close_current(start);
                self.neutral = true;
                self.header_done = true;
                self.push_scope(ScopeKind::Mutual, None);
                return ke;
            }
            "end" if col0 || !prefixed => {
                self.close_current(start);
                self.neutral = true;
                self.header_done = true;
                let at = self.skip_space(ke, false);
                let name = self.read_name(at).unwrap_or_default();
                self.end(&name_segments(&name));
                return (at + name.len()).max(ke);
            }
            "import" if !self.header_done => {
                self.imports(ke);
                return ke;
            }
            "prelude" | "module" if !self.header_done => return ke,
            _ => {}
        }
        let deriving_instance = keyword == "deriving" && {
            let (ws, we) = self.word_at(self.skip_space(ke, false));
            &self.m[ws..we] == "instance"
        };
        if GENERATORS.contains(&keyword) || deriving_instance {
            if col0 || prefixed {
                self.close_current(start);
                self.neutral = true;
                self.header_done = true;
                self.out.partial("commands that generate declarations (macro, elab, …) are not interpreted");
            }
            return ke;
        }
        if (OTHER_COMMANDS.contains(&keyword) && keyword != "deriving") || keyword.starts_with('#') {
            if col0 || prefixed {
                self.close_current(start);
                self.neutral = true;
                if !matches!(keyword, "import" | "prelude" | "module") {
                    self.header_done = true;
                }
            }
            return ke;
        }
        if prefixed {
            // Attributes/modifiers before a command this extractor does not know.
            self.close_current(start);
            self.neutral = true;
            self.out.partial(format!("command {} is not interpreted", short(keyword)));
            return ke;
        }
        if col0 && self.neutral && keyword.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
            self.header_done = true;
            self.out.partial(format!("command {} is not interpreted", short(keyword)));
        }
        s
    }

    fn push_scope(&mut self, kind: ScopeKind, name: Option<String>) {
        if self.scopes.len() >= MAX_SCOPES {
            self.out.partial("scopes are nested too deeply; deeper scopes were not tracked");
            return;
        }
        self.scopes.push(Scope { kind, name });
    }

    fn end(&mut self, parts: &[String]) {
        let name = parts.join(".");
        if parts.is_empty() {
            match self.scopes.last() {
                Some(Scope { name: None, .. }) => {
                    self.scopes.pop();
                }
                _ => self.out.partial("an end without a name does not match the open scope"),
            }
            return;
        }
        let n = parts.len();
        let matches_at = |scopes: &[Scope], top: usize| {
            top >= n && scopes[top - n..top].iter().zip(parts).all(|(s, p)| s.name.as_deref() == Some(p.as_str()))
        };
        let len = self.scopes.len();
        if matches_at(&self.scopes, len) {
            self.scopes.truncate(len - n);
            return;
        }
        self.out.partial(format!("end {} does not match the open scope", short(&name)));
        // Recover: close down to a matching scope if there is one nearby.
        if let Some(top) = (len.saturating_sub(16)..len).rev().find(|&t| matches_at(&self.scopes, t)) {
            self.scopes.truncate(top - n);
        }
    }

    fn imports(&mut self, mut at: usize) {
        loop {
            at = self.skip_space(at, false);
            let (ws, we) = self.word_at(at);
            if we == ws {
                return;
            }
            let word = &self.m[ws..we];
            if !matches!(word, "import" | "all" | "public" | "meta" | "private") {
                let module = word.trim_end_matches('.');
                if module.starts_with(|c: char| c.is_alphabetic() || c == '_')
                    && let Some(module) = self.out.checked_name(module)
                {
                    self.out.link(RelationKind::Imports, LinkFrom::Resource, LinkTarget::LeanModule(module), ws..we);
                }
            }
            at = we;
        }
    }

    fn declaration(&mut self, h: &Header, keyword: &str, start: usize, ke: usize) {
        let mut display = keyword.to_owned();
        let mut at = self.skip_space(ke, true);
        if keyword == "class" {
            let (ws, we) = self.word_at(at);
            if &self.m[ws..we] == "inductive" {
                display = "class inductive".to_owned();
                at = self.skip_space(we, true);
            }
        }
        if keyword == "instance" && self.m.get(at..).is_some_and(|r| r.starts_with('(')) {
            // `(priority := n)` comes before the name.
            let inner = self.m[at + 1..].trim_start();
            if inner.starts_with("priority")
                && let Some(close) = self.m[at..].find(')').filter(|&k| k < 256)
            {
                at = self.skip_space(at + close + 1, true);
            }
        }
        let written = self.read_name(at);
        if keyword == "instance" && written.is_none() {
            self.anonymous_instances += 1;
            self.neutral = false;
            return;
        }
        let ns = self.namespace();
        let name = written.as_deref().and_then(|w| {
            let mut segments = name_segments(w);
            let rooted = segments.first().is_some_and(|s| s == "_root_");
            if rooted {
                segments.remove(0);
            }
            let full = match (rooted, ns.is_empty()) {
                (true, _) | (false, true) => segments.join("."),
                (false, false) => format!("{ns}.{}", segments.join(".")),
            };
            if valid_lean_name(&full) {
                Some(full)
            } else {
                // Same reporting as other unusable names.
                self.out.checked_name(&full)
            }
        });
        if written.is_none() {
            self.out.partial(format!("a {display} without a recognisable name was indexed without one"));
        }
        let mut attributes = BTreeMap::new();
        attributes.insert("lean.kind".to_owned(), display.clone());
        if !h.modifiers.is_empty() {
            attributes.insert("lean.modifiers".to_owned(), h.modifiers.join(" "));
        }
        if !h.attrs.is_empty() {
            attributes.insert("lean.attributes".to_owned(), normalize(&h.attrs.join(", "), 200));
        }
        if !ns.is_empty() {
            attributes.insert("lean.namespace".to_owned(), ns.clone());
        }
        let header = format!("{display} {}", written.as_deref().unwrap_or(""));
        self.current = self.out.open(DeclSpec {
            kind: EntityKind::FormalDeclaration,
            syntax: format!("lean.{keyword}"),
            name,
            title: None,
            start,
            header: &header,
            context: (!ns.is_empty()).then_some(ns.as_str()),
            attributes,
        });
        self.neutral = false;
    }

    /// A declared name at `at` (identifier segments, `«…»` allowed).
    fn read_name(&self, at: usize) -> Option<String> {
        let rest = self.m.get(at..)?;
        let first = rest.chars().next()?;
        if !(first.is_alphabetic() || first == '_' || first == '«') {
            return None;
        }
        let mut end = 0;
        let mut chars = rest.char_indices().peekable();
        while let Some((k, c)) = chars.next() {
            if c == '«' {
                let close = rest[k..].find('»')?;
                if close > 256 {
                    return None;
                }
                end = k + close + '»'.len_utf8();
                while chars.peek().is_some_and(|&(j, _)| j < end) {
                    chars.next();
                }
                continue;
            }
            if ident_char(c) || c == '.' {
                end = k + c.len_utf8();
            } else {
                break;
            }
            if end > 512 {
                return None;
            }
        }
        let name = rest[..end].trim_end_matches('.');
        (!name.is_empty()).then(|| name.to_owned())
    }
}

fn short(s: &str) -> String {
    normalize(s, 60)
}

#[cfg(test)]
mod tests {
    use super::super::latex::common::{check_invariants, garbage, slice, truncations};
    use super::*;

    #[test]
    fn names_in_guillemets_are_qualified_as_lean_prints_them() {
        let x = run("namespace Review\ntheorem «weird name» : 1 + 1 = 2 := rfl\ntheorem «plain» : True := trivial\n\
             namespace «Odd ns»\ndef «a.b» := 1\nend «Odd ns»\nend Review\ntheorem «x y».z : True := trivial\n");
        let names: Vec<&str> = x.declarations.iter().filter_map(|d| d.name.as_deref()).collect();
        assert_eq!(names, ["Review.«weird name»", "Review.plain", "Review.«Odd ns».«a.b»", "«x y».z"], "{x:?}");
        assert!(x.partial.is_empty(), "{:?}", x.partial);
        assert!(!valid_lean_name("a b"));
        assert!(valid_lean_name("A.«b c»"));
        assert!(!valid_lean_name("A.«b"));
    }

    fn run(text: &str) -> Extraction {
        let x = extract(&WorkspacePath::parse("Paper/A.lean").unwrap(), text);
        check_invariants(text, &x);
        x
    }

    fn names(x: &Extraction) -> Vec<&str> {
        x.declarations.iter().map(|d| d.name.as_deref().unwrap_or("?")).collect()
    }

    fn imports(x: &Extraction) -> Vec<String> {
        x.links
            .iter()
            .map(|l| match &l.target {
                LinkTarget::LeanModule(m) => m.clone(),
                other => panic!("unexpected {other:?}"),
            })
            .collect()
    }

    const SAMPLE: &str = r#"import Mathlib.Data.Nat
import LeanBasic.Defs

/-! Module doc mentioning theorem fake : True -/

namespace A.B

/-- Doc for foo. -/
@[simp, norm_cast]
protected theorem foo (n : Nat) : n = n := rfl

private def bar := "-- not a comment"   -- trailing comment

theorem _root_.baz : True := trivial

instance : Inhabited Nat := ⟨0⟩
instance (priority := 100) instFoo : Inhabited Nat := ⟨1⟩

section S
variable (x : Nat)
lemma add_zero' : x + 0 = x := by
  /- nested /- block -/ comment
     theorem hidden : False -/
  simp
end S

end B
noncomputable def mid := 'a'
end A

structure Point where
  /-- x coordinate -/
  x : Nat
  y : Nat
deriving Repr

class inductive Good | yes

macro "foo" : term => `(1)

end Oops
"#;

    #[test]
    fn declarations_namespaces_and_attributes() {
        let x = run(SAMPLE);
        assert_eq!(names(&x), ["A.B.foo", "A.B.bar", "baz", "A.B.instFoo", "A.B.add_zero'", "A.mid", "Point", "Good"]);
        assert!(x.declarations.iter().all(|d| d.kind == EntityKind::FormalDeclaration && d.title.is_none()));
        let foo = &x.declarations[0];
        assert_eq!(foo.syntax, "lean.theorem");
        assert_eq!(foo.context.as_deref(), Some("A.B"));
        assert_eq!(foo.attributes["lean.kind"], "theorem");
        assert_eq!(foo.attributes["lean.modifiers"], "protected");
        assert_eq!(foo.attributes["lean.attributes"], "simp, norm_cast");
        assert_eq!(foo.attributes["lean.namespace"], "A.B");
        let text = slice(SAMPLE, foo.range);
        assert!(text.starts_with("/-- Doc"), "{text}");
        assert!(text.ends_with(":= rfl"), "{text}");
        let bar = &x.declarations[1];
        assert_eq!(bar.attributes["lean.modifiers"], "private");
        assert!(slice(SAMPLE, bar.range).ends_with("\"-- not a comment\""));
        let baz = &x.declarations[2];
        assert_eq!(baz.context.as_deref(), Some("A.B"));
        assert_eq!(x.declarations[3].syntax, "lean.instance");
        let lemma = &x.declarations[4];
        assert_eq!(lemma.syntax, "lean.lemma");
        assert!(slice(SAMPLE, lemma.range).ends_with("simp"));
        let mid = &x.declarations[5];
        assert_eq!(mid.attributes["lean.modifiers"], "noncomputable");
        assert_eq!(mid.attributes["lean.namespace"], "A");
        let point = &x.declarations[6];
        assert_eq!(point.syntax, "lean.structure");
        assert!(!point.attributes.contains_key("lean.namespace"));
        assert_eq!(point.context, None);
        let p = slice(SAMPLE, point.range);
        assert!(p.contains("x coordinate") && p.ends_with("deriving Repr"), "{p}");
        let good = &x.declarations[7];
        assert_eq!(good.syntax, "lean.class");
        assert_eq!(good.attributes["lean.kind"], "class inductive");
        assert_eq!(imports(&x), ["Mathlib.Data.Nat", "LeanBasic.Defs"]);
        assert_eq!(x.notes, ["anonymous instances are not admitted as entities (1 in this file)"]);
        assert!(x.partial.contains(&"commands that generate declarations (macro, elab, …) are not interpreted".into()));
        assert!(x.partial.contains(&"end Oops does not match the open scope".into()));
        assert_eq!(x.partial.len(), 2, "{:?}", x.partial);
    }

    #[test]
    fn real_fixtures() {
        let gauss = include_str!("../../../../../tests/fixtures/m6-paper/Paper/Gauss.lean");
        let x = run(gauss);
        assert_eq!(names(&x), ["Paper.sumTo", "Paper.two_mul_sumTo"]);
        assert_eq!(x.declarations[0].syntax, "lean.def");
        assert_eq!(x.declarations[1].syntax, "lean.theorem");
        let def = slice(gauss, x.declarations[0].range);
        assert!(def.starts_with("/-- The sum") && def.ends_with("| n + 1 => sumTo n + (n + 1)"), "{def}");
        let thm = slice(gauss, x.declarations[1].range);
        assert!(thm.starts_with("/-- Gauss's formula") && thm.ends_with("grind"), "{thm}");
        assert!(x.partial.is_empty() && x.links.is_empty());

        let root = include_str!("../../../../../tests/fixtures/m6-paper/Paper.lean");
        assert_eq!(imports(&run(root)), ["Paper.Gauss"]);

        let defs = include_str!("../../../../../tests/fixtures/lean-basic/LeanBasic/Defs.lean");
        assert_eq!(names(&run(defs)), ["LeanBasic.double", "LeanBasic.double_zero"]);
        let proofs = include_str!("../../../../../tests/fixtures/lean-basic/LeanBasic/Proofs.lean");
        let x = run(proofs);
        assert_eq!(names(&x), ["LeanBasic.double_add", "LeanBasic.add_zero'", "LeanBasic.add_comm'"]);
        assert_eq!(imports(&x), ["LeanBasic.Defs"]);
        assert!(slice(proofs, x.declarations[1].range).ends_with("sorry"));
        assert!(x.partial.is_empty());
        let basic = include_str!("../../../../../tests/fixtures/lean-basic/LeanBasic.lean");
        assert_eq!(imports(&run(basic)), ["LeanBasic.Defs"]);
    }

    #[test]
    fn scopes_and_unmatched_ends() {
        let x = run("namespace A.B\nend B\ndef x := 1\nend A\nsection\ndef y := 2\nend\nend\ndef z := 3");
        assert_eq!(names(&x), ["A.x", "y", "z"]);
        assert_eq!(x.partial, ["an end without a name does not match the open scope"]);
        let x = run("namespace A\nsection S\nend A\ndef w := 1");
        assert_eq!(names(&x), ["w"]);
        assert_eq!(x.partial, ["end A does not match the open scope"]);
    }

    #[test]
    fn commands_and_continuations() {
        let text = "/-- field doc only -/\ntheorem t : True := by\n  open Classical in\n  trivial\nopen Nat\nmy_command foo\n\
                    @[simp]\ntheorem u : True := trivial\nexample : True := trivial\nnot_a_command here\n\
                    set_option maxHeartbeats 400 in\ndef v := 1\nderiving instance Repr for Foo\n";
        let x = run(text);
        assert_eq!(names(&x), ["t", "u", "v"]);
        assert!(slice(text, x.declarations[0].range).ends_with("trivial"));
        let u = slice(text, x.declarations[1].range);
        assert_eq!(u, "@[simp]\ntheorem u : True := trivial");
        assert_eq!(x.declarations[1].attributes["lean.attributes"], "simp");
        let joined = x.partial.join("\n");
        assert!(joined.contains("command my_command is not interpreted"), "{joined}");
        assert!(joined.contains("commands that generate declarations"), "{joined}");
        // Imports after the header are not module imports.
        assert!(run("def a := 1\nimport X").links.is_empty());
    }

    #[test]
    fn strings_chars_and_comments_hide_keywords() {
        let text = "def s := \"\ntheorem fake : True\" ++ r#\"theorem raw\"# ++ toString '\"'\n-- theorem c\ntheorem real : True := trivial";
        let x = run(text);
        assert_eq!(names(&x), ["s", "real"]);
        assert!(x.partial.is_empty(), "{:?}", x.partial);
    }

    #[test]
    fn robustness() {
        for text in [
            "",
            "/-",
            "/--",
            "\"",
            "'",
            "r#\"",
            "namespace",
            "end",
            "@[",
            "@[simp",
            "instance (priority",
            "theorem «",
            "theorem «a b» : True",
            "end ..",
            "namespace .\nend .",
        ] {
            run(text);
        }
        truncations(SAMPLE, 1, |t| extract(&WorkspacePath::parse("a.lean").unwrap(), t));
        let alphabet = [
            "theorem ",
            "def ",
            "namespace ",
            "end ",
            "A",
            ".",
            "\n",
            "/-",
            "-/",
            "--",
            "\"",
            "'",
            "@[",
            "]",
            "é",
            " ",
            "private ",
            "instance ",
            ":",
            "x",
        ];
        for seed in 0..40 {
            run(&garbage(seed, 400, &alphabet));
        }
        run(&"namespace A\n".repeat(1000));
        run(&"@[".repeat(10_000));
        run(&"/-".repeat(10_000));
    }
}
