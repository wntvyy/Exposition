//! Bounded scanning of the project folder under the inclusion policy.
//!
//! The scan never follows links or junctions: a link that leaves the project
//! must not be read, and one that stays inside only names a file that is
//! scanned under its own (canonical) path. Depth, directory entries, and
//! admitted files are bounded; exceeding a bound is reported, not guessed.
//! Reading a file is bounded too (hashing and extraction limits).

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use exposition_core::path::WorkspacePath;
use sha2::{Digest, Sha256};

use crate::admission::{Admission, classify, scan_directory};
use crate::vocabulary::ResourceRole;

/// Deepest folder level scanned.
pub const MAX_DEPTH: usize = 32;
/// Directory entries visited per scan.
pub const MAX_ENTRIES: usize = 200_000;
/// Files admitted per scan.
pub const MAX_FILES: usize = 20_000;
/// Files larger than this are admitted but not fingerprinted.
pub const MAX_HASH_BYTES: u64 = 256 * 1024 * 1024;

/// What the scan saw of one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileMeta {
    pub size: u64,
    pub modified_ms: Option<u64>,
    pub roles: Vec<ResourceRole>,
}

#[derive(Clone, Debug, Default)]
pub struct ScanOutcome {
    pub files: BTreeMap<WorkspacePath, FileMeta>,
    /// A bound was reached; files beyond it are unknown (not removed).
    pub truncated: Option<String>,
    /// Links/junctions not followed.
    pub links_skipped: u32,
}

fn modified_ms(meta: &std::fs::Metadata) -> Option<u64> {
    meta.modified().ok()?.duration_since(UNIX_EPOCH).ok().map(|d| d.as_millis() as u64)
}

/// Lexical absolute path of a workspace path under `root` (the canonical root).
pub fn absolute(root: &Path, path: &WorkspacePath) -> PathBuf {
    let mut p = root.to_path_buf();
    for c in path.components() {
        p.push(c);
    }
    p
}

/// Scan `root` (or the folder `start` inside it), admitting files under the
/// inclusion policy. On-request files (generated artifacts) are not scanned.
pub fn scan_tree(root: &Path, start: Option<&WorkspacePath>, out: &mut ScanOutcome) {
    let mut stack: Vec<(PathBuf, Option<WorkspacePath>, usize)> = Vec::new();
    match start {
        Some(dir) => {
            if !scan_directory(dir) {
                return;
            }
            // `start` itself must be a real folder (not a link) at every level.
            if !real_dir_chain(root, dir) {
                return;
            }
            stack.push((absolute(root, dir), Some(dir.clone()), dir.components().count()));
        }
        None => stack.push((root.to_path_buf(), None, 0)),
    }
    let mut entries = 0usize;
    while let Some((abs, rel, depth)) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&abs) else { continue };
        for entry in read.flatten() {
            entries += 1;
            if entries > MAX_ENTRIES {
                out.truncated = Some(format!("the scan stopped after {MAX_ENTRIES} folder entries"));
                return;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
            let path = match &rel {
                Some(dir) => dir.join(&name),
                None => WorkspacePath::parse(&name),
            };
            let Ok(path) = path else { continue };
            let Ok(file_type) = entry.file_type() else { continue };
            if file_type.is_symlink() {
                out.links_skipped += 1;
                continue;
            }
            if file_type.is_dir() {
                if depth + 1 > MAX_DEPTH {
                    out.truncated = Some(format!("folders deeper than {MAX_DEPTH} levels were not scanned"));
                    continue;
                }
                if scan_directory(&path) {
                    stack.push((entry.path(), Some(path), depth + 1));
                }
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let Admission::Admit(roles) = classify(&path) else { continue };
            let Ok(meta) = entry.metadata() else { continue };
            if out.files.len() >= MAX_FILES {
                out.truncated = Some(format!("only the first {MAX_FILES} project files were indexed"));
                return;
            }
            out.files.insert(path, FileMeta { size: meta.len(), modified_ms: modified_ms(&meta), roles });
        }
    }
}

/// Every component of `dir` under `root` is a real folder (no links).
fn real_dir_chain(root: &Path, dir: &WorkspacePath) -> bool {
    let mut p = root.to_path_buf();
    for c in dir.components() {
        p.push(c);
        match std::fs::symlink_metadata(&p) {
            Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {}
            _ => return false,
        }
    }
    true
}

/// Stat one file (no links anywhere on its path). `None` when it does not
/// exist, is not a regular file, or is reached through a link.
pub fn stat_file(root: &Path, path: &WorkspacePath) -> Option<FileMeta> {
    if let Some(parent) = path.parent()
        && !real_dir_chain(root, &parent)
    {
        return None;
    }
    let meta = std::fs::symlink_metadata(absolute(root, path)).ok()?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return None;
    }
    let roles = match classify(path) {
        Admission::Admit(roles) | Admission::OnRequest(roles) => roles,
        Admission::Exclude(_) => Vec::new(),
    };
    Some(FileMeta { size: meta.len(), modified_ms: modified_ms(&meta), roles })
}

/// Is `path` (a file or folder) present as a real folder?
pub fn is_real_dir(root: &Path, path: &WorkspacePath) -> bool {
    real_dir_chain(root, path)
}

/// The content of one file as indexed.
#[derive(Clone, Debug, Default)]
pub struct FileContent {
    /// SHA-256 (hex) of the exact bytes; `None` when too large or unreadable.
    pub hash: Option<String>,
    /// Opaque physical identity (volume + file index), for rename confirmation.
    pub identity: Option<String>,
    /// Decoded text, when the file has an extractor and is text within limits.
    pub text: Option<String>,
    /// Why the file could not be read/decoded.
    pub problem: Option<String>,
}

/// Opaque, stable text form of a file's physical identity.
pub fn identity_key(abs: &Path) -> Option<String> {
    let id = exposition_core::identity::file_identity(abs)?;
    let digest = Sha256::digest(format!("{id:?}").as_bytes());
    Some(digest.iter().take(10).map(|b| format!("{b:02x}")).collect())
}

/// Read, hash, and (when `want_text`) decode one file.
pub fn read_file(root: &Path, path: &WorkspacePath, size: u64, want_text: bool) -> FileContent {
    let abs = absolute(root, path);
    let mut out = FileContent { identity: identity_key(&abs), ..Default::default() };
    if size > MAX_HASH_BYTES {
        out.problem = Some(format!("{:.0} MB is too large to fingerprint", size as f64 / 1_048_576.0));
        return out;
    }
    let mut file = match std::fs::File::open(&abs) {
        Ok(f) => f,
        Err(e) => {
            out.problem = Some(format!("unreadable: {e}"));
            return out;
        }
    };
    let mut bytes = Vec::with_capacity(size.min(64 * 1024 * 1024) as usize);
    if let Err(e) = (&mut file).take(MAX_HASH_BYTES + 1).read_to_end(&mut bytes) {
        out.problem = Some(format!("unreadable: {e}"));
        return out;
    }
    if bytes.len() as u64 > MAX_HASH_BYTES {
        out.problem = Some("too large to fingerprint".into());
        return out;
    }
    out.hash = Some(sha256_hex(&bytes));
    if want_text {
        if bytes.len() > super::extract::MAX_EXTRACT_BYTES {
            out.problem = Some(format!(
                "{:.1} MB is larger than the {} MB indexing limit",
                bytes.len() as f64 / 1_048_576.0,
                super::extract::MAX_EXTRACT_BYTES / 1_048_576
            ));
        } else {
            match exposition_workspace::encoding::decode(&bytes) {
                Ok(decoded) => out.text = Some(decoded.text),
                Err(_) => out.problem = Some("not readable as text".into()),
            }
        }
    }
    out
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wp(s: &str) -> WorkspacePath {
        WorkspacePath::parse(s).unwrap()
    }

    #[cfg(windows)]
    fn junction(link: &Path, target: &Path) {
        let status = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[cfg(unix)]
    fn junction(link: &Path, target: &Path) {
        std::os::unix::fs::symlink(target, link).unwrap();
    }

    #[test]
    fn scan_admits_sources_and_skips_caches_runs_artifacts_and_links() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap().join("p");
        let outside = tmp.path().join("outside");
        for d in [
            "chapters",
            ".git",
            "node_modules/x",
            ".exposition/runs",
            ".exposition/artifacts/run-1",
            ".exposition/research/rs-1",
            "build/__pycache__",
        ] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::create_dir_all(&outside).unwrap();
        for (f, body) in [
            ("main.tex", "x"),
            ("chapters/a.tex", "y"),
            ("refs.bib", ""),
            ("main.log", "log"),
            (".git/HEAD", "ref"),
            ("node_modules/x/i.js", ""),
            (".exposition/runs/run-1.json", "{}"),
            (".exposition/artifacts/run-1/output-0.png", "png"),
            (".exposition/research/rs-1/fit.py", "print(1)"),
            (".exposition/project.json", "{}"),
            (".exposition/graph.sqlite3", ""),
            ("CLAUDE.md", "planning"),
            ("build/__pycache__/m.pyc", ""),
        ] {
            std::fs::write(root.join(f), body).unwrap();
        }
        std::fs::write(outside.join("secret.tex"), "secret").unwrap();
        junction(&root.join("escape"), &outside);
        junction(&root.join("alias"), &root.join("chapters"));
        let mut out = ScanOutcome::default();
        scan_tree(&root, None, &mut out);
        let paths: Vec<&str> = out.files.keys().map(|p| p.as_str()).collect();
        assert_eq!(
            paths,
            [".exposition/project.json", ".exposition/research/rs-1/fit.py", "chapters/a.tex", "main.tex", "refs.bib"]
        );
        assert_eq!(out.links_skipped, 2, "links are never followed");
        assert!(out.truncated.is_none());
        // A subtree scan, and stat of a file behind a link is refused.
        let mut sub = ScanOutcome::default();
        scan_tree(&root, Some(&wp("chapters")), &mut sub);
        assert_eq!(sub.files.len(), 1);
        assert!(stat_file(&root, &wp("escape/secret.tex")).is_none());
        assert!(stat_file(&root, &wp("alias/a.tex")).is_none());
        assert!(stat_file(&root, &wp("main.tex")).is_some());
        let c = read_file(&root, &wp("main.tex"), 1, true);
        assert_eq!(c.text.as_deref(), Some("x"));
        assert_eq!(c.hash.as_deref(), Some(sha256_hex(b"x").as_str()));
        assert!(c.identity.is_some());
    }

    #[test]
    fn identity_survives_a_rename_but_not_a_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.tex");
        std::fs::write(&a, "same").unwrap();
        let before = identity_key(&a).unwrap();
        let b = tmp.path().join("b.tex");
        std::fs::rename(&a, &b).unwrap();
        assert_eq!(identity_key(&b).unwrap(), before);
        let c = tmp.path().join("c.tex");
        std::fs::copy(&b, &c).unwrap();
        assert_ne!(identity_key(&c).unwrap(), before);
    }
}
