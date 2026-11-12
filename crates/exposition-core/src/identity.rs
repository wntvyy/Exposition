//! File system object identity.
//!
//! One folder can be reached through many names that text comparison cannot
//! relate: the local administrative share (`\\localhost\C$\Users\me`,
//! `\\127.0.0.1\C$\…`, `\\<computer>\C$\…`, `\\?\UNC\localhost\C$\…`,
//! `\\localhost\ADMIN$`), a drive mapped to such a share, and so on. Their
//! identity (volume serial number and file index on Windows, device and inode
//! on Unix) is the same. Rules about which folders may be opened, trusted, or
//! hold tool locations compare identities as well as canonical text, so an
//! alias cannot pass a rule its target fails.

use std::path::Path;

/// The identity of an existing file or directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileIdentity {
    volume: u64,
    index: u64,
}

/// The identity of `path`, if it exists and can be opened for its attributes.
pub fn file_identity(path: &Path) -> Option<FileIdentity> {
    platform::identity(path)
}

/// Identities of `path` and each of its ancestors that can be read, nearest
/// first.
pub fn identity_chain(path: &Path) -> Vec<FileIdentity> {
    path.ancestors().filter_map(file_identity).collect()
}

/// `path` is `ancestor` or lies beneath it, by identity (the same folder under
/// any name). False when either cannot be read; callers also compare text.
pub fn within_by_identity(path: &Path, ancestor: &Path) -> bool {
    file_identity(ancestor).is_some_and(|target| identity_chain(path).contains(&target))
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod platform {
    use std::ffi::c_void;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;

    use super::FileIdentity;

    /// `BY_HANDLE_FILE_INFORMATION` (fileapi.h).
    #[repr(C)]
    #[derive(Default)]
    struct ByHandleFileInformation {
        file_attributes: u32,
        creation_time: [u32; 2],
        last_access_time: [u32; 2],
        last_write_time: [u32; 2],
        volume_serial_number: u32,
        file_size_high: u32,
        file_size_low: u32,
        number_of_links: u32,
        file_index_high: u32,
        file_index_low: u32,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandle(file: *mut c_void, information: *mut ByHandleFileInformation) -> i32;
    }

    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    const FILE_SHARE_ALL: u32 = 0x1 | 0x2 | 0x4;
    /// Required to open a directory handle.
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

    pub(super) fn identity(path: &Path) -> Option<FileIdentity> {
        let file = std::fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_ALL)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .ok()?;
        let mut info = ByHandleFileInformation::default();
        // SAFETY: `file` is an open handle for the duration of the call and
        // `info` is a properly sized, writable BY_HANDLE_FILE_INFORMATION.
        let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) };
        if ok == 0 {
            return None;
        }
        Some(FileIdentity {
            volume: u64::from(info.volume_serial_number),
            index: (u64::from(info.file_index_high) << 32) | u64::from(info.file_index_low),
        })
    }
}

#[cfg(unix)]
mod platform {
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    use super::FileIdentity;

    pub(super) fn identity(path: &Path) -> Option<FileIdentity> {
        let meta = std::fs::metadata(path).ok()?;
        Some(FileIdentity { volume: meta.dev(), index: meta.ino() })
    }
}

#[cfg(not(any(windows, unix)))]
mod platform {
    use std::path::Path;

    use super::FileIdentity;

    pub(super) fn identity(_path: &Path) -> Option<FileIdentity> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_folder_has_one_identity_under_every_name() {
        let tmp = std::env::temp_dir().join(format!("exposition-identity-{}", std::process::id()));
        let inner = tmp.join("a").join("b");
        std::fs::create_dir_all(&inner).unwrap();
        let canonical = std::fs::canonicalize(&inner).unwrap();
        assert_eq!(file_identity(&inner), file_identity(&canonical));
        assert!(file_identity(&inner).is_some());
        assert_ne!(file_identity(&inner), file_identity(&tmp));
        assert!(within_by_identity(&inner, &tmp));
        assert!(within_by_identity(&inner, &inner));
        assert!(!within_by_identity(&tmp, &inner));
        assert!(file_identity(&tmp.join("missing")).is_none());
        assert!(!within_by_identity(&inner, &tmp.join("missing")));
        #[cfg(windows)]
        {
            // The local administrative share names the same folder.
            let text = canonical.display().to_string();
            let plain = text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned();
            if plain.as_bytes().get(1) == Some(&b':') {
                let share = format!(r"\\localhost\{}${}", &plain[..1], &plain[2..]);
                if Path::new(&share).is_dir() {
                    assert_eq!(file_identity(Path::new(&share)), file_identity(&inner), "{share}");
                    assert!(within_by_identity(Path::new(&share), &tmp));
                }
            }
        }
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
