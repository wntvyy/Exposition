//! Versioned source extractors: pure functions from a file's text to the
//! declarations, labels, and links it states literally.
//!
//! Extractors never interpret macros, run code, or guess. What they do not
//! understand is reported (`partial`) so coverage says so; what they see is an
//! *observation* (`references` is a citation or mention, never a dependency).
//! Paths and labels are returned symbolically; the reconciler resolves them
//! against the project (other files, labels, citation keys).
//!
//! Each extractor has a name and version (`latex/1`). Bump the version when its
//! output changes meaningfully so stored coverage is re-extracted.

pub mod bibtex;
pub mod code;
pub mod latex;
pub mod lean;
pub mod markdown;
pub mod markers;
pub mod typst;

use std::collections::BTreeMap;

use exposition_core::path::WorkspacePath;
use exposition_core::text::TextRange;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::vocabulary::{EntityKind, RelationKind};

/// Largest text an extractor reads (bigger files are reported `failed`).
pub const MAX_EXTRACT_BYTES: usize = 8 * 1024 * 1024;
/// Declarations kept per file (more are reported as partial coverage).
pub const MAX_DECLARATIONS: usize = 5_000;
/// Links kept per file.
pub const MAX_LINKS: usize = 20_000;
/// Labels kept per file.
pub const MAX_LABELS: usize = 20_000;
/// Names/labels longer than this are ignored (reported as partial).
pub const MAX_NAME: usize = 256;

/// What one extractor saw in one file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extraction {
    /// Extractor name and version, e.g. `latex/1`.
    pub extractor: String,
    /// Identifiable declarations in source order (outer before inner).
    pub declarations: Vec<Declaration>,
    /// Observed links, in source order.
    pub links: Vec<Link>,
    /// Every label/anchor the file defines (including ones that are not
    /// entities: sections, equations, figures). Used to tell a reference to a
    /// non-entity label from an unresolved one.
    pub labels: Vec<String>,
    /// Why extraction is incomplete (unsupported macros, non-literal
    /// arguments, limits reached). Empty means complete for supported syntax.
    pub partial: Vec<String>,
    /// Conventions applied and other plain-language notes (never errors).
    pub notes: Vec<String>,
    /// Attachment blocks (`exposition:begin <key>` … `end <key>`) found.
    pub attachments: Vec<Attachment>,
}

/// A declaration recognised literally (a theorem environment, a Lean
/// declaration, a BibTeX entry, ...).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Declaration {
    pub kind: EntityKind,
    /// The construct, e.g. `latex.env:theorem`, `typst.call:lemma`,
    /// `markdown.div:theorem`, `lean.theorem`, `bibtex.entry`.
    pub syntax: String,
    /// Declared name: LaTeX/Typst label, Lean fully qualified name, BibTeX key.
    pub name: Option<String>,
    /// Optional title written in the source (`[Main result]`, a BibTeX title).
    pub title: Option<String>,
    /// UTF-16 range of the whole construct in the extracted text.
    pub range: TextRange,
    /// Containing context in words (section title, Lean namespace), if any.
    pub context: Option<String>,
    /// Hash of the construct's whitespace-normalized text ([`fingerprint`]).
    pub body_fingerprint: String,
    /// Hash of the construct's header, its context, and a little preceding
    /// text ([`context_fingerprint`]); survives edits inside the body.
    pub context_fingerprint: String,
    /// Extra metadata (`lean.kind`, `lean.modifiers`, `bibtex.type`, ...);
    /// keys `[a-zA-Z0-9._-]{1,64}`, values short.
    pub attributes: BTreeMap<String, String>,
}

/// Where a link starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinkFrom {
    /// The file itself.
    Resource,
    /// A declaration of this extraction (index into `declarations`).
    Declaration(usize),
}

/// How a written path is resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PathBase {
    /// Relative to the source file's folder (Typst, Markdown, code).
    /// A leading `/` means the project root (Typst convention).
    File,
    /// Relative to the project root.
    Root,
    /// LaTeX: relative to the folder of the main file that is being compiled,
    /// tried as the file's folder, the project root, then the folders of the
    /// project's LaTeX main files (`\documentclass`).
    LatexMain,
}

/// A symbolic link target, resolved by the reconciler.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinkTarget {
    /// A label defined in this file or elsewhere in the project.
    Label(String),
    /// A bibliography key (a `Reference` entity from a `.bib` file).
    Citation(String),
    /// Typst/Pandoc `@x`: a label when one exists, else a citation key.
    LabelOrCitation(String),
    /// A project file as written in the source, with the extensions to try
    /// (in order) when the written path has none. Never resolved outside the
    /// project; URLs and package references are not links.
    Path { written: String, base: PathBase, extensions: Vec<String> },
    /// A Lean module name (`Paper.Gauss`), resolved through the Lake package
    /// layout of the project.
    LeanModule(String),
    /// A declaration of this extraction (e.g. the statement a proof follows).
    Declaration(usize),
}

/// One observed link.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub kind: RelationKind,
    pub from: LinkFrom,
    pub target: LinkTarget,
    /// UTF-16 range of the referencing syntax (for diagnostics).
    pub range: TextRange,
}

/// An attachment block written by the format adapters
/// (`exposition:begin <key>` … `exposition:end <key>`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub key: String,
    /// The image path inside the block as written (figures), if any.
    pub image: Option<String>,
    /// How that path is resolved (format dependent).
    pub base: PathBase,
    pub range: TextRange,
}

/// Name and version of the extractor for a path, if one exists.
pub fn extractor_for(path: &WorkspacePath) -> Option<&'static str> {
    let ext = path.extension()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "tex" | "ltx" => latex::EXTRACTOR,
        "typ" => typst::EXTRACTOR,
        "md" | "markdown" => markdown::EXTRACTOR,
        "lean" => lean::EXTRACTOR,
        "bib" => bibtex::EXTRACTOR,
        "py" | "pyw" | "ipy" | "r" | "jl" => code::EXTRACTOR,
        _ => return None,
    })
}

/// Extract a file's text with the extractor for its path. `None` when no
/// extractor applies. Text longer than [`MAX_EXTRACT_BYTES`] is refused by the
/// caller before this is reached.
pub fn extract(path: &WorkspacePath, text: &str) -> Option<Extraction> {
    let ext = path.extension()?.to_ascii_lowercase();
    let mut out = match ext.as_str() {
        "tex" | "ltx" => latex::extract(path, text),
        "typ" => typst::extract(path, text),
        "md" | "markdown" => markdown::extract(path, text),
        "lean" => lean::extract(path, text),
        "bib" => bibtex::extract(path, text),
        "py" | "pyw" | "ipy" => code::extract(path, text, code::Language::Python),
        "r" => code::extract(path, text, code::Language::R),
        "jl" => code::extract(path, text, code::Language::Julia),
        _ => return None,
    };
    enforce_limits(&mut out);
    Some(out)
}

fn enforce_limits(out: &mut Extraction) {
    if out.declarations.len() > MAX_DECLARATIONS {
        let dropped = out.declarations.len() - MAX_DECLARATIONS;
        out.declarations.truncate(MAX_DECLARATIONS);
        // Links from dropped declarations would dangle.
        out.links.retain(|l| match (l.from, &l.target) {
            (LinkFrom::Declaration(i), _) if i >= MAX_DECLARATIONS => false,
            (_, LinkTarget::Declaration(i)) if *i >= MAX_DECLARATIONS => false,
            _ => true,
        });
        out.partial.push(format!("{dropped} declarations beyond the first {MAX_DECLARATIONS} were not indexed"));
    }
    if out.links.len() > MAX_LINKS {
        let dropped = out.links.len() - MAX_LINKS;
        out.links.truncate(MAX_LINKS);
        out.partial.push(format!("{dropped} references beyond the first {MAX_LINKS} were not indexed"));
    }
    if out.labels.len() > MAX_LABELS {
        out.labels.truncate(MAX_LABELS);
        out.partial.push(format!("labels beyond the first {MAX_LABELS} were not indexed"));
    }
    out.labels.sort();
    out.labels.dedup();
    out.partial.sort();
    out.partial.dedup();
    out.partial.truncate(50);
    out.notes.sort();
    out.notes.dedup();
    out.notes.truncate(20);
}

// ----- helpers shared by the extractors -----

/// Hex SHA-256 (first 32 hex digits) of whitespace-normalized text: runs of
/// whitespace become one space and the ends are trimmed, so re-indentation or
/// re-wrapping does not change it.
pub fn fingerprint(text: &str) -> String {
    let mut h = Sha256::new();
    let mut first = true;
    for word in text.split_whitespace() {
        if !first {
            h.update(b" ");
        }
        h.update(word.as_bytes());
        first = false;
    }
    hex32(&h.finalize())
}

/// Contextual fingerprint of a declaration: its header (e.g.
/// `\begin{theorem}[Main]`), its containing context, and up to 160 bytes of
/// normalized text before it. Typing inside the body leaves it unchanged.
pub fn context_fingerprint(header: &str, context: Option<&str>, preceding: &str) -> String {
    let mut tail: String = preceding.split_whitespace().collect::<Vec<_>>().join(" ");
    if tail.len() > 160 {
        let mut cut = tail.len() - 160;
        while !tail.is_char_boundary(cut) {
            cut += 1;
        }
        tail = tail[cut..].to_owned();
    }
    fingerprint(&format!("{header}\u{1}{}\u{1}{tail}", context.unwrap_or("")))
}

fn hex32(bytes: &[u8]) -> String {
    bytes.iter().take(16).map(|b| format!("{b:02x}")).collect()
}

/// Converts byte offsets of one text to UTF-16 offsets in O(log n) each after
/// an O(n) build (checkpoints every 256 bytes).
pub struct Utf16Index<'a> {
    text: &'a str,
    /// (byte offset, utf16 offset) at char boundaries, ascending.
    checkpoints: Vec<(usize, u32)>,
}

impl<'a> Utf16Index<'a> {
    pub fn new(text: &'a str) -> Self {
        let mut checkpoints = vec![(0usize, 0u32)];
        let mut units = 0u32;
        let mut next = 256usize;
        for (i, ch) in text.char_indices() {
            if i >= next {
                checkpoints.push((i, units));
                next = i + 256;
            }
            units += ch.len_utf16() as u32;
        }
        Self { text, checkpoints }
    }

    /// UTF-16 offset of byte offset `byte` (clamped to the text; offsets
    /// inside a code point round down to its start).
    pub fn utf16(&self, byte: usize) -> u32 {
        let byte = byte.min(self.text.len());
        let at = match self.checkpoints.binary_search_by_key(&byte, |c| c.0) {
            Ok(i) => return self.checkpoints[i].1,
            Err(i) => i - 1,
        };
        let (start, mut units) = self.checkpoints[at];
        for (i, ch) in self.text[start..].char_indices() {
            if start + i >= byte {
                break;
            }
            if start + i + ch.len_utf8() > byte {
                break;
            }
            units += ch.len_utf16() as u32;
        }
        units
    }

    pub fn range(&self, bytes: std::ops::Range<usize>) -> TextRange {
        TextRange::new(self.utf16(bytes.start), self.utf16(bytes.end))
    }
}

/// A plausible label/key: non-empty, bounded, no whitespace or control
/// characters.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= MAX_NAME && !name.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Resolve `written` against `dir` (a folder inside the project, `None` = the
/// root): `.` and `..` are applied; a path that would leave the project, is
/// absolute, or is a URL yields `None`.
pub fn join_relative(dir: Option<&WorkspacePath>, written: &str) -> Option<WorkspacePath> {
    let written = written.trim();
    if written.is_empty() || written.contains("://") || written.starts_with("data:") {
        return None;
    }
    let unified = written.replace('\\', "/");
    if unified.len() > 1 && unified.as_bytes()[1] == b':' {
        return None;
    }
    let mut parts: Vec<String> = match (dir, unified.starts_with('/')) {
        (Some(d), false) => d.components().map(str::to_owned).collect(),
        _ => Vec::new(),
    };
    for c in unified.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other.to_owned()),
        }
    }
    if parts.is_empty() {
        return None;
    }
    WorkspacePath::parse(&parts.join("/")).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_ignore_whitespace_layout() {
        assert_eq!(fingerprint("a  b\n c"), fingerprint(" a b c "));
        assert_ne!(fingerprint("a b c"), fingerprint("a b d"));
        assert_eq!(fingerprint("x").len(), 32);
    }

    #[test]
    fn utf16_index_matches_direct_conversion() {
        let text = "ab\u{1F600}cd é\n".repeat(200);
        let idx = Utf16Index::new(&text);
        for (b, _) in text.char_indices() {
            let expected = exposition_core::text::byte_to_utf16(&text, b).unwrap() as u32;
            assert_eq!(idx.utf16(b), expected, "byte {b}");
        }
        assert_eq!(idx.utf16(text.len()), exposition_core::text::utf16_len(&text) as u32);
    }

    #[test]
    fn relative_paths_stay_inside_the_project() {
        let dir = WorkspacePath::parse("chapters/one").unwrap();
        assert_eq!(join_relative(Some(&dir), "../figs/a.png").unwrap().as_str(), "chapters/figs/a.png");
        assert_eq!(join_relative(Some(&dir), "/figs/a.png").unwrap().as_str(), "figs/a.png");
        assert_eq!(join_relative(None, "a/./b.tex").unwrap().as_str(), "a/b.tex");
        assert!(join_relative(None, "../outside.tex").is_none());
        assert!(join_relative(Some(&dir), "../../../x").is_none());
        assert!(join_relative(None, "https://example.com/x.png").is_none());
        assert!(join_relative(None, "C:/x.png").is_none());
    }

    #[test]
    fn extractors_are_chosen_by_extension() {
        let p = |s: &str| WorkspacePath::parse(s).unwrap();
        assert_eq!(extractor_for(&p("a/main.tex")), Some(latex::EXTRACTOR));
        assert_eq!(extractor_for(&p("x.typ")), Some(typst::EXTRACTOR));
        assert_eq!(extractor_for(&p("Paper/Gauss.lean")), Some(lean::EXTRACTOR));
        assert_eq!(extractor_for(&p("refs.bib")), Some(bibtex::EXTRACTOR));
        assert_eq!(extractor_for(&p("fit.py")), Some(code::EXTRACTOR));
        assert_eq!(extractor_for(&p("data.csv")), None);
    }
}
