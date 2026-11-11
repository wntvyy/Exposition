use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Error shape returned by application commands that have no richer typed error.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS, thiserror::Error)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
#[error("{code}: {message}")]
pub struct CommandError {
    /// Stable machine-readable code, e.g. `path.outsideWorkspace`.
    pub code: String,
    /// Human-readable message, safe to show (never contains secrets).
    pub message: String,
}

impl CommandError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into() }
    }
}
