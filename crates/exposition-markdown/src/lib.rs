//! The Exposition Markdown print profile (`exposition-markdown-print/1`).
//!
//! Markdown stays the native source. For layout, a document is translated to
//! Typst with one fixed print template and compiled by the Typst pipeline;
//! a [`SourceMap`] links every generated character back to the Markdown it
//! came from, so rendered glyphs can be edited on the page and the edit
//! written back as a minimal Markdown patch ([`MarkdownInput`]).
//!
//! # Declared profile
//!
//! - **Syntax**: CommonMark with GFM tables, strikethrough (`~~x~~`),
//!   footnotes (`[^n]` / `[^n]: ...`) and task lists (`- [ ]`); YAML front
//!   matter. Smart punctuation is typographic: `--` → en dash, `---` → em
//!   dash, `...` → ellipsis, straight quotes → curly quotes. Every other
//!   character prints literally (Typst syntax is escaped).
//! - **Front matter** keys: `title`, `author` (one value or a list),
//!   `date` (title block; also the PDF metadata when it is a calendar date,
//!   `YYYY-MM-DD`); `papersize` (a3, a4, a5, b5,
//!   letter, legal), `fontsize` (6–24pt), `margin` (cm/mm/in/pt length),
//!   `lang`. Other keys are reported and ignored.
//! - **Blocks**: headings (unnumbered), paragraphs, block quotes, ordered and
//!   bullet lists (tight/loose, start number, `.`/`)` style), task lists,
//!   fenced/indented code (raw, with language), tables with column alignment
//!   and a bold header row, horizontal rules, footnotes (repeated references
//!   share one note). A paragraph holding only an image is a figure whose
//!   caption is the alt text.
//! - **Inline**: emphasis, strong, strikethrough, inline code, links and
//!   autolinks, images (inline at text height), hard and soft line breaks.
//! - **Images** are workspace files given relative to the document (or `/`
//!   for the workspace root); URLs, absolute paths and paths leaving the
//!   workspace are refused with a warning and a visible marker; a missing
//!   file is a compiler error located at the image.
//! - **Raw HTML** is not interpreted: it prints verbatim (subdued) and is
//!   reported. HTML comments (e.g. research attachment markers) print nothing.
//! - **Mathematics**: `$...$` inline and `$$...$$` display, in the LaTeX
//!   subset listed in [`math`] (letters, digits, operators, `^`/`_`,
//!   `\frac`, `\sqrt`, `\left`/`\right`, `\text`, Greek, relations, arrows,
//!   large operators with limits, `\mathbb`/`\mathbf`/`\mathrm`/...,
//!   accents, spacing, `aligned`-family environments, `cases`, matrices).
//!   Anything else renders as a red `unsupported: ...` marker and is reported;
//!   it is never dropped.
//! - **Template**: New Computer Modern (text and math, embedded), DejaVu
//!   Sans Mono for code, justified paragraphs, A4 with 2.5 cm margins and
//!   page numbers by default.
//!
//! See `docs/markdown-profile.md` for the user-facing description.

mod emit;
pub mod front_matter;
pub mod input;
pub mod map;
pub mod math;
pub mod profile;
mod translate;

use exposition_core::build::{Diagnostic, Severity, SourceLocation};
use exposition_core::path::WorkspacePath;
use exposition_core::text::TextRange;

pub use front_matter::FrontMatter;
pub use input::{EditPlan, EditRefusal, MarkdownInput, PageEditOp, PageInputRules};
pub use map::{EditMode, MappedOffset, MappedRange, Segment, SegmentKind, SourceMap};
pub use profile::{Layout, PROFILE_ID};
pub use translate::{parser_options, resolve_image_path};

/// Options for one translation.
#[derive(Clone, Debug, Default)]
pub struct TranslateOptions {
    /// Workspace path of the Markdown document (resolves relative images).
    pub document: Option<WorkspacePath>,
}

/// A problem in the Markdown source found by translation (Typst compiler
/// diagnostics are separate and are mapped through the [`SourceMap`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkdownDiagnostic {
    pub severity: Severity,
    pub message: String,
    /// UTF-16 range in the Markdown source.
    pub range: TextRange,
    pub hints: Vec<String>,
}

impl MarkdownDiagnostic {
    pub fn to_diagnostic(&self, path: &WorkspacePath) -> Diagnostic {
        Diagnostic {
            severity: self.severity,
            message: self.message.clone(),
            location: Some(SourceLocation { path: path.clone(), range: self.range }),
            hints: self.hints.clone(),
        }
    }
}

/// An equation in the Markdown source and its generated Typst equation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EquationRef {
    /// UTF-16 range including the `$` / `$$` delimiters.
    pub markdown: TextRange,
    /// UTF-16 range of the generated Typst equation (with its `$`).
    pub generated: TextRange,
    pub block: bool,
}

/// An image reference and its resolved workspace path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageRef {
    pub markdown: TextRange,
    /// Resolved workspace file, `None` when refused.
    pub path: Option<WorkspacePath>,
    /// The destination as written.
    pub source: String,
}

/// The result of translating one Markdown document.
#[derive(Clone, Debug)]
pub struct Translation {
    /// Generated Typst source (template + body).
    pub typst: String,
    pub map: SourceMap,
    pub diagnostics: Vec<MarkdownDiagnostic>,
    pub equations: Vec<EquationRef>,
    pub images: Vec<ImageRef>,
    pub front_matter: Option<FrontMatter>,
    pub layout: Layout,
    /// Always [`PROFILE_ID`].
    pub profile: &'static str,
}

/// Translate Markdown source to Typst with a source map.
pub fn translate(markdown: &str, options: &TranslateOptions) -> Translation {
    translate::translate(markdown, options)
}
