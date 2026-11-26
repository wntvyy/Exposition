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
//!
//! A [`FileVersion`] adds the file's change time to its identity: what tells
//! one version of a file from another without reading it (a cache of what a
//! file held checks it).

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

/// One version of a file, known without reading it: its identity and its
/// change time, which every write and every change of the file's times
/// moves (also a copy over the file, and setting its modification time
/// back; ordinary tools never set it back). Unix: device, inode, change time
/// (seconds, nanoseconds). Windows: volume, file index, change time and
/// creation time. Two writes within one tick of the file system's clock are
/// not told apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileVersion([u64; 4]);

impl FileVersion {
    /// The four words compared (for callers that keep versions as data).
    pub fn words(&self) -> [u64; 4] {
        self.0
    }
}

/// The version of an open file (read before its content is, so that a change
/// made after it is a change of the version).
pub fn file_version(file: &std::fs::File) -> Option<FileVersion> {
    platform::version(file)
}

/// The version of the file at `path` now. On Windows the file is opened for
/// its attributes only, which another program holding it does not prevent.
pub fn path_version(path: &Path) -> Option<FileVersion> {
    platform::path_version(path)
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

    use super::{FileIdentity, FileVersion};

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

    /// `FILE_BASIC_INFO` (winbase.h): times in 100 ns units since 1601.
    #[repr(C)]
    #[derive(Default)]
    struct FileBasicInfo {
        creation_time: i64,
        last_access_time: i64,
        last_write_time: i64,
        change_time: i64,
        file_attributes: u32,
    }

    /// `FileBasicInfo` of `FILE_INFO_BY_HANDLE_CLASS`.
    const FILE_BASIC_INFO_CLASS: i32 = 0;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandle(file: *mut c_void, information: *mut ByHandleFileInformation) -> i32;
        fn GetFileInformationByHandleEx(file: *mut c_void, class: i32, information: *mut c_void, size: u32) -> i32;
    }

    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    const FILE_SHARE_ALL: u32 = 0x1 | 0x2 | 0x4;
    /// Required to open a directory handle.
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

    /// `path` opened for its attributes only (shared with every other
    /// opener; a directory too).
    fn open_for_attributes(path: &Path) -> Option<std::fs::File> {
        std::fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_ALL)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .ok()
    }

    fn handle_information(file: &std::fs::File) -> Option<ByHandleFileInformation> {
        let mut info = ByHandleFileInformation::default();
        // SAFETY: `file` is an open handle for the duration of the call and
        // `info` is a properly sized, writable BY_HANDLE_FILE_INFORMATION.
        let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) };
        (ok != 0).then_some(info)
    }

    fn identity_of(info: &ByHandleFileInformation) -> FileIdentity {
        FileIdentity {
            volume: u64::from(info.volume_serial_number),
            index: (u64::from(info.file_index_high) << 32) | u64::from(info.file_index_low),
        }
    }

    pub(super) fn identity(path: &Path) -> Option<FileIdentity> {
        handle_information(&open_for_attributes(path)?).map(|info| identity_of(&info))
    }

    pub(super) fn version(file: &std::fs::File) -> Option<FileVersion> {
        let identity = identity_of(&handle_information(file)?);
        let mut basic = FileBasicInfo::default();
        // SAFETY: `file` is an open handle for the duration of the call;
        // `basic` is a writable FILE_BASIC_INFO of exactly the size passed,
        // and the call writes only into it.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FILE_BASIC_INFO_CLASS,
                (&mut basic as *mut FileBasicInfo).cast(),
                std::mem::size_of::<FileBasicInfo>() as u32,
            )
        };
        if ok == 0 {
            return None;
        }
        Some(FileVersion([identity.volume, identity.index, basic.change_time as u64, basic.creation_time as u64]))
    }

    pub(super) fn path_version(path: &Path) -> Option<FileVersion> {
        version(&open_for_attributes(path)?)
    }
}

#[cfg(unix)]
mod platform {
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    use super::{FileIdentity, FileVersion};

    pub(super) fn identity(path: &Path) -> Option<FileIdentity> {
        let meta = std::fs::metadata(path).ok()?;
        Some(FileIdentity { volume: meta.dev(), index: meta.ino() })
    }

    fn version_of(meta: &std::fs::Metadata) -> FileVersion {
        FileVersion([meta.dev(), meta.ino(), meta.ctime() as u64, meta.ctime_nsec() as u64])
    }

    pub(super) fn version(file: &std::fs::File) -> Option<FileVersion> {
        file.metadata().ok().map(|meta| version_of(&meta))
    }

    pub(super) fn path_version(path: &Path) -> Option<FileVersion> {
        std::fs::metadata(path).ok().map(|meta| version_of(&meta))
    }
}

#[cfg(not(any(windows, unix)))]
mod platform {
    use std::path::Path;

    use super::{FileIdentity, FileVersion};

    pub(super) fn identity(_path: &Path) -> Option<FileIdentity> {
        None
    }

    pub(super) fn version(_file: &std::fs::File) -> Option<FileVersion> {
        None
    }

    pub(super) fn path_version(_path: &Path) -> Option<FileVersion> {
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

    /// A file's version holds while nothing changes it, by name and by
    /// handle, and moves with a file put in its place, a rewrite in place,
    /// and a copy over it, also when the size and modification time are
    /// set back to what they were.
    #[test]
    fn a_file_version_moves_with_every_write_and_holds_otherwise() {
        let tmp = std::env::temp_dir().join(format!("exposition-version-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let file = tmp.join("record.json");
        std::fs::write(&file, "one").unwrap();
        let first = path_version(&file).unwrap();
        assert_eq!(path_version(&file), Some(first));
        assert_eq!(file_version(&std::fs::File::open(&file).unwrap()), Some(first), "by name and by handle");
        let modified = std::fs::metadata(&file).unwrap().modified().unwrap();
        let keep_time = || std::fs::OpenOptions::new().write(true).open(&file).unwrap().set_modified(modified).unwrap();
        // One tick of the file system's clock (NTFS: the system timer; Linux: a scheduler tick).
        let tick = || std::thread::sleep(std::time::Duration::from_millis(40));

        let fresh = tmp.join("record.fresh");
        std::fs::write(&fresh, "two").unwrap();
        std::fs::rename(&fresh, &file).unwrap();
        keep_time();
        let replaced = path_version(&file).unwrap();
        assert_ne!(replaced, first, "a file put in its place");

        tick();
        std::fs::write(&file, "TWO").unwrap();
        keep_time();
        let rewritten = path_version(&file).unwrap();
        assert_ne!(rewritten, replaced, "a rewrite in place, size and time kept");

        tick();
        let backup = tmp.join("backup.json");
        std::fs::write(&backup, "two").unwrap();
        std::fs::copy(&backup, &file).unwrap();
        keep_time();
        assert_eq!(std::fs::metadata(&file).unwrap().modified().unwrap(), modified);
        let copied = path_version(&file).unwrap();
        assert_ne!(copied, rewritten, "a copy over it, size and time kept");
        assert_eq!(path_version(&file), Some(copied), "and then it holds");
        assert_eq!(path_version(&tmp.join("missing")), None);
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
