//! Explorer file actions: New File, Rename, and importing files dropped from
//! the operating system (QOL E/G). Names are validated here, in the backend,
//! with messages the explorer shows as they are; the service methods live in
//! `service::files`.

use exposition_core::document::DocumentInfo;
use exposition_core::ops::OpStatus;
use exposition_core::path::WorkspacePath;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{WorkspaceError, WorkspaceResult};

/// Longest name (UTF-16 code units), the common file-system component limit.
pub const MAX_NAME_UNITS: usize = 255;

/// Check one file or folder name typed by the user (a single path component).
/// Any extension, a leading dot, and no extension at all are valid; spaces
/// inside and Unicode are valid. Refused: empty, separators, `.`/`..`,
/// characters Windows cannot store, a trailing space or period (Windows drops
/// them, so two spellings would name one file), reserved device names, and
/// over-long names.
pub fn validate_name(name: &str) -> WorkspaceResult<()> {
    let refuse = |code: &'static str, message: String| Err(WorkspaceError::new(code, message));
    if name.trim().is_empty() {
        return refuse("name.empty", "Enter a name.".into());
    }
    if name.contains('/') || name.contains('\\') {
        return refuse("name.separator", "A name cannot contain / or \\.".into());
    }
    if name == "." || name == ".." {
        return refuse("name.invalid", format!("“{name}” is not a file name."));
    }
    if name.chars().any(|c| c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*')) {
        return refuse("name.invalid", "A name cannot contain < > : \" | ? * or control characters.".into());
    }
    if name.ends_with(' ') || name.ends_with('.') {
        return refuse(
            "name.trailing",
            "A name cannot end with a space or a period (Windows removes them, so the file would get another name)."
                .into(),
        );
    }
    if name.encode_utf16().count() > MAX_NAME_UNITS {
        return refuse("name.tooLong", format!("A name can have at most {MAX_NAME_UNITS} characters."));
    }
    if WorkspacePath::parse(name).is_err() {
        // The remaining rule: Windows device names, with or without an extension.
        let stem = name.split('.').next().unwrap_or(name).trim_end();
        return refuse("name.reserved", format!("“{stem}” is a reserved name on Windows and cannot be used."));
    }
    Ok(())
}

/// A file created by New File (empty; opened by the editor afterwards).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct FileCreated {
    pub path: WorkspacePath,
}

/// Something that still uses a renamed path by its old name. The rename does
/// not rewrite it (a rename is not authority to edit other files); the
/// explorer shows it so the person can update it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PathReference {
    /// `manuscriptDependency`, `workflowInput`, `retainedEvidence`,
    /// `projectEntry`, `buildTarget`, `trigger`, `activeWork`, …
    pub kind: String,
    /// The consumer, by name (a manuscript, a workflow, the project settings).
    pub label: String,
    pub message: String,
}

/// What renaming `path` would leave pointing at the old name (nothing changes).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct RenameCheck {
    pub path: WorkspacePath,
    pub is_dir: bool,
    pub references: Vec<PathReference>,
    /// The references could not be determined (e.g. the project graph is
    /// unavailable); absence of references then proves nothing.
    pub unknown: Option<String>,
}

/// Whether the project graph has recorded a file change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum GraphRecording {
    /// Recorded in the same step: the resource kept its identity.
    Recorded { renamed: u32 },
    /// The file changed but the graph has not recorded it yet; it is retried
    /// and reconciled from the files (nothing is changed twice).
    Pending { message: String },
    /// No project graph is attached (it records the file when it attaches).
    Unavailable,
}

/// A completed rename.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct FileRenamed {
    pub from: WorkspacePath,
    pub to: WorkspacePath,
    pub is_dir: bool,
    /// Open documents at or under the old path, now at the new path (same
    /// document ids, text, revisions and undo history).
    pub documents: Vec<DocumentInfo>,
    pub graph: GraphRecording,
    /// Uses of the old path that were NOT changed.
    pub references: Vec<PathReference>,
}

/// Import files dropped from the operating system (copies; the originals
/// are never changed). Bound to the project that was open when they were
/// dropped (`root`, as `WorkspaceInfo::root`) and to `folder` in it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileImportRequest {
    pub root: String,
    /// Destination folder (`None`: the project folder).
    pub folder: Option<WorkspacePath>,
    /// Absolute paths of the dropped files and folders.
    pub sources: Vec<String>,
    /// Retrying an earlier import: sources it already imported are reported,
    /// never copied again.
    pub retry_of: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct FileImportItem {
    /// As dropped (absolute).
    pub source: String,
    pub status: OpStatus,
    /// The copy in the project (or, for a source an earlier attempt already
    /// imported, that copy).
    pub created: Option<WorkspacePath>,
    pub code: Option<String>,
    pub message: Option<String>,
}

/// Progress and result of an import (`file-import` event; the last one has
/// `finished`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct FileImportProgress {
    pub import: String,
    pub root: String,
    pub folder: Option<WorkspacePath>,
    pub total: u32,
    /// Items with an outcome so far.
    pub done: u32,
    pub finished: bool,
    pub canceled: bool,
    pub items: Vec<FileImportItem>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_names_are_valid_whatever_their_extension() {
        for ok in [
            "Untitled.txt",
            "main.tex",
            "notes.typ",
            "README.md",
            "fit.py",
            "Basic.lean",
            ".gitignore",
            ".env.local",
            "Makefile",
            "with space.txt",
            "  leading.txt",
            "Ünïcödé 数学.md",
            "a.b.c",
            "connect.txt",
            "console.log.txt",
        ] {
            assert!(validate_name(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn invalid_names_say_why() {
        for (bad, code) in [
            ("", "name.empty"),
            ("   ", "name.empty"),
            ("a/b.txt", "name.separator"),
            ("a\\b.txt", "name.separator"),
            (".", "name.invalid"),
            ("..", "name.invalid"),
            ("a:b", "name.invalid"),
            ("what?.md", "name.invalid"),
            ("tab\there", "name.invalid"),
            ("trailing.", "name.trailing"),
            ("trailing ", "name.trailing"),
            ("CON", "name.reserved"),
            ("con.txt", "name.reserved"),
            ("Nul .md", "name.reserved"),
            ("LPT1.tex", "name.reserved"),
            ("COM\u{b9}.txt", "name.reserved"),
        ] {
            let err = validate_name(bad).unwrap_err();
            assert_eq!(err.code, code, "{bad:?}: {err}");
            assert!(!err.message.is_empty());
        }
        let long = "x".repeat(MAX_NAME_UNITS + 1);
        assert_eq!(validate_name(&long).unwrap_err().code, "name.tooLong");
        assert!(validate_name(&"x".repeat(MAX_NAME_UNITS)).is_ok());
    }
}
