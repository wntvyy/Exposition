//! Typst extractor (`typst/1`).
//!
//! A mode-aware scanner (markup, code, content blocks, math, strings, raw
//! text, nested comments) reads literal constructs only: theorem-like calls
//! (`#theorem[…]`, `#lemma("Title")[…]`, plus `#let NAME = thmbox(…)`
//! definitions of the ctheorems package in the same file), labels `<x>`,
//! references `@x`, `ref`/`cite`/`label` calls, `#import`/`#include` of
//! files, `image(…)`, and `bibliography(…)`. Nothing is evaluated.

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;

use exposition_core::path::WorkspacePath;

use super::latex::common::{
    Budget, Builder, DeclSpec, char_len_at, commentary, default_kind, kind_by_words, normalize, trim_end_pos,
};
use super::markers::MarkerFormat;
use super::{Extraction, LinkFrom, LinkTarget, PathBase};
use crate::vocabulary::{EntityKind, RelationKind};

pub const EXTRACTOR: &str = "typst/1";

/// Keywords that start a statement running to the end of the line.
const STATEMENT_KEYWORDS: &[&str] =
    &["let", "set", "show", "import", "include", "if", "for", "while", "context", "return", "break", "continue"];
const MAX_FRAMES: usize = 256;
const ARGS_LIMIT: usize = 1 << 16;

pub fn extract(_path: &WorkspacePath, text: &str) -> Extraction {
    let mut ty = Typst {
        t: text,
        out: Builder::new(EXTRACTOR, text),
        budget: Budget::new(text.len()),
        frames: Vec::new(),
        open_decls: Vec::new(),
        heading: None,
        pending_label: None,
        line_start: true,
        thm: HashMap::new(),
        noted_commentary: false,
        stopped: false,
    };
    ty.run();
    if ty.budget.exhausted() {
        ty.out.partial("the file is too irregular to read completely; some arguments were not interpreted");
    }
    ty.out.attachments(MarkerFormat::Typst);
    ty.out.finish()
}

// ----- lexical helpers (pure) -----

fn line_end(b: &[u8], from: usize) -> usize {
    b.get(from..).and_then(|r| r.iter().position(|&c| c == b'\n')).map_or(b.len(), |k| from + k)
}

/// End of the (nested) block comment at `i` (`/*`).
fn comment_end(b: &[u8], i: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut j = i;
    while j + 1 < b.len() {
        if b[j] == b'/' && b[j + 1] == b'*' {
            depth += 1;
            j += 2;
        } else if b[j] == b'*' && b[j + 1] == b'/' {
            depth -= 1;
            j += 2;
            if depth == 0 {
                return Some(j);
            }
        } else {
            j += 1;
        }
    }
    None
}

/// End (after the closing quote) of the string at `i` (`"`).
fn string_end(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            b'"' => return Some(j + 1),
            _ => j += 1,
        }
    }
    None
}

/// End of the raw text at `i` (a run of backticks).
fn raw_end(t: &str, i: usize) -> Option<usize> {
    let b = t.as_bytes();
    let n = b[i..].iter().take_while(|&&c| c == b'`').count();
    if n == 2 {
        return Some(i + 2);
    }
    let fence = "`".repeat(if n >= 3 { n } else { 1 });
    t.get(i + n..)?.find(&fence).map(|k| i + n + k + fence.len())
}

/// End of the math at `i` (`$`).
fn math_end(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            b'$' => return Some(j + 1),
            b'"' => j = string_end(b, j)?,
            b'/' if b.get(j + 1) == Some(&b'/') => j = line_end(b, j),
            b'/' if b.get(j + 1) == Some(&b'*') => j = comment_end(b, j)?,
            _ => j += 1,
        }
    }
    None
}

fn ident_end(t: &str, i: usize) -> Option<usize> {
    let rest = t.get(i..)?;
    let first = rest.chars().next()?;
    if !(first.is_alphabetic() || first == '_') {
        return None;
    }
    let len = rest
        .char_indices()
        .find(|&(_, c)| !(c.is_alphanumeric() || c == '_' || c == '-'))
        .map_or(rest.len(), |(k, _)| k);
    Some(i + len)
}

fn label_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | ':' | '.')
}

/// End (after `>`) of the label `<name>` at `i`.
fn label_end(t: &str, i: usize) -> Option<usize> {
    let rest = t.get(i + 1..)?;
    let len = rest.char_indices().find(|&(_, c)| !label_char(c)).map_or(rest.len(), |(k, _)| k);
    (len > 0 && rest[len..].starts_with('>')).then_some(i + 1 + len + 1)
}

/// The contents of a string literal (escapes decoded).
fn decode(inner: &str) -> String {
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('u') => {
                let rest = chars.as_str();
                if let Some(body) = rest.strip_prefix('{')
                    && let Some(close) = body.find('}')
                    && let Some(ch) = u32::from_str_radix(&body[..close], 16).ok().and_then(char::from_u32)
                {
                    out.push(ch);
                    chars = body[close + 1..].chars();
                } else {
                    out.push('u');
                }
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FrameKind {
    Top,
    Content,
    Paren,
    Brace,
    Stmt,
}

impl FrameKind {
    fn closer(self) -> Option<u8> {
        match self {
            FrameKind::Content => Some(b']'),
            FrameKind::Paren => Some(b')'),
            FrameKind::Brace => Some(b'}'),
            FrameKind::Top | FrameKind::Stmt => None,
        }
    }

    fn markup(self) -> bool {
        matches!(self, FrameKind::Top | FrameKind::Content)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Theorem,
    RefCite,
    Other,
}

#[derive(Clone, Copy)]
struct Call {
    start: usize,
    decl: Option<usize>,
    role: Role,
}

#[derive(Clone, Copy)]
struct Frame {
    kind: FrameKind,
    /// The call whose argument group this frame is.
    call: Option<Call>,
    /// The frame belongs to an expression embedded in markup with `#`.
    chain: bool,
}

struct Arg {
    name: Option<Range<usize>>,
    value: Range<usize>,
}

struct Typst<'t> {
    t: &'t str,
    out: Builder<'t>,
    budget: Budget,
    frames: Vec<Frame>,
    open_decls: Vec<usize>,
    heading: Option<String>,
    /// A theorem call just ended in markup: a following label names it.
    pending_label: Option<usize>,
    line_start: bool,
    /// Theorem functions defined in this file (`#let NAME = thmbox(…)`).
    thm: HashMap<String, Option<EntityKind>>,
    noted_commentary: bool,
    stopped: bool,
}

impl<'t> Typst<'t> {
    fn run(&mut self) {
        self.frames.push(Frame { kind: FrameKind::Top, call: None, chain: false });
        let n = self.t.len();
        let mut i = 0;
        while i < n && !self.stopped {
            let top = self.frames.last().map_or(FrameKind::Top, |f| f.kind);
            let next = if top.markup() { self.markup(i) } else { self.code(i, top) };
            i = if next > i { next } else { i + char_len_at(self.t, i) };
        }
        let end = trim_end_pos(self.t, n);
        let mut unbalanced = false;
        while let Some(f) = self.frames.pop() {
            if matches!(f.kind, FrameKind::Content | FrameKind::Paren | FrameKind::Brace) {
                unbalanced = true;
            }
        }
        if unbalanced && !self.stopped {
            self.out.partial("brackets are not balanced at the end of the file");
        }
        for d in std::mem::take(&mut self.open_decls) {
            self.out.close(d, end);
        }
    }

    fn b(&self) -> &'t [u8] {
        self.t.as_bytes()
    }

    fn from(&self) -> LinkFrom {
        self.open_decls.last().map_or(LinkFrom::Resource, |&d| LinkFrom::Declaration(d))
    }

    fn thm_kind(&self, name: &str) -> Option<EntityKind> {
        match self.thm.get(name) {
            Some(kind) => *kind,
            None => default_kind(name),
        }
    }

    fn push_frame(&mut self, kind: FrameKind, call: Option<Call>, chain: bool) -> bool {
        if self.frames.len() >= MAX_FRAMES {
            self.out.partial("brackets are nested too deeply; the rest of the file was not read");
            self.stopped = true;
            return false;
        }
        self.frames.push(Frame { kind, call, chain });
        true
    }

    /// Push a frame for the bracket at `i`; returns the offset after it.
    fn push(&mut self, i: usize, kind: FrameKind, call: Option<Call>, chain: bool) -> usize {
        if self.push_frame(kind, call, chain) { i + 1 } else { self.t.len() }
    }

    fn unterminated(&mut self, what: &str) -> usize {
        self.out.partial(format!("unterminated {what} hides the rest of the file"));
        self.t.len()
    }

    fn skip_lexical(&mut self, i: usize) -> Option<usize> {
        let b = self.b();
        Some(match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => line_end(b, i),
            b'/' if b.get(i + 1) == Some(&b'*') => comment_end(b, i).unwrap_or_else(|| self.unterminated("comment")),
            b'`' => raw_end(self.t, i).unwrap_or_else(|| self.unterminated("raw text")),
            b'$' => math_end(b, i).unwrap_or_else(|| self.unterminated("math")),
            _ => return None,
        })
    }

    fn markup(&mut self, i: usize) -> usize {
        let b = self.b();
        let c = b[i];
        match c {
            b'\n' => {
                self.line_start = true;
                return i + 1;
            }
            b' ' | b'\t' | b'\r' => return i + 1,
            _ => {}
        }
        let at_line_start = std::mem::replace(&mut self.line_start, false);
        if c != b'<' {
            self.pending_label = None;
        }
        if let Some(next) = self.skip_lexical(i) {
            return next;
        }
        match c {
            b'\\' => i + 1 + char_len_at(self.t, i + 1),
            b'#' => self.hash(i),
            b'<' => self.markup_label(i),
            b'@' => self.reference(i),
            b'[' => self.push(i, FrameKind::Content, None, false),
            b']' => self.close(i, b']', true),
            b'=' if at_line_start => self.heading(i),
            b'h' => self.url(i),
            _ => i + char_len_at(self.t, i),
        }
    }

    fn code(&mut self, i: usize, top: FrameKind) -> usize {
        let b = self.b();
        let c = b[i];
        match c {
            b'\n' => {
                self.line_start = true;
                if top == FrameKind::Stmt {
                    self.frames.pop();
                }
                return i + 1;
            }
            b' ' | b'\t' | b'\r' => return i + 1,
            _ => {}
        }
        self.line_start = false;
        if let Some(next) = self.skip_lexical(i) {
            return next;
        }
        match c {
            b';' if top == FrameKind::Stmt => {
                self.frames.pop();
                i + 1
            }
            b'"' => string_end(b, i).unwrap_or_else(|| self.unterminated("string")),
            b'(' => self.push(i, FrameKind::Paren, None, false),
            b'{' => self.push(i, FrameKind::Brace, None, false),
            b'[' => self.push(i, FrameKind::Content, None, false),
            b')' | b'}' | b']' => self.close(i, c, false),
            b'<' => label_end(self.t, i).unwrap_or(i + 1),
            _ => match ident_end(self.t, i) {
                Some(e) => self.code_ident(i, e),
                None => i + char_len_at(self.t, i),
            },
        }
    }

    fn close(&mut self, i: usize, closer: u8, markup: bool) -> usize {
        let len = self.frames.len();
        let found = (len.saturating_sub(32)..len).rev().find(|&k| self.frames[k].kind.closer() == Some(closer));
        let Some(k) = found else {
            if !markup {
                self.out.partial("an unmatched closing bracket was ignored");
            }
            return i + 1;
        };
        while self.frames.len() > k + 1 {
            if let Some(f) = self.frames.pop() {
                if f.kind != FrameKind::Stmt {
                    self.out.partial("brackets are not balanced");
                }
                if let Some(call) = f.call {
                    self.end_call(call, i, false);
                }
            }
        }
        let Some(f) = self.frames.pop() else { return i + 1 };
        let after = i + 1;
        if let Some(call) = f.call {
            match self.b().get(after) {
                Some(b'(') => return self.push(after, FrameKind::Paren, Some(call), f.chain),
                Some(b'[') => return self.push(after, FrameKind::Content, Some(call), f.chain),
                _ => self.end_call(call, after, f.chain),
            }
        }
        if f.chain {
            let start = f.call.map_or(i, |c| c.start);
            if self.b().get(after) == Some(&b'.')
                && let Some(e) = ident_end(self.t, after + 1)
            {
                return self.chain(start, after + 1, e);
            }
        }
        after
    }

    fn end_call(&mut self, call: Call, end: usize, in_markup: bool) {
        let Some(d) = call.decl else { return };
        self.out.close(d, end);
        if let Some(p) = self.open_decls.iter().rposition(|&x| x == d) {
            self.open_decls.remove(p);
        }
        if in_markup {
            self.pending_label = Some(d);
        }
    }

    /// `#` in markup: an embedded expression.
    fn hash(&mut self, i: usize) -> usize {
        match self.b().get(i + 1) {
            Some(b'(') => self.push(i + 1, FrameKind::Paren, None, true),
            Some(b'{') => self.push(i + 1, FrameKind::Brace, None, true),
            Some(b'[') => self.push(i + 1, FrameKind::Content, None, true),
            Some(b'"') => string_end(self.b(), i + 1).unwrap_or_else(|| self.unterminated("string")),
            _ => match ident_end(self.t, i + 1) {
                None => i + 1,
                Some(e) => {
                    let word = &self.t[i + 1..e];
                    if STATEMENT_KEYWORDS.contains(&word) {
                        if !self.push_frame(FrameKind::Stmt, None, false) {
                            return self.t.len();
                        }
                        return self.keyword(word, i + 1, e);
                    }
                    self.chain(i, i + 1, e)
                }
            },
        }
    }

    /// An identifier chain in markup (`#a.b(…)[…]`): calls and field access.
    fn chain(&mut self, start: usize, mut s: usize, mut e: usize) -> usize {
        loop {
            match self.b().get(e) {
                Some(b'(' | b'[') => {
                    let name = &self.t[s..e];
                    return self.open_call(name, start, e, true);
                }
                Some(b'.') => match ident_end(self.t, e + 1) {
                    Some(ne) => {
                        s = e + 1;
                        e = ne;
                    }
                    None => return e,
                },
                _ => return e,
            }
        }
    }

    fn code_ident(&mut self, s: usize, e: usize) -> usize {
        let name = &self.t[s..e];
        if matches!(name, "import" | "include" | "let") {
            return self.keyword(name, s, e);
        }
        match self.b().get(e) {
            Some(b'(' | b'[') => self.open_call(name, s, e, false),
            _ => e,
        }
    }

    fn skip_spaces(&self, mut i: usize) -> usize {
        let b = self.b();
        while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
            i += 1;
        }
        i
    }

    fn keyword(&mut self, word: &str, s: usize, e: usize) -> usize {
        match word {
            "import" | "include" => self.import(word, s, e),
            "let" => {
                self.let_peek(e);
                e
            }
            _ => e,
        }
    }

    fn import(&mut self, word: &str, s: usize, e: usize) -> usize {
        let j = self.skip_spaces(e);
        let b = self.b();
        if b.get(j) == Some(&b'"') {
            let Some(end) = string_end(b, j) else { return self.unterminated("string") };
            let path = decode(&self.t[j + 1..end - 1]);
            let package = word == "import" && path.starts_with('@');
            if !package && !path.trim().is_empty() {
                let target = LinkTarget::Path { written: path, base: PathBase::File, extensions: Vec::new() };
                self.out.link(RelationKind::Imports, LinkFrom::Resource, target, s..end);
            }
            return end;
        }
        // `import module: …` imports from a module value, not a file.
        let module_value = word == "import" && ident_end(self.t, j).is_some();
        if !module_value && j < b.len() && b[j] != b'\n' {
            self.out.partial(format!("#{word} with a non-literal path is not interpreted"));
        }
        j
    }

    /// `let NAME = thmbox("id", "Title", …)` (ctheorems) defines a theorem
    /// function in this file.
    fn let_peek(&mut self, e: usize) {
        let t = self.t;
        let b = self.b();
        let s1 = self.skip_spaces(e);
        let Some(ne) = ident_end(t, s1) else { return };
        let k = self.skip_spaces(ne);
        if b.get(k) != Some(&b'=') || b.get(k + 1) == Some(&b'=') {
            return;
        }
        let f = self.skip_spaces(k + 1);
        let Some(fe) = ident_end(t, f) else { return };
        let func = &t[f..fe];
        if !matches!(func, "thmbox" | "thmplain" | "thmproof") || b.get(fe) != Some(&b'(') {
            return;
        }
        let Some((args, _)) = self.args(fe) else { return };
        let positional: Vec<String> =
            args.iter().filter(|a| a.name.is_none()).filter_map(|a| self.string_value(a.value.clone())).collect();
        let id = positional.first().map(String::as_str);
        let title = positional.get(1).map(String::as_str);
        let kind = if func == "thmproof" {
            Some(EntityKind::Proof)
        } else {
            title.and_then(kind_by_words).or_else(|| id.and_then(default_kind)).or_else(|| id.and_then(kind_by_words))
        };
        let name = &t[s1..ne];
        if kind.is_none() {
            let shown = normalize(title.or(id).unwrap_or(""), 60);
            self.out.note(format!("{} ({shown}) is not admitted as an entity", normalize(name, 60)));
        }
        self.thm.insert(name.to_owned(), kind);
    }

    fn open_call(&mut self, name: &str, start: usize, open: usize, chain: bool) -> usize {
        let paren = self.b()[open] == b'(';
        let parent_role = self.frames.last().and_then(|f| f.call).map(|c| c.role);
        let mut decl = None;
        let kind = self.thm_kind(name);
        let kind = kind.filter(|_| self.out.may_nest(self.open_decls.len()));
        let role = if let Some(kind) = kind {
            let title = if paren { self.call_title(open) } else { None };
            let header = match &title {
                Some(t) => format!("#{name}({t})"),
                None => format!("#{name}"),
            };
            decl = self.out.open(DeclSpec {
                kind,
                syntax: format!("typst.call:{name}"),
                name: None,
                title,
                start,
                header: &header,
                context: self.heading.as_deref(),
                attributes: BTreeMap::new(),
            });
            if let Some(d) = decl {
                self.open_decls.push(d);
            }
            Role::Theorem
        } else {
            if commentary(name) && !self.thm.contains_key(name) && !self.noted_commentary {
                self.noted_commentary = true;
                self.out.note("remarks and examples are not admitted as entities");
            }
            if paren {
                self.special(name, start, open, parent_role);
            }
            if matches!(name, "ref" | "cite") { Role::RefCite } else { Role::Other }
        };
        let kind = if paren { FrameKind::Paren } else { FrameKind::Content };
        self.push(open, kind, Some(Call { start, decl, role }), chain)
    }

    /// Calls whose literal arguments are links or labels.
    fn special(&mut self, name: &str, start: usize, open: usize, parent_role: Option<Role>) {
        if !matches!(name, "image" | "bibliography" | "ref" | "cite" | "label") {
            return;
        }
        let Some((args, end)) = self.args(open) else { return };
        let mut positional = args.iter().filter(|a| a.name.is_none()).map(|a| a.value.clone());
        let first = positional.next();
        match name {
            "image" => {
                let Some(v) = first else { return };
                match self.string_value(v).filter(|p| !p.trim().is_empty()) {
                    Some(path) => self.path_link(RelationKind::UsesAsset, path, start..end),
                    None => self.out.partial("image with a non-literal path is not interpreted"),
                }
            }
            "bibliography" => {
                let Some(v) = first else { return };
                if let Some(path) = self.string_value(v.clone()) {
                    self.path_link(RelationKind::Imports, path, start..end);
                } else if self.b().get(v.start) == Some(&b'(')
                    && let Some((items, _)) = self.args(v.start)
                {
                    for item in items {
                        match self.string_value(item.value) {
                            Some(path) => self.path_link(RelationKind::Imports, path, start..end),
                            None => self.out.partial("bibliography with a non-literal path is not interpreted"),
                        }
                    }
                } else {
                    self.out.partial("bibliography with a non-literal path is not interpreted");
                }
            }
            "ref" => {
                let Some(v) = first else { return };
                match self.label_value(v) {
                    Some(key) => self.key_link(LinkTarget::LabelOrCitation, &key, start..end),
                    None => self.out.partial("ref with a non-literal label is not interpreted"),
                }
            }
            "cite" => {
                for v in first.into_iter().chain(positional) {
                    let key = self.label_value(v.clone()).or_else(|| self.string_value(v));
                    match key {
                        Some(key) => self.key_link(LinkTarget::Citation, &key, start..end),
                        None => self.out.partial("cite with a non-literal key is not interpreted"),
                    }
                }
            }
            "label" if parent_role != Some(Role::RefCite) => {
                if let Some(v) = first
                    && let Some(label) = self.string_value(v)
                {
                    self.out.label(&label);
                }
            }
            _ => {}
        }
    }

    fn path_link(&mut self, kind: RelationKind, written: String, range: Range<usize>) {
        if written.trim().is_empty() {
            return;
        }
        let target = LinkTarget::Path { written, base: PathBase::File, extensions: Vec::new() };
        self.out.link(kind, LinkFrom::Resource, target, range);
    }

    fn key_link(&mut self, target: fn(String) -> LinkTarget, key: &str, range: Range<usize>) {
        if let Some(key) = self.out.checked_name(key) {
            let from = self.from();
            self.out.link(RelationKind::References, from, target(key), range);
        }
    }

    /// The top-level arguments of the call group at `open` (`(`).
    fn args(&mut self, open: usize) -> Option<(Vec<Arg>, usize)> {
        let t = self.t;
        let b = self.b();
        let limit = open.saturating_add(self.budget.allow(ARGS_LIMIT)).min(b.len());
        let mut stack: Vec<u8> = Vec::new();
        let mut out = Vec::new();
        let mut arg_start = open + 1;
        let mut j = open + 1;
        while j < limit {
            let c = b[j];
            if stack.last() == Some(&b']') {
                // Markup inside a content block.
                j = match c {
                    b'\\' => j + 1 + char_len_at(t, j + 1),
                    b'[' => {
                        stack.push(b']');
                        j + 1
                    }
                    b']' => {
                        stack.pop();
                        j + 1
                    }
                    b'`' => raw_end(t, j)?,
                    b'$' => math_end(b, j)?,
                    b'/' if b.get(j + 1) == Some(&b'/') => line_end(b, j),
                    b'/' if b.get(j + 1) == Some(&b'*') => comment_end(b, j)?,
                    _ => j + 1,
                };
            } else {
                j = match c {
                    b'"' => string_end(b, j)?,
                    b'`' => raw_end(t, j)?,
                    b'$' => math_end(b, j)?,
                    b'/' if b.get(j + 1) == Some(&b'/') => line_end(b, j),
                    b'/' if b.get(j + 1) == Some(&b'*') => comment_end(b, j)?,
                    b'(' | b'[' | b'{' => {
                        stack.push(match c {
                            b'(' => b')',
                            b'[' => b']',
                            _ => b'}',
                        });
                        j + 1
                    }
                    b')' | b'}' => {
                        if stack.last() == Some(&c) {
                            stack.pop();
                        } else if stack.is_empty() && c == b')' {
                            self.push_arg(&mut out, arg_start..j);
                            self.budget.spend(j + 1 - open);
                            return Some((out, j + 1));
                        } else {
                            return None;
                        }
                        j + 1
                    }
                    b',' if stack.is_empty() => {
                        self.push_arg(&mut out, arg_start..j);
                        arg_start = j + 1;
                        j + 1
                    }
                    _ => j + 1,
                };
            }
            if stack.len() > 64 || out.len() > 64 {
                return None;
            }
        }
        self.budget.spend(limit.saturating_sub(open));
        None
    }

    fn push_arg(&self, out: &mut Vec<Arg>, range: Range<usize>) {
        let Some(raw) = self.t.get(range.clone()) else { return };
        let lead = raw.len() - raw.trim_start().len();
        let s = range.start + lead;
        let e = range.start + raw.trim_end().len();
        if s >= e {
            return;
        }
        if let Some(ne) = ident_end(self.t, s).filter(|&ne| ne < e) {
            let k = self.skip_spaces(ne);
            if self.b().get(k) == Some(&b':') && self.b().get(k + 1) != Some(&b':') && k < e {
                let vs = self.skip_spaces(k + 1).min(e);
                let vs = vs + self.t[vs..e].len() - self.t[vs..e].trim_start().len();
                out.push(Arg { name: Some(s..ne), value: vs..e });
                return;
            }
        }
        out.push(Arg { name: None, value: s..e });
    }

    /// The value of a string literal argument.
    fn string_value(&self, v: Range<usize>) -> Option<String> {
        let b = self.b();
        (b.get(v.start) == Some(&b'"') && string_end(b, v.start) == Some(v.end))
            .then(|| decode(&self.t[v.start + 1..v.end - 1]))
    }

    /// `<x>` or `label("x")`.
    fn label_value(&self, v: Range<usize>) -> Option<String> {
        if label_end(self.t, v.start) == Some(v.end) && self.b().get(v.start) == Some(&b'<') {
            return Some(self.t[v.start + 1..v.end - 1].to_owned());
        }
        let raw = self.t.get(v.clone())?;
        let inner = raw.strip_prefix("label(")?.strip_suffix(')')?;
        let lead = inner.len() - inner.trim_start().len();
        let s = v.start + "label(".len() + lead;
        self.string_value(s..s + inner.trim().len())
    }

    /// `title:`/`name:` (string or plain content), else the first
    /// positional string.
    fn call_title(&mut self, open: usize) -> Option<String> {
        let (args, _) = self.args(open)?;
        let named =
            args.iter().find(|a| a.name.as_ref().is_some_and(|n| matches!(&self.t[n.clone()], "title" | "name")));
        let title = match named {
            Some(a) => self.string_value(a.value.clone()).or_else(|| {
                let raw = &self.t[a.value.clone()];
                let inner = raw.strip_prefix('[')?.strip_suffix(']')?;
                (!inner.contains(['#', '[', ']', '$', '<', '@', '\\'])).then(|| inner.to_owned())
            }),
            None => args.iter().filter(|a| a.name.is_none()).find_map(|a| self.string_value(a.value.clone())),
        }?;
        let title = normalize(&title, 200);
        (!title.is_empty()).then_some(title)
    }

    fn markup_label(&mut self, i: usize) -> usize {
        let pending = self.pending_label.take();
        let Some(end) = label_end(self.t, i) else { return i + 1 };
        if let Some(name) = self.out.label(&self.t[i + 1..end - 1])
            && let Some(d) = pending
            && !self.out.has_name(d)
        {
            self.out.set_name(d, name);
        }
        end
    }

    fn reference(&mut self, i: usize) -> usize {
        if self.t[..i].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_') {
            return i + 1; // an e-mail address
        }
        let rest = &self.t[i + 1..];
        let len = rest.char_indices().find(|&(_, c)| !label_char(c)).map_or(rest.len(), |(k, _)| k);
        let name = rest[..len].trim_end_matches(['.', ':']);
        if name.is_empty() {
            return i + 1;
        }
        let end = i + 1 + name.len();
        self.key_link(LinkTarget::LabelOrCitation, name, i..end);
        end
    }

    fn heading(&mut self, i: usize) -> usize {
        let b = self.b();
        let mut j = i;
        while b.get(j) == Some(&b'=') {
            j += 1;
        }
        if !matches!(b.get(j), Some(b' ' | b'\t')) {
            return j;
        }
        let le = line_end(b, j);
        let mut title = self.t[j..le].trim();
        if let Some(cut) = title.find(" //") {
            title = title[..cut].trim_end();
        }
        if title.ends_with('>')
            && let Some(lt) = title.rfind('<')
            && label_end(title, lt) == Some(title.len())
        {
            title = title[..lt].trim_end();
        }
        let title = normalize(title, 200);
        self.heading = (!title.is_empty()).then_some(title);
        j
    }

    /// Skip a bare URL in markup (its `//` is not a comment).
    fn url(&mut self, i: usize) -> usize {
        let rest = &self.t[i..];
        let prev_word = self.t[..i].chars().next_back().is_some_and(char::is_alphanumeric);
        if prev_word || !(rest.starts_with("http://") || rest.starts_with("https://")) {
            return i + 1;
        }
        let len = rest
            .char_indices()
            .find(|&(_, c)| c.is_whitespace() || matches!(c, '<' | '>' | '[' | ']' | '(' | ')' | '"'))
            .map_or(rest.len(), |(k, _)| k);
        i + len
    }
}

#[cfg(test)]
mod tests {
    use super::super::latex::common::{check_invariants, garbage, slice, truncations};
    use super::*;

    fn run(text: &str) -> Extraction {
        let x = extract(&WorkspacePath::parse("paper/main.typ").unwrap(), text);
        check_invariants(text, &x);
        x
    }

    fn links(x: &Extraction, kind: RelationKind) -> Vec<(LinkFrom, LinkTarget)> {
        x.links.iter().filter(|l| l.kind == kind).map(|l| (l.from, l.target.clone())).collect()
    }

    fn path(w: &str) -> LinkTarget {
        LinkTarget::Path { written: w.into(), base: PathBase::File, extensions: Vec::new() }
    }

    fn either(s: &str) -> LinkTarget {
        LinkTarget::LabelOrCitation(s.into())
    }

    const SAMPLE: &str = r##"#import "@preview/ctheorems:1.1.3": *
#import "lib/defs.typ": helper
#include "chapters/intro.typ"
#let theorem = thmbox("theorem", "Theorem", fill: rgb("#eeffee"))
#let remark = thmplain("remark", "Remark")
#let lemma = thmbox("lemma", "Hilfssatz")
#show: thmrules

= Introduction <sec:intro>
// #theorem[commented out] <thm:ghost>
/* block #lemma[also out] /* nested */ still */
Write to me at someone@example.com. See @thm:main and @knuth1984.
`#theorem[raw text] @no-ref` and https://example.com/a//b@c.

== Results
#theorem("Main result")[
  For all $x @nomath$ we have #ref(<eq:one>) and #cite(<knuth1984>).
  #figure(image("figs/plot.png", width: 50%), caption: [Plot]) <fig:plot>
] <thm:main>

#proof[
  By @thm:main.
]
#lemma(title: "Aux")[aux é]
#remark[not an entity]
#definition[Def body]<def:x>
#image(path)
#bibliography(("refs.bib", "more.bib"))
#label("manual")
#cite(label("lamport"))
#let s = "a // not a comment @no"
"##;

    #[test]
    fn theorem_calls_labels_and_titles() {
        let x = run(SAMPLE);
        let kinds: Vec<_> = x.declarations.iter().map(|d| (d.kind, d.syntax.as_str())).collect();
        assert_eq!(
            kinds,
            [
                (EntityKind::Theorem, "typst.call:theorem"),
                (EntityKind::Proof, "typst.call:proof"),
                (EntityKind::Lemma, "typst.call:lemma"),
                (EntityKind::Definition, "typst.call:definition"),
            ]
        );
        let thm = &x.declarations[0];
        assert_eq!(thm.name.as_deref(), Some("thm:main"), "the figure label is not the theorem's name");
        assert_eq!(thm.title.as_deref(), Some("Main result"));
        assert_eq!(thm.context.as_deref(), Some("Results"));
        let body = slice(SAMPLE, thm.range);
        assert!(body.starts_with("#theorem(\"Main result\")[") && body.ends_with(']'), "{body}");
        assert_eq!(x.declarations[1].name, None);
        assert_eq!(x.declarations[2].title.as_deref(), Some("Aux"));
        assert_eq!(slice(SAMPLE, x.declarations[2].range), "#lemma(title: \"Aux\")[aux é]");
        assert_eq!(x.declarations[3].name.as_deref(), Some("def:x"));
        for l in ["sec:intro", "fig:plot", "thm:main", "def:x", "manual"] {
            assert!(x.labels.contains(&l.to_owned()), "{l}");
        }
        assert!(!x.labels.contains(&"thm:ghost".to_owned()));
        assert!(
            x.labels.iter().all(|l| l != "lamport" && l != "eq:one"),
            "label values are references, not definitions"
        );
        assert!(x.notes.iter().any(|n| n == "remark (Remark) is not admitted as an entity"), "{:?}", x.notes);
        assert_eq!(x.partial, ["image with a non-literal path is not interpreted"]);
    }

    #[test]
    fn references_citations_and_files() {
        let x = run(SAMPLE);
        assert_eq!(
            links(&x, RelationKind::References),
            [
                (LinkFrom::Resource, either("thm:main")),
                (LinkFrom::Resource, either("knuth1984")),
                (LinkFrom::Declaration(0), either("eq:one")),
                (LinkFrom::Declaration(0), LinkTarget::Citation("knuth1984".into())),
                (LinkFrom::Declaration(1), either("thm:main")),
                (LinkFrom::Resource, LinkTarget::Citation("lamport".into())),
            ]
        );
        assert_eq!(
            links(&x, RelationKind::Imports),
            [
                (LinkFrom::Resource, path("lib/defs.typ")),
                (LinkFrom::Resource, path("chapters/intro.typ")),
                (LinkFrom::Resource, path("refs.bib")),
                (LinkFrom::Resource, path("more.bib")),
            ]
        );
        assert_eq!(links(&x, RelationKind::UsesAsset), [(LinkFrom::Resource, path("figs/plot.png"))]);
        let at = x.links.iter().find(|l| l.target == either("thm:main")).unwrap();
        assert_eq!(slice(SAMPLE, at.range), "@thm:main");
    }

    #[test]
    fn real_fixture() {
        let main = include_str!("../../../../../tests/fixtures/m6-paper/main.typ");
        let x = run(main);
        assert!(x.declarations.is_empty() && x.links.is_empty() && x.labels.is_empty());
        assert!(x.partial.is_empty(), "{:?}", x.partial);
        assert!(x.attachments.is_empty());
    }

    #[test]
    fn code_blocks_statements_and_nesting() {
        let text = "#{\n  let x = 1\n  theorem[in code] \n  import \"inner.typ\"\n}\n#if true [#lemma[cond]] else [x]\n\
                    #let f(body) = [#body]\n= Heading // comment\n#corollary[c] <cor:a>\n";
        let x = run(text);
        let kinds: Vec<_> = x.declarations.iter().map(|d| d.kind).collect();
        assert_eq!(kinds, [EntityKind::Theorem, EntityKind::Lemma, EntityKind::Corollary]);
        assert_eq!(slice(text, x.declarations[0].range), "theorem[in code]");
        assert_eq!(x.declarations[2].name.as_deref(), Some("cor:a"));
        assert_eq!(x.declarations[2].context.as_deref(), Some("Heading"));
        assert_eq!(links(&x, RelationKind::Imports), [(LinkFrom::Resource, path("inner.typ"))]);
        assert!(x.partial.is_empty(), "{:?}", x.partial);
    }

    #[test]
    fn attachments() {
        let text =
            "// exposition:begin fig-a\n#figure(image(\"a.png\"))\n// exposition:end fig-a\n// exposition:end other\n";
        let x = run(text);
        assert_eq!(x.attachments.len(), 1);
        assert_eq!(x.attachments[0].image.as_deref(), Some("a.png"));
        assert_eq!(x.partial, ["attachment markers for key other are unpaired or duplicated"]);
    }

    #[test]
    fn robustness() {
        for text in [
            "",
            "#",
            "#theorem[",
            "#theorem(",
            "#theorem(\"",
            "$",
            "`",
            "```",
            "/*",
            "/* /*",
            "<",
            "@",
            "#import",
            "#import \"",
            "#let x = thmbox(",
            "]]]",
            ")))",
            "#{",
            "#(",
            "= ",
            "\\",
            "#é[",
            "@é",
            "#image(\"é",
            "#cite(label(\"",
            "#ref(<",
        ] {
            run(text);
        }
        truncations(SAMPLE, 1, |t| extract(&WorkspacePath::parse("a.typ").unwrap(), t));
        let alphabet = [
            "#theorem",
            "[",
            "]",
            "(",
            ")",
            "{",
            "}",
            "\"",
            "<a>",
            "@b",
            "$",
            "`",
            "//",
            "/*",
            "*/",
            "\n",
            "= ",
            "é",
            " ",
            "#let ",
            "image(\"x\")",
            ",",
            ":",
        ];
        for seed in 0..40 {
            run(&garbage(seed, 400, &alphabet));
        }
        run(&"#theorem[".repeat(1000));
        run(&"#image(".repeat(20_000));
        run(&"(".repeat(100_000));
    }
}
