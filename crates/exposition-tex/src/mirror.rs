//! The engine's view of the project: an application-owned mirror of the
//! project tree with the open buffers written over their files.
//!
//! TeX opens explicitly relative names (`./x`, `../x`) relative to its working
//! directory, and kpathsea does not search `TEXINPUTS` for them. The engine
//! must therefore run in a directory whose surroundings are the project's, so
//! that `\includegraphics{../figs/x.png}` from `sub/report.tex` reaches
//! `figs/x.png`. The mirror holds the project's files at their project paths,
//! copied (nothing the engine does can modify the project), with each open
//! buffer's bytes in place of the file on disk.
//!
//! The mirror persists (also across application restarts) and [`sync`] brings
//! it up to date before each build: a copy carries its source's modification
//! time, so a mirror file with the size and modification time of its project
//! file is current and is not copied again. Everything else is fixed: changed
//! project files are copied, files gone from the project (or written by the
//! engine) are removed, a file the engine changed is restored, a buffer is
//! written when its bytes differ, and a closed buffer reverts to the disk file.
//!
//! Not mirrored, but still found through the `TEXINPUTS` fallback to the
//! project for names that are not explicitly relative: the directories the
//! workspace hides (`.git`, `node_modules`, ...), other dot-directories except
//! `.exposition` (tool caches and virtual environments), `__pycache__`, links
//! (which could lead out of the workspace), files larger than
//! [`MAX_FILE_BYTES`], and files beyond [`MAX_FILES`] (the first build copies
//! about 0.5 ms per file on Windows; later builds only compare metadata).

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use exposition_core::path::WorkspacePath;
use exposition_workspace::fsutil::TEMP_MARKER;
use exposition_workspace::tree::HIDDEN_NAMES;

/// Most project files mirrored; later files are reached only through `TEXINPUTS`.
pub const MAX_FILES: usize = 10_000;
/// Larger files are not copied.
pub const MAX_FILE_BYTES: u64 = 64 << 20;
const MAX_DEPTH: usize = 32;
/// Dot-directories that are mirrored (research artifacts live here).
const MIRRORED_DOT_DIRS: &[&str] = &[".exposition"];
const SKIPPED_DIRS: &[&str] = &["__pycache__"];

/// A buffer to place in the mirror.
pub struct MirrorBuffer<'a> {
    pub path: &'a WorkspacePath,
    pub bytes: &'a [u8],
}

/// What one synchronization did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MirrorStats {
    /// Project files present in the mirror (copied or current).
    pub files: usize,
    /// Files and buffers written by this synchronization.
    pub written: usize,
    /// Project files left out by the size or count limit.
    pub skipped: usize,
}

/// Make `mirror` hold the project's files with `buffers` over them.
pub fn sync(project: &Path, mirror: &Path, buffers: &[MirrorBuffer<'_>]) -> io::Result<MirrorStats> {
    fs::create_dir_all(mirror)?;
    let mut buffer_dirs = HashSet::new();
    for buffer in buffers {
        let mut dir = buffer.path.parent();
        while let Some(d) = dir {
            buffer_dirs.insert(key(d.as_str()));
            dir = d.parent();
        }
    }
    let mut walk = Walk {
        buffers: buffers.iter().map(|b| key(b.path.as_str())).collect(),
        buffer_dirs,
        stats: MirrorStats::default(),
    };
    walk.dir(project, mirror, "", 0);
    for buffer in buffers {
        let dest = native(mirror, buffer.path.as_str());
        if fs::read(&dest).is_ok_and(|bytes| bytes == buffer.bytes) {
            continue;
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        if fs::symlink_metadata(&dest).is_ok_and(|m| m.is_dir()) {
            fs::remove_dir_all(&dest)?;
        }
        fs::write(&dest, buffer.bytes)?;
        // The buffer must not look like a current copy of the disk file (same
        // size, written within the same clock tick), or closing it would not
        // restore the file.
        let disk = native(project, buffer.path.as_str());
        if let (Ok(written), Ok(on_disk)) = (fs::metadata(&dest), fs::metadata(&disk))
            && same(&written, &on_disk)
            && let Ok(time) = on_disk.modified()
        {
            fs::File::options().write(true).open(&dest)?.set_modified(time + std::time::Duration::from_secs(1))?;
        }
        walk.stats.written += 1;
    }
    Ok(walk.stats)
}

/// Comparison key of a relative path, by the file system's case rules.
fn key(rel: &str) -> String {
    if cfg!(windows) { rel.to_lowercase() } else { rel.to_owned() }
}

fn child(rel: &str, name: &str) -> String {
    if rel.is_empty() { name.to_owned() } else { format!("{rel}/{name}") }
}

fn native(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').filter(|c| !c.is_empty()).fold(root.to_path_buf(), |p, c| p.join(c))
}

fn same(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    a.len() == b.len() && a.modified().ok().is_some() && a.modified().ok() == b.modified().ok()
}

fn mirrored_dir(name: &str) -> bool {
    if HIDDEN_NAMES.contains(&name) || SKIPPED_DIRS.contains(&name) {
        return false;
    }
    !name.starts_with('.') || MIRRORED_DOT_DIRS.contains(&name)
}

/// Copy `from` to `to` (replacing it) with `from`'s modification time.
fn copy(from: &Path, to: &Path, modified: Option<SystemTime>) -> io::Result<()> {
    // Never write through an existing name: remove it first.
    if fs::symlink_metadata(to).is_ok() {
        fs::remove_file(to)?;
    }
    fs::copy(from, to)?;
    if let Some(time) = modified {
        fs::File::options().write(true).open(to)?.set_modified(time)?;
    }
    Ok(())
}

struct Walk {
    /// Keys of the buffer paths (written after the walk).
    buffers: HashSet<String>,
    /// Keys of the directories that contain a buffer.
    buffer_dirs: HashSet<String>,
    stats: MirrorStats,
}

impl Walk {
    fn dir(&mut self, project: &Path, mirror: &Path, rel: &str, depth: usize) {
        // The project's entries at this level that are mirrored.
        let mut files: Vec<(String, fs::Metadata)> = Vec::new();
        let mut dirs: Vec<String> = Vec::new();
        let mut entries: Vec<fs::DirEntry> = fs::read_dir(project).map(|r| r.flatten().collect()).unwrap_or_default();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
            if name.contains(TEMP_MARKER) || WorkspacePath::parse(&child(rel, &name)).is_err() {
                continue;
            }
            let Ok(file_type) = entry.file_type() else { continue };
            if file_type.is_symlink() {
                continue; // also junctions
            }
            if file_type.is_dir() {
                if depth < MAX_DEPTH && mirrored_dir(&name) {
                    dirs.push(name);
                }
            } else if file_type.is_file() && !self.buffers.contains(&key(&child(rel, &name))) {
                let Ok(meta) = entry.metadata() else { continue };
                if meta.len() > MAX_FILE_BYTES || self.stats.files >= MAX_FILES {
                    self.stats.skipped += 1;
                    continue;
                }
                self.stats.files += 1;
                files.push((name, meta));
            }
        }
        let wanted_dirs: HashSet<String> = dirs.iter().map(|n| key(n)).collect();
        let wanted_files: HashSet<String> = files.iter().map(|(n, _)| key(n)).collect();
        // Remove what the mirror holds beyond that (deleted project files,
        // engine leftovers, a closed buffer's new file, kind changes).
        if let Ok(read) = fs::read_dir(mirror) {
            for entry in read.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
                let k = key(&name);
                let keep = if is_dir { wanted_dirs.contains(&k) } else { wanted_files.contains(&k) }
                    || self.buffers.contains(&key(&child(rel, &name)))
                    || (is_dir && self.buffer_dirs.contains(&key(&child(rel, &name))));
                if !keep {
                    let path = entry.path();
                    let _ = if is_dir { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
                }
            }
        }
        if fs::create_dir_all(mirror).is_err() {
            return;
        }
        for (name, meta) in &files {
            let dest = mirror.join(name);
            if fs::metadata(&dest).is_ok_and(|m| m.is_file() && same(&m, meta)) {
                continue;
            }
            match copy(&project.join(name), &dest, meta.modified().ok()) {
                Ok(()) => self.stats.written += 1,
                Err(_) => {
                    // Unreadable (e.g. locked): leave it to the TEXINPUTS fallback.
                    let _ = fs::remove_file(&dest);
                    self.stats.files -= 1;
                    self.stats.skipped += 1;
                }
            }
        }
        for name in dirs {
            self.dir(&project.join(&name), &mirror.join(&name), &child(rel, &name), depth + 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let path = native(root, rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn read(root: &Path, rel: &str) -> Option<String> {
        fs::read_to_string(native(root, rel)).ok()
    }

    fn buffer<'a>(path: &'a WorkspacePath, text: &'a str) -> MirrorBuffer<'a> {
        MirrorBuffer { path, bytes: text.as_bytes() }
    }

    #[test]
    fn mirrors_the_project_with_buffers_over_it_incrementally() {
        let project = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let mirror = work.path().join("src");
        write(project.path(), "main.tex", "main");
        write(project.path(), "sub/report.tex", "report");
        write(project.path(), "figs/x.png", "png");
        write(project.path(), ".exposition/artifacts/run-1/output-1.png", "artifact");
        write(project.path(), ".git/config", "git");
        write(project.path(), ".venv/lib/site.py", "venv");
        write(project.path(), "node_modules/m/index.js", "js");
        write(project.path(), &format!("sub/.a{TEMP_MARKER}1.tmp"), "temp");

        let stats = sync(project.path(), &mirror, &[]).unwrap();
        assert_eq!(stats, MirrorStats { files: 4, written: 4, skipped: 0 });
        assert_eq!(read(&mirror, "sub/report.tex").as_deref(), Some("report"));
        assert_eq!(read(&mirror, ".exposition/artifacts/run-1/output-1.png").as_deref(), Some("artifact"));
        for skipped in [".git", ".venv", "node_modules"] {
            assert!(!mirror.join(skipped).exists(), "{skipped}");
        }
        assert!(fs::read_dir(mirror.join("sub")).unwrap().count() == 1, "no temp files");

        // Nothing changed: nothing is copied.
        assert_eq!(sync(project.path(), &mirror, &[]).unwrap().written, 0);

        // A buffer replaces its file; an unchanged buffer is not rewritten.
        let path = WorkspacePath::parse("sub/report.tex").unwrap();
        let stats = sync(project.path(), &mirror, &[buffer(&path, "edited")]).unwrap();
        assert_eq!((stats.files, stats.written), (3, 1));
        assert_eq!(read(&mirror, "sub/report.tex").as_deref(), Some("edited"));
        assert_eq!(sync(project.path(), &mirror, &[buffer(&path, "edited")]).unwrap().written, 0);
        // A buffer without a file on disk (in a new folder) is placed too.
        let new = WorkspacePath::parse("new/dir/a.tex").unwrap();
        sync(project.path(), &mirror, &[buffer(&path, "edited"), buffer(&new, "new")]).unwrap();
        assert_eq!(read(&mirror, "new/dir/a.tex").as_deref(), Some("new"));

        // Closing the buffers reverts to disk and drops the buffer-only file.
        let stats = sync(project.path(), &mirror, &[]).unwrap();
        assert_eq!(stats.written, 1);
        assert_eq!(read(&mirror, "sub/report.tex").as_deref(), Some("report"));
        assert!(!mirror.join("new").exists());

        // Disk changes, deletions, and files the engine changed or added.
        write(project.path(), "figs/x.png", "png, changed");
        fs::remove_file(project.path().join("main.tex")).unwrap();
        write(&mirror, "sub/report.tex", "changed by the engine");
        write(&mirror, "sub/leftover.txt", "x");
        let stats = sync(project.path(), &mirror, &[]).unwrap();
        assert_eq!(stats.written, 2);
        assert_eq!(read(&mirror, "figs/x.png").as_deref(), Some("png, changed"));
        assert_eq!(read(&mirror, "sub/report.tex").as_deref(), Some("report"));
        assert!(!mirror.join("main.tex").exists() && !mirror.join("sub/leftover.txt").exists());

        // Files over the size limit are left out (and counted).
        fs::File::create(project.path().join("figs/huge.bin")).unwrap().set_len(MAX_FILE_BYTES + 1).unwrap();
        let stats = sync(project.path(), &mirror, &[]).unwrap();
        assert_eq!((stats.written, stats.skipped), (0, 1));
        assert!(!mirror.join("figs/huge.bin").exists());

        // The project itself was never written.
        assert_eq!(read(project.path(), "sub/report.tex").as_deref(), Some("report"));
        assert_eq!(read(project.path(), "figs/x.png").as_deref(), Some("png, changed"));
        assert!(!project.path().join("new").exists());
    }

    #[test]
    fn links_are_not_followed() {
        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        write(outside.path(), "secret.tex", "outside");
        write(project.path(), "main.tex", "main");
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_dir(outside.path(), project.path().join("linked"));
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(outside.path(), project.path().join("linked"));
        if let Err(e) = linked {
            // Windows without Developer Mode refuses symbolic links.
            eprintln!("SKIPPED: cannot create a symbolic link: {e}");
            return;
        }
        let work = tempfile::tempdir().unwrap();
        let mirror = work.path().join("src");
        sync(project.path(), &mirror, &[]).unwrap();
        assert!(mirror.join("main.tex").exists());
        assert!(!mirror.join("linked").exists());
    }
}
