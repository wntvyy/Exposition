//! Format-owned artifact attachment.
//!
//! Implements [`exposition_core::artifact::AttachArtifactRequest`]: given a
//! document's current text and format (Typst `.typ`, LaTeX `.tex`, Markdown
//! `.md`), produce a document transaction that inserts a figure or table with a
//! caption and a stable attachment key, or updates exactly the block previously
//! inserted under that key while preserving every other byte of the source.
//!
//! Inserted blocks are delimited by marker comment lines carrying the key:
//!
//! | Format   | Begin marker                         | Label           |
//! |----------|--------------------------------------|-----------------|
//! | Typst    | `// exposition:begin <key>`          | `<key>`         |
//! | LaTeX    | `% exposition:begin <key>`           | `\label{<key>}` |
//! | Markdown | `<!-- exposition:begin <key> -->`    | (marker only)   |
//!
//! with matching `end` markers. Update refuses when the markers for the key are
//! missing or ambiguous instead of guessing. Users may edit inside a block (for
//! example the caption); an update without a new caption keeps the caption
//! source found in the block.
//!
//! Captions and table cells are plain text, escaped for the format so they
//! render literally (Typst markup and shorthands, LaTeX specials, every ASCII
//! punctuation character in Markdown). Markdown table captions are written as
//! a `Table: <caption>` paragraph after the table (Pandoc's convention); the
//! Markdown print profile prints it as an ordinary paragraph below the table.
//!
//! Figure paths are relative to the document for Typst and Markdown. LaTeX
//! resolves graphics against the main file's directory, so a figure attached
//! to an included file (e.g. `chapters/ch1.tex` included by `book.tex`) is
//! written relative to the main file: the document itself when it has
//! `\documentclass`, else the project target entry that includes it
//! (`.exposition/project.json`), else a main file in the project that
//! `\input`s/`\include`s it; a warning is returned when none is found.
//!
//! This is format code: research supplies the artifact and run, and the
//! typesetting owner owns this crate going forward.

mod blocks;
mod latex;
mod table;

use std::path::Path;

use exposition_core::artifact::{
    ArtifactKind, ArtifactRecord, AttachArtifactOutcome, AttachArtifactRequest, AttachmentPlacement,
};
use exposition_core::document::{
    ConflictPolicy, DocumentEdit, DocumentId, TextPatch, TransactionError, TransactionId, TransactionRequest,
    UndoBehavior,
};
use exposition_core::path::WorkspacePath;
use exposition_core::text::{TextRange, byte_to_utf16, utf16_len, utf16_to_byte};
use exposition_workspace::DocumentService;
use sha2::{Digest, Sha256};

pub use blocks::Caption;
pub use table::TableGrid;

/// Largest CSV accepted for a table attachment.
const TABLE_FILE_LIMIT: u64 = 4 * 1024 * 1024;
const TABLE_ROW_LIMIT: usize = 500;
const TABLE_COLUMN_LIMIT: usize = 50;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Typst,
    Latex,
    Markdown,
}

impl Format {
    pub fn from_path(path: &WorkspacePath) -> Option<Format> {
        match path.extension()?.to_ascii_lowercase().as_str() {
            "typ" => Some(Format::Typst),
            "tex" | "ltx" => Some(Format::Latex),
            "md" | "markdown" => Some(Format::Markdown),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AttachError {
    #[error("documents of this type do not support attachments (Typst, LaTeX, and Markdown do)")]
    UnsupportedFormat,
    #[error("{0}")]
    Document(#[from] TransactionError),
    #[error("the document changed (revision {base} is now {current}); choose the position again")]
    Stale { base: u64, current: u64 },
    #[error("no attachment with key {0:?} was found (its begin/end markers are missing)")]
    MissingMarkers(String),
    #[error("the markers for attachment {key:?} are ambiguous ({begin} begin and {end} end markers)")]
    AmbiguousMarkers { key: String, begin: usize, end: usize },
    #[error("an attachment with key {0:?} already exists; update it instead")]
    DuplicateKey(String),
    #[error("{0:?} is not a valid attachment key (letters, digits, '-', '_', ':', '.'; starts with a letter)")]
    InvalidKey(String),
    #[error("{0}")]
    InvalidPlacement(String),
    #[error("{0}")]
    Artifact(String),
    #[error("the artifact file changed since the run recorded it (expected {expected}, found {found})")]
    ArtifactChanged { expected: String, found: String },
    #[error("{0}")]
    Unsupported(String),
    #[error("{0}")]
    InvalidTable(String),
}

impl AttachError {
    pub fn code(&self) -> &'static str {
        match self {
            AttachError::UnsupportedFormat => "attach.unsupportedFormat",
            AttachError::Document(_) => "attach.document",
            AttachError::Stale { .. } => "attach.stale",
            AttachError::MissingMarkers(_) => "attach.missingMarkers",
            AttachError::AmbiguousMarkers { .. } => "attach.ambiguousMarkers",
            AttachError::DuplicateKey(_) => "attach.duplicateKey",
            AttachError::InvalidKey(_) => "attach.invalidKey",
            AttachError::InvalidPlacement(_) => "attach.invalidPlacement",
            AttachError::Artifact(_) => "attach.artifact",
            AttachError::ArtifactChanged { .. } => "attach.artifactChanged",
            AttachError::Unsupported(_) => "attach.unsupported",
            AttachError::InvalidTable(_) => "attach.invalidTable",
        }
    }
}

/// What the block shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttachContent {
    /// An image, referenced by a path relative to the document.
    Figure {
        path: String,
        media_type: String,
    },
    Table(TableGrid),
}

/// A planned attachment transaction (pure; nothing applied yet).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachPlan {
    pub transaction: TransactionRequest,
    pub attachment_key: String,
    /// Index of the patch carrying the block within the transaction's edit.
    pub block_patch: usize,
    /// UTF-16 offset of the block start inside that patch's text.
    pub block_offset: u32,
    /// UTF-16 length of the block.
    pub block_len: u32,
    pub warnings: Vec<String>,
}

/// Inputs for planning against one exact document revision.
pub struct PlanInput<'a> {
    pub format: Format,
    pub document: &'a DocumentId,
    pub document_path: &'a WorkspacePath,
    pub revision: u64,
    pub text: &'a str,
}

/// Plan an insert or update. Pure: no I/O, no document access.
pub fn plan(
    input: &PlanInput<'_>,
    request: &AttachArtifactRequest,
    content: &AttachContent,
) -> Result<AttachPlan, AttachError> {
    let format = input.format;
    let text = input.text;
    let newline = blocks::line_ending(text);
    let is_table = matches!(content, AttachContent::Table(_));
    let mut warnings = Vec::new();
    let (patches, key, block_patch, block_offset, block_len, label) = match &request.placement {
        AttachmentPlacement::Insert { offset } => {
            if request.base_revision != input.revision {
                return Err(AttachError::Stale { base: request.base_revision, current: input.revision });
            }
            let key = match &request.attachment_key {
                Some(key) => {
                    validate_key(key)?;
                    if blocks::find_markers(format, text, key) != (0, 0) || blocks::label_in_use(format, text, key) {
                        return Err(AttachError::DuplicateKey(key.clone()));
                    }
                    key.clone()
                }
                None => derive_key(format, text, is_table, &request.artifact),
            };
            let at = utf16_to_byte(text, *offset as usize)
                .map_err(|e| AttachError::InvalidPlacement(format!("insert position is invalid: {e}")))?;
            if blocks::inside_block(format, text, at) {
                return Err(AttachError::InvalidPlacement(
                    "the insert position is inside another attachment; move the cursor outside it".into(),
                ));
            }
            if let Some(message) = blocks::placement_problem(format, text, at) {
                return Err(AttachError::InvalidPlacement(message));
            }
            let caption = request.caption.as_deref().map(|c| Caption::Plain(c.to_owned()));
            let block = blocks::render(format, &key, content, caption.as_ref(), newline);
            let before = &text[..at];
            let after = &text[at..];
            let prefix = if before.is_empty() || before.ends_with('\n') { "" } else { newline };
            let suffix = if after.starts_with('\n') || after.starts_with("\r\n") { "" } else { newline };
            let mut patches = Vec::new();
            if format == Format::Latex {
                for package in blocks::missing_latex_packages(text, is_table) {
                    match blocks::preamble_insert_point(text) {
                        Some(point) if point <= at => {
                            let at16 = byte_to_utf16(text, point).expect("line start is a boundary") as u32;
                            patches.push(TextPatch::insert(at16, format!("\\usepackage{{{package}}}{newline}")));
                        }
                        _ => warnings.push(format!(
                            "Add \\usepackage{{{package}}} to the preamble; this file has no \\begin{{document}} to place it before."
                        )),
                    }
                }
            }
            let block_offset = utf16_len(prefix) as u32;
            let block_len = utf16_len(&block) as u32;
            patches.push(TextPatch::insert(*offset, format!("{prefix}{block}{suffix}")));
            let index = patches.len() - 1;
            let label = format!("Insert {} {key}", if is_table { "table" } else { "figure" });
            (patches, key, index, block_offset, block_len, label)
        }
        AttachmentPlacement::Update { attachment_key } => {
            validate_key(attachment_key)?;
            let range = blocks::locate(format, text, attachment_key)?;
            let existing = &text[range.clone()];
            let caption = match &request.caption {
                Some(caption) => Some(Caption::Plain(caption.clone())),
                None => blocks::extract_caption(format, existing).map(Caption::Raw),
            };
            let block = blocks::render(format, attachment_key, content, caption.as_ref(), newline);
            if format == Format::Latex {
                for package in blocks::missing_latex_packages(text, is_table) {
                    warnings.push(format!("Add \\usepackage{{{package}}} to the preamble."));
                }
            }
            let start = byte_to_utf16(text, range.start).expect("marker boundary") as u32;
            let end = byte_to_utf16(text, range.end).expect("marker boundary") as u32;
            let block_len = utf16_len(&block) as u32;
            let label = format!("Update {} {attachment_key}", if is_table { "table" } else { "figure" });
            (vec![TextPatch::replace(start, end, block)], attachment_key.clone(), 0, 0, block_len, label)
        }
    };
    let transaction = TransactionRequest {
        id: TransactionId(format!("attach-{}-{}-{}", input.document.0, input.revision, key)),
        origin: request.origin.clone(),
        edits: vec![DocumentEdit { document: input.document.clone(), base_revision: input.revision, patches }],
        undo: UndoBehavior::NewGroup,
        conflict: ConflictPolicy::RebaseDisjoint,
        label: Some(label),
        selection: None,
    };
    Ok(AttachPlan { transaction, attachment_key: key, block_patch, block_offset, block_len, warnings })
}

/// Load the artifact named by `request` from the workspace, verifying it still
/// matches the run's recorded hash, and turn it into attachable content.
pub fn load_content(
    workspace_root: &Path,
    format: Format,
    document_path: &WorkspacePath,
    artifact: &ArtifactRecord,
) -> Result<AttachContent, AttachError> {
    load_content_with(workspace_root, format, document_path, None, artifact).map(|(content, _)| content)
}

/// [`load_content`] with the document's current text (to find the LaTeX main
/// file whose directory graphics paths resolve against) and placement warnings.
pub fn load_content_with(
    workspace_root: &Path,
    format: Format,
    document_path: &WorkspacePath,
    document_text: Option<&str>,
    artifact: &ArtifactRecord,
) -> Result<(AttachContent, Vec<String>), AttachError> {
    let mut warnings = Vec::new();
    let path = artifact
        .path
        .as_ref()
        .ok_or_else(|| AttachError::Artifact("the artifact is not stored in the project".into()))?;
    let root = std::fs::canonicalize(workspace_root)
        .map_err(|e| AttachError::Artifact(format!("workspace root cannot be opened: {e}")))?;
    let file = path.components().fold(root.clone(), |p, c| p.join(c));
    let file = std::fs::canonicalize(&file)
        .map_err(|_| AttachError::Artifact(format!("the artifact file {path} no longer exists")))?;
    if !file.starts_with(&root) || !file.is_file() {
        return Err(AttachError::Artifact(format!("{path} is not a file inside the workspace")));
    }
    let bytes = std::fs::read(&file).map_err(|e| AttachError::Artifact(format!("reading {path}: {e}")))?;
    let found = format!("sha256:{}", hex(&Sha256::digest(&bytes)));
    if found != artifact.content_hash {
        return Err(AttachError::ArtifactChanged { expected: artifact.content_hash.clone(), found });
    }
    match artifact.kind {
        ArtifactKind::Figure => {
            let media_type = artifact.media_type.as_str();
            let supported = match format {
                Format::Latex => matches!(media_type, "image/png" | "image/jpeg" | "application/pdf"),
                Format::Typst => matches!(media_type, "image/png" | "image/jpeg" | "image/gif" | "image/svg+xml"),
                Format::Markdown => media_type.starts_with("image/"),
            };
            if !supported {
                return Err(AttachError::Unsupported(match (format, media_type) {
                    (Format::Latex, "image/svg+xml") => "pdfLaTeX cannot include SVG figures; produce a PNG (for matplotlib: %config InlineBackend.figure_formats = ['png']) and rerun.".into(),
                    _ => format!("{media_type} figures cannot be included in this document format"),
                }));
            }
            // LaTeX resolves graphics against the main file's directory;
            // Typst and Markdown against the document's own directory.
            let base = if format == Format::Latex {
                let (base, warning) = latex::graphics_base(&root, document_path, document_text);
                warnings.extend(warning);
                base
            } else {
                document_path.parent()
            };
            let content =
                AttachContent::Figure { path: relative_from(base.as_ref(), path), media_type: media_type.into() };
            Ok((content, warnings))
        }
        ArtifactKind::Table => {
            if bytes.len() as u64 > TABLE_FILE_LIMIT {
                return Err(AttachError::InvalidTable("the table is too large to insert into a document".into()));
            }
            let text = String::from_utf8(bytes).map_err(|_| AttachError::InvalidTable("table is not UTF-8".into()))?;
            let grid = TableGrid::from_csv(&text)?;
            if grid.rows.len() > TABLE_ROW_LIMIT || grid.header.len() > TABLE_COLUMN_LIMIT {
                return Err(AttachError::InvalidTable(format!(
                    "the table has {} rows and {} columns; documents accept at most {TABLE_ROW_LIMIT} rows and {TABLE_COLUMN_LIMIT} columns",
                    grid.rows.len(),
                    grid.header.len()
                )));
            }
            Ok((AttachContent::Table(grid), warnings))
        }
        other => {
            Err(AttachError::Unsupported(format!("{other:?} artifacts cannot be attached; choose a figure or table")))
        }
    }
}

/// Insert or update an artifact in an open document through the document
/// service (one undoable transaction with the request's origin).
pub fn attach_artifact(
    documents: &DocumentService,
    workspace_root: &Path,
    request: &AttachArtifactRequest,
) -> Result<(AttachArtifactOutcome, Vec<String>), AttachError> {
    let snapshot = documents.snapshot(&request.document)?;
    let document_path = snapshot.path.clone().ok_or(AttachError::UnsupportedFormat)?;
    let format = Format::from_path(&document_path).ok_or(AttachError::UnsupportedFormat)?;
    let (content, load_warnings) =
        load_content_with(workspace_root, format, &document_path, Some(&snapshot.text), &request.artifact)?;
    let input = PlanInput {
        format,
        document: &request.document,
        document_path: &document_path,
        revision: snapshot.revision,
        text: &snapshot.text,
    };
    let mut plan = plan(&input, request, &content)?;
    if format == Format::Latex
        && let Some(main) = latex::main_file(workspace_root, &document_path, Some(&snapshot.text))
        && main != document_path
    {
        // An included file: preamble advice only when the main file lacks the package.
        plan.warnings.retain(|warning| {
            !["graphicx", "booktabs"].iter().any(|package| {
                warning.contains(&format!("\\usepackage{{{package}}}"))
                    && latex::loads_package(workspace_root, &main, package)
            })
        });
    }
    plan.warnings.splice(0..0, load_warnings);
    let outcome = documents.apply(plan.transaction.clone())?;
    let applied = outcome
        .applied
        .into_iter()
        .find(|edit| edit.document == request.document)
        .ok_or_else(|| AttachError::Artifact("the document service applied no edit".into()))?;
    // Locate the block in the new revision from the patches as applied (after any rebase).
    let mut shift: i64 = 0;
    let mut start = 0u32;
    for (index, patch) in applied.patches.iter().enumerate() {
        if index == plan.block_patch {
            start = (i64::from(patch.range.start) + shift) as u32 + plan.block_offset;
            break;
        }
        shift += utf16_len(&patch.text) as i64 - i64::from(patch.range.len());
    }
    let outcome = AttachArtifactOutcome {
        document: request.document.clone(),
        revision: applied.revision,
        attachment_key: plan.attachment_key,
        range: TextRange::new(start, start + plan.block_len),
    };
    Ok((outcome, plan.warnings))
}

fn validate_key(key: &str) -> Result<(), AttachError> {
    let valid = !key.is_empty()
        && key.len() <= 64
        && key.starts_with(|c: char| c.is_ascii_alphabetic())
        && key.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'))
        && !key.contains("--")
        && !key.ends_with('.');
    if valid { Ok(()) } else { Err(AttachError::InvalidKey(key.to_owned())) }
}

fn derive_key(format: Format, text: &str, table: bool, artifact: &ArtifactRecord) -> String {
    let digest = artifact.content_hash.trim_start_matches("sha256:");
    let short: String = digest.chars().filter(char::is_ascii_hexdigit).take(8).collect();
    let short = if short.is_empty() { "artifact".to_owned() } else { short };
    let base = format!("{}-{short}", if table { "tab" } else { "fig" });
    let mut key = base.clone();
    let mut n = 2;
    while blocks::find_markers(format, text, &key) != (0, 0) || blocks::label_in_use(format, text, &key) {
        key = format!("{base}-{n}");
        n += 1;
    }
    key
}

/// Path of `target` relative to the directory containing `document`.
pub fn relative_path(document: &WorkspacePath, target: &WorkspacePath) -> String {
    relative_from(document.parent().as_ref(), target)
}

/// Path of `target` relative to directory `dir` (`None` = workspace root).
pub fn relative_from(dir: Option<&WorkspacePath>, target: &WorkspacePath) -> String {
    let doc_dir: Vec<String> = dir.map(|p| p.components().map(str::to_owned).collect()).unwrap_or_default();
    let target: Vec<&str> = target.components().collect();
    let common = doc_dir.iter().zip(&target).take_while(|(a, b)| a.as_str() == **b).count();
    let mut parts: Vec<&str> = vec![".."; doc_dir.len() - common];
    parts.extend(&target[common..]);
    parts.join("/")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests;
