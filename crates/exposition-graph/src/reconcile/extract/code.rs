//! Python/R/Julia extractor (`code/1`): literal imports of other project
//! files only (no declarations).
//!
//! - Python: `import a.b`, `from a.b import c` (project-root relative) and
//!   relative `from .m import y` / `from . import x` (file relative), at
//!   statement start, outside comments and strings.
//! - R: `source("x.R")`, `source(file = "x.R")`, `sys.source("x.R")`.
//! - Julia: `include("x.jl")`.
//!
//! Non-literal arguments are ignored. The reconciler drops targets that are
//! not project files (standard library and installed packages).

use exposition_core::path::WorkspacePath;

use super::latex::common::{Builder, char_len_at};
use super::{Extraction, LinkFrom, LinkTarget, PathBase};
use crate::vocabulary::RelationKind;

pub const EXTRACTOR: &str = "code/1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Python,
    R,
    Julia,
}

pub fn extract(_path: &WorkspacePath, text: &str, language: Language) -> Extraction {
    let mut out = Builder::new(EXTRACTOR, text);
    match language {
        Language::Python => python(text, &mut out),
        Language::R | Language::Julia => calls(text, language, &mut out),
    }
    out.finish()
}

fn blank(m: &mut [u8], from: usize, to: usize) {
    let to = to.min(m.len());
    for byte in &mut m[from.min(to)..to] {
        if *byte != b'\n' && *byte != b'\r' {
            *byte = b' ';
        }
    }
}

fn import_link(out: &mut Builder<'_>, written: String, base: PathBase, range: std::ops::Range<usize>) {
    if written.is_empty() || written.len() > 1024 {
        return;
    }
    let target = LinkTarget::Path { written, base, extensions: vec!["py".to_owned()] };
    out.link(RelationKind::Imports, LinkFrom::Resource, target, range);
}

// ----- Python -----

/// Blank comments and string contents (keeping quotes and line breaks).
fn mask_python(text: &str) -> String {
    let b = text.as_bytes();
    let mut m = b.to_vec();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'#' => {
                let e = b[i..].iter().position(|&c| c == b'\n').map_or(b.len(), |k| i + k);
                blank(&mut m, i, e);
                i = e;
            }
            q @ (b'"' | b'\'') => {
                let triple = b.get(i + 1) == Some(&q) && b.get(i + 2) == Some(&q);
                let open = if triple { i + 3 } else { i + 1 };
                let mut j = open;
                let close = loop {
                    match b.get(j) {
                        None => break b.len(),
                        Some(b'\\') => j += 1 + char_len_at(text, j + 1),
                        Some(b'\n') if !triple => break j,
                        Some(&c) if c == q && (!triple || (b.get(j + 1) == Some(&q) && b.get(j + 2) == Some(&q))) => {
                            break j;
                        }
                        Some(_) => j += 1,
                    }
                };
                let close = close.min(b.len());
                blank(&mut m, open.min(close), close);
                i = if close >= b.len() || b[close] == b'\n' {
                    close
                } else if triple {
                    close + 3
                } else {
                    close + 1
                };
            }
            _ => i += 1,
        }
    }
    String::from_utf8(m).unwrap_or_else(|_| text.to_owned())
}

/// Statement scanner over the masked text (same byte offsets as the text
/// the builder converts ranges for).
struct Python<'m, 'o, 't> {
    m: &'m str,
    out: &'o mut Builder<'t>,
}

impl Python<'_, '_, '_> {
    fn scan(&mut self) {
        let b = self.m.as_bytes();
        let mut depth = 0usize;
        let mut start = true;
        let mut i = 0;
        while i < b.len() {
            if start {
                start = false;
                let j = self.skip(i, false);
                if depth == 0
                    && let Some(end) = self.statement(j)
                {
                    i = end.max(j);
                    continue;
                }
                i = j;
                continue;
            }
            match b[i] {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth = depth.saturating_sub(1),
                b'\\' if matches!(b.get(i + 1), Some(b'\n' | b'\r')) => {
                    i += 2;
                    continue;
                }
                b'\n' | b';' | b':' if depth == 0 => start = true,
                _ => {}
            }
            i += 1;
        }
    }

    /// Skip spaces, tabs, and backslash line continuations (and line
    /// breaks too when inside parentheses).
    fn skip(&self, mut i: usize, newlines: bool) -> usize {
        let b = self.m.as_bytes();
        while i < b.len() {
            match b[i] {
                b' ' | b'\t' | b'\x0c' => i += 1,
                b'\r' | b'\n' if newlines => i += 1,
                b'\\' if matches!(b.get(i + 1), Some(b'\n')) => i += 2,
                b'\\' if b.get(i + 1) == Some(&b'\r') && b.get(i + 2) == Some(&b'\n') => i += 3,
                _ => break,
            }
        }
        i
    }

    fn ident(&self, i: usize) -> usize {
        let rest = self.m.get(i..).unwrap_or("");
        let mut end = 0;
        for (k, c) in rest.char_indices() {
            if c.is_alphanumeric() || c == '_' {
                end = k + c.len_utf8();
            } else {
                break;
            }
        }
        if rest.starts_with(|c: char| c.is_numeric()) { i } else { i + end }
    }

    fn dotted(&self, mut i: usize) -> usize {
        let start = i;
        loop {
            let e = self.ident(i);
            if e == i {
                return if i > start { i - 1 } else { start };
            }
            i = e;
            if self.m.as_bytes().get(i) != Some(&b'.') {
                return i;
            }
            i += 1;
        }
    }

    fn keyword(&self, i: usize, word: &str) -> bool {
        self.m.get(i..).is_some_and(|r| r.starts_with(word)) && self.ident(i) == i + word.len()
    }

    /// Parse an import statement at `i`; returns where parsing stopped.
    fn statement(&mut self, i: usize) -> Option<usize> {
        if self.keyword(i, "import") {
            let mut k = i + "import".len();
            loop {
                k = self.skip(k, false);
                let e = self.dotted(k);
                if e == k {
                    return Some(k);
                }
                let module = &self.m[k..e];
                import_link(self.out, module.replace('.', "/"), PathBase::Root, k..e);
                k = self.skip(e, false);
                if self.keyword(k, "as") {
                    k = self.ident(self.skip(k + 2, false));
                    k = self.skip(k, false);
                }
                if self.m.as_bytes().get(k) != Some(&b',') {
                    return Some(k);
                }
                k += 1;
            }
        }
        if !self.keyword(i, "from") {
            return None;
        }
        let b = self.m.as_bytes();
        let mut k = self.skip(i + "from".len(), false);
        let dots_start = k;
        while b.get(k) == Some(&b'.') {
            k += 1;
        }
        let dots = k - dots_start;
        k = self.skip(k, false);
        let ms = k;
        // `from . import x`: no module after the dots.
        let me = if dots > 0 && self.keyword(k, "import") { k } else { self.dotted(k) };
        let module = &self.m[ms..me];
        k = self.skip(me, false);
        if !self.keyword(k, "import") || (dots == 0 && module.is_empty()) {
            return Some(k);
        }
        if dots == 0 {
            import_link(self.out, module.replace('.', "/"), PathBase::Root, ms..me);
            return Some(k + "import".len());
        }
        let prefix = "../".repeat(dots - 1);
        if !module.is_empty() {
            import_link(self.out, format!("{prefix}{}", module.replace('.', "/")), PathBase::File, dots_start..me);
            return Some(k + "import".len());
        }
        // `from . import a, b as c`: each name may be a module of the package.
        k = self.skip(k + "import".len(), false);
        let parens = b.get(k) == Some(&b'(');
        if parens {
            k += 1;
        }
        loop {
            k = self.skip(k, parens);
            let e = self.ident(k);
            if e == k {
                break;
            }
            import_link(self.out, format!("{prefix}{}", &self.m[k..e]), PathBase::File, k..e);
            k = self.skip(e, parens);
            if self.keyword(k, "as") {
                k = self.skip(self.ident(self.skip(k + 2, parens)), parens);
            }
            if b.get(k) != Some(&b',') {
                break;
            }
            k += 1;
        }
        if parens && b.get(k) == Some(&b')') {
            k += 1;
        }
        Some(k)
    }
}

fn python(text: &str, out: &mut Builder<'_>) {
    let masked = mask_python(text);
    let mut p = Python { m: &masked, out };
    p.scan();
}

// ----- R and Julia -----

fn calls(text: &str, language: Language, out: &mut Builder<'_>) {
    let b = text.as_bytes();
    let julia = language == Language::Julia;
    let ident_start = |c: char| c.is_alphabetic() || c == '_' || (!julia && c == '.');
    let ident_char = |c: char| c.is_alphanumeric() || c == '_' || (if julia { c == '!' } else { c == '.' });
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'#' if julia && b.get(i + 1) == Some(&b'=') => {
                // Nested `#= … =#` block comment.
                let mut depth = 0usize;
                let mut j = i;
                while j < b.len() {
                    if b[j] == b'#' && b.get(j + 1) == Some(&b'=') {
                        depth += 1;
                        j += 2;
                    } else if b[j] == b'=' && b.get(j + 1) == Some(&b'#') {
                        depth -= 1;
                        j += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        j += 1;
                    }
                }
                i = j;
            }
            b'#' => i = b[i..].iter().position(|&c| c == b'\n').map_or(b.len(), |k| i + k),
            b'"' => i = string_end(text, i).1,
            b'\'' if !julia => i = string_end(text, i).1,
            b'\'' => {
                // Julia: a character literal, or the transpose operator.
                let prev = text[..i].chars().next_back();
                let transpose = prev.is_some_and(|c| c.is_alphanumeric() || matches!(c, ')' | ']' | '}' | '_' | '\''));
                let close = (!transpose)
                    .then(|| b.get(i + 1..(i + 12).min(b.len())))
                    .flatten()
                    .and_then(|r| r.iter().position(|&c| c == b'\'' || c == b'\n'))
                    .map(|k| i + 1 + k)
                    .filter(|&k| b[k] == b'\'');
                i = close.map_or(i + 1, |k| k + 1);
            }
            _ => {
                let Some(c) = text.get(i..).and_then(|r| r.chars().next()) else {
                    i += 1;
                    continue;
                };
                if !ident_start(c) || text[..i].chars().next_back().is_some_and(ident_char) {
                    i += c.len_utf8();
                    continue;
                }
                let rest = &text[i..];
                let len = rest.char_indices().find(|&(_, c)| !ident_char(c)).map_or(rest.len(), |(k, _)| k);
                let name = &rest[..len];
                let end = i + len;
                let wanted = if julia { name == "include" } else { name == "source" || name == "sys.source" };
                if wanted && let Some((written, close)) = call_path(text, end, !julia) {
                    let target = LinkTarget::Path { written, base: PathBase::File, extensions: Vec::new() };
                    out.link(RelationKind::Imports, LinkFrom::Resource, target, i..close);
                }
                i = end;
            }
        }
    }
}

/// (unescaped content, end after the closing quote) of the string at `open`
/// (Julia/Python-style triple quotes included); the content is `None` when
/// unterminated.
fn string_end(text: &str, open: usize) -> (Option<String>, usize) {
    let b = text.as_bytes();
    let q = b[open];
    let triple = q == b'"' && b.get(open + 1) == Some(&q) && b.get(open + 2) == Some(&q);
    let start = if triple { open + 3 } else { open + 1 };
    let mut j = start;
    let mut content = String::new();
    while j < b.len() {
        match b[j] {
            b'\\' => {
                let len = char_len_at(text, j + 1);
                if let Some(c) = text.get(j + 1..j + 1 + len) {
                    content.push_str(c);
                }
                j += 1 + len;
            }
            c if c == q && (!triple || (b.get(j + 1) == Some(&q) && b.get(j + 2) == Some(&q))) => {
                return (Some(content), if triple { j + 3 } else { j + 1 });
            }
            _ => {
                let len = char_len_at(text, j);
                if content.len() < 4096
                    && let Some(c) = text.get(j..j + len)
                {
                    content.push_str(c);
                }
                j += len;
            }
        }
    }
    (None, b.len())
}

/// The literal path of a call `(…"path"…)` starting at `after_name`:
/// `("x")`, or `(file = "x")` when `file_arg`.
fn call_path(text: &str, after_name: usize, file_arg: bool) -> Option<(String, usize)> {
    let b = text.as_bytes();
    let skip = |mut i: usize| {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        i
    };
    let mut i = skip(after_name);
    if b.get(i) != Some(&b'(') {
        return None;
    }
    i = skip(i + 1);
    if file_arg && text.get(i..).is_some_and(|r| r.starts_with("file")) {
        let k = skip(i + 4);
        if b.get(k) == Some(&b'=') && b.get(k + 1) != Some(&b'=') {
            i = skip(k + 1);
        }
    }
    if !matches!(b.get(i), Some(b'"' | b'\'')) {
        return None;
    }
    let (content, end) = string_end(text, i);
    let content = content?;
    if content.is_empty() || content.contains('$') || content.len() > 1024 {
        return None;
    }
    Some((content, end))
}

#[cfg(test)]
mod tests {
    use super::super::latex::common::{check_invariants, garbage, slice, truncations};
    use super::*;

    fn run(text: &str, language: Language) -> Extraction {
        let x = extract(&WorkspacePath::parse("src/pkg/mod.py").unwrap(), text, language);
        check_invariants(text, &x);
        assert!(x.declarations.is_empty());
        x
    }

    fn paths(x: &Extraction) -> Vec<(String, PathBase)> {
        x.links
            .iter()
            .map(|l| {
                assert_eq!(l.kind, RelationKind::Imports);
                match &l.target {
                    LinkTarget::Path { written, base, .. } => (written.clone(), *base),
                    other => panic!("unexpected {other:?}"),
                }
            })
            .collect()
    }

    const PY: &str = r#""""Docstring: import fake
from fake import nothing"""
import os, a.b as c
from . import x, y as z
from .m import q
from ..pkg.mod import (r,
    s)
from __future__ import annotations
v = 1; import late
if True:
    import nested
s = "import nope"  # import comment
t = 'from nope import x'
from a.b.c import d
def f(): return g(import_thing)
"#;

    #[test]
    fn python_imports() {
        let x = run(PY, Language::Python);
        let root = |s: &str| (s.to_owned(), PathBase::Root);
        let file = |s: &str| (s.to_owned(), PathBase::File);
        assert_eq!(
            paths(&x),
            [
                root("os"),
                root("a/b"),
                file("x"),
                file("y"),
                file("m"),
                file("../pkg/mod"),
                root("__future__"),
                root("late"),
                root("nested"),
                root("a/b/c"),
            ]
        );
        assert!(
            x.links.iter().all(|l| matches!(&l.target, LinkTarget::Path { extensions, .. } if extensions == &["py"]))
        );
        assert_eq!(slice(PY, x.links[1].range), "a.b");
        assert_eq!(slice(PY, x.links[5].range), "..pkg.mod");
        assert!(x.partial.is_empty());
    }

    #[test]
    fn python_fixture() {
        let fit = include_str!("../../../../../tests/fixtures/m6-paper/analysis/fit.py");
        let x = run(fit, Language::Python);
        let root = |s: &str| (s.to_owned(), PathBase::Root);
        assert_eq!(paths(&x), [root("matplotlib/pyplot"), root("numpy"), root("pandas")]);
    }

    #[test]
    fn r_and_julia_sources() {
        let r = "# source(\"commented.R\")\nsource(\"a.R\")\nsource(file = 'b/c.R', local = TRUE)\nsys.source( \"d.R\" )\n\
                 base::source(\"e.R\")\nmysource(\"no.R\")\nx <- \"source('nope.R')\"\nsource(path)\nsource(file == \"no\")";
        let x = run(r, Language::R);
        let file = |s: &str| (s.to_owned(), PathBase::File);
        assert_eq!(paths(&x), [file("a.R"), file("b/c.R"), file("d.R"), file("e.R")]);
        assert!(
            x.links.iter().all(|l| matches!(&l.target, LinkTarget::Path { extensions, .. } if extensions.is_empty()))
        );
        assert_eq!(slice(r, x.links[0].range), "source(\"a.R\"");

        let jl = "#= include(\"no1.jl\") #= nested =# still =#\ninclude(\"a.jl\")\nx' * include(\"b.jl\")\n\
                  s = \"include(\\\"no2.jl\\\")\"\nc = 'i'\ninclude(\"$(dir)/c.jl\")\n# include(\"no3.jl\")\nmyinclude(\"no4.jl\")";
        let x = run(jl, Language::Julia);
        assert_eq!(paths(&x), [file("a.jl"), file("b.jl")]);
    }

    #[test]
    fn robustness() {
        for text in [
            "",
            "import",
            "from",
            "from .",
            "from . import (",
            "\"\"\"",
            "'",
            "\\",
            "import a.",
            "from ...",
            "from a import",
            "import é.ü as ö",
        ] {
            run(text, Language::Python);
        }
        for text in ["", "source(", "source(\"", "sys.source(file=", "#=", "include(\"", "'", "include('\\"] {
            run(text, Language::R);
            run(text, Language::Julia);
        }
        truncations(PY, 1, |t| extract(&WorkspacePath::parse("a.py").unwrap(), t, Language::Python));
        let alphabet = [
            "import ", "from ", ".", "a", "\n", "(", ")", ",", "\"", "'", "#", "\\", " ", "é", "as ", ";", "source(",
            "include(", "#=",
        ];
        for seed in 0..40 {
            let text = garbage(seed, 300, &alphabet);
            run(&text, Language::Python);
            run(&text, Language::R);
            run(&text, Language::Julia);
        }
    }
}
