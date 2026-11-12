//! Workspace-relative paths at the IPC boundary.
//!
//! A [`WorkspacePath`] is a normalized, relative, forward-slash path. It can never
//! be absolute, contain a drive or UNC prefix, traverse upward, or contain
//! characters that are invalid in file names on supported platforms. Resolving it
//! against a canonical workspace root (and rejecting symlink escapes) is the
//! workspace service's job; this type only guarantees the lexical rules.

use std::fmt;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, TS)]
#[ts(export, type = "string")]
pub struct WorkspacePath(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("path is empty")]
    Empty,
    #[error("path must be relative to the workspace: {0}")]
    Absolute(String),
    #[error("path may not traverse outside the workspace: {0}")]
    Traversal(String),
    #[error("path contains an invalid component: {0}")]
    InvalidComponent(String),
}

/// Windows reserved device names (case-insensitive, with or without extension).
const RESERVED: &[&str] = &[
    "CON",
    "PRN",
    "AUX",
    "NUL",
    "COM1",
    "COM2",
    "COM3",
    "COM4",
    "COM5",
    "COM6",
    "COM7",
    "COM8",
    "COM9",
    "LPT1",
    "LPT2",
    "LPT3",
    "LPT4",
    "LPT5",
    "LPT6",
    "LPT7",
    "LPT8",
    "LPT9",
    // Also treated as devices by Windows: port 0, superscript digits, and the
    // console streams.
    "COM0",
    "LPT0",
    "COM\u{b9}",
    "COM\u{b2}",
    "COM\u{b3}",
    "LPT\u{b9}",
    "LPT\u{b2}",
    "LPT\u{b3}",
    "CONIN$",
    "CONOUT$",
    "CLOCK$",
];

impl WorkspacePath {
    /// Parse and normalize a workspace-relative path. Backslashes are treated as
    /// separators; `.` components and duplicate separators are removed.
    pub fn parse(raw: &str) -> Result<Self, PathError> {
        if raw.is_empty() {
            return Err(PathError::Empty);
        }
        let unified = raw.replace('\\', "/");
        if unified.starts_with('/') || has_drive_prefix(&unified) {
            return Err(PathError::Absolute(raw.to_owned()));
        }
        let mut parts: Vec<&str> = Vec::new();
        for component in unified.split('/') {
            match component {
                "" | "." => continue,
                ".." => return Err(PathError::Traversal(raw.to_owned())),
                part => {
                    validate_component(part).map_err(|_| PathError::InvalidComponent(part.to_owned()))?;
                    parts.push(part);
                }
            }
        }
        if parts.is_empty() {
            return Err(PathError::Empty);
        }
        Ok(Self(parts.join("/")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }

    pub fn file_name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }

    pub fn extension(&self) -> Option<&str> {
        let name = self.file_name();
        name.rfind('.').filter(|&i| i > 0).map(|i| &name[i + 1..])
    }

    pub fn parent(&self) -> Option<WorkspacePath> {
        self.0.rfind('/').map(|i| WorkspacePath(self.0[..i].to_owned()))
    }

    pub fn join(&self, child: &str) -> Result<WorkspacePath, PathError> {
        WorkspacePath::parse(&format!("{}/{}", self.0, child))
    }
}

impl fmt::Display for WorkspacePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for WorkspacePath {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        WorkspacePath::parse(&raw).map_err(serde::de::Error::custom)
    }
}

fn has_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn validate_component(part: &str) -> Result<(), ()> {
    if part.chars().any(|c| c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*')) {
        return Err(());
    }
    // Trailing dots/spaces are silently stripped by Windows, which makes two
    // different strings name the same file.
    if part.ends_with('.') || part.ends_with(' ') {
        return Err(());
    }
    // Windows ignores trailing spaces before the extension (`nul .txt`).
    let stem = part.split('.').next().unwrap_or(part).trim_end_matches(' ').to_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        return Err(());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_relative_paths() {
        assert_eq!(WorkspacePath::parse("a/./b//c.typ").unwrap().as_str(), "a/b/c.typ");
        assert_eq!(WorkspacePath::parse(r"chapters\intro.tex").unwrap().as_str(), "chapters/intro.tex");
        let p = WorkspacePath::parse("figs/plot.final.png").unwrap();
        assert_eq!(p.file_name(), "plot.final.png");
        assert_eq!(p.extension(), Some("png"));
        assert_eq!(p.parent().unwrap().as_str(), "figs");
        assert_eq!(WorkspacePath::parse(".gitignore").unwrap().extension(), None);
    }

    #[test]
    fn rejects_escapes_and_invalid_names() {
        for bad in [
            "",
            ".",
            "/etc/passwd",
            r"C:\Windows",
            "c:relative",
            "../x",
            "a/../../x",
            r"\\server\share",
            "a/con.txt",
            "a/NUL",
            "COM\u{b9}",
            "LPT\u{b2}.tex",
            "COM0",
            "LPT0.txt",
            "CONIN$",
            "conout$.log",
            "nul .txt",
            "a/b?.tex",
            "trailing.",
            "a/space ",
            "bad\u{0}name",
        ] {
            assert!(WorkspacePath::parse(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn deserialization_validates() {
        assert!(serde_json::from_str::<WorkspacePath>("\"../secret\"").is_err());
        let ok: WorkspacePath = serde_json::from_str("\"main.typ\"").unwrap();
        assert_eq!(ok.as_str(), "main.typ");
    }
}
