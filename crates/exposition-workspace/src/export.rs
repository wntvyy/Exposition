//! Destinations of exported output copies (a document's PDF): files the
//! person chose, inside or outside the project. An export is written here,
//! not through the document service: the file is an output copy, never a
//! project document.

use std::io;
use std::path::{Path, PathBuf};

use exposition_core::path::WorkspacePath;

use crate::fsutil;
use crate::root::display_path;

/// Check a chosen export destination before anything is written: an absolute
/// path whose file name is valid on every supported platform (no device name,
/// no `:` stream, no trailing dot or space) and ends in `.{extension}` (any
/// case), in a folder that exists. Returns the destination under the
/// canonical folder (links and `..` resolved), so the write goes where the
/// checks looked. The message names what is wrong.
pub fn checked_destination(path: &Path, extension: &str) -> Result<PathBuf, String> {
    let has_extension = path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case(extension));
    let name = path.file_name().and_then(|n| n.to_str());
    let (Some(name), true, true) = (name, has_extension, path.is_absolute()) else {
        return Err(format!("an absolute .{extension} path is required"));
    };
    // The project's file-name rules: Windows silently maps device names,
    // `name:stream`, and trailing dots or spaces to something else.
    if name.contains(['/', '\\']) || WorkspacePath::parse(name).is_err() {
        return Err(format!("{name} is not a valid file name"));
    }
    let folder = path.parent().filter(|p| !p.as_os_str().is_empty()).ok_or("the destination has no folder")?;
    let canonical = std::fs::canonicalize(folder).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => format!("the destination folder does not exist ({})", folder.display()),
        _ => format!("the destination folder cannot be used: {}", describe_io(&e, folder)),
    })?;
    if !canonical.is_dir() {
        return Err(format!("the destination folder does not exist ({})", folder.display()));
    }
    Ok(canonical.join(name))
}

/// Write an export to a [`checked_destination`]: atomically (a temporary file
/// in the same folder, synced, then renamed), so a failed export leaves an
/// existing destination file as it was and no partial file behind. The error
/// says why the destination could not be written; nothing is written
/// anywhere else instead.
pub fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    fsutil::write_atomic(path, bytes).map_err(|e| {
        let kept = if path.exists() { " The existing file was not changed." } else { "" };
        format!("{}.{kept}", describe_io(&e, path))
    })
}

/// Why `path` could not be read or written, in words (permission, missing
/// folder, a file in use, a full or unavailable device), followed by the
/// system's own message.
pub fn describe_io(error: &io::Error, path: &Path) -> String {
    let shown = display_path(path);
    let reason = match (error.kind(), error.raw_os_error()) {
        // ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION.
        (_, Some(32 | 33)) if cfg!(windows) => "is in use by another program; close it there and export again",
        // ERROR_NOT_READY, ERROR_BAD_NETPATH, ERROR_DEV_NOT_EXIST, ERROR_BAD_NET_NAME, ERROR_DEVICE_NOT_CONNECTED.
        (_, Some(21 | 53 | 55 | 67 | 1167)) if cfg!(windows) => {
            "is on a drive or network location that is not available"
        }
        (io::ErrorKind::PermissionDenied, _) => {
            "cannot be written: permission was denied (the folder or file is read-only, protected, or open in another program)"
        }
        (io::ErrorKind::NotFound, _) => "cannot be written: its folder no longer exists",
        (io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded, _) => "cannot be written: the disk is full",
        (io::ErrorKind::ReadOnlyFilesystem, _) => "cannot be written: the location is read-only",
        _ => "cannot be written",
    };
    format!("{shown} {reason} ({error})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destinations_must_be_absolute_named_files_in_an_existing_folder() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Mes documents — été 数学");
        std::fs::create_dir_all(&folder).unwrap();

        let ok = checked_destination(&folder.join("Thèse finale (v2).PDF"), "pdf").unwrap();
        assert_eq!(ok.file_name().unwrap(), "Thèse finale (v2).PDF");
        assert_eq!(ok.parent().unwrap(), std::fs::canonicalize(&folder).unwrap());

        // `..` is resolved before anything is written.
        let dotted = checked_destination(&folder.join("..").join("out.pdf"), "pdf").unwrap();
        assert_eq!(dotted, std::fs::canonicalize(dir.path()).unwrap().join("out.pdf"));

        let missing = checked_destination(&dir.path().join("missing").join("out.pdf"), "pdf").unwrap_err();
        assert!(missing.contains("does not exist"), "{missing}");
        for refused in [
            PathBuf::from("relative.pdf"),
            folder.join("notes.txt"),
            folder.join("no-extension"),
            folder.join("NUL.pdf"),
            folder.join("con.pdf"),
            folder.join("trailing .pdf "),
            folder.clone(),
        ] {
            assert!(checked_destination(&refused, "pdf").is_err(), "{} accepted", refused.display());
        }
        #[cfg(windows)]
        {
            let stream = PathBuf::from(format!("{}:x.pdf", folder.join("main.typ").display()));
            let error = checked_destination(&stream, "pdf").unwrap_err();
            assert!(error.contains("not a valid file name"), "{error}");
        }
        // A file is not a folder to export into.
        std::fs::write(folder.join("file"), "x").unwrap();
        assert!(checked_destination(&folder.join("file").join("out.pdf"), "pdf").is_err());
    }

    #[test]
    fn a_failed_write_keeps_the_existing_file_and_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let target = checked_destination(&dir.path().join("out.pdf"), "pdf").unwrap();
        write(&target, b"%PDF-1.7 first").unwrap();
        write(&target, b"%PDF-1.7 second").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"%PDF-1.7 second");

        // The folder disappears between the check and the write.
        let gone = dir.path().join("gone");
        std::fs::create_dir(&gone).unwrap();
        let inside = checked_destination(&gone.join("out.pdf"), "pdf").unwrap();
        std::fs::remove_dir(&gone).unwrap();
        let error = write(&inside, b"%PDF-1.7").unwrap_err();
        assert!(error.contains("out.pdf") && error.contains("no longer exists"), "{error}");
        assert!(!error.contains(r"\\?\"), "shown without the verbatim prefix: {error}");

        // Another program holds the destination open (a PDF viewer).
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(&target).unwrap();
            let error = write(&target, b"%PDF-1.7 third").unwrap_err();
            assert!(error.contains("another program"), "{error}");
            assert!(error.contains("The existing file was not changed."), "{error}");
            drop(held);
            assert_eq!(std::fs::read(&target).unwrap(), b"%PDF-1.7 second", "the old export is intact");
        }
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n != "out.pdf")
            .collect();
        assert!(leftovers.is_empty(), "temporary files left: {leftovers:?}");
    }
}
