//! Errors reported by workspace operations.
//!
//! Every error carries a stable machine-readable `code` (e.g.
//! `path.outsideWorkspace`) and a human-readable message that is safe to show:
//! messages name workspace-relative paths, never secrets or file contents.

use std::fmt;
use std::io;

use exposition_core::document::TransactionError;
use exposition_core::path::PathError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceError {
    pub code: &'static str,
    pub message: String,
}

impl WorkspaceError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }

    pub fn no_workspace() -> Self {
        Self::new("workspace.notOpen", "No folder is open.")
    }

    pub fn outside(path: impl fmt::Display) -> Self {
        Self::new("path.outsideWorkspace", format!("{path} resolves outside the open folder."))
    }

    pub fn not_found(path: impl fmt::Display) -> Self {
        Self::new("path.notFound", format!("{path} does not exist."))
    }

    /// Map an I/O error for `what` (a display path) to a stable code.
    pub fn io(what: impl fmt::Display, error: &io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::NotFound => Self::not_found(what),
            io::ErrorKind::PermissionDenied => {
                Self::new("file.permissionDenied", format!("Permission denied for {what}."))
            }
            io::ErrorKind::ReadOnlyFilesystem => {
                Self::new("file.readOnly", format!("{what} is on a read-only file system."))
            }
            _ => Self::new("file.io", format!("Could not access {what}: {error}.")),
        }
    }
}

impl fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for WorkspaceError {}

impl From<PathError> for WorkspaceError {
    fn from(error: PathError) -> Self {
        Self::new("path.invalid", error.to_string())
    }
}

impl From<TransactionError> for WorkspaceError {
    fn from(error: TransactionError) -> Self {
        let code = match &error {
            TransactionError::UnknownDocument { .. } => "document.notOpen",
            TransactionError::ReadOnly { .. } => "document.readOnly",
            TransactionError::MixedLineEndings { .. } => "document.mixedLineEndings",
            TransactionError::StaleRevision { .. } | TransactionError::Conflict { .. } => "document.changed",
            _ => "document.rejected",
        };
        Self::new(code, error.to_string())
    }
}

pub type WorkspaceResult<T> = Result<T, WorkspaceError>;
