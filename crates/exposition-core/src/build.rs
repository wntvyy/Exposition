//! Build result contract.
//!
//! A build compiles one immutable input snapshot (including unsaved buffers).
//! Page artifacts, geometry, and source mappings identify that snapshot, so a
//! page view can never apply stale offsets to newer source without validation.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::document::DocumentId;
use crate::path::WorkspacePath;
use crate::text::TextRange;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BuildId(pub String);

/// Content-derived identifier of an immutable build input snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SnapshotId(pub String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum DocumentFormat {
    Typst,
    Latex,
    Markdown,
}

/// A typesetting engine/profile for a format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum Engine {
    /// The Typst compiler (in-process library).
    Typst,
    /// Standard LaTeX via pdfLaTeX.
    PdfLatex,
    /// LuaLaTeX.
    LuaLatex,
    /// Markdown through Exposition's declared print profile (rendered by Typst).
    MarkdownPrint,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct BuildTarget {
    pub format: DocumentFormat,
    pub engine: Engine,
    /// Entry file of the build, relative to the workspace root.
    pub entry: WorkspacePath,
}

/// One document revision included in a snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SourceRevision {
    pub path: WorkspacePath,
    /// Open document and revision, when the file was taken from a buffer.
    pub document: Option<DocumentId>,
    pub revision: Option<u64>,
    /// Content hash of the exact bytes compiled.
    pub content_hash: String,
}

/// Compiler, font, and template configuration that determines layout.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct BuildConfiguration {
    pub engine: Engine,
    /// Compiler name and exact version, e.g. "typst 0.15.1" or "pdfTeX 3.141592653-2.6-1.40.27".
    pub compiler: String,
    /// Fingerprint of the font set visible to the compiler.
    pub font_fingerprint: String,
    /// Print profile/template identifier where applicable (e.g. Markdown profile).
    pub profile: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SourceLocation {
    pub path: WorkspacePath,
    /// UTF-16 range over the snapshot text of `path`.
    pub range: TextRange,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub location: Option<SourceLocation>,
    pub hints: Vec<String>,
}

/// How precisely page content maps back to source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum MappingPrecision {
    /// No mapping available.
    None,
    /// Line-level navigation (e.g. SyncTeX).
    Line,
    /// Character/glyph-level mapping suitable for direct page editing of
    /// supported regions.
    Glyph,
}

/// One rendered page of the build output.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PageArtifact {
    pub index: u32,
    pub width_pt: f64,
    pub height_pt: f64,
    /// Opaque reference used to fetch the rendered page (SVG/raster) for this build.
    pub render_ref: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum ExportKind {
    Pdf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ExportArtifact {
    pub kind: ExportKind,
    /// Absolute path of the produced file.
    pub path: String,
    pub content_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum BuildStatus {
    Succeeded,
    /// The compiler reported errors; see diagnostics.
    Failed,
    Canceled,
    /// A required toolchain is missing; `guidance` is actionable setup text.
    ToolUnavailable {
        tool: String,
        guidance: String,
    },
    /// The build could not run for another reason (e.g. invalid snapshot).
    Error {
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct BuildResult {
    pub id: BuildId,
    pub target: BuildTarget,
    pub snapshot: SnapshotId,
    pub sources: Vec<SourceRevision>,
    pub configuration: Option<BuildConfiguration>,
    pub status: BuildStatus,
    pub diagnostics: Vec<Diagnostic>,
    pub pages: Vec<PageArtifact>,
    pub mapping: MappingPrecision,
    pub exports: Vec<ExportArtifact>,
    pub duration_ms: u64,
}
