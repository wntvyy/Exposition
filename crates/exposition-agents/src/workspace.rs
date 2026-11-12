//! Project file access for agent tools.
//!
//! Open documents always come from the document service (the single
//! authority for buffer text). Files that are not open are read from disk
//! through [`ProjectFiles`], which resolves workspace-relative paths against a
//! canonical root and refuses anything that escapes it (including through
//! symlinks or junctions). Text is decoded strictly: undecodable or binary
//! files are refused, never lossily replaced.
//!
//! When the application's [`WorkspaceService`] is present, documents are
//! opened and closed through it (disk bookkeeping, recovery journals, open
//! editors) and a task may only act while its folder is the open workspace:
//! the document service keys documents by workspace-relative path, so acting
//! on any other folder could touch the wrong project's buffers.

use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use exposition_core::document::{DocumentId, DocumentInfo, TextEncoding};
use exposition_core::path::WorkspacePath;
use exposition_workspace::{DocumentService, OpenDocument, WorkspaceService};
use serde::Serialize;
use sha2::{Digest, Sha256};

/// Largest file the agent reads or creates.
pub const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// Directories never listed.
pub(crate) const SKIPPED_DIRS: &[&str] = &[".git", "node_modules", "target", ".venv", "__pycache__", ".lake"];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FileError {
    #[error("{0} does not exist")]
    NotFound(String),
    #[error("{0} resolves outside the project folder")]
    OutsideRoot(String),
    #[error("{0} already exists")]
    AlreadyExists(String),
    #[error("{0} is a directory")]
    IsDirectory(String),
    #[error("{0} is not a directory")]
    NotDirectory(String),
    #[error("{path} is too large ({size} bytes; limit {limit})")]
    TooLarge { path: String, size: u64, limit: u64 },
    #[error("{0} is not text in a supported encoding (UTF-8 or UTF-16 with BOM); it was not read")]
    NotText(String),
    #[error("project folder is invalid: {0}")]
    InvalidRoot(String),
    #[error("file system error for {path}: {message}")]
    Io { path: String, message: String },
    #[error("{0}")]
    Workspace(String),
}

fn io_err(path: &str, error: std::io::Error) -> FileError {
    if error.kind() == std::io::ErrorKind::NotFound {
        FileError::NotFound(path.to_owned())
    } else {
        FileError::Io { path: path.to_owned(), message: error.kind().to_string() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub path: String,
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Listing {
    pub entries: Vec<FileEntry>,
    pub truncated: bool,
}

#[derive(Debug, Clone)]
pub struct LoadedText {
    pub text: String,
    pub encoding: TextEncoding,
}

/// Disk access confined to one project root.
pub trait ProjectFiles: Send + Sync {
    /// Canonical root.
    fn root(&self) -> &Path;
    fn list(&self, dir: Option<&WorkspacePath>, recursive: bool, limit: usize) -> Result<Listing, FileError>;
    fn read_text(&self, path: &WorkspacePath) -> Result<LoadedText, FileError>;
    fn exists(&self, path: &WorkspacePath) -> Result<bool, FileError>;
    /// Create a new file; never overwrites.
    fn create_text(&self, path: &WorkspacePath, text: &str) -> Result<(), FileError>;
    fn remove_file(&self, path: &WorkspacePath) -> Result<(), FileError>;
}

#[derive(Debug, Clone)]
pub struct LocalProjectFiles {
    root: PathBuf,
}

impl LocalProjectFiles {
    /// Open a project root: an existing directory that is not a filesystem root.
    pub fn open(root: &Path) -> Result<Self, FileError> {
        if !root.is_absolute() {
            return Err(FileError::InvalidRoot("the project folder must be an absolute path".into()));
        }
        let canonical =
            fs::canonicalize(root).map_err(|_| FileError::InvalidRoot("the folder does not exist".into()))?;
        if !canonical.is_dir() {
            return Err(FileError::InvalidRoot("the path is not a folder".into()));
        }
        let depth = canonical.components().filter(|c| matches!(c, Component::Normal(_))).count();
        if depth == 0 {
            return Err(FileError::InvalidRoot("a drive or filesystem root cannot be a project folder".into()));
        }
        Ok(Self { root: canonical })
    }

    /// The root without Windows verbatim prefixes, for display.
    pub fn display_root(&self) -> String {
        display_path(&self.root)
    }

    fn joined(&self, path: &WorkspacePath) -> PathBuf {
        let mut out = self.root.clone();
        for part in path.components() {
            out.push(part);
        }
        out
    }

    /// Resolve an existing path, refusing escapes through links.
    fn resolve_existing(&self, path: &WorkspacePath) -> Result<PathBuf, FileError> {
        let canonical = fs::canonicalize(self.joined(path)).map_err(|e| io_err(path.as_str(), e))?;
        if !canonical.starts_with(&self.root) {
            return Err(FileError::OutsideRoot(path.to_string()));
        }
        Ok(canonical)
    }

    /// Resolve a path that may not exist yet: its nearest existing ancestor
    /// must resolve inside the root.
    fn resolve_new(&self, path: &WorkspacePath) -> Result<PathBuf, FileError> {
        let target = self.joined(path);
        let mut ancestor = target.parent();
        while let Some(dir) = ancestor {
            if dir.exists() {
                let canonical = fs::canonicalize(dir).map_err(|e| io_err(path.as_str(), e))?;
                if !canonical.starts_with(&self.root) {
                    return Err(FileError::OutsideRoot(path.to_string()));
                }
                if !canonical.is_dir() {
                    return Err(FileError::NotDirectory(display_path(dir)));
                }
                return Ok(target);
            }
            ancestor = dir.parent();
        }
        Err(FileError::OutsideRoot(path.to_string()))
    }

    fn relative(&self, absolute: &Path) -> Option<String> {
        let rel = absolute.strip_prefix(&self.root).ok()?;
        let parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
        Some(parts.join("/"))
    }
}

impl ProjectFiles for LocalProjectFiles {
    fn root(&self) -> &Path {
        &self.root
    }

    fn list(&self, dir: Option<&WorkspacePath>, recursive: bool, limit: usize) -> Result<Listing, FileError> {
        let start = match dir {
            Some(path) => {
                let resolved = self.resolve_existing(path)?;
                if !resolved.is_dir() {
                    return Err(FileError::NotDirectory(path.to_string()));
                }
                resolved
            }
            None => self.root.clone(),
        };
        let mut entries = Vec::new();
        let mut truncated = false;
        let mut stack = vec![start];
        while let Some(current) = stack.pop() {
            let read = match fs::read_dir(&current) {
                Ok(read) => read,
                Err(_) => continue,
            };
            let mut children: Vec<_> = read.filter_map(Result::ok).collect();
            children.sort_by_key(|e| e.file_name());
            let mut subdirs = Vec::new();
            for child in children {
                let name = child.file_name().to_string_lossy().into_owned();
                let Ok(meta) = fs::symlink_metadata(child.path()) else { continue };
                // Links (symlinks, junctions) are not followed or listed.
                if meta.file_type().is_symlink() {
                    continue;
                }
                let Ok(canonical) = fs::canonicalize(child.path()) else { continue };
                if !canonical.starts_with(&self.root) {
                    continue;
                }
                let Some(rel) = self.relative(&canonical) else { continue };
                if entries.len() >= limit {
                    truncated = true;
                    break;
                }
                if meta.is_dir() {
                    if SKIPPED_DIRS.contains(&name.as_str()) {
                        continue;
                    }
                    entries.push(FileEntry { path: rel, kind: "directory", size: None });
                    if recursive {
                        subdirs.push(canonical);
                    }
                } else {
                    entries.push(FileEntry { path: rel, kind: "file", size: Some(meta.len()) });
                }
            }
            if truncated {
                break;
            }
            // Depth-first in name order.
            stack.extend(subdirs.into_iter().rev());
        }
        Ok(Listing { entries, truncated })
    }

    fn read_text(&self, path: &WorkspacePath) -> Result<LoadedText, FileError> {
        let resolved = self.resolve_existing(path)?;
        let meta = fs::metadata(&resolved).map_err(|e| io_err(path.as_str(), e))?;
        if meta.is_dir() {
            return Err(FileError::IsDirectory(path.to_string()));
        }
        if meta.len() > MAX_FILE_BYTES {
            return Err(FileError::TooLarge { path: path.to_string(), size: meta.len(), limit: MAX_FILE_BYTES });
        }
        let bytes = fs::read(&resolved).map_err(|e| io_err(path.as_str(), e))?;
        decode_text(&bytes).ok_or_else(|| FileError::NotText(path.to_string()))
    }

    fn exists(&self, path: &WorkspacePath) -> Result<bool, FileError> {
        let target = self.resolve_new(path)?;
        match fs::symlink_metadata(&target) {
            Ok(_) => {
                // Exists: make sure it does not resolve outside the root either.
                self.resolve_existing(path)?;
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(io_err(path.as_str(), e)),
        }
    }

    fn create_text(&self, path: &WorkspacePath, text: &str) -> Result<(), FileError> {
        if text.len() as u64 > MAX_FILE_BYTES {
            return Err(FileError::TooLarge { path: path.to_string(), size: text.len() as u64, limit: MAX_FILE_BYTES });
        }
        let target = self.resolve_new(path)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| io_err(path.as_str(), e))?;
            // Re-check after creating directories.
            let canonical = fs::canonicalize(parent).map_err(|e| io_err(path.as_str(), e))?;
            if !canonical.starts_with(&self.root) {
                return Err(FileError::OutsideRoot(path.to_string()));
            }
        }
        // Reserve the name first so an existing file is never overwritten,
        // then write through a temporary file and replace the reservation.
        fs::OpenOptions::new().write(true).create_new(true).open(&target).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                FileError::AlreadyExists(path.to_string())
            } else {
                io_err(path.as_str(), e)
            }
        })?;
        let temp = target.with_file_name(format!(
            ".{}.exposition-{}.tmp",
            target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            uuid::Uuid::new_v4().simple()
        ));
        let write = (|| -> std::io::Result<()> {
            let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&temp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temp, &target)
        })();
        if let Err(e) = write {
            let _ = fs::remove_file(&temp);
            let _ = fs::remove_file(&target);
            return Err(io_err(path.as_str(), e));
        }
        Ok(())
    }

    fn remove_file(&self, path: &WorkspacePath) -> Result<(), FileError> {
        let resolved = self.resolve_existing(path)?;
        if resolved.is_dir() {
            return Err(FileError::IsDirectory(path.to_string()));
        }
        fs::remove_file(&resolved).map_err(|e| io_err(path.as_str(), e))
    }
}

/// Strict decoding: UTF-8 (optionally with BOM) or UTF-16 with BOM. Text with
/// NUL characters is treated as binary.
pub fn decode_text(bytes: &[u8]) -> Option<LoadedText> {
    let (text, encoding) = if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        (String::from_utf8(rest.to_vec()).ok()?, TextEncoding::Utf8Bom)
    } else if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        (decode_utf16(rest, u16::from_le_bytes)?, TextEncoding::Utf16Le)
    } else if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        (decode_utf16(rest, u16::from_be_bytes)?, TextEncoding::Utf16Be)
    } else {
        (String::from_utf8(bytes.to_vec()).ok()?, TextEncoding::Utf8)
    };
    if text.contains('\0') {
        return None;
    }
    Some(LoadedText { text, encoding })
}

fn decode_utf16(bytes: &[u8], read: fn([u8; 2]) -> u16) -> Option<String> {
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let units = bytes.chunks_exact(2).map(|pair| read([pair[0], pair[1]]));
    char::decode_utf16(units).collect::<Result<String, _>>().ok()
}

pub fn display_path(path: &Path) -> String {
    let raw = path.display().to_string();
    raw.strip_prefix(r"\\?\UNC\")
        .map(|rest| format!(r"\\{rest}"))
        .or_else(|| raw.strip_prefix(r"\\?\").map(str::to_owned))
        .unwrap_or(raw)
}

pub fn content_hash(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Editor language identifier for a path.
pub fn language_for(path: &WorkspacePath) -> &'static str {
    match path.extension().map(str::to_ascii_lowercase).as_deref() {
        Some("typ") => "typst",
        Some("tex" | "sty" | "cls" | "ltx") => "latex",
        Some("bib") => "bibtex",
        Some("md" | "markdown") => "markdown",
        Some("py") => "python",
        Some("r") => "r",
        Some("jl") => "julia",
        Some("lean") => "lean",
        Some("json") => "json",
        Some("toml") => "toml",
        Some("yaml" | "yml") => "yaml",
        Some("csv") => "csv",
        _ => "plaintext",
    }
}

/// Ok when the task's folder is the open workspace (or no workspace service is
/// in use); otherwise a message explaining why the agent cannot act.
pub fn check_root(workspace: Option<&WorkspaceService>, root: &Path) -> Result<(), String> {
    let Some(workspace) = workspace else { return Ok(()) };
    let Some(current) = workspace.current() else {
        return Err(format!(
            "No project folder is open. Open {} in Exposition to let the agent work on it.",
            display_path(root)
        ));
    };
    let open = fs::canonicalize(&current.root).unwrap_or_else(|_| PathBuf::from(&current.root));
    let task = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    if open == task {
        Ok(())
    } else {
        Err(format!(
            "The open project folder is {}, not this task's folder {}. Open the task's folder to continue.",
            current.root,
            display_path(root)
        ))
    }
}

/// Open `path` as a document: through the workspace service when present,
/// otherwise directly from disk.
pub fn open_document_in(
    workspace: Option<&WorkspaceService>,
    documents: &DocumentService,
    files: &dyn ProjectFiles,
    path: &WorkspacePath,
) -> Result<DocumentId, FileError> {
    match workspace {
        Some(workspace) => workspace.open_document(path).map(|opened| opened.state.info.id).map_err(|e| match e.code {
            "path.notFound" => FileError::NotFound(path.to_string()),
            _ => FileError::Workspace(e.message),
        }),
        None => open_document(documents, files, path).map(|(id, _)| id),
    }
}

/// Close a document (through the workspace service when present).
pub fn close_document_in(
    workspace: Option<&WorkspaceService>,
    documents: &DocumentService,
    id: &DocumentId,
) -> Result<(), String> {
    match workspace {
        Some(workspace) => workspace.close_document(id).map_err(|e| e.message),
        None => documents.close(id).map_err(|e| e.to_string()),
    }
}

/// Return the open document for `path`, opening it from disk if needed.
pub fn open_document(
    documents: &DocumentService,
    files: &dyn ProjectFiles,
    path: &WorkspacePath,
) -> Result<(DocumentId, bool), FileError> {
    if let Some(id) = documents.find_by_path(path) {
        return Ok((id, false));
    }
    let loaded = files.read_text(path)?;
    let info: DocumentInfo = documents.open(OpenDocument {
        path: Some(path.clone()),
        text: loaded.text,
        language: language_for(path).to_owned(),
        encoding: loaded.encoding,
        read_only: None,
    });
    Ok((info.id, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> WorkspacePath {
        WorkspacePath::parse(s).unwrap()
    }

    #[test]
    fn reads_lists_and_creates_inside_root() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("main.typ"), "= Title\n").unwrap();
        fs::create_dir(dir.path().join("chapters")).unwrap();
        fs::write(dir.path().join("chapters/one.typ"), "One").unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        let files = LocalProjectFiles::open(dir.path()).unwrap();

        let listing = files.list(None, true, 100).unwrap();
        let paths: Vec<_> = listing.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, vec!["chapters", "main.typ", "chapters/one.typ"]);

        assert_eq!(files.read_text(&p("main.typ")).unwrap().text, "= Title\n");
        files.create_text(&p("figures/new.typ"), "new").unwrap();
        assert_eq!(fs::read_to_string(dir.path().join("figures/new.typ")).unwrap(), "new");
        assert_eq!(files.create_text(&p("main.typ"), "x"), Err(FileError::AlreadyExists("main.typ".into())));
        assert_eq!(fs::read_to_string(dir.path().join("main.typ")).unwrap(), "= Title\n");
        assert!(matches!(files.read_text(&p("missing.typ")), Err(FileError::NotFound(_))));
    }

    #[test]
    fn refuses_undecodable_and_binary_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("latin1.tex"), [0x63, 0x61, 0x66, 0xE9]).unwrap();
        fs::write(dir.path().join("bin.dat"), [0x00, 0x01, 0x02]).unwrap();
        let files = LocalProjectFiles::open(dir.path()).unwrap();
        assert!(matches!(files.read_text(&p("latin1.tex")), Err(FileError::NotText(_))));
        assert!(matches!(files.read_text(&p("bin.dat")), Err(FileError::NotText(_))));
        let utf16 = decode_text(&[0xFF, 0xFE, b'h', 0, b'i', 0]).unwrap();
        assert_eq!((utf16.text.as_str(), utf16.encoding), ("hi", TextEncoding::Utf16Le));
    }

    #[test]
    fn rejects_filesystem_roots_and_relative_roots() {
        assert!(LocalProjectFiles::open(Path::new("relative/dir")).is_err());
        let root = if cfg!(windows) { Path::new("C:\\") } else { Path::new("/") };
        assert!(matches!(LocalProjectFiles::open(root), Err(FileError::InvalidRoot(_))));
    }

    #[cfg(windows)]
    #[test]
    fn refuses_junction_escapes() {
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("link");
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(outside.path())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !made {
            eprintln!("skipped: could not create a junction");
            return;
        }
        let files = LocalProjectFiles::open(dir.path()).unwrap();
        assert!(matches!(files.read_text(&p("link/secret.txt")), Err(FileError::OutsideRoot(_))));
        assert!(matches!(files.create_text(&p("link/new.txt"), "x"), Err(FileError::OutsideRoot(_))));
        assert!(!outside.path().join("new.txt").exists());
        let listed = files.list(None, true, 100).unwrap();
        assert!(listed.entries.iter().all(|e| !e.path.starts_with("link")), "{:?}", listed.entries);
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlink_escapes() {
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        let files = LocalProjectFiles::open(dir.path()).unwrap();
        assert!(matches!(files.read_text(&p("link/secret.txt")), Err(FileError::OutsideRoot(_))));
    }
}
