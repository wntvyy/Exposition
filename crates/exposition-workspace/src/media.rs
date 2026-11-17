//! Project images for view-only display: the image editor tab and inline image
//! previews in source. Read-only and bound to the open project.
//!
//! - Only files under the open folder are read, resolved like every other
//!   project path (`WorkspaceRoot::resolve_existing`: links and junctions that
//!   lead outside the folder are refused, not followed).
//! - Only image extensions in [`IMAGE_EXTENSIONS`] are read, so this cannot be
//!   used to read other project files as bytes.
//! - At most [`MAX_IMAGE_BYTES`] are read; larger files are refused unread.
//! - The answer belongs to the folder that was open when the call started: if
//!   another folder was opened meanwhile, the call fails (`workspace.changed`).
//!
//! Nothing here writes a file, a document, or project state.

use std::fs;

use exposition_core::path::WorkspacePath;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{WorkspaceError, WorkspaceResult};
use crate::fsutil::read_limited;
use crate::root::WorkspaceRoot;
use crate::service::WorkspaceService;

/// Largest image file that is read (bytes).
pub const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;

/// Extensions read as images (lowercase). The webview decides whether the
/// bytes are actually a supported image.
pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "avif", "ico"];

/// Image formats recognized by extension that cannot be displayed.
pub const UNSUPPORTED_IMAGE_EXTENSIONS: &[&str] =
    &["tif", "tiff", "heic", "heif", "psd", "eps", "ps", "pdf", "raw", "dng", "jxl", "xcf"];

fn extension(path: &WorkspacePath) -> Option<String> {
    path.extension().map(str::to_ascii_lowercase)
}

/// True when `path` has an extension this module reads.
pub fn is_image_path(path: &WorkspacePath) -> bool {
    extension(path).is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.as_str()))
}

/// Where an image reference leads, checked against the open folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum ImageLocation {
    /// An existing image file inside the folder.
    Found { path: WorkspacePath },
    /// An existing file in a format that is not displayed (PDF, EPS, TIFF…).
    Unsupported { path: WorkspacePath },
    /// The path leads outside the open folder (through `..`, a link, or a
    /// junction); it is never read.
    Blocked,
    /// No candidate exists.
    Missing,
}

impl WorkspaceService {
    /// Fail when the folder open now is not `root` (another one was opened, or
    /// it was closed, while the call ran).
    fn still_open(&self, root: &WorkspaceRoot) -> WorkspaceResult<()> {
        match self.open_root() {
            Some(now) if now.as_path() == root.path() => Ok(()),
            _ => Err(WorkspaceError::new("workspace.changed", "The project changed while the image was being read.")),
        }
    }

    /// The bytes of a project image, for display only.
    pub fn read_image(&self, path: &WorkspacePath) -> WorkspaceResult<Vec<u8>> {
        let root = self.root()?;
        let ext = extension(path).unwrap_or_default();
        if !IMAGE_EXTENSIONS.contains(&ext.as_str()) {
            return Err(if UNSUPPORTED_IMAGE_EXTENSIONS.contains(&ext.as_str()) {
                WorkspaceError::new("image.unsupported", format!("{path} is in a format that cannot be displayed."))
            } else {
                WorkspaceError::new("image.notImage", format!("{path} is not an image file."))
            });
        }
        let target = root.resolve_existing(path)?;
        let meta = fs::metadata(&target).map_err(|e| WorkspaceError::io(path, &e))?;
        if !meta.is_file() {
            return Err(WorkspaceError::new("file.isDirectory", format!("{path} is not a file.")));
        }
        let bytes =
            read_limited(&target, MAX_IMAGE_BYTES).map_err(|e| WorkspaceError::io(path, &e))?.map_err(|size| {
                WorkspaceError::new(
                    "image.tooLarge",
                    format!(
                        "{path} is {} MB; images up to {} MB are shown.",
                        size / (1024 * 1024),
                        MAX_IMAGE_BYTES / (1024 * 1024)
                    ),
                )
            })?;
        self.still_open(&root)?;
        Ok(bytes)
    }

    /// The first of `candidates` (in order) that is an existing file inside
    /// the open folder. A candidate that exists but leads outside the folder
    /// makes the answer `Blocked` unless an earlier candidate was found.
    pub fn locate_image(&self, candidates: &[WorkspacePath]) -> WorkspaceResult<ImageLocation> {
        let root = self.root()?;
        let mut blocked = false;
        let mut found = None;
        for candidate in candidates.iter().take(64) {
            match root.resolve_existing(candidate) {
                Ok(target) => {
                    if !fs::metadata(&target).is_ok_and(|m| m.is_file()) {
                        continue;
                    }
                    let shown = root.relativize(&target).unwrap_or_else(|| candidate.clone());
                    let ext = extension(candidate).unwrap_or_default();
                    found = Some(if IMAGE_EXTENSIONS.contains(&ext.as_str()) {
                        ImageLocation::Found { path: shown }
                    } else {
                        ImageLocation::Unsupported { path: shown }
                    });
                    break;
                }
                Err(e) if e.code == "path.outsideWorkspace" => blocked = true,
                Err(_) => {}
            }
        }
        self.still_open(&root)?;
        Ok(found.unwrap_or(if blocked { ImageLocation::Blocked } else { ImageLocation::Missing }))
    }
}
