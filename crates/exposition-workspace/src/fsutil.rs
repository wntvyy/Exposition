//! Durable file writes and content hashing.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

/// Marker in temporary file names, so the watcher can ignore them.
pub const TEMP_MARKER: &str = ".exposition-save-";

/// SHA-256 of `bytes` as lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// SHA-256 of a file's current bytes, streamed.
pub fn hash_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let mut out = String::with_capacity(64);
    for byte in hasher.finalize() {
        out.push_str(&format!("{byte:02x}"));
    }
    Ok(out)
}

/// Read a whole file, refusing files larger than `limit` bytes without reading them.
pub fn read_limited(path: &Path, limit: u64) -> io::Result<Result<Vec<u8>, u64>> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    if len > limit {
        return Ok(Err(len));
    }
    let mut bytes = Vec::with_capacity(len as usize);
    // Guard against growth between metadata and read.
    Read::take(&mut file, limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Ok(Err(bytes.len() as u64));
    }
    Ok(Ok(bytes))
}

/// Identity of a physical file (volume + file index on Windows, device + inode
/// elsewhere). Two paths with the same identity are the same file, including
/// hard links.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileIdentity {
    volume: u64,
    index: u64,
}

impl FileIdentity {
    pub fn of(file: &File) -> io::Result<Self> {
        #[cfg(windows)]
        {
            let info = winapi_util::file::information(file)?;
            Ok(Self { volume: info.volume_serial_number(), index: info.file_index() })
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let meta = file.metadata()?;
            Ok(Self { volume: meta.dev(), index: meta.ino() })
        }
    }

    pub fn of_path(path: &Path) -> io::Result<Self> {
        Self::of(&File::open(path)?)
    }
}

/// A file read for opening as a document.
pub struct FileRead {
    /// The bytes, or the file size when it exceeds the limit.
    pub bytes: Result<Vec<u8>, u64>,
    pub identity: Option<FileIdentity>,
}

/// Read a whole file (refusing files larger than `limit`) together with its identity.
pub fn read_with_identity(path: &Path, limit: u64) -> io::Result<FileRead> {
    let mut file = File::open(path)?;
    let identity = FileIdentity::of(&file).ok();
    let len = file.metadata()?.len();
    if len > limit {
        return Ok(FileRead { bytes: Err(len), identity });
    }
    let mut bytes = Vec::with_capacity(len as usize);
    Read::take(&mut file, limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Ok(FileRead { bytes: Err(bytes.len() as u64), identity });
    }
    Ok(FileRead { bytes: Ok(bytes), identity })
}

pub fn now_millis() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_path_for(target: &Path) -> io::Result<PathBuf> {
    let dir = target.parent().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "target has no parent"))?;
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let unique = format!("{:x}{:x}{:x}", std::process::id(), now_millis(), COUNTER.fetch_add(1, Ordering::Relaxed));
    Ok(dir.join(format!(".{name}{TEMP_MARKER}{unique}.tmp")))
}

/// Atomically replace (or create) `target` with `bytes`.
///
/// The bytes are written to a new temporary file in the same directory,
/// flushed and synced to disk, then renamed over the target (an atomic replace
/// on NTFS and POSIX file systems). On any failure the temporary file is
/// removed and the original target is left untouched.
pub fn write_atomic(target: &Path, bytes: &[u8]) -> io::Result<()> {
    let temp = temp_path_for(target)?;
    let result = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&temp)?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// True if the file exists and carries the OS read-only attribute/permission.
pub fn is_read_only(path: &Path) -> bool {
    fs::metadata(path).map(|m| m.permissions().readonly()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_known_vectors() {
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn writes_new_files_and_replaces_existing_ones() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a.txt");
        write_atomic(&target, b"one").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"one");
        write_atomic(&target, b"two\r\n").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"two\r\n");
        let leftovers: Vec<_> = fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(leftovers.len(), 1, "no temporary files remain: {leftovers:?}");
    }

    #[test]
    fn failed_replace_keeps_the_original_and_removes_the_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("locked.txt");
        fs::write(&target, b"original").unwrap();
        let mut perms = fs::metadata(&target).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&target, perms.clone()).unwrap();

        let err = write_atomic(&target, b"replacement");
        // Restore permissions before asserting so the temp dir can be cleaned.
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        fs::set_permissions(&target, perms).unwrap();

        if cfg!(windows) {
            assert!(err.is_err(), "replacing a read-only file must fail on Windows");
            assert_eq!(fs::read(&target).unwrap(), b"original");
        }
        let names: Vec<String> =
            fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert!(names.iter().all(|n| !n.contains(TEMP_MARKER)), "temp file cleaned: {names:?}");
    }

    #[test]
    fn failed_write_into_missing_directory_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("missing").join("a.txt");
        assert!(write_atomic(&target, b"x").is_err());
        assert!(!dir.path().join("missing").exists());
    }

    #[test]
    fn read_limited_refuses_large_files() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("big.bin");
        fs::write(&target, vec![b'a'; 100]).unwrap();
        assert_eq!(read_limited(&target, 100).unwrap().unwrap().len(), 100);
        assert_eq!(read_limited(&target, 99).unwrap(), Err(100));
    }
}
