//! LaTeX extractor (`latex/1`).
//!
//! Reads literal constructs only: theorem-like environments (the standard
//! names plus literal `\newtheorem`/`\declaretheorem` in the same file),
//! labels, references, citations, `\input`-style includes, graphics, and
//! bibliography files. Comments, `\verb`, and verbatim-like environments are
//! masked first so commented-out constructs are not seen. Macros are never
//! expanded: a definition that could hide tracked constructs is reported as
//! partial coverage.
//!
//! The [`common`] module holds helpers shared by all format extractors.

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;

use exposition_core::path::WorkspacePath;

use self::common::{
    Budget, Builder, DeclSpec, char_len_at, commentary, default_kind, is_statement, kind_by_words, normalize,
    trim_end_pos,
};
use super::markers::MarkerFormat;
use super::{Extraction, LinkFrom, LinkTarget, PathBase};
use crate::vocabulary::{EntityKind, RelationKind};

pub const EXTRACTOR: &str = "latex/1";

const VERBATIM_ENVS: &[&str] = &[
    "verbatim",
    "verbatim*",
    "Verbatim",
    "Verbatim*",
    "BVerbatim",
    "LVerbatim",
    "lstlisting",
    "minted",
    "comment",
    "filecontents",
    "filecontents*",
];

const REF_COMMANDS: &[&str] = &["ref", "eqref", "cref", "Cref", "autoref", "pageref", "vref", "nameref"];
/// Reference commands whose argument is a comma-separated list.
const REF_LIST_COMMANDS: &[&str] = &["cref", "Cref", "vref"];
const CITE_COMMANDS: &[&str] = &[
    "cite",
    "citep",
    "citet",
    "citeauthor",
    "citeyear",
    "parencite",
    "textcite",
    "autocite",
    "footcite",
    "fullcite",
    "nocite",
    "Cite",
    "Citep",
    "Citet",
    "Citeauthor",
    "Parencite",
    "Textcite",
    "Autocite",
    "citealt",
    "citealp",
];
const INPUT_COMMANDS: &[&str] = &["input", "include", "subfile"];
const IMPORT_COMMANDS: &[&str] = &["import", "subimport"];
const HEADING_COMMANDS: &[&str] = &["part", "chapter", "section", "subsection", "subsubsection"];
const GRAPHICS_EXTENSIONS: &[&str] = &["pdf", "png", "jpg", "jpeg", "eps", "svg"];
/// Commands whose use inside a macro body hides a tracked construct.
const TRACKED_COMMANDS: &[&str] = &[
    "label",
    "input",
    "include",
    "subfile",
    "import",
    "subimport",
    "includegraphics",
    "bibliography",
    "addbibresource",
    "newtheorem",
    "declaretheorem",
];
const MAX_ENV_DEPTH: usize = 512;
const KEY_LIMIT: usize = 2048;
const TITLE_LIMIT: usize = 4096;
const BODY_LIMIT: usize = 1 << 16;

pub fn extract(_path: &WorkspacePath, text: &str) -> Extraction {
    let (masked, mask_partial) = mask(text);
    let mut lx = Latex {
        m: &masked,
        out: Builder::new(EXTRACTOR, text),
        budget: Budget::new(text.len()),
        envs: HashMap::new(),
        frames: Vec::new(),
        open_decls: Vec::new(),
        section: None,
        stmt_gap: None,
        proof_title: None,
        noted_commentary: false,
    };
    for reason in mask_partial {
        lx.out.partial(reason);
    }
    lx.run();
    if lx.budget.exhausted() {
        lx.out.partial("the file is too irregular to read completely; some arguments were not interpreted");
    }
    lx.out.attachments(MarkerFormat::Latex);
    lx.out.finish()
}

/// Blank comments, `\verb`, verbatim-like environment bodies, and text after
/// `\endinput` (keeping line breaks), so the scanner below never sees them.
/// Byte offsets are preserved.
fn mask(text: &str) -> (String, Vec<&'static str>) {
    let b = text.as_bytes();
    let mut m = b.to_vec();
    let mut partial = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let e = line_end(b, i);
                blank(&mut m, i, e);
                i = e;
            }
            b'\\' => {
                let ne = command_end(b, i + 1);
                if ne == i + 1 {
                    i += 1 + char_len_at(text, i + 1);
                    continue;
                }
                match &text[i + 1..ne] {
                    "verb" => {
                        let mut j = ne;
                        if b.get(j) == Some(&b'*') {
                            j += 1;
                        }
                        match text.get(j..).and_then(|r| r.chars().next()) {
                            Some(d) if !d.is_whitespace() && !d.is_alphabetic() => {
                                let start = j + d.len_utf8();
                                let le = line_end(b, start);
                                let close = text[start..le].find(d).map_or(le, |k| start + k + d.len_utf8());
                                blank(&mut m, i, close);
                                i = close;
                            }
                            _ => i = ne,
                        }
                    }
                    "begin" => {
                        let open = skip_ws(b, ne);
                        let env_end = (b.get(open) == Some(&b'{'))
                            .then(|| text.get(open + 1..).and_then(|r| r.find('}')).map(|k| open + 1 + k))
                            .flatten()
                            .filter(|&e| e - open < 64);
                        match env_end {
                            Some(e) if VERBATIM_ENVS.contains(&text[open + 1..e].trim()) => {
                                let env = text[open + 1..e].trim();
                                let after = e + 1;
                                let closing = format!("\\end{{{env}}}");
                                match text[after..].find(&closing) {
                                    Some(k) => {
                                        blank(&mut m, after, after + k);
                                        i = after + k;
                                    }
                                    None => {
                                        blank(&mut m, after, b.len());
                                        partial.push("a verbatim environment is not closed");
                                        i = b.len();
                                    }
                                }
                            }
                            _ => i = ne,
                        }
                    }
                    "endinput" => {
                        let e = line_end(b, ne);
                        blank(&mut m, e, b.len());
                        i = b.len();
                    }
                    _ => i = ne,
                }
            }
            _ => i += 1,
        }
    }
    // Only whole characters were replaced by ASCII spaces, so this is valid.
    let masked = String::from_utf8(m).unwrap_or_else(|_| text.to_owned());
    (masked, partial)
}

fn blank(m: &mut [u8], from: usize, to: usize) {
    let to = to.min(m.len());
    for byte in &mut m[from.min(to)..to] {
        if *byte != b'\n' && *byte != b'\r' {
            *byte = b' ';
        }
    }
}

fn line_end(b: &[u8], from: usize) -> usize {
    b.get(from..).and_then(|r| r.iter().position(|&c| c == b'\n')).map_or(b.len(), |k| from + k)
}

/// End of a command name starting at `from` (letters and `@`).
fn command_end(b: &[u8], mut from: usize) -> usize {
    while from < b.len() && (b[from].is_ascii_alphabetic() || b[from] == b'@') {
        from += 1;
    }
    from
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// A balanced `{...}` group at the first non-space byte from `i`:
/// (inner range, offset after `}`).
fn brace_group(b: &[u8], i: usize, limit: usize, budget: &mut Budget) -> Option<(Range<usize>, usize)> {
    let start = skip_ws(b, i);
    if b.get(start) != Some(&b'{') {
        return None;
    }
    let max = start.saturating_add(budget.allow(limit)).min(b.len());
    let mut depth = 0usize;
    let mut j = start;
    while j < max {
        match b[j] {
            b'\\' => j += 1,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    budget.spend(j + 1 - start);
                    return Some((start + 1..j, j + 1));
                }
            }
            _ => {}
        }
        j += 1;
    }
    budget.spend(max - start);
    None
}

/// An optional `[...]` argument at the first non-space byte from `i`: the
/// first `]` outside braces ends it.
fn bracket_group(b: &[u8], i: usize, limit: usize, budget: &mut Budget) -> Option<(Range<usize>, usize)> {
    let start = skip_ws(b, i);
    if b.get(start) != Some(&b'[') {
        return None;
    }
    let max = start.saturating_add(budget.allow(limit)).min(b.len());
    let mut depth = 0usize;
    let mut j = start + 1;
    while j < max {
        match b[j] {
            b'\\' => j += 1,
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            b']' if depth == 0 => {
                budget.spend(j + 1 - start);
                return Some((start + 1..j, j + 1));
            }
            _ => {}
        }
        j += 1;
    }
    budget.spend(max - start);
    None
}

fn literal(s: &str) -> bool {
    !s.contains(['\\', '#', '{', '}'])
}

/// Command names used in `s` (masked text), in order.
fn commands(s: &str) -> impl Iterator<Item = (usize, &str)> {
    let b = s.as_bytes();
    let mut i = 0;
    std::iter::from_fn(move || {
        while i < b.len() {
            if b[i] == b'\\' {
                let ne = command_end(b, i + 1);
                let at = i;
                i = if ne == i + 1 { i + 1 + char_len_at(s, i + 1) } else { ne };
                if ne > at + 1 {
                    return Some((at, &s[at + 1..ne]));
                }
            } else {
                i += 1;
            }
        }
        None
    })
}

enum Step {
    Continue(usize),
    Stop,
}

struct Frame {
    env: String,
    decl: Option<usize>,
}

/// The optional argument of an open proof that names its statement.
struct ProofTitle {
    decl: usize,
    range: Range<usize>,
    used: bool,
}

struct Latex<'a> {
    m: &'a str,
    out: Builder<'a>,
    budget: Budget,
    /// Environment kinds from `\newtheorem`/`\declaretheorem` in this file.
    envs: HashMap<String, Option<EntityKind>>,
    frames: Vec<Frame>,
    open_decls: Vec<usize>,
    section: Option<String>,
    /// A statement whose `\end` has been followed only by whitespace and
    /// comments so far (for the proof-after-statement convention).
    stmt_gap: Option<usize>,
    proof_title: Option<ProofTitle>,
    noted_commentary: bool,
}

impl<'a> Latex<'a> {
    fn run(&mut self) {
        let b = self.m.as_bytes();
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b'\\' => {
                    let ne = command_end(b, i + 1);
                    if ne == i + 1 {
                        self.stmt_gap = None;
                        i += 1 + char_len_at(self.m, i + 1);
                        continue;
                    }
                    let name = &self.m[i + 1..ne];
                    match self.command(name, i, ne) {
                        Step::Continue(next) => i = next.max(ne),
                        Step::Stop => return,
                    }
                }
                c if c.is_ascii_whitespace() => i += 1,
                _ => {
                    self.stmt_gap = None;
                    i += 1;
                }
            }
        }
        let end = trim_end_pos(self.m, self.m.len());
        self.close_all(end, "the end of the file");
    }

    fn command(&mut self, name: &str, at: usize, ne: usize) -> Step {
        let gap = self.stmt_gap.take();
        let next = match name {
            "begin" => self.begin(at, ne, gap),
            "end" => return self.end(at, ne),
            "label" => self.label(ne),
            n if REF_COMMANDS.contains(&n) => self.reference(n, at, ne),
            n if CITE_COMMANDS.contains(&n) => self.cite(at, ne),
            n if INPUT_COMMANDS.contains(&n) => self.input(n, at, ne),
            n if IMPORT_COMMANDS.contains(&n) => self.import(at, ne),
            "includegraphics" => self.graphics(at, ne),
            "bibliography" | "addbibresource" => self.bibliography(name, at, ne),
            n if HEADING_COMMANDS.contains(&n) => self.heading(ne),
            "newtheorem" => self.newtheorem(ne),
            "declaretheorem" => self.declaretheorem(ne),
            "newcommand" | "renewcommand" | "providecommand" | "DeclareRobustCommand" => self.newcommand(ne, false),
            "NewDocumentCommand"
            | "RenewDocumentCommand"
            | "ProvideDocumentCommand"
            | "DeclareDocumentCommand"
            | "NewExpandableDocumentCommand" => self.newcommand(ne, true),
            "def" | "gdef" | "edef" | "xdef" => self.def(ne),
            "let" => self.let_(ne),
            "newenvironment" | "renewenvironment" | "provideenvironment" => self.newenvironment(ne, false),
            "NewDocumentEnvironment"
            | "RenewDocumentEnvironment"
            | "ProvideDocumentEnvironment"
            | "DeclareDocumentEnvironment" => self.newenvironment(ne, true),
            _ => ne,
        };
        Step::Continue(next)
    }

    fn b(&self) -> &'a [u8] {
        self.m.as_bytes()
    }

    fn skip_star(&self, i: usize) -> usize {
        if self.b().get(i) == Some(&b'*') { i + 1 } else { i }
    }

    fn from(&self) -> LinkFrom {
        self.open_decls.last().map_or(LinkFrom::Resource, |&d| LinkFrom::Declaration(d))
    }

    fn env_kind(&self, base: &str) -> Option<EntityKind> {
        match self.envs.get(base) {
            Some(kind) => *kind,
            None => default_kind(base),
        }
    }

    /// A key-like argument: `Ok(Some((trimmed text, end)))` when literal,
    /// `Ok(None)` when absent, `Err(end)` when present but not literal.
    fn key(&mut self, i: usize) -> Result<Option<(&'a str, usize)>, usize> {
        let b = self.b();
        if b.get(skip_ws(b, i)) != Some(&b'{') {
            return Ok(None);
        }
        match brace_group(b, i, KEY_LIMIT, &mut self.budget) {
            Some((inner, end)) => {
                let s = self.m[inner].trim();
                if literal(s) { Ok(Some((s, end))) } else { Err(end) }
            }
            None => Err(i),
        }
    }

    fn begin(&mut self, at: usize, ne: usize, gap: Option<usize>) -> usize {
        let Some((inner, after)) = brace_group(self.b(), ne, 256, &mut self.budget) else {
            self.out.partial("a \\begin without a readable environment name was skipped");
            return ne;
        };
        let env = normalize(&self.m[inner], 128);
        if self.frames.len() >= MAX_ENV_DEPTH {
            self.out.partial("environments are nested too deeply; deeper environments were not interpreted");
            return after;
        }
        let base = env.strip_suffix('*').unwrap_or(&env);
        let kind = if literal(&env) { self.env_kind(base) } else { None };
        if !literal(&env) {
            self.out.partial("\\begin with a non-literal environment name is not interpreted");
        }
        if kind.is_none() && commentary(base) && !self.envs.contains_key(base) && !self.noted_commentary {
            self.noted_commentary = true;
            self.out.note("remarks and examples are not admitted as entities");
        }
        let mut decl = None;
        let kind = kind.filter(|_| self.out.may_nest(self.open_decls.len()));
        if let Some(kind) = kind {
            let title_group = bracket_group(self.b(), after, TITLE_LIMIT, &mut self.budget);
            let header_end = title_group.as_ref().map_or(after, |(_, e)| *e);
            let title = title_group.as_ref().map(|(r, _)| normalize(&self.m[r.clone()], 200)).filter(|t| !t.is_empty());
            let header = normalize(&self.m[at..header_end], 400);
            decl = self.out.open(DeclSpec {
                kind,
                syntax: format!("latex.env:{env}"),
                name: None,
                title,
                start: at,
                header: &header,
                context: self.section.as_deref(),
                attributes: BTreeMap::new(),
            });
            if kind == EntityKind::Proof
                && let Some(d) = decl
            {
                let named = title_group
                    .filter(|(r, _)| commands(&self.m[r.clone()]).any(|(_, c)| REF_COMMANDS.contains(&c)))
                    .map(|(r, _)| r);
                if let Some(range) = named {
                    self.proof_title = Some(ProofTitle { decl: d, range, used: false });
                } else if let Some(stmt) = gap {
                    self.out.link(
                        RelationKind::ProofOf,
                        LinkFrom::Declaration(d),
                        LinkTarget::Declaration(stmt),
                        at..after,
                    );
                    self.out.note("a proof immediately following a statement is taken as its proof");
                }
            }
        }
        self.frames.push(Frame { env, decl });
        if let Some(d) = decl {
            self.open_decls.push(d);
        }
        after
    }

    fn end(&mut self, at: usize, ne: usize) -> Step {
        let Some((inner, after)) = brace_group(self.b(), ne, 256, &mut self.budget) else {
            self.out.partial("an \\end without a readable environment name was skipped");
            return Step::Continue(ne);
        };
        let env = normalize(&self.m[inner], 128);
        if env == "document" {
            let end = trim_end_pos(self.m, at);
            self.close_all(end, "\\end{document}");
            return Step::Stop;
        }
        let depth = self.frames.iter().rev().take(32).position(|f| f.env == env);
        match depth {
            Some(k) => {
                let implicit_end = trim_end_pos(self.m, at);
                for _ in 0..k {
                    if let Some(f) = self.frames.pop() {
                        self.out.partial(format!("environment {} is not closed before \\end{{{env}}}", short(&f.env)));
                        self.close_frame(f, implicit_end, false);
                    }
                }
                if let Some(f) = self.frames.pop() {
                    self.close_frame(f, after, true);
                }
            }
            None => self.out.partial(format!("\\end{{{}}} has no matching \\begin", short(&env))),
        }
        Step::Continue(after)
    }

    fn close_frame(&mut self, f: Frame, end: usize, proper: bool) {
        let Some(d) = f.decl else { return };
        self.out.close(d, end);
        if let Some(pos) = self.open_decls.iter().rposition(|&x| x == d) {
            self.open_decls.remove(pos);
        }
        if self.proof_title.as_ref().is_some_and(|p| p.decl == d) {
            self.proof_title = None;
        }
        if proper && self.out.kind(d).is_some_and(is_statement) {
            self.stmt_gap = Some(d);
        }
    }

    fn close_all(&mut self, end: usize, at: &str) {
        while let Some(f) = self.frames.pop() {
            if f.env != "document" || at != "\\end{document}" {
                self.out.partial(format!("environment {} is not closed before {at}", short(&f.env)));
            }
            self.close_frame(f, end, false);
        }
    }

    fn label(&mut self, ne: usize) -> usize {
        match self.key(ne) {
            Ok(Some((raw, end))) => {
                if let Some(name) = self.out.label(raw)
                    && let Some(Frame { decl: Some(d), .. }) = self.frames.last()
                    && !self.out.has_name(*d)
                {
                    self.out.set_name(*d, name);
                }
                end
            }
            Ok(None) => ne,
            Err(end) => {
                self.out.partial("labels with non-literal arguments are not interpreted");
                end
            }
        }
    }

    fn reference(&mut self, name: &str, at: usize, ne: usize) -> usize {
        let i = self.skip_star(ne);
        match self.key(i) {
            Ok(Some((raw, end))) => {
                let keys: Vec<&str> = if REF_LIST_COMMANDS.contains(&name) {
                    raw.split(',').map(str::trim).filter(|k| !k.is_empty()).collect()
                } else {
                    vec![raw]
                };
                let proof_of = match &mut self.proof_title {
                    Some(p) if !p.used && p.range.contains(&at) => {
                        p.used = true;
                        Some(p.decl)
                    }
                    _ => None,
                };
                for key in keys {
                    let Some(key) = self.out.checked_name(key) else { continue };
                    match proof_of {
                        Some(d) => self.out.link(
                            RelationKind::ProofOf,
                            LinkFrom::Declaration(d),
                            LinkTarget::Label(key),
                            at..end,
                        ),
                        None => {
                            let from = self.from();
                            self.out.link(RelationKind::References, from, LinkTarget::Label(key), at..end);
                        }
                    }
                }
                end
            }
            Ok(None) => ne,
            Err(end) => {
                self.out.partial(format!("\\{name} with a non-literal argument is not interpreted"));
                end
            }
        }
    }

    fn cite(&mut self, at: usize, ne: usize) -> usize {
        let mut i = self.skip_star(ne);
        for _ in 0..2 {
            match bracket_group(self.b(), i, TITLE_LIMIT, &mut self.budget) {
                Some((_, end)) => i = end,
                None => break,
            }
        }
        match self.key(i) {
            Ok(Some((raw, end))) => {
                for key in raw.split(',').map(str::trim).filter(|k| !k.is_empty() && *k != "*") {
                    let Some(key) = self.out.checked_name(key) else { continue };
                    let from = self.from();
                    self.out.link(RelationKind::References, from, LinkTarget::Citation(key), at..end);
                }
                end
            }
            Ok(None) => i,
            Err(end) => {
                self.out.partial("citations with non-literal keys are not interpreted");
                end
            }
        }
    }

    fn path_link(&mut self, kind: RelationKind, written: &str, extensions: &[&str], range: Range<usize>) {
        if written.is_empty() {
            return;
        }
        let target = LinkTarget::Path {
            written: written.to_owned(),
            base: PathBase::LatexMain,
            extensions: extensions.iter().map(|e| (*e).to_owned()).collect(),
        };
        self.out.link(kind, LinkFrom::Resource, target, range);
    }

    fn input(&mut self, name: &str, at: usize, ne: usize) -> usize {
        match self.key(ne) {
            Ok(Some((path, end))) => {
                self.path_link(RelationKind::Imports, path, &["tex"], at..end);
                end
            }
            Ok(None) if name == "input" => {
                // TeX primitive syntax: `\input file`.
                let b = self.b();
                let start = skip_ws(b, ne);
                let mut end = start;
                while end < b.len() && !b[end].is_ascii_whitespace() && !matches!(b[end], b'\\' | b'{' | b'}' | b'%') {
                    end += 1;
                }
                if end > start && start - ne <= 2 && self.m.is_char_boundary(end) {
                    let path = &self.m[start..end];
                    if path.contains('#') {
                        self.out.partial("\\input with a non-literal path is not interpreted");
                    } else {
                        self.path_link(RelationKind::Imports, path, &["tex"], at..end);
                    }
                }
                end.max(ne)
            }
            Ok(None) => ne,
            Err(end) => {
                self.out.partial(format!("\\{name} with a non-literal path is not interpreted"));
                end
            }
        }
    }

    fn import(&mut self, at: usize, ne: usize) -> usize {
        let i = self.skip_star(ne);
        let dir = self.key(i);
        let Ok(Some((dir, after_dir))) = dir else {
            if dir.is_err() {
                self.out.partial("\\import with a non-literal path is not interpreted");
            }
            return i;
        };
        match self.key(after_dir) {
            Ok(Some((file, end))) => {
                let written = if dir.is_empty() || file.starts_with('/') {
                    file.to_owned()
                } else {
                    format!("{}/{file}", dir.trim_end_matches('/'))
                };
                self.path_link(RelationKind::Imports, &written, &["tex"], at..end);
                end
            }
            Ok(None) => after_dir,
            Err(end) => {
                self.out.partial("\\import with a non-literal path is not interpreted");
                end
            }
        }
    }

    fn graphics(&mut self, at: usize, ne: usize) -> usize {
        let mut i = self.skip_star(ne);
        for _ in 0..2 {
            match bracket_group(self.b(), i, TITLE_LIMIT, &mut self.budget) {
                Some((_, end)) => i = end,
                None => break,
            }
        }
        match self.key(i) {
            Ok(Some((path, end))) => {
                self.path_link(RelationKind::UsesAsset, path, GRAPHICS_EXTENSIONS, at..end);
                end
            }
            Ok(None) => i,
            Err(end) => {
                self.out.partial("\\includegraphics with a non-literal path is not interpreted");
                end
            }
        }
    }

    fn bibliography(&mut self, name: &str, at: usize, ne: usize) -> usize {
        let mut i = ne;
        if name == "addbibresource"
            && let Some((_, end)) = bracket_group(self.b(), i, TITLE_LIMIT, &mut self.budget)
        {
            i = end;
        }
        match self.key(i) {
            Ok(Some((raw, end))) => {
                for file in raw.split(',').map(str::trim).filter(|f| !f.is_empty()) {
                    self.path_link(RelationKind::Imports, file, &["bib"], at..end);
                }
                end
            }
            Ok(None) => i,
            Err(end) => {
                self.out.partial(format!("\\{name} with a non-literal path is not interpreted"));
                end
            }
        }
    }

    fn heading(&mut self, ne: usize) -> usize {
        let mut i = self.skip_star(ne);
        if let Some((_, end)) = bracket_group(self.b(), i, TITLE_LIMIT, &mut self.budget) {
            i = end;
        }
        if let Some((inner, _)) = brace_group(self.b(), i, TITLE_LIMIT, &mut self.budget) {
            let title = normalize(&self.m[inner], 200);
            self.section = (!title.is_empty()).then_some(title);
        }
        // The title itself is scanned too (it may contain labels or references).
        ne
    }

    fn define_env(&mut self, env: &str, display: &str) {
        let kind = kind_by_words(display);
        if kind.is_none() {
            self.out.note(format!("environment {} ({}) is not admitted as an entity", short(env), short(display)));
        }
        self.envs.insert(env.to_owned(), kind);
    }

    fn newtheorem(&mut self, ne: usize) -> usize {
        let i = self.skip_star(ne);
        let Some((env_r, mut j)) = brace_group(self.b(), i, 256, &mut self.budget) else {
            self.out.partial("a \\newtheorem could not be read");
            return i;
        };
        if let Some((_, end)) = bracket_group(self.b(), j, 256, &mut self.budget) {
            j = end;
        }
        let Some((name_r, end)) = brace_group(self.b(), j, 1024, &mut self.budget) else {
            self.out.partial("a \\newtheorem could not be read");
            return j;
        };
        let env = self.m[env_r].trim();
        let display = normalize(&self.m[name_r], 200);
        if !literal(env) || !literal(&display) || env.is_empty() {
            self.out.partial("\\newtheorem with non-literal arguments is not interpreted");
        } else {
            self.define_env(env, &display);
        }
        end
    }

    fn declaretheorem(&mut self, ne: usize) -> usize {
        let mut i = ne;
        let mut opts = bracket_group(self.b(), i, 1024, &mut self.budget).map(|(r, end)| {
            i = end;
            r
        });
        let Some((envs_r, mut end)) = brace_group(self.b(), i, 512, &mut self.budget) else {
            self.out.partial("a \\declaretheorem could not be read");
            return i;
        };
        if opts.is_none()
            && self.b().get(end) == Some(&b'[')
            && let Some((r, e)) = bracket_group(self.b(), end, 1024, &mut self.budget)
        {
            opts = Some(r);
            end = e;
        }
        let name = opts.and_then(|r| theorem_option_name(&self.m[r]));
        let envs: Vec<String> =
            self.m[envs_r].split(',').map(str::trim).filter(|e| !e.is_empty()).map(str::to_owned).collect();
        for env in envs {
            let display = name.clone().unwrap_or_else(|| env.clone());
            if !literal(&env) || !literal(&display) {
                self.out.partial("\\declaretheorem with non-literal arguments is not interpreted");
                continue;
            }
            self.define_env(&env, &display);
        }
        end
    }

    /// Why a definition body may hide tracked constructs (the first tracked
    /// command it uses), if it does.
    fn hides_tracked(&mut self, body: &str) -> Option<String> {
        let b = body.as_bytes();
        for (at, cmd) in commands(body) {
            if TRACKED_COMMANDS.contains(&cmd) || REF_COMMANDS.contains(&cmd) || CITE_COMMANDS.contains(&cmd) {
                return Some(format!("\\{cmd}"));
            }
            if cmd == "begin" || cmd == "end" {
                let after = at + 1 + cmd.len();
                let env = brace_group(b, after, 256, &mut self.budget).map(|(r, _)| body[r].trim().to_owned());
                match env {
                    Some(env) if literal(&env) => {
                        let base = env.strip_suffix('*').unwrap_or(&env);
                        if self.env_kind(base).is_some() {
                            return Some(format!("\\{cmd}{{{env}}}"));
                        }
                    }
                    _ => return Some(format!("\\{cmd}")),
                }
                continue;
            }
            let base = cmd.strip_prefix("end").unwrap_or(cmd);
            if self.env_kind(cmd).is_some() || (!base.is_empty() && self.env_kind(base).is_some()) {
                return Some(format!("\\{cmd}"));
            }
        }
        None
    }

    fn macro_partial(&mut self, name: &str, body: &str) {
        if let Some(cmd) = self.hides_tracked(body) {
            self.out.partial(format!("macro {} is not interpreted (its definition uses {})", short(name), short(&cmd)));
        }
    }

    /// The macro being defined at `i`: `{\name}` or `\name`.
    fn macro_name(&mut self, i: usize) -> Option<(String, usize)> {
        let b = self.b();
        let k = skip_ws(b, i);
        match b.get(k) {
            Some(b'{') => {
                let (inner, end) = brace_group(b, k, 256, &mut self.budget)?;
                Some((self.m[inner].trim().to_owned(), end))
            }
            Some(b'\\') => {
                let ne = command_end(b, k + 1);
                let end = if ne == k + 1 { k + 1 + char_len_at(self.m, k + 1) } else { ne };
                Some((self.m[k..end].to_owned(), end))
            }
            _ => None,
        }
    }

    fn newcommand(&mut self, ne: usize, document: bool) -> usize {
        let i = self.skip_star(ne);
        let Some((name, mut j)) = self.macro_name(i) else { return i };
        if document {
            match brace_group(self.b(), j, 1024, &mut self.budget) {
                Some((_, end)) => j = end,
                None => return j,
            }
        } else {
            for _ in 0..2 {
                match bracket_group(self.b(), j, 1024, &mut self.budget) {
                    Some((_, end)) => j = end,
                    None => break,
                }
            }
        }
        let Some((body, end)) = brace_group(self.b(), j, BODY_LIMIT, &mut self.budget) else {
            self.out.partial(format!("the definition of {} could not be read", short(&name)));
            return j;
        };
        self.macro_partial(&name, &self.m[body]);
        end
    }

    fn def(&mut self, ne: usize) -> usize {
        let b = self.b();
        let k = skip_ws(b, ne);
        if b.get(k) != Some(&b'\\') {
            return ne;
        }
        let name_end = command_end(b, k + 1);
        let name_end = if name_end == k + 1 { k + 1 + char_len_at(self.m, k + 1) } else { name_end };
        let name = &self.m[k..name_end];
        // Parameter text up to the body's `{`.
        let limit = (name_end + 256).min(b.len());
        let Some(open) = b[name_end..limit].iter().position(|&c| c == b'{' || c == b'}').map(|p| name_end + p) else {
            return name_end;
        };
        if b[open] != b'{' {
            return name_end;
        }
        let Some((body, end)) = brace_group(b, open, BODY_LIMIT, &mut self.budget) else {
            self.out.partial(format!("the definition of {} could not be read", short(name)));
            return name_end;
        };
        self.macro_partial(name, &self.m[body]);
        end
    }

    fn let_(&mut self, ne: usize) -> usize {
        let b = self.b();
        let k = skip_ws(b, ne);
        if b.get(k) != Some(&b'\\') {
            return ne;
        }
        let name_end = command_end(b, k + 1);
        let name_end = if name_end == k + 1 { k + 1 + char_len_at(self.m, k + 1) } else { name_end };
        let mut j = skip_ws(b, name_end);
        if b.get(j) == Some(&b'=') {
            j = skip_ws(b, j + 1);
        }
        if b.get(j) != Some(&b'\\') {
            return name_end;
        }
        let target_end = command_end(b, j + 1);
        if target_end == j + 1 {
            return name_end;
        }
        let name = &self.m[k..name_end];
        let target = &self.m[j..target_end];
        self.macro_partial(name, target);
        target_end
    }

    fn newenvironment(&mut self, ne: usize, document: bool) -> usize {
        let i = self.skip_star(ne);
        let Some((name_r, mut j)) = brace_group(self.b(), i, 256, &mut self.budget) else { return i };
        let name = normalize(&self.m[name_r], 128);
        if document {
            match brace_group(self.b(), j, 1024, &mut self.budget) {
                Some((_, end)) => j = end,
                None => return j,
            }
        } else {
            for _ in 0..2 {
                match bracket_group(self.b(), j, 1024, &mut self.budget) {
                    Some((_, end)) => j = end,
                    None => break,
                }
            }
        }
        let Some((begin, after_begin)) = brace_group(self.b(), j, BODY_LIMIT, &mut self.budget) else {
            self.out.partial(format!("the definition of environment {} could not be read", short(&name)));
            return j;
        };
        let Some((end_code, end)) = brace_group(self.b(), after_begin, BODY_LIMIT, &mut self.budget) else {
            self.out.partial(format!("the definition of environment {} could not be read", short(&name)));
            return after_begin;
        };
        let used = self.hides_tracked(&self.m[begin]).or_else(|| self.hides_tracked(&self.m[end_code]));
        if let Some(cmd) = used {
            self.out.partial(format!(
                "environment {} is not interpreted (its definition uses {}, so it may wrap theorem environments)",
                short(&name),
                short(&cmd)
            ));
        }
        end
    }
}

/// `name=` (or `title=`) in thmtools options.
fn theorem_option_name(opts: &str) -> Option<String> {
    let mut depth = 0usize;
    let mut start = 0;
    let mut parts = Vec::new();
    for (k, c) in opts.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&opts[start..k]);
                start = k + 1;
            }
            _ => {}
        }
    }
    parts.push(&opts[start..]);
    parts.into_iter().find_map(|part| {
        let (key, value) = part.split_once('=')?;
        let key = key.trim();
        (key == "name" || key == "title").then(|| {
            let v = value.trim();
            let v = v.strip_prefix('{').and_then(|v| v.strip_suffix('}')).unwrap_or(v);
            normalize(v, 200)
        })
    })
}

/// A short, display-safe rendering of a name for messages.
fn short(s: &str) -> String {
    let n = normalize(s, 60);
    if n.is_empty() { "(unnamed)".to_owned() } else { n }
}

/// Helpers shared by the format extractors.
pub(in crate::reconcile::extract) mod common {
    use std::collections::{BTreeMap, HashSet};
    use std::ops::Range;

    use exposition_core::text::TextRange;

    use crate::reconcile::extract::markers::{self, MarkerFormat};
    use crate::reconcile::extract::{
        Declaration, Extraction, Link, LinkFrom, LinkTarget, MAX_DECLARATIONS, MAX_LABELS, MAX_LINKS, Utf16Index,
        context_fingerprint, fingerprint, valid_name,
    };
    use crate::vocabulary::{EntityKind, RelationKind};

    const MAX_REASONS: usize = 64;
    const PRECEDING_BYTES: usize = 200;
    /// Declarations open at once (nesting). Each open declaration is hashed
    /// over its whole range when closed, so unbounded nesting of unclosed
    /// constructs would make extraction quadratic.
    const MAX_OPEN_DECLARATIONS: usize = 8;

    /// A declaration as it is opened (its end is given when it is closed).
    pub struct DeclSpec<'a> {
        pub kind: EntityKind,
        pub syntax: String,
        pub name: Option<String>,
        pub title: Option<String>,
        /// Byte offset of the construct's start.
        pub start: usize,
        /// The construct's header (`\begin{theorem}[Main]`, `theorem foo`).
        pub header: &'a str,
        pub context: Option<&'a str>,
        pub attributes: BTreeMap<String, String>,
    }

    /// Accumulates one extraction, enforcing the per-file limits while
    /// extracting (so hostile input cannot grow memory without bound) and
    /// converting byte offsets to UTF-16 ranges.
    pub struct Builder<'t> {
        text: &'t str,
        index: Utf16Index<'t>,
        out: Extraction,
        starts: Vec<usize>,
        labels: HashSet<String>,
        dropped_declarations: usize,
        dropped_links: usize,
        dropped_labels: usize,
        invalid_names: bool,
    }

    impl<'t> Builder<'t> {
        pub fn new(extractor: &str, text: &'t str) -> Self {
            Self {
                text,
                index: Utf16Index::new(text),
                out: Extraction { extractor: extractor.to_owned(), ..Default::default() },
                starts: Vec::new(),
                labels: HashSet::new(),
                dropped_declarations: 0,
                dropped_links: 0,
                dropped_labels: 0,
                invalid_names: false,
            }
        }

        fn range(&self, bytes: Range<usize>) -> TextRange {
            let start = floor_boundary(self.text, bytes.start);
            let end = floor_boundary(self.text, bytes.end).max(start);
            self.index.range(start..end)
        }

        /// Open a declaration; `None` when the declaration limit is reached.
        pub fn open(&mut self, spec: DeclSpec<'_>) -> Option<usize> {
            if self.out.declarations.len() >= MAX_DECLARATIONS {
                self.dropped_declarations += 1;
                return None;
            }
            let start = floor_boundary(self.text, spec.start);
            let preceding = preceding(self.text, start);
            let context_fp = context_fingerprint(spec.header, spec.context, preceding);
            self.out.declarations.push(Declaration {
                kind: spec.kind,
                syntax: spec.syntax,
                name: spec.name,
                title: spec.title,
                range: self.range(start..start),
                context: spec.context.map(str::to_owned),
                body_fingerprint: fingerprint(""),
                context_fingerprint: context_fp,
                attributes: spec.attributes,
            });
            self.starts.push(start);
            Some(self.out.declarations.len() - 1)
        }

        /// Close declaration `index` at byte offset `end` (exclusive).
        pub fn close(&mut self, index: usize, end: usize) {
            let Some(&start) = self.starts.get(index) else { return };
            let end = floor_boundary(self.text, end).max(start);
            let range = self.range(start..end);
            let body = fingerprint(&self.text[start..end]);
            if let Some(d) = self.out.declarations.get_mut(index) {
                d.range = range;
                d.body_fingerprint = body;
            }
        }

        pub fn declare(&mut self, spec: DeclSpec<'_>, end: usize) -> Option<usize> {
            let index = self.open(spec)?;
            self.close(index, end);
            Some(index)
        }

        /// Whether another declaration may open inside `open` open ones;
        /// reports partial coverage when not.
        pub fn may_nest(&mut self, open: usize) -> bool {
            if open < MAX_OPEN_DECLARATIONS {
                return true;
            }
            self.partial(format!("declarations nested more than {MAX_OPEN_DECLARATIONS} deep were not indexed"));
            false
        }

        pub fn kind(&self, index: usize) -> Option<EntityKind> {
            self.out.declarations.get(index).map(|d| d.kind)
        }

        pub fn has_name(&self, index: usize) -> bool {
            self.out.declarations.get(index).is_some_and(|d| d.name.is_some())
        }

        pub fn set_name(&mut self, index: usize, name: String) {
            if let Some(d) = self.out.declarations.get_mut(index) {
                d.name = Some(name);
            }
        }

        pub fn link(&mut self, kind: RelationKind, from: LinkFrom, target: LinkTarget, bytes: Range<usize>) {
            if self.out.links.len() >= MAX_LINKS {
                self.dropped_links += 1;
                return;
            }
            let range = self.range(bytes);
            self.out.links.push(Link { kind, from, target, range });
        }

        /// `raw` (trimmed) when it is a valid name; otherwise reports partial
        /// coverage once and returns `None`.
        pub fn checked_name(&mut self, raw: &str) -> Option<String> {
            let name = raw.trim();
            if valid_name(name) {
                Some(name.to_owned())
            } else {
                if !self.invalid_names {
                    self.invalid_names = true;
                    self.partial("names that are empty, longer than 256 characters, or contain spaces were ignored");
                }
                None
            }
        }

        /// Record a defined label; returns it when valid.
        pub fn label(&mut self, raw: &str) -> Option<String> {
            let name = self.checked_name(raw)?;
            if !self.labels.contains(&name) {
                if self.labels.len() >= MAX_LABELS {
                    self.dropped_labels += 1;
                    return Some(name);
                }
                self.labels.insert(name.clone());
                self.out.labels.push(name.clone());
            }
            Some(name)
        }

        pub fn partial(&mut self, reason: impl Into<String>) {
            let reason = reason.into();
            if self.out.partial.len() < MAX_REASONS && !self.out.partial.contains(&reason) {
                self.out.partial.push(reason);
            }
        }

        pub fn note(&mut self, note: impl Into<String>) {
            let note = note.into();
            if self.out.notes.len() < MAX_REASONS && !self.out.notes.contains(&note) {
                self.out.notes.push(note);
            }
        }

        /// Attachment blocks of this text, reporting malformed markers.
        pub fn attachments(&mut self, format: MarkerFormat) {
            self.out.attachments = markers::scan(format, self.text, &self.index);
            for key in markers::malformed_keys(format, self.text) {
                self.partial(format!("attachment markers for key {key} are unpaired or duplicated"));
            }
        }

        pub fn finish(mut self) -> Extraction {
            if self.dropped_declarations > 0 {
                self.partial(format!(
                    "{} declarations beyond the first {MAX_DECLARATIONS} were not indexed",
                    self.dropped_declarations
                ));
            }
            if self.dropped_links > 0 {
                self.partial(format!(
                    "{} references beyond the first {MAX_LINKS} were not indexed",
                    self.dropped_links
                ));
            }
            if self.dropped_labels > 0 {
                self.partial(format!("labels beyond the first {MAX_LABELS} were not indexed"));
            }
            self.out
        }
    }

    /// Bounds the extra scanning done by look-ahead parsers so the total
    /// work stays linear in the text length even for hostile input.
    pub struct Budget {
        left: usize,
        exhausted: bool,
    }

    impl Budget {
        pub fn new(text_len: usize) -> Self {
            Self { left: text_len.saturating_mul(8).saturating_add(1 << 20), exhausted: false }
        }

        /// How many bytes a scan limited to `limit` may look at.
        pub fn allow(&mut self, limit: usize) -> usize {
            if self.left < limit {
                self.exhausted = true;
            }
            limit.min(self.left)
        }

        pub fn spend(&mut self, n: usize) {
            self.left = self.left.saturating_sub(n);
        }

        pub fn exhausted(&self) -> bool {
            self.exhausted
        }
    }

    /// Largest char boundary `<= i` (clamped to the text).
    pub fn floor_boundary(text: &str, i: usize) -> usize {
        let mut i = i.min(text.len());
        while !text.is_char_boundary(i) {
            i -= 1;
        }
        i
    }

    /// Byte length of the character at `i` (1 when there is none or `i` is
    /// not a boundary, so callers always advance).
    pub fn char_len_at(text: &str, i: usize) -> usize {
        text.get(i..).and_then(|r| r.chars().next()).map_or(1, char::len_utf8)
    }

    /// Up to ~200 bytes of text before `start`.
    pub fn preceding(text: &str, start: usize) -> &str {
        let start = floor_boundary(text, start);
        let mut from = start.saturating_sub(PRECEDING_BYTES);
        while !text.is_char_boundary(from) {
            from += 1;
        }
        &text[from..start]
    }

    /// `end` moved back over trailing whitespace (never before 0).
    pub fn trim_end_pos(text: &str, end: usize) -> usize {
        let end = floor_boundary(text, end);
        text[..end].trim_end().len()
    }

    /// Whitespace collapsed to single spaces, trimmed, at most `max_chars`.
    pub fn normalize(s: &str, max_chars: usize) -> String {
        let mut out = String::new();
        let mut count = 0;
        for word in s.split_whitespace() {
            if !out.is_empty() {
                if count >= max_chars {
                    break;
                }
                out.push(' ');
                count += 1;
            }
            for c in word.chars() {
                if count >= max_chars {
                    break;
                }
                out.push(c);
                count += 1;
            }
        }
        out
    }

    /// Kind of a standard theorem-like environment/function/class name.
    pub fn default_kind(name: &str) -> Option<EntityKind> {
        Some(match name {
            "theorem" | "thm" => EntityKind::Theorem,
            "lemma" | "lem" => EntityKind::Lemma,
            "proposition" | "prop" => EntityKind::Proposition,
            "corollary" | "cor" => EntityKind::Corollary,
            "definition" | "defn" | "dfn" => EntityKind::Definition,
            "conjecture" | "conj" => EntityKind::Conjecture,
            "claim" => EntityKind::Claim,
            "assumption" | "assumptions" | "hypothesis" | "hyp" => EntityKind::Assumption,
            "question" | "problem" | "openproblem" => EntityKind::Question,
            "proof" => EntityKind::Proof,
            _ => return None,
        })
    }

    /// Commentary environments that are deliberately not entities.
    pub fn commentary(name: &str) -> bool {
        matches!(
            name,
            "remark" | "remarks" | "rem" | "example" | "examples" | "note" | "notes" | "exercise" | "exercises"
        )
    }

    /// Kind from the words of a display name (`Main Theorem`, `Lemme`…):
    /// the earliest recognised word wins.
    pub fn kind_by_words(name: &str) -> Option<EntityKind> {
        const WORDS: &[(&str, EntityKind)] = &[
            ("theorem", EntityKind::Theorem),
            ("lemma", EntityKind::Lemma),
            ("proposition", EntityKind::Proposition),
            ("corollary", EntityKind::Corollary),
            ("definition", EntityKind::Definition),
            ("conjecture", EntityKind::Conjecture),
            ("claim", EntityKind::Claim),
            ("assumption", EntityKind::Assumption),
            ("hypothesis", EntityKind::Assumption),
            ("axiom", EntityKind::Assumption),
            ("question", EntityKind::Question),
            ("problem", EntityKind::Question),
            ("proof", EntityKind::Proof),
        ];
        let lower = name.to_lowercase();
        WORDS.iter().filter_map(|(w, k)| lower.find(w).map(|at| (at, *k))).min_by_key(|(at, _)| *at).map(|(_, k)| k)
    }

    /// Statement kinds a proof can be the proof of.
    pub fn is_statement(kind: EntityKind) -> bool {
        matches!(
            kind,
            EntityKind::Conjecture
                | EntityKind::Lemma
                | EntityKind::Theorem
                | EntityKind::Proposition
                | EntityKind::Corollary
                | EntityKind::Claim
        )
    }

    /// Test helpers: invariants every extraction must satisfy.
    #[cfg(test)]
    pub fn check_invariants(text: &str, x: &Extraction) {
        use exposition_core::text::utf16_range_to_bytes;
        for d in &x.declarations {
            let r = utf16_range_to_bytes(text, d.range).expect("declaration range converts");
            assert!(r.start <= r.end && r.end <= text.len());
            assert_eq!(d.body_fingerprint.len(), 32);
            if let Some(n) = &d.name {
                assert!(valid_name(n), "invalid name {n:?}");
            }
        }
        for l in &x.links {
            utf16_range_to_bytes(text, l.range).expect("link range converts");
            if let LinkFrom::Declaration(i) = l.from {
                assert!(i < x.declarations.len());
            }
            if let LinkTarget::Declaration(i) = l.target {
                assert!(i < x.declarations.len());
            }
            assert!(!matches!(l.kind, RelationKind::DependsOn), "observations are never dependencies");
        }
        for a in &x.attachments {
            utf16_range_to_bytes(text, a.range).expect("attachment range converts");
        }
        for label in &x.labels {
            assert!(valid_name(label));
        }
    }

    /// Test helper: the text of a UTF-16 range.
    #[cfg(test)]
    pub fn slice(text: &str, range: TextRange) -> &str {
        let r = exposition_core::text::utf16_range_to_bytes(text, range).expect("range converts");
        &text[r]
    }

    /// Test helper: deterministic pseudo-random strings (valid UTF-8).
    #[cfg(test)]
    pub fn garbage(seed: u64, len: usize, alphabet: &[&str]) -> String {
        let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let mut out = String::new();
        for _ in 0..len {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            out.push_str(alphabet[(state >> 33) as usize % alphabet.len()]);
        }
        out
    }

    /// Test helper: every char-boundary prefix of `text` (stepping `step`
    /// bytes) is extracted without panicking and satisfies the invariants.
    #[cfg(test)]
    pub fn truncations(text: &str, step: usize, extract: impl Fn(&str) -> Extraction) {
        let mut i = 0;
        while i <= text.len() {
            if text.is_char_boundary(i) {
                let prefix = &text[..i];
                check_invariants(prefix, &extract(prefix));
                let suffix = &text[i..];
                check_invariants(suffix, &extract(suffix));
            }
            i += step;
        }
    }
}

#[cfg(test)]
mod tests {
    use exposition_core::text::utf16_range_to_bytes;

    use super::common::{check_invariants, garbage, slice, truncations};
    use super::*;

    fn run(text: &str) -> Extraction {
        let x = extract(&WorkspacePath::parse("paper/main.tex").unwrap(), text);
        check_invariants(text, &x);
        x
    }

    fn targets(x: &Extraction, kind: RelationKind) -> Vec<(LinkFrom, LinkTarget)> {
        x.links.iter().filter(|l| l.kind == kind).map(|l| (l.from, l.target.clone())).collect()
    }

    fn label(s: &str) -> LinkTarget {
        LinkTarget::Label(s.into())
    }

    const SAMPLE: &str = r"\documentclass{article}
\usepackage{amsthm}
\newtheorem{thm}{Theorem}[section]
\newtheorem{mainlem}[thm]{Main Lemma}
\newtheorem*{rem}{Remark}
\declaretheorem[name=Conjecture]{guess}
\newcommand{\R}{\mathbb{R}}
\begin{document}
\section{Introduction}\label{sec:intro}
% \begin{thm}commented out\label{thm:ghost}\end{thm}
\begin{thm}[Main result]\label{thm:main}
For all $x \in \R$ we have
\begin{equation}\label{eq:inner} x = x \end{equation}
as in \cite[p.~3]{knuth1984, lamport} and \eqref{eq:inner}.
\end{thm}
% a comment between
\begin{proof}
Follows from \cref{lem:aux,thm:main}.
\end{proof}
\subsection{More}
\begin{mainlem}\label{lem:aux} Aux é. \end{mainlem}
Text.
\begin{proof}[Proof of Theorem~\ref{thm:main}]
Done, see \ref{sec:intro}.
\end{proof}
\begin{rem} not an entity \end{rem}
\begin{guess} open \end{guess}
\verb|\label{verb:no}| and \verb*+\ref{x}+
\begin{verbatim}
\begin{theorem}\label{thm:verbatim}\end{theorem}
\end{verbatim}
\begin{figure}\includegraphics[width=3cm]{figs/plot}\label{fig:plot}\end{figure}
\input{sections/results}\include{appendix}\subimport{chapters/}{intro}
\bibliography{refs,more}
\end{document}
\begin{theorem}after the end\end{theorem}
";

    #[test]
    fn theorem_environments_labels_titles_and_ranges() {
        let x = run(SAMPLE);
        let kinds: Vec<_> = x.declarations.iter().map(|d| (d.kind, d.syntax.as_str())).collect();
        assert_eq!(
            kinds,
            [
                (EntityKind::Theorem, "latex.env:thm"),
                (EntityKind::Proof, "latex.env:proof"),
                (EntityKind::Lemma, "latex.env:mainlem"),
                (EntityKind::Proof, "latex.env:proof"),
                (EntityKind::Conjecture, "latex.env:guess"),
            ]
        );
        let thm = &x.declarations[0];
        assert_eq!(thm.name.as_deref(), Some("thm:main"), "the equation label is not the theorem's name");
        assert_eq!(thm.title.as_deref(), Some("Main result"));
        assert_eq!(thm.context.as_deref(), Some("Introduction"));
        let body = slice(SAMPLE, thm.range);
        assert!(body.starts_with("\\begin{thm}[Main result]"));
        assert!(body.ends_with("\\end{thm}"));
        assert_eq!(x.declarations[2].context.as_deref(), Some("More"));
        assert_eq!(x.declarations[2].name.as_deref(), Some("lem:aux"));
        assert_eq!(x.declarations[1].name, None);
        assert_eq!(x.declarations[3].title.as_deref(), Some("Proof of Theorem~\\ref{thm:main}"));
        // Labels: every one, but none from comments or verbatim.
        for l in ["sec:intro", "thm:main", "eq:inner", "lem:aux", "fig:plot"] {
            assert!(x.labels.contains(&l.to_owned()), "{l}");
        }
        for l in ["thm:ghost", "verb:no", "thm:verbatim"] {
            assert!(!x.labels.contains(&l.to_owned()), "{l}");
        }
        assert!(x.notes.iter().any(|n| n.contains("rem (Remark) is not admitted")), "{:?}", x.notes);
        assert!(x.partial.is_empty(), "{:?}", x.partial);
    }

    #[test]
    fn references_citations_and_proofs() {
        let x = run(SAMPLE);
        let refs = targets(&x, RelationKind::References);
        assert!(refs.contains(&(LinkFrom::Declaration(0), LinkTarget::Citation("knuth1984".into()))));
        assert!(refs.contains(&(LinkFrom::Declaration(0), LinkTarget::Citation("lamport".into()))));
        assert!(refs.contains(&(LinkFrom::Declaration(0), label("eq:inner"))));
        assert!(refs.contains(&(LinkFrom::Declaration(1), label("lem:aux"))));
        assert!(refs.contains(&(LinkFrom::Declaration(1), label("thm:main"))));
        assert!(refs.contains(&(LinkFrom::Declaration(3), label("sec:intro"))));
        assert!(!refs.iter().any(|(_, t)| *t == label("x")), "\\verb content is skipped");
        let proofs = targets(&x, RelationKind::ProofOf);
        assert_eq!(
            proofs,
            [(LinkFrom::Declaration(1), LinkTarget::Declaration(0)), (LinkFrom::Declaration(3), label("thm:main")),]
        );
        // The named proof's \ref is the proofOf, not also a reference.
        assert!(!refs.contains(&(LinkFrom::Declaration(3), label("thm:main"))));
        assert!(x.notes.iter().any(|n| n.contains("immediately following")));
    }

    #[test]
    fn files_graphics_and_bibliography() {
        let x = run(SAMPLE);
        let path = |w: &str, ext: &[&str]| LinkTarget::Path {
            written: w.into(),
            base: PathBase::LatexMain,
            extensions: ext.iter().map(|e| (*e).to_owned()).collect(),
        };
        let imports: Vec<_> = targets(&x, RelationKind::Imports).into_iter().map(|(_, t)| t).collect();
        assert_eq!(
            imports,
            [
                path("sections/results", &["tex"]),
                path("appendix", &["tex"]),
                path("chapters/intro", &["tex"]),
                path("refs", &["bib"]),
                path("more", &["bib"]),
            ]
        );
        let assets = targets(&x, RelationKind::UsesAsset);
        assert_eq!(assets, [(LinkFrom::Resource, path("figs/plot", GRAPHICS_EXTENSIONS))]);
        // Nothing after \end{document}.
        assert!(!x.declarations.iter().any(|d| d.syntax == "latex.env:theorem"));
    }

    #[test]
    fn proof_convention_requires_adjacency() {
        let text = "\\begin{lemma}A\\end{lemma}\nSome text.\n\\begin{proof}P\\end{proof}\n\
                    \\begin{definition}D\\end{definition}\n\\begin{proof}Q\\end{proof}\n\
                    \\begin{claim}C\\end{claim}%c\n  % more\n\n\\begin{proof}R\\end{proof}";
        let x = run(text);
        assert_eq!(targets(&x, RelationKind::ProofOf), [(LinkFrom::Declaration(5), LinkTarget::Declaration(4))]);
    }

    #[test]
    fn starred_and_default_environments() {
        let text = "\\begin{theorem*}\\label{a}x\\end{theorem*}\\begin{defn}y\\end{defn}\\begin{openproblem}q\\end{openproblem}\
                    \\begin{assumptions}z\\end{assumptions}\\begin{remark}r\\end{remark}\\begin{example}e\\end{example}";
        let x = run(text);
        let kinds: Vec<_> = x.declarations.iter().map(|d| d.kind).collect();
        assert_eq!(kinds, [EntityKind::Theorem, EntityKind::Definition, EntityKind::Question, EntityKind::Assumption]);
        assert_eq!(x.declarations[0].syntax, "latex.env:theorem*");
        assert_eq!(x.declarations[0].name.as_deref(), Some("a"));
        assert_eq!(x.notes, ["remarks and examples are not admitted as entities"]);
    }

    #[test]
    fn newtheorem_mappings_override_defaults() {
        let text = "\\newtheorem{lemma}{Hilfssatz}\\newtheorem{prop}{Proposition}[section]\
                    \\declaretheorem{claim}\\declaretheorem[style=plain, name={Main Theorem}]{mt}\
                    \\newtheorem{weird}{\\textbf{Theorem}}\
                    \\begin{lemma}a\\end{lemma}\\begin{prop}b\\end{prop}\\begin{claim}c\\end{claim}\\begin{mt}d\\end{mt}";
        let x = run(text);
        let kinds: Vec<_> = x.declarations.iter().map(|d| (d.kind, d.syntax.as_str())).collect();
        assert_eq!(
            kinds,
            [
                (EntityKind::Proposition, "latex.env:prop"),
                (EntityKind::Claim, "latex.env:claim"),
                (EntityKind::Theorem, "latex.env:mt"),
            ]
        );
        assert!(x.notes.iter().any(|n| n.contains("lemma (Hilfssatz)")));
        assert!(x.partial.iter().any(|p| p.contains("\\newtheorem with non-literal")));
    }

    #[test]
    fn macros_that_hide_constructs_are_partial() {
        let plain = run("\\newcommand{\\R}{\\mathbb{R}}\\def\\eps{\\varepsilon}\\let\\phi\\varphi\
                         \\newenvironment{myquote}{\\begin{quote}\\itshape}{\\end{quote}}");
        assert!(plain.partial.is_empty(), "{:?}", plain.partial);
        let x = run("\\newcommand{\\thmref}[1]{Theorem~\\ref{#1}}\\def\\mylabel#1{\\label{#1}}\
                     \\let\\mycite\\cite\\newenvironment{mythm}{\\begin{theorem}}{\\end{theorem}}\
                     \\providecommand\\fig[2][]{\\includegraphics[#1]{#2}}");
        let joined = x.partial.join("\n");
        for m in ["\\thmref", "\\mylabel", "\\mycite", "mythm", "\\fig"] {
            assert!(joined.contains(m), "{m} in {joined}");
        }
        // Definition bodies are not content: no theorem, no label.
        assert!(x.declarations.is_empty());
        assert!(x.labels.is_empty());
    }

    #[test]
    fn non_literal_arguments_and_unbalanced_environments() {
        let x = run("\\label{thm:\\thesection}\\ref{#1}\\cite{\\x}\\input{\\dir/a}\\includegraphics{\\f}\
                     \\begin{theorem}\\begin{itemize}\\end{theorem}\\end{lemma}\\begin{lemma}open");
        let joined = x.partial.join("\n");
        for m in ["labels with non-literal", "\\ref with", "citations with", "\\input with", "\\includegraphics with"] {
            assert!(joined.contains(m), "{m} in {joined}");
        }
        assert!(joined.contains("environment itemize is not closed"));
        assert!(joined.contains("\\end{lemma} has no matching"));
        assert!(joined.contains("environment lemma is not closed before the end of the file"));
        assert_eq!(x.declarations.len(), 2);
        let open = &x.declarations[1];
        assert_eq!(
            slice(
                "\\label{thm:\\thesection}\\ref{#1}\\cite{\\x}\\input{\\dir/a}\\includegraphics{\\f}\\begin{theorem}\\begin{itemize}\\end{theorem}\\end{lemma}\\begin{lemma}open",
                open.range
            ),
            "\\begin{lemma}open"
        );
        assert!(x.links.is_empty());
    }

    #[test]
    fn primitive_input_and_end_document() {
        let text = "\\input preamble.tex\n\\begin{document}\\begin{theorem}x\n\\end{document}";
        let x = run(text);
        assert_eq!(
            targets(&x, RelationKind::Imports),
            [(
                LinkFrom::Resource,
                LinkTarget::Path {
                    written: "preamble.tex".into(),
                    base: PathBase::LatexMain,
                    extensions: vec!["tex".into()]
                }
            )]
        );
        assert_eq!(slice(text, x.declarations[0].range), "\\begin{theorem}x");
        assert!(x.partial.iter().any(|p| p.contains("theorem is not closed before \\end{document}")));
        assert!(!x.partial.iter().any(|p| p.contains("document is not closed")));
    }

    #[test]
    fn fingerprints_follow_body_and_context() {
        let a = run("\\section{S}\\begin{theorem}[T]\\label{x}\nbody one\n\\end{theorem}");
        let b = run("\\section{S}\\begin{theorem}[T]\\label{x}\nbody   two\n\\end{theorem}");
        let c = run("\\section{S}\\begin{theorem}[T]\\label{x}\nbody one\n\\end{theorem}  ");
        assert_eq!(a.declarations[0].context_fingerprint, b.declarations[0].context_fingerprint);
        assert_ne!(a.declarations[0].body_fingerprint, b.declarations[0].body_fingerprint);
        assert_eq!(a.declarations[0].body_fingerprint, c.declarations[0].body_fingerprint);
    }

    #[test]
    fn attachments_and_malformed_markers() {
        let text =
            "% exposition:begin fig1\n\\includegraphics{a.png}\n% exposition:end fig1\n% exposition:begin lone\n";
        let x = run(text);
        assert_eq!(x.attachments.len(), 1);
        assert_eq!(x.attachments[0].image.as_deref(), Some("a.png"));
        assert_eq!(x.attachments[0].base, PathBase::LatexMain);
        assert!(x.partial.iter().any(|p| p == "attachment markers for key lone are unpaired or duplicated"));
        // The graphic inside the block is also an ordinary asset use.
        assert_eq!(targets(&x, RelationKind::UsesAsset).len(), 1);
    }

    #[test]
    fn real_fixtures() {
        let main = include_str!("../../../../../tests/fixtures/latex-basic/main.tex");
        let x = run(main);
        assert!(x.declarations.is_empty());
        assert_eq!(x.labels, ["sec:intro", "eq:energy", "fig:box"]);
        let refs = targets(&x, RelationKind::References);
        assert_eq!(
            refs,
            [
                (LinkFrom::Resource, label("sec:results")),
                (LinkFrom::Resource, LinkTarget::Citation("knuth1984".into())),
                (LinkFrom::Resource, label("eq:energy")),
            ]
        );
        let imports: Vec<_> = targets(&x, RelationKind::Imports).into_iter().map(|(_, t)| t).collect();
        assert_eq!(
            imports,
            [
                LinkTarget::Path {
                    written: "sections/results".into(),
                    base: PathBase::LatexMain,
                    extensions: vec!["tex".into()]
                },
                LinkTarget::Path { written: "refs".into(), base: PathBase::LatexMain, extensions: vec!["bib".into()] },
            ]
        );
        let assets = targets(&x, RelationKind::UsesAsset);
        assert!(
            matches!(&assets[..], [(LinkFrom::Resource, LinkTarget::Path { written, .. })] if written == "figures/box.png")
        );
        assert!(x.partial.is_empty(), "{:?}", x.partial);
        let r = utf16_range_to_bytes(main, x.links[0].range).unwrap();
        assert_eq!(&main[r], "\\ref{sec:results}");

        let results = include_str!("../../../../../tests/fixtures/latex-basic/sections/results.tex");
        let x = run(results);
        assert_eq!(x.labels, ["sec:results"]);
        assert_eq!(targets(&x, RelationKind::References), [(LinkFrom::Resource, label("fig:box"))]);
        assert!(x.partial.is_empty());
    }

    #[test]
    fn robustness() {
        for text in [
            "",
            "\\",
            "%",
            "\\begin",
            "\\begin{",
            "\\begin{theorem}[",
            "\\begin{proof}[\\ref{",
            "\\verb",
            "\\verb|",
            "\\verbé",
            "\\newcommand",
            "\\newcommand{\\x}[",
            "\\def\\x#1",
            "\\let",
            "\\let\\a=",
            "\\end{verbatim}",
            "\\begin{verbatim}",
            "\\label{é",
            "\\ref{a}}}}",
            "}}}{{{",
            "\\input",
            "\\import{a}",
            "\\declaretheorem[name=",
            "\\section{\\label{x}",
        ] {
            run(text);
        }
        truncations(SAMPLE, 1, |t| extract(&WorkspacePath::parse("a.tex").unwrap(), t));
        let alphabet = [
            "\\begin{",
            "\\end{",
            "theorem}",
            "proof}",
            "[",
            "]",
            "{",
            "}",
            "\\label{a}",
            "\\ref{",
            "%",
            "\n",
            "é",
            "\\verb|",
            "|",
            "\\newcommand{\\x}{",
            "#",
            " ",
            "\\cite{k}",
            "\\section{",
        ];
        for seed in 0..40 {
            run(&garbage(seed, 400, &alphabet));
        }
        // Deep nesting and long unbalanced input stay bounded.
        run(&"\\begin{theorem}".repeat(2000));
        run(&"{".repeat(100_000));
        run(&"\\ref{".repeat(20_000));
        run(&"\\section{\\newcommand{".repeat(5_000));
    }
}
