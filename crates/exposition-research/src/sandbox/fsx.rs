//! File-system checks shared by staging and output registration: link
//! detection without following links, no-follow opens, and link counts.

use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::path::Path;

/// Whether `lstat` metadata describes a link of any kind: a symbolic link, a
/// junction, or any other reparse point (Windows; WSL-style links created
/// through a container bind mount are reparse points too).
pub(crate) fn is_link_like(meta: &Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

/// Whether `lstat` metadata describes a link that names another file: a
/// symbolic link, junction, or WSL link (name-surrogate reparse points).
/// Other reparse points (cloud placeholders, deduplicated files) are files.
pub(crate) fn is_name_link(meta: &Metadata) -> bool {
    meta.file_type().is_symlink()
}

/// Open a file for reading without following a final link (Windows opens the
/// reparse point itself; elsewhere the caller compares identities with the
/// `lstat` result, see [`same_file_as`]).
pub(crate) fn open_no_follow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

/// Whether the opened file is the one `lstat` described (no swap between
/// the check and the open).
#[cfg(unix)]
pub(crate) fn same_file_as(file: &File, lstat: &Metadata) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let opened = file.metadata()?;
    Ok(opened.dev() == lstat.dev() && opened.ino() == lstat.ino())
}

/// Windows: the handle was opened without following reparse points, so its
/// own metadata is checked instead (see [`checked_regular_file`]).
#[cfg(windows)]
pub(crate) fn same_file_as(_file: &File, _lstat: &Metadata) -> io::Result<bool> {
    Ok(true)
}

/// Number of names (hard links) the opened file has.
pub(crate) fn link_count(file: &File) -> io::Result<u64> {
    #[cfg(windows)]
    {
        Ok(winapi_util::file::information(file)?.number_of_links())
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(file.metadata()?.nlink())
    }
}

/// Why a path is not a plain regular file (checked with `lstat`, then on the
/// opened handle). On success returns the open handle and its length.
pub(crate) fn checked_regular_file(path: &Path) -> Result<(File, u64), String> {
    let lstat = std::fs::symlink_metadata(path).map_err(|e| format!("cannot be inspected ({e})"))?;
    if is_link_like(&lstat) {
        // Docker Desktop shows links and FIFOs a container creates in a bind
        // mount as reparse points on Windows.
        return Err(
            "a link or special file (symbolic link, junction, or other reparse point such as a container-made FIFO); never followed"
                .into(),
        );
    }
    if lstat.is_dir() {
        return Err("a directory (only files directly in the output folder are exported)".into());
    }
    if !lstat.is_file() {
        return Err("not a regular file (device, FIFO or socket)".into());
    }
    let file = open_no_follow(path).map_err(|e| format!("cannot be opened ({e})"))?;
    let meta = file.metadata().map_err(|e| format!("cannot be inspected ({e})"))?;
    if is_link_like(&meta) || !meta.is_file() {
        return Err("changed into a link or special file while it was checked".into());
    }
    if !same_file_as(&file, &lstat).map_err(|e| format!("cannot be inspected ({e})"))? {
        return Err("was replaced while it was checked".into());
    }
    let links = link_count(&file).map_err(|e| format!("cannot be inspected ({e})"))?;
    if links != 1 {
        return Err(format!("a hard link ({links} names for one file)"));
    }
    Ok((file, meta.len()))
}
