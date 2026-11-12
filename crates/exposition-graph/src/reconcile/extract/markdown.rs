//! Markdown/Pandoc extractor (`markdown/1`).
//!
//! YAML front matter, fenced code blocks, and HTML comments are masked
//! first (attachment markers inside comments are found separately); inline
//! code spans and math are skipped while scanning paragraphs. Literal
//! constructs: images (`![…](path)`, `<img src>`), local links and reference
//! definitions, Pandoc citations (`[@key]`, bare `@key`), `{#id}` attributes,
//! headings (context), and theorem-like fenced divs (`::: {.theorem #id}`).

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;

use exposition_core::path::WorkspacePath;

use super::latex::common::{Builder, DeclSpec, char_len_at, commentary, default_kind, normalize};
use super::markers::MarkerFormat;
use super::{Extraction, LinkFrom, LinkTarget, PathBase};
use crate::vocabulary::RelationKind;

pub const EXTRACTOR: &str = "markdown/1";

const MAX_DIVS: usize = 64;
const MAX_BRACKETS: usize = 256;
/// Inline syntax (destinations, titles, attribute blocks, tags) is bounded.
const INLINE_LIMIT: usize = 2048;

pub fn extract(_path: &WorkspacePath, text: &str) -> Extraction {
    let mut out = Builder::new(EXTRACTOR, text);
    let (masked, info_labels) = mask(text);
    for label in info_labels {
        out.label(&label);
    }
    let mut md = Markdown { m: &masked, out, divs: Vec::new(), heading: None, noted_commentary: false };
    md.blocks();
    md.out.attachments(MarkerFormat::Markdown);
    md.out.finish()
}

fn blank(m: &mut [u8], from: usize, to: usize) {
    let to = to.min(m.len());
    for byte in &mut m[from.min(to)..to] {
        if *byte != b'\n' && *byte != b'\r' {
            *byte = b' ';
        }
    }
}

/// Lines with their byte offsets: (start, end before the line break).
fn lines(text: &str) -> impl Iterator<Item = (usize, usize)> + '_ {
    let mut pos = 0;
    text.split_inclusive('\n').map(move |line| {
        let start = pos;
        pos += line.len();
        (start, start + line.trim_end_matches(['\n', '\r']).len())
    })
}

/// An opening code fence: (fence char, run length, info string).
fn opening_fence(line: &str) -> Option<(u8, usize, &str)> {
    let t = line.trim_start();
    let ch = *t.as_bytes().first()?;
    if ch != b'`' && ch != b'~' {
        return None;
    }
    let n = t.bytes().take_while(|&c| c == ch).count();
    let info = &t[n..];
    (n >= 3 && !(ch == b'`' && info.contains('`'))).then_some((ch, n, info))
}

fn closing_fence(line: &str, ch: u8, n: usize) -> bool {
    let t = line.trim();
    t.len() >= n && t.bytes().all(|c| c == ch)
}

/// Mask front matter, fenced code, and HTML comments (offsets preserved).
/// Returns the masked text and the `#id`s of code-block attributes.
fn mask(text: &str) -> (String, Vec<String>) {
    let mut m = text.as_bytes().to_vec();
    let mut labels = Vec::new();
    let mut body_start = 0;
    if (text.starts_with("---\n") || text.starts_with("---\r\n"))
        && let Some((_, end)) = lines(text).skip(1).find(|&(s, e)| matches!(text[s..e].trim_end(), "---" | "..."))
    {
        blank(&mut m, 0, end);
        body_start = end;
    }
    let mut fence: Option<(u8, usize)> = None;
    for (start, end) in lines(text).filter(|&(s, _)| s >= body_start) {
        let line = &text[start..end];
        match fence {
            Some((ch, n)) => {
                blank(&mut m, start, end);
                if closing_fence(line, ch, n) {
                    fence = None;
                }
            }
            None => {
                if let Some((ch, n, info)) = opening_fence(line) {
                    labels.extend(attribute_ids(info));
                    blank(&mut m, start, end);
                    fence = Some((ch, n));
                }
            }
        }
    }
    mask_comments(&mut m);
    (String::from_utf8(m).unwrap_or_else(|_| text.to_owned()), labels)
}

fn mask_comments(m: &mut [u8]) {
    let mut i = 0;
    while let Some(k) = find(m, i, b"<!--") {
        let end = find(m, k + 4, b"-->").map_or(m.len(), |e| e + 3);
        blank(m, k, end);
        i = end;
    }
}

fn find(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    hay.get(from..)?.windows(needle.len()).position(|w| w == needle).map(|k| from + k)
}

/// `#id`s of the first `{…}` attribute block in `s`.
fn attribute_ids(s: &str) -> Vec<String> {
    s.find('{').map(|k| parse_attributes(&s[k..]).1).unwrap_or_default()
}

/// (classes, ids, key/values) of a Pandoc attribute block's contents.
fn parse_attributes(attrs: &str) -> (Vec<String>, Vec<String>, HashMap<String, String>) {
    let inner = attrs.trim();
    let inner = inner.strip_prefix('{').map_or(inner, |r| r.split('}').next().unwrap_or(r));
    let mut classes = Vec::new();
    let mut ids = Vec::new();
    let mut kv = HashMap::new();
    let mut rest = inner.trim_start();
    while !rest.is_empty() {
        let token_end = rest.find(|c: char| c.is_whitespace() || c == '=').unwrap_or(rest.len());
        let token = &rest[..token_end];
        rest = &rest[token_end..];
        if let Some(value_rest) = rest.strip_prefix('=') {
            let (value, after) = if let Some(q) = value_rest.strip_prefix('"') {
                match q.find('"') {
                    Some(k) => (&q[..k], &q[k + 1..]),
                    None => (q, ""),
                }
            } else {
                let e = value_rest.find(char::is_whitespace).unwrap_or(value_rest.len());
                (&value_rest[..e], &value_rest[e..])
            };
            kv.insert(token.to_owned(), value.to_owned());
            rest = after;
        } else if let Some(class) = token.strip_prefix('.') {
            classes.push(class.to_owned());
        } else if let Some(id) = token.strip_prefix('#') {
            ids.push(id.to_owned());
        }
        rest = rest.trim_start();
    }
    (classes, ids, kv)
}

/// A local file path from a link destination, or `None` for URLs, anchors,
/// and data. Fragments and queries are stripped; `%XX` is decoded.
fn local_path(dest: &str) -> Option<String> {
    let d = dest.trim();
    if d.is_empty() || d.starts_with('#') || d.starts_with("//") {
        return None;
    }
    let scheme_len = d.find(':').filter(|&k| {
        k > 0
            && d[..k].starts_with(|c: char| c.is_ascii_alphabetic())
            && d[..k].chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
    });
    if scheme_len.is_some() {
        return None;
    }
    let p = &d[..d.find(['#', '?']).unwrap_or(d.len())];
    if p.is_empty() {
        return None;
    }
    Some(percent_decode(p))
}

fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_owned();
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(hex) = s.get(i + 1..i + 3)
            && let Ok(v) = u8::from_str_radix(hex, 16)
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_owned())
}

struct Div {
    decl: Option<usize>,
}

struct Markdown<'a> {
    m: &'a str,
    out: Builder<'a>,
    divs: Vec<Div>,
    heading: Option<String>,
    noted_commentary: bool,
}

impl<'a> Markdown<'a> {
    fn from(&self) -> LinkFrom {
        self.divs.iter().rev().find_map(|d| d.decl).map_or(LinkFrom::Resource, LinkFrom::Declaration)
    }

    fn blocks(&mut self) {
        let m = self.m;
        let mut para: Option<(usize, usize)> = None;
        for (start, end) in lines(m) {
            let line = &m[start..end];
            let trimmed = line.trim();
            if trimmed.is_empty() {
                self.flush(&mut para);
                continue;
            }
            let indent = line.len() - line.trim_start().len();
            if trimmed.starts_with(":::") {
                self.flush(&mut para);
                self.div_fence(start, end, trimmed);
                continue;
            }
            if indent <= 3
                && let Some(title) = atx_heading(trimmed)
            {
                self.flush(&mut para);
                self.set_heading(title);
                self.inline(start, end);
                continue;
            }
            if let Some((ps, pe)) = para
                && indent <= 3
                && setext_underline(trimmed)
                && single_paragraph_line(&m[ps..pe])
            {
                self.set_heading(&m[ps..pe]);
                self.flush(&mut para);
                continue;
            }
            if para.is_none() && indent <= 3 && self.reference_definition(start + indent, end) {
                continue;
            }
            para = Some((para.map_or(start, |(ps, _)| ps), end));
        }
        self.flush(&mut para);
        let end = m.len();
        let unclosed = !self.divs.is_empty();
        while let Some(div) = self.divs.pop() {
            if let Some(d) = div.decl {
                self.out.close(d, super::latex::common::trim_end_pos(m, end));
            }
        }
        if unclosed {
            self.out.partial("a fenced div is not closed");
        }
    }

    fn flush(&mut self, para: &mut Option<(usize, usize)>) {
        if let Some((s, e)) = para.take() {
            self.inline(s, e);
        }
    }

    fn set_heading(&mut self, raw: &str) {
        let mut title = raw.trim();
        if title.ends_with('}')
            && let Some(open) = title.rfind('{')
        {
            title = title[..open].trim_end();
        }
        let title = normalize(title, 200);
        self.heading = (!title.is_empty()).then_some(title);
    }

    fn div_fence(&mut self, start: usize, end: usize, trimmed: &str) {
        let colons = trimmed.bytes().take_while(|&c| c == b':').count();
        if colons < 3 {
            return;
        }
        let rest = trimmed[colons..].trim().trim_end_matches(':').trim();
        if rest.is_empty() {
            if let Some(div) = self.divs.pop()
                && let Some(d) = div.decl
            {
                self.out.close(d, end);
            }
            return;
        }
        if self.divs.len() >= MAX_DIVS {
            self.out.partial("fenced divs are nested too deeply; deeper divs were not interpreted");
            return;
        }
        let (classes, ids, kv) = if rest.starts_with('{') {
            parse_attributes(rest)
        } else {
            let word = rest.split_whitespace().next().unwrap_or("");
            (vec![word.to_owned()], Vec::new(), HashMap::new())
        };
        let mut name = None;
        if let Some(id) = ids.first() {
            name = self.out.label(id);
        }
        let theorem = classes.iter().find_map(|c| default_kind(c).map(|k| (c, k)));
        if theorem.is_none() && classes.iter().any(|c| commentary(c)) && !self.noted_commentary {
            self.noted_commentary = true;
            self.out.note("remarks and examples are not admitted as entities");
        }
        let open = self.divs.iter().filter(|d| d.decl.is_some()).count();
        let theorem = theorem.filter(|_| self.out.may_nest(open));
        let decl = theorem.and_then(|(class, kind)| {
            let title = kv.get("title").map(|t| normalize(t, 200)).filter(|t| !t.is_empty());
            let header = normalize(trimmed, 400);
            self.out.open(DeclSpec {
                kind,
                syntax: format!("markdown.div:{class}"),
                name: name.clone(),
                title,
                start,
                header: &header,
                context: self.heading.as_deref(),
                attributes: BTreeMap::new(),
            })
        });
        self.divs.push(Div { decl });
    }

    /// `[id]: destination` (not a footnote `[^id]:`).
    fn reference_definition(&mut self, s: usize, end: usize) -> bool {
        let line = &self.m[s..end];
        let Some(rest) = line.strip_prefix('[') else { return false };
        if rest.starts_with('^') {
            return false;
        }
        let Some(close) = rest.find("]:") else { return false };
        if rest[..close].contains(['[', ']']) || close == 0 {
            return false;
        }
        let dest = rest[close + 2..].trim_start();
        let dest = if let Some(d) = dest.strip_prefix('<') {
            &d[..d.find('>').unwrap_or(d.len())]
        } else {
            dest.split_whitespace().next().unwrap_or("")
        };
        if let Some(path) = local_path(dest) {
            let target = LinkTarget::Path { written: path, base: PathBase::File, extensions: Vec::new() };
            let from = self.from();
            self.out.link(RelationKind::References, from, target, s..end);
        }
        true
    }

    /// Scan one paragraph (or heading line) `[ps, pe)` for inline syntax.
    fn inline(&mut self, ps: usize, pe: usize) {
        let m = self.m;
        let b = m.as_bytes();
        // Backtick runs (outside escapes), and their positions by length,
        // so each code span's closer is found without rescanning.
        let mut runs: Vec<(usize, usize)> = Vec::new();
        let mut j = ps;
        while j < pe {
            match b[j] {
                b'\\' => j += 1 + char_len_at(m, j + 1),
                b'`' => {
                    let n = b[j..pe].iter().take_while(|&&c| c == b'`').count();
                    runs.push((j, n));
                    j += n;
                }
                _ => j += 1,
            }
        }
        let mut by_len: HashMap<usize, Vec<usize>> = HashMap::new();
        for &(pos, n) in &runs {
            by_len.entry(n).or_default().push(pos);
        }
        let mut brackets: Vec<(usize, bool)> = Vec::new();
        let mut dollar_failed = false;
        let mut i = ps;
        while i < pe {
            i = match b[i] {
                b'\\' => i + 1 + char_len_at(m, i + 1),
                b'`' => {
                    let n = b[i..pe].iter().take_while(|&&c| c == b'`').count();
                    let closer = by_len.get(&n).and_then(|list| {
                        let k = list.partition_point(|&p| p <= i);
                        list.get(k).copied()
                    });
                    closer.map_or(i + n, |c| c + n)
                }
                b'$' => self.math(i, pe, &mut dollar_failed),
                b'!' if b.get(i + 1) == Some(&b'[') => {
                    if brackets.len() < MAX_BRACKETS {
                        brackets.push((i, true));
                    }
                    i + 2
                }
                b'[' => {
                    if brackets.len() < MAX_BRACKETS {
                        brackets.push((i, false));
                    }
                    i + 1
                }
                b']' => self.close_bracket(i, pe, brackets.pop()),
                b'<' => self.angle(i, pe),
                b'@' => self.citation(i, pe),
                b'{' if b.get(i + 1) == Some(&b'#') => self.attributes(i, pe),
                _ => i + char_len_at(m, i),
            };
        }
    }

    fn math(&mut self, i: usize, pe: usize, failed: &mut bool) -> usize {
        let m = self.m;
        let b = m.as_bytes();
        if b.get(i + 1) == Some(&b'$') {
            return find(&b[..pe], i + 2, b"$$").map_or(i + 2, |k| k + 2);
        }
        // Pandoc: an opener is followed by a non-space; a closer follows a
        // non-space and is not followed by a digit. A failed search means no
        // later `$` in this paragraph can close either.
        if *failed || b.get(i + 1).is_none_or(|c| c.is_ascii_whitespace()) {
            return i + 1;
        }
        let mut j = i + 1;
        while let Some(k) = b[..pe].get(j..).and_then(|r| r.iter().position(|&c| c == b'$')).map(|k| j + k) {
            let ok = k > i + 1
                && !b[k - 1].is_ascii_whitespace()
                && b[k - 1] != b'\\'
                && !b.get(k + 1).is_some_and(u8::is_ascii_digit);
            if ok {
                return k + 1;
            }
            j = k + 1;
        }
        *failed = true;
        i + 1
    }

    fn close_bracket(&mut self, i: usize, pe: usize, open: Option<(usize, bool)>) -> usize {
        let Some((_, image)) = open else { return i + 1 };
        let b = self.m.as_bytes();
        let after = i + 1;
        match b.get(after) {
            Some(b'(') if after < pe => {
                let Some((dest, end)) = self.destination(after, pe) else { return after };
                let dest = &self.m[dest];
                if let Some(path) = local_path(dest) {
                    let target = LinkTarget::Path { written: path, base: PathBase::File, extensions: Vec::new() };
                    if image {
                        self.out.link(
                            RelationKind::UsesAsset,
                            LinkFrom::Resource,
                            target,
                            open.map_or(i, |o| o.0)..end,
                        );
                    } else {
                        let from = self.from();
                        self.out.link(RelationKind::References, from, target, open.map_or(i, |o| o.0)..end);
                    }
                }
                if b.get(end) == Some(&b'{') && end < pe { self.attributes(end, pe) } else { end }
            }
            Some(b'{') if after < pe => self.attributes(after, pe),
            _ => after,
        }
    }

    /// `(dest "title")` at `open`: (destination range, offset after `)`).
    fn destination(&self, open: usize, pe: usize) -> Option<(Range<usize>, usize)> {
        let b = self.m.as_bytes();
        let limit = (open + INLINE_LIMIT).min(pe);
        let skip = |mut k: usize| {
            while k < limit && b[k].is_ascii_whitespace() {
                k += 1;
            }
            k
        };
        let mut k = skip(open + 1);
        let dest = if b.get(k) == Some(&b'<') {
            let s = k + 1;
            let e = (s..limit).find(|&p| b[p] == b'>' || b[p] == b'\n')?;
            if b[e] != b'>' {
                return None;
            }
            k = e + 1;
            s..e
        } else {
            let s = k;
            let mut depth = 0usize;
            while k < limit && !b[k].is_ascii_whitespace() {
                match b[k] {
                    b'\\' => k += char_len_at(self.m, k + 1),
                    b'(' => depth += 1,
                    b')' if depth == 0 => break,
                    b')' => depth -= 1,
                    _ => {}
                }
                k += 1;
            }
            s..k.min(limit)
        };
        k = skip(k);
        if let Some(&q) = b.get(k).filter(|&&q| matches!(q, b'"' | b'\'' | b'(') && k < limit) {
            let close = if q == b'(' { b')' } else { q };
            let e = (k + 1..limit).find(|&p| b[p] == close && b[p - 1] != b'\\')?;
            k = skip(e + 1);
        }
        (b.get(k) == Some(&b')') && k < limit).then_some((dest, k + 1))
    }

    /// `<img src="…">`, autolinks, and other tags.
    fn angle(&mut self, i: usize, pe: usize) -> usize {
        let m = self.m;
        let b = m.as_bytes();
        let limit = (i + INLINE_LIMIT).min(pe);
        let tag_end = (i + 1..limit).find(|&p| b[p] == b'>' || b[p] == b'<');
        let Some(te) = tag_end.filter(|&p| b[p] == b'>') else { return i + 1 };
        let tag = &m[i + 1..te];
        let is_img = tag.get(..3).is_some_and(|p| p.eq_ignore_ascii_case("img"))
            && tag.get(3..).is_some_and(|r| r.starts_with(|c: char| c.is_whitespace() || c == '/'));
        if !is_img {
            // An autolink or other inline HTML: skipped whole.
            return if tag.contains(char::is_whitespace) && !tag.contains('=') { i + 1 } else { te + 1 };
        }
        let lower = tag.to_ascii_lowercase();
        let mut from = 3;
        while let Some(k) = lower.get(from..).and_then(|r| r.find("src")).map(|k| from + k) {
            from = k + 3;
            let before_ok = tag[..k].ends_with(char::is_whitespace);
            let rest = tag[k + 3..].trim_start();
            if !before_ok || !rest.starts_with('=') {
                continue;
            }
            let value = rest[1..].trim_start();
            let src = match value.chars().next() {
                Some(q @ ('"' | '\'')) => value[1..].split(q).next().unwrap_or(""),
                _ => value.split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or(""),
            };
            if let Some(path) = local_path(src) {
                let target = LinkTarget::Path { written: path, base: PathBase::File, extensions: Vec::new() };
                self.out.link(RelationKind::UsesAsset, LinkFrom::Resource, target, i..te + 1);
            }
            break;
        }
        te + 1
    }

    /// Pandoc citation `@key` / `@{key}` (not inside a word: e-mails).
    fn citation(&mut self, i: usize, pe: usize) -> usize {
        let m = self.m;
        if m[..i].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_') {
            return i + 1;
        }
        let rest = &m[i + 1..pe];
        let (key, len) = if let Some(braced) = rest.strip_prefix('{') {
            match braced.find('}').filter(|&k| k <= 256 && !braced[..k].contains('\n')) {
                Some(k) => (braced[..k].trim(), k + 2),
                None => return i + 1,
            }
        } else {
            let mut end = 0;
            let mut chars = rest.char_indices().peekable();
            while let Some((k, c)) = chars.next() {
                if c.is_alphanumeric() || c == '_' {
                    end = k + c.len_utf8();
                } else if k > 0
                    && ":.#$%&-+?<>~/".contains(c)
                    && chars.peek().is_some_and(|&(_, n)| n.is_alphanumeric() || n == '_')
                {
                    continue;
                } else {
                    break;
                }
                if end > 512 {
                    break;
                }
            }
            if end == 0 || !rest.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
                return i + 1;
            }
            (&rest[..end], end)
        };
        if let Some(key) = self.out.checked_name(key) {
            let from = self.from();
            self.out.link(RelationKind::References, from, LinkTarget::LabelOrCitation(key), i..i + 1 + len);
        }
        i + 1 + len
    }

    /// `{#id …}` attribute block: its ids are labels.
    fn attributes(&mut self, i: usize, pe: usize) -> usize {
        let b = self.m.as_bytes();
        let limit = (i + INLINE_LIMIT).min(pe);
        let Some(close) = (i + 1..limit).find(|&p| b[p] == b'}' || b[p] == b'\n' || b[p] == b'{') else {
            return i + 1;
        };
        if b[close] != b'}' {
            return i + 1;
        }
        for id in attribute_ids(&self.m[i..=close]) {
            self.out.label(&id);
        }
        close + 1
    }
}

/// The text of an ATX heading line (`# Title #`), if it is one.
fn atx_heading(trimmed: &str) -> Option<&str> {
    let n = trimmed.bytes().take_while(|&c| c == b'#').count();
    if !(1..=6).contains(&n) {
        return None;
    }
    let rest = &trimmed[n..];
    if !(rest.is_empty() || rest.starts_with([' ', '\t'])) {
        return None;
    }
    let rest = rest.trim();
    let without_closing = rest.trim_end_matches('#');
    Some(if without_closing.is_empty() || without_closing.ends_with([' ', '\t']) { without_closing } else { rest })
}

fn setext_underline(trimmed: &str) -> bool {
    !trimmed.is_empty()
        && (trimmed.bytes().all(|c| c == b'=') || (trimmed.len() >= 2 && trimmed.bytes().all(|c| c == b'-')))
}

/// A paragraph that can be a setext heading: one line that is not a list
/// item, quote, or table row.
fn single_paragraph_line(text: &str) -> bool {
    let t = text.trim();
    !t.contains('\n')
        && !t.starts_with(['-', '*', '+', '>', '|'])
        && !t.split_once(". ").is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::super::latex::common::{check_invariants, garbage, slice, truncations};
    use super::*;
    use crate::vocabulary::EntityKind;

    fn run(text: &str) -> Extraction {
        let x = extract(&WorkspacePath::parse("notes/main.md").unwrap(), text);
        check_invariants(text, &x);
        x
    }

    fn links(x: &Extraction, kind: RelationKind) -> Vec<(LinkFrom, LinkTarget)> {
        x.links.iter().filter(|l| l.kind == kind).map(|l| (l.from, l.target.clone())).collect()
    }

    fn path(w: &str) -> LinkTarget {
        LinkTarget::Path { written: w.into(), base: PathBase::File, extensions: Vec::new() }
    }

    fn cite(s: &str) -> LinkTarget {
        LinkTarget::LabelOrCitation(s.into())
    }

    const SAMPLE: &str = r#"---
title: Notes
author: someone@example.org and @nobody
---

# Introduction {#sec:intro}

See [@knuth1984, p. 3; -@lamport] and @fig:plot. Mail me at a@b.com or `@code` or $a@b$.
A [local note](other/note.md#part "Title"), an [external one](https://example.com), [mail](mailto:x@y.z),
an [anchor](#sec:intro), and ![a plot](figs/plot%20one.png){#fig:plot width=50%}.
<img src="figs/raw.png" alt="x"> and <https://example.com/@user>.

```python {#lst:code}
![not an image](no.png) @nocite
```

<!-- ![commented](no2.png) @nocite2 -->

Setext Heading
==============

::: {.theorem #thm:main title="Main result"}
For all $x$, see @lemma:aux and [the note](note.md).

:::: {.proof}
Trivially ![inner](inner.png).
::::
:::

::: lemma
Aux.
:::

::: remark
Not an entity.
:::

[ref]: ../shared/defs.md "Definitions"
[^fn]: A footnote with @fncite.
"#;

    #[test]
    fn divs_headings_and_labels() {
        let x = run(SAMPLE);
        let kinds: Vec<_> = x.declarations.iter().map(|d| (d.kind, d.syntax.as_str())).collect();
        assert_eq!(
            kinds,
            [
                (EntityKind::Theorem, "markdown.div:theorem"),
                (EntityKind::Proof, "markdown.div:proof"),
                (EntityKind::Lemma, "markdown.div:lemma"),
            ]
        );
        let thm = &x.declarations[0];
        assert_eq!(thm.name.as_deref(), Some("thm:main"));
        assert_eq!(thm.title.as_deref(), Some("Main result"));
        assert_eq!(thm.context.as_deref(), Some("Setext Heading"));
        let body = slice(SAMPLE, thm.range);
        assert!(
            body.starts_with("::: {.theorem #thm:main") && body.ends_with(":::") && body.contains("::::"),
            "{body}"
        );
        let proof = slice(SAMPLE, x.declarations[1].range);
        assert!(proof.starts_with(":::: {.proof}") && proof.ends_with("::::"), "{proof}");
        for l in ["sec:intro", "fig:plot", "lst:code", "thm:main"] {
            assert!(x.labels.contains(&l.to_owned()), "{l}: {:?}", x.labels);
        }
        assert_eq!(x.notes, ["remarks and examples are not admitted as entities"]);
        assert!(x.partial.is_empty(), "{:?}", x.partial);
    }

    #[test]
    fn citations_links_and_images() {
        let x = run(SAMPLE);
        assert_eq!(
            links(&x, RelationKind::References),
            [
                (LinkFrom::Resource, cite("knuth1984")),
                (LinkFrom::Resource, cite("lamport")),
                (LinkFrom::Resource, cite("fig:plot")),
                (LinkFrom::Resource, path("other/note.md")),
                (LinkFrom::Declaration(0), cite("lemma:aux")),
                (LinkFrom::Declaration(0), path("note.md")),
                (LinkFrom::Resource, path("../shared/defs.md")),
                (LinkFrom::Resource, cite("fncite")),
            ]
        );
        assert_eq!(
            links(&x, RelationKind::UsesAsset),
            [
                (LinkFrom::Resource, path("figs/plot one.png")),
                (LinkFrom::Resource, path("figs/raw.png")),
                (LinkFrom::Resource, path("inner.png")),
            ]
        );
        let img = x.links.iter().find(|l| l.target == path("figs/plot one.png")).unwrap();
        assert_eq!(slice(SAMPLE, img.range), "![a plot](figs/plot%20one.png)");
    }

    #[test]
    fn real_fixture() {
        let main = include_str!("../../../../../tests/fixtures/markdown-basic/main.md");
        let x = run(main);
        assert!(x.declarations.is_empty());
        assert_eq!(links(&x, RelationKind::UsesAsset), [(LinkFrom::Resource, path("figure.png"))]);
        assert_eq!(x.links.len(), 1, "{:?}", x.links);
        assert!(x.partial.is_empty() && x.labels.is_empty());
    }

    #[test]
    fn code_spans_and_unclosed_constructs() {
        let x = run("``a ` @no ``, `x` @yes, ```b``` and `` ` `` [x](y.md\n\n:::: theorem\nopen");
        assert_eq!(links(&x, RelationKind::References), [(LinkFrom::Resource, cite("yes"))]);
        assert_eq!(x.declarations.len(), 1);
        assert_eq!(x.partial, ["a fenced div is not closed"]);
        // An unterminated fence masks the rest; an unterminated front matter is text.
        let x = run("```\n@hidden\n");
        assert!(x.links.is_empty());
        let x = run("---\n@shown\n");
        assert_eq!(links(&x, RelationKind::References), [(LinkFrom::Resource, cite("shown"))]);
    }

    #[test]
    fn attachments_inside_comments() {
        let text = "<!-- exposition:begin plot -->\n![Plot](out/p.png)\n<!-- exposition:end plot -->\n";
        let x = run(text);
        assert_eq!(x.attachments.len(), 1);
        assert_eq!(x.attachments[0].image.as_deref(), Some("out/p.png"));
        assert_eq!(links(&x, RelationKind::UsesAsset), [(LinkFrom::Resource, path("out/p.png"))]);
    }

    #[test]
    fn robustness() {
        for text in [
            "",
            "[",
            "![",
            "](",
            "![a](",
            "![a](<b",
            "[a](b \"c",
            "<img",
            "<img src=",
            "@",
            "@{",
            "`",
            "``",
            "$",
            "$$",
            "{#",
            ":::",
            "::: {",
            "---",
            "<!--",
            "[x]:",
            "# ",
            "é@é",
            "[é](é",
        ] {
            run(text);
        }
        truncations(SAMPLE, 1, |t| extract(&WorkspacePath::parse("a.md").unwrap(), t));
        let alphabet = [
            "[",
            "]",
            "(",
            ")",
            "!",
            "`",
            "$",
            "@k",
            "{#a}",
            "::: theorem\n",
            ":::\n",
            "\n",
            "\n\n",
            "<img src=\"a\">",
            "é",
            " ",
            "<!--",
            "-->",
            "```\n",
            "a.png",
            "\"",
            "#",
        ];
        for seed in 0..40 {
            run(&garbage(seed, 400, &alphabet));
        }
        run(&"![".repeat(50_000));
        run(&"`".repeat(10_000));
        run(&"` ``".repeat(10_000));
        run(&"$a ".repeat(30_000));
        run(&"<img ".repeat(30_000));
        run(&"[a](b \"".repeat(10_000));
    }
}
