//! Independent review of `identity::file_version` / `path_version` (the
//! research history's record stamp): exercised on real files of the host's
//! file system. Tests marked `#[ignore = "review finding: ..."]` fail against
//! the change and document a finding; every other test passes.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use exposition_core::identity::{FileVersion, file_version, path_version};

struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn dir(name: &str) -> Dir {
    let path = std::env::temp_dir().join(format!("exposition-review-version-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    Dir(path)
}

/// One tick of the file system's clock (NTFS: the system timer).
fn tick() {
    std::thread::sleep(Duration::from_millis(40));
}

fn version(path: &Path) -> FileVersion {
    path_version(path).expect("a version")
}

/// Many reads with no write in between, by name and by handle (also handles
/// opened for reading only, and while another handle has it open for
/// writing): always the same version.
#[test]
fn review_a_version_is_stable_while_nothing_writes() {
    let d = dir("stable");
    let file = d.0.join("record.json");
    std::fs::write(&file, "{}").unwrap();
    let first = version(&file);
    for _ in 0..500 {
        assert_eq!(version(&file), first);
    }
    let reader = std::fs::File::open(&file).unwrap();
    let _writer = std::fs::OpenOptions::new().append(true).open(&file).unwrap();
    for _ in 0..100 {
        assert_eq!(file_version(&reader), Some(first), "by handle");
        assert_eq!(version(&file), first, "by name while another handle is open");
    }
    // Reading the content does not move it.
    let _ = std::fs::read(&file).unwrap();
    assert_eq!(version(&file), first, "a read is not a write");
}

/// A file another program holds open without sharing anything (Windows):
/// its version is still read (the file is opened for its attributes only),
/// and it is the version it had.
#[cfg(windows)]
#[test]
fn review_a_file_held_without_sharing_still_has_its_version() {
    use std::os::windows::fs::OpenOptionsExt;
    let d = dir("held");
    let file = d.0.join("record.json");
    std::fs::write(&file, "{}").unwrap();
    let before = version(&file);
    let _held = std::fs::OpenOptions::new().read(true).write(true).share_mode(0).open(&file).unwrap();
    assert!(std::fs::read(&file).is_err(), "the content cannot be opened");
    assert_eq!(path_version(&file), Some(before), "the version can");
}

/// Writes through a handle that stays open (a tool repairing the record in
/// place): the version moves with the write, before the handle is closed,
/// also when the modification time is set back through that handle.
#[test]
fn review_a_write_through_an_open_handle_moves_the_version_before_close() {
    let d = dir("open-write");
    let file = d.0.join("record.json");
    std::fs::write(&file, "{ damaged }").unwrap();
    let modified = std::fs::metadata(&file).unwrap().modified().unwrap();
    let before = version(&file);
    tick();
    let mut handle = std::fs::OpenOptions::new().write(true).open(&file).unwrap();
    handle.write_all(b"{ repaired}").unwrap();
    handle.flush().unwrap();
    handle.set_modified(modified).unwrap();
    assert_eq!(std::fs::metadata(&file).unwrap().len(), 11);
    let during = version(&file);
    assert_ne!(during, before, "moved while the writer still holds the file");
    drop(handle);
    let after = version(&file);
    assert_eq!(std::fs::metadata(&file).unwrap().modified().unwrap(), modified, "time kept");
    assert_ne!(after, before, "and after it closed");
}

/// A copy over the file (CopyFile: what `copy`, Copy-Item, robocopy and
/// Explorer do) and a rename of another file over it, each keeping size and
/// modification time: the version moves each time, and holds afterwards.
#[test]
fn review_copy_over_and_rename_over_move_the_version() {
    let d = dir("copy");
    let file = d.0.join("record.json");
    std::fs::write(&file, "aaaa").unwrap();
    let modified = std::fs::metadata(&file).unwrap().modified().unwrap();
    let keep_time = || std::fs::OpenOptions::new().write(true).open(&file).unwrap().set_modified(modified).unwrap();
    let mut seen = vec![version(&file)];
    for k in 0..5 {
        tick();
        let source = d.0.join(format!("backup-{k}.json"));
        std::fs::write(&source, format!("bbb{k}")).unwrap();
        if k % 2 == 0 {
            std::fs::copy(&source, &file).unwrap();
        } else {
            std::fs::rename(&source, &file).unwrap();
        }
        keep_time();
        let now = version(&file);
        assert!(!seen.contains(&now), "step {k}: a version seen before: {now:?} in {seen:?}");
        assert_eq!(version(&file), now, "step {k}: and then it holds");
        seen.push(now);
    }
}

/// A folder has a version too (opened with backup semantics), a missing
/// path has none, and two names of one file (a hard link) have one version.
#[test]
fn review_folders_links_and_missing_paths() {
    let d = dir("kinds");
    assert!(path_version(&d.0).is_some(), "a folder");
    assert_eq!(path_version(&d.0.join("missing.json")), None);
    assert_eq!(path_version(&d.0.join("missing").join("deeper.json")), None);
    let file = d.0.join("record.json");
    std::fs::write(&file, "{}").unwrap();
    let link = d.0.join("link.json");
    std::fs::hard_link(&file, &link).unwrap();
    assert_eq!(path_version(&link), path_version(&file), "one file under two names");
}

/// This process's open handles, as the system counts them.
#[cfg(windows)]
fn handle_count() -> i64 {
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command"])
        .arg(format!("(Get-Process -Id {}).HandleCount", std::process::id()))
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

/// Handles opened for a version are closed again: thousands of queries
/// (found and missing paths, by name and by handle) leave the process's
/// handle count where it was.
#[cfg(windows)]
#[test]
fn review_versions_leave_no_handle_open() {
    let d = dir("handles");
    let file = d.0.join("record.json");
    std::fs::write(&file, "{}").unwrap();
    let _ = path_version(&file);
    let before = handle_count();
    for _ in 0..5_000 {
        let _ = path_version(&file);
        let _ = path_version(&d.0.join("missing.json"));
        let _ = file_version(&std::fs::File::open(&file).unwrap());
    }
    let after = handle_count();
    assert!(after - before < 100, "{before} handles before, {after} after 15,000 queries");
}
