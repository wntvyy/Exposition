//! Staging a batch's service-owned working copy: only the declared inputs and
//! the code, copied at their project-relative paths and verified against the
//! declared SHA-256 while copying. Nothing else from the project is staged.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use exposition_core::artifact::RunInput;
use exposition_core::path::WorkspacePath;
use exposition_graph::Admission;
use sha2::{Digest, Sha256};

use super::fsx::{is_name_link, open_no_follow, same_file_as};
use crate::batch::{BatchCode, BatchRequest};
use crate::error::ResearchError;
use crate::helper::sha256_hex;
use crate::types::{CodeOrigin, CodeProvenance};

/// File the inline code is written to (at the working-copy root).
pub const INLINE_ENTRY: &str = "__exposition_main__.py";
/// Declared inputs per batch.
pub const MAX_INPUTS: usize = 32;
/// Largest declared input.
pub const INPUT_LIMIT: u64 = 1024 * 1024 * 1024;
/// Largest total of all staged files.
pub const STAGED_TOTAL_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
/// Largest inline code.
pub const CODE_LIMIT: usize = 4 * 1024 * 1024;
/// Code kept verbatim in the manifest (larger code is recorded by hash).
const MANIFEST_CODE_LIMIT: usize = crate::runs::MANIFEST_CODE_LIMIT;

/// A staged working copy.
#[derive(Debug)]
pub(crate) struct Staged {
    /// Canonical working-copy root.
    pub work: PathBuf,
    /// Entry point relative to the working copy (`/`-separated).
    pub entry: String,
    /// Inputs as staged (the script too), pinned by content hash.
    pub inputs: Vec<RunInput>,
    pub code: CodeProvenance,
}

/// `research.inputRefused`: a declared path that may not be staged.
fn refused(path: &WorkspacePath, why: &str) -> ResearchError {
    ResearchError::coded("research.inputRefused", format!("{path} cannot be staged for the batch: {why}"))
}

/// `sha256:<hex>` or `<hex>` → lowercase hex, or `None` when malformed.
pub(crate) fn normalize_sha256(hash: &str) -> Option<String> {
    let bare = hash.strip_prefix("sha256:").or_else(|| hash.strip_prefix("SHA256:")).unwrap_or(hash);
    (bare.len() == 64 && bare.bytes().all(|b| b.is_ascii_hexdigit())).then(|| bare.to_ascii_lowercase())
}

/// Check a declared project path: inside the project by construction
/// (`WorkspacePath` is relative, without `..`), never project metadata the
/// graph excludes (graph storage, run records, version control, tooling,
/// agent instructions), and no link or junction anywhere on the way.
/// Returns the host path of the regular file.
pub(crate) fn check_project_file(root: &Path, path: &WorkspacePath) -> Result<PathBuf, ResearchError> {
    excluded(path, path)?;
    let mut current = root.to_path_buf();
    let parts: Vec<&str> = path.components().collect();
    for (i, part) in parts.iter().enumerate() {
        current.push(part);
        let meta = std::fs::symlink_metadata(&current).map_err(|e| {
            ResearchError::coded("research.inputMissing", format!("{path} could not be opened for the batch: {e}"))
        })?;
        // Links (symbolic links, junctions, WSL links: name-surrogate
        // reparse points) are refused; other reparse points (cloud-sync
        // placeholders, deduplicated files) hold the file's own data.
        if is_name_link(&meta) {
            return Err(refused(path, "it is or passes through a link or junction (links are never staged)"));
        }
        let last = i + 1 == parts.len();
        if !last && !meta.is_dir() {
            return Err(refused(path, "a parent is not a folder"));
        }
        if last && !meta.is_file() {
            return Err(refused(path, "it is not a regular file"));
        }
    }
    // The name the file system resolves (Windows short names such as
    // `GRAPH~1.SQL` or other aliases) must pass the same policy.
    let canonical = std::fs::canonicalize(&current).map_err(|e| ResearchError::io("resolving a declared input", e))?;
    let relative = canonical
        .strip_prefix(root)
        .ok()
        .map(|r| r.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/"))
        .and_then(|r| WorkspacePath::parse(&r).ok())
        .ok_or_else(|| refused(path, "it resolves outside the project"))?;
    excluded(path, &relative)?;
    // Only the file's own name (letter case aside) is accepted: no alias.
    if !relative.as_str().eq_ignore_ascii_case(path.as_str()) {
        return Err(refused(path, &format!("it is an alias of {relative}; declare the file under its own name")));
    }
    Ok(current)
}

/// Refuse `resolved` (the declared `path`, or what it resolves to) when the
/// graph's inclusion policy says it is never project content. Unrecognised
/// or generated file types are still data a batch may read (any other
/// exclusion fails closed, even if its wording changes).
fn excluded(path: &WorkspacePath, resolved: &WorkspacePath) -> Result<(), ResearchError> {
    if exposition_graph::is_graph_storage(resolved) {
        return Err(refused(path, "it is the project graph's own storage"));
    }
    if let Admission::Exclude(reason) = exposition_graph::classify(resolved)
        && reason != "not a recognised project resource type"
        && reason != "not a recognised research source type"
        && reason != "a generated build or cache file"
    {
        return Err(refused(path, reason));
    }
    Ok(())
}

/// Copy `source` to `dest` (created new, never replacing anything), hashing
/// while copying; the result must match `expected` (`research.inputChanged`).
fn copy_verified(
    path: &WorkspacePath,
    source: &Path,
    dest: &Path,
    expected: &str,
    budget: &mut u64,
) -> Result<(), ResearchError> {
    let lstat = std::fs::symlink_metadata(source).map_err(|e| ResearchError::io("reading a declared input", e))?;
    let mut file = open_no_follow(source).map_err(|e| ResearchError::io("reading a declared input", e))?;
    let meta = file.metadata().map_err(|e| ResearchError::io("reading a declared input", e))?;
    if is_name_link(&meta) || !meta.is_file() || !same_file_as(&file, &lstat).unwrap_or(false) {
        return Err(refused(path, "it changed into a link or special file while it was staged"));
    }
    if meta.len() > INPUT_LIMIT {
        return Err(refused(path, &format!("it is larger than {} MiB", INPUT_LIMIT / (1024 * 1024))));
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ResearchError::io("creating the working copy", e))?;
    }
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest)
        .map_err(|e| ResearchError::io(&format!("staging {path}"), e))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut copied: u64 = 0;
    loop {
        let n = file.read(&mut buffer).map_err(|e| ResearchError::io("reading a declared input", e))?;
        if n == 0 {
            break;
        }
        copied += n as u64;
        if copied > INPUT_LIMIT || copied > *budget {
            drop(out);
            let _ = std::fs::remove_file(dest);
            return Err(refused(path, "the staged inputs exceed the size limit"));
        }
        hasher.update(&buffer[..n]);
        out.write_all(&buffer[..n]).map_err(|e| ResearchError::io(&format!("staging {path}"), e))?;
    }
    out.flush().map_err(|e| ResearchError::io(&format!("staging {path}"), e))?;
    drop(out);
    *budget -= copied;
    let found: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if found != expected {
        let _ = std::fs::remove_file(dest);
        return Err(ResearchError::coded(
            "research.inputChanged",
            format!(
                "{path} changed since it was declared (expected sha256 {}, found {}); the batch was not run",
                &expected[..12],
                &found[..12]
            ),
        ));
    }
    Ok(())
}

/// Host path of a project-relative path inside `base`.
fn under(base: &Path, path: &WorkspacePath) -> PathBuf {
    path.components().fold(base.to_path_buf(), |p, c| p.join(c))
}

/// Stage `request` from the project `root` into the new folder `work`.
/// `canceled` is checked between files (a cancel during staging returns
/// `research.batchCanceled`).
pub(crate) fn stage(
    root: &Path,
    work: &Path,
    canceled: &dyn Fn() -> bool,
    request: &BatchRequest,
) -> Result<Staged, ResearchError> {
    if request.inputs.len() > MAX_INPUTS {
        return Err(ResearchError::InvalidRequest(format!("a batch declares at most {MAX_INPUTS} inputs")));
    }
    // Declared files (inputs and a script), deduplicated without regard to
    // case (one file on Windows), each with one expected digest.
    let mut declared: Vec<(WorkspacePath, String)> = Vec::new();
    let mut add = |path: &WorkspacePath, hash: &str| -> Result<(), ResearchError> {
        let expected = normalize_sha256(hash)
            .ok_or_else(|| ResearchError::InvalidRequest(format!("{path}: the declared SHA-256 is not valid")))?;
        let key = path.as_str().to_ascii_lowercase();
        if let Some((_, existing)) = declared.iter().find(|(p, _)| p.as_str().to_ascii_lowercase() == key) {
            if *existing != expected {
                return Err(ResearchError::InvalidRequest(format!("{path} is declared twice with different digests")));
            }
            return Ok(());
        }
        if key == INLINE_ENTRY.to_ascii_lowercase() {
            return Err(refused(path, "the name is reserved for the batch's inline code"));
        }
        declared.push((path.clone(), expected));
        Ok(())
    };
    for input in &request.inputs {
        add(&input.path, &input.sha256)?;
    }
    let (entry, code) = match &request.code {
        BatchCode::Inline { source } => {
            if source.len() > CODE_LIMIT {
                return Err(ResearchError::InvalidRequest(format!(
                    "the batch code is larger than {} MiB",
                    CODE_LIMIT / (1024 * 1024)
                )));
            }
            let code = CodeProvenance {
                origin: CodeOrigin::Input,
                sha256: sha256_hex(source.as_bytes()),
                bytes: source.len() as u64,
                text: (source.len() <= MANIFEST_CODE_LIMIT).then(|| source.clone()),
            };
            (INLINE_ENTRY.to_owned(), code)
        }
        BatchCode::Script { path, sha256 } => {
            if !path.file_name().to_ascii_lowercase().ends_with(".py") {
                return Err(ResearchError::InvalidRequest(format!("{path} is not a Python script (.py)")));
            }
            add(path, sha256)?;
            let digest = normalize_sha256(sha256).unwrap_or_default();
            let code = CodeProvenance {
                origin: CodeOrigin::File { path: path.clone(), revision: None },
                sha256: digest,
                bytes: 0,
                text: None,
            };
            (path.as_str().to_owned(), code)
        }
    };
    // Validate every declared path before copying anything.
    let mut sources = Vec::with_capacity(declared.len());
    for (path, expected) in &declared {
        sources.push((path, expected, check_project_file(root, path)?));
    }
    std::fs::create_dir_all(work).map_err(|e| ResearchError::io("creating the working copy", e))?;
    let work = std::fs::canonicalize(work).map_err(|e| ResearchError::io("resolving the working copy", e))?;
    let mut budget = STAGED_TOTAL_LIMIT;
    let mut inputs = Vec::with_capacity(sources.len());
    for (path, expected, source) in sources {
        if canceled() {
            return Err(ResearchError::coded("research.batchCanceled", "The batch was canceled before it started."));
        }
        copy_verified(path, &source, &under(&work, path), expected, &mut budget)?;
        inputs.push(RunInput { path: path.clone(), revision: None, content_hash: Some(format!("sha256:{expected}")) });
    }
    let mut code = code;
    match &request.code {
        BatchCode::Inline { source } => {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(work.join(INLINE_ENTRY))
                .map_err(|e| ResearchError::io("staging the batch code", e))?;
            file.write_all(source.as_bytes()).map_err(|e| ResearchError::io("staging the batch code", e))?;
        }
        BatchCode::Script { path, .. } => {
            let staged = under(&work, path);
            code.bytes = std::fs::metadata(&staged).map(|m| m.len()).unwrap_or(0);
            if code.bytes as usize <= MANIFEST_CODE_LIMIT {
                code.text = std::fs::read_to_string(&staged).ok();
            }
        }
    }
    Ok(Staged { work, entry, inputs, code })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::batch::{BatchInput, BatchMode, BatchOwner};
    use std::time::Duration;

    fn no_cancel() -> bool {
        false
    }
    const NO_CANCEL: &dyn Fn() -> bool = &no_cancel;

    fn request(code: BatchCode, inputs: Vec<BatchInput>) -> BatchRequest {
        BatchRequest {
            mode: BatchMode::Restricted,
            owner: BatchOwner { run: "wf-1".into(), step: "s1".into() },
            code,
            inputs,
            outputs: Vec::new(),
            time_limit: Duration::from_secs(10),
            max_output_bytes: 1 << 20,
        }
    }

    fn input(root: &Path, rel: &str, bytes: &[u8]) -> BatchInput {
        let path = WorkspacePath::parse(rel).unwrap();
        let host = under(root, &path);
        std::fs::create_dir_all(host.parent().unwrap()).unwrap();
        std::fs::write(&host, bytes).unwrap();
        BatchInput { path, sha256: format!("sha256:{}", sha256_hex(bytes)) }
    }

    fn dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        let root = base.join("project");
        std::fs::create_dir_all(&root).unwrap();
        (tmp, root, base.join("work"))
    }

    fn files_under(dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    out.push(p.strip_prefix(dir).unwrap().to_string_lossy().replace('\\', "/"));
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn only_declared_inputs_and_the_code_are_staged() {
        let (_tmp, root, work) = dirs();
        let data = input(&root, "data/table.csv", b"a,b\n1,2\n");
        std::fs::write(root.join("secret.txt"), "not declared").unwrap();
        std::fs::write(root.join("helpers.py"), "X = 1").unwrap();
        let staged =
            stage(&root, &work, NO_CANCEL, &request(BatchCode::Inline { source: "print(1)".into() }, vec![data]))
                .unwrap();
        assert_eq!(files_under(&staged.work), ["__exposition_main__.py", "data/table.csv"]);
        assert_eq!(staged.entry, INLINE_ENTRY);
        assert_eq!(std::fs::read(staged.work.join("data/table.csv")).unwrap(), b"a,b\n1,2\n");
        assert_eq!(staged.inputs.len(), 1);
        assert_eq!(staged.code.text.as_deref(), Some("print(1)"));
    }

    #[test]
    fn a_changed_input_refuses_the_batch() {
        let (_tmp, root, work) = dirs();
        let mut data = input(&root, "data.csv", b"1\n");
        data.sha256 = sha256_hex(b"2\n");
        let err = stage(&root, &work, NO_CANCEL, &request(BatchCode::Inline { source: String::new() }, vec![data]))
            .unwrap_err();
        assert_eq!(err.code(), "research.inputChanged", "{err}");
        assert!(!work.join("data.csv").exists(), "the mismatching copy is removed");
    }

    #[test]
    fn a_script_is_verified_and_staged_at_its_path() {
        let (_tmp, root, work) = dirs();
        let script = input(&root, "src/analysis.py", b"print('hi')\n");
        let code = BatchCode::Script { path: script.path.clone(), sha256: script.sha256.clone() };
        let staged = stage(&root, &work, NO_CANCEL, &request(code, Vec::new())).unwrap();
        assert_eq!(staged.entry, "src/analysis.py");
        assert_eq!(files_under(&staged.work), ["src/analysis.py"]);
        assert!(matches!(staged.code.origin, CodeOrigin::File { .. }));
        let bad = BatchCode::Script { path: script.path.clone(), sha256: sha256_hex(b"other") };
        let err = stage(&root, &tempfile::tempdir().unwrap().path().join("w"), NO_CANCEL, &request(bad, Vec::new()))
            .unwrap_err();
        assert_eq!(err.code(), "research.inputChanged");
    }

    #[test]
    fn metadata_graph_storage_and_tooling_paths_are_refused() {
        let (_tmp, root, work) = dirs();
        for rel in [".exposition/graph.sqlite3", ".exposition/runs/run-a.json", ".git/config", "CLAUDE.md"] {
            let declared = input(&root, rel, b"x");
            let err =
                stage(&root, &work, NO_CANCEL, &request(BatchCode::Inline { source: String::new() }, vec![declared]))
                    .unwrap_err();
            assert_eq!(err.code(), "research.inputRefused", "{rel}: {err}");
        }
        // A Windows short name (here of the `.exposition` folder) resolves to
        // the excluded long name.
        #[cfg(windows)]
        {
            let declared = input(&root, ".exposition/graph.sqlite3", b"graph");
            let out = std::process::Command::new("cmd").args(["/C", "dir", "/X", "/A"]).arg(&root).output().unwrap();
            let listing = String::from_utf8_lossy(&out.stdout).to_string();
            let short = listing
                .lines()
                .find(|l| l.trim_end().ends_with(" .exposition"))
                .and_then(|l| l.split_whitespace().rev().nth(1).filter(|s| s.contains('~')).map(str::to_owned));
            match short {
                Some(short) => {
                    let alias = BatchInput {
                        path: WorkspacePath::parse(&format!("{short}/graph.sqlite3")).unwrap(),
                        sha256: declared.sha256.clone(),
                    };
                    let err = stage(
                        &root,
                        &work,
                        NO_CANCEL,
                        &request(BatchCode::Inline { source: String::new() }, vec![alias]),
                    )
                    .unwrap_err();
                    assert_eq!(err.code(), "research.inputRefused", "{short}: {err}");
                    assert!(err.to_string().contains("graph"), "{err}");
                }
                None => eprintln!("SKIPPED short-name case: 8.3 names are disabled on this volume"),
            }
            // Any alias is refused, not only aliases of excluded paths.
            let long = input(
                &root,
                "data/long_observation_table.csv",
                b"1
",
            );
            let out =
                std::process::Command::new("cmd").args(["/C", "dir", "/X"]).arg(root.join("data")).output().unwrap();
            let listing = String::from_utf8_lossy(&out.stdout).to_string();
            if let Some(short) = listing
                .lines()
                .find(|l| l.contains("long_observation_table.csv"))
                .and_then(|l| l.split_whitespace().rev().nth(1).filter(|s| s.contains('~')).map(str::to_owned))
            {
                let alias =
                    BatchInput { path: WorkspacePath::parse(&format!("data/{short}")).unwrap(), sha256: long.sha256 };
                let err =
                    stage(&root, &work, NO_CANCEL, &request(BatchCode::Inline { source: String::new() }, vec![alias]))
                        .unwrap_err();
                assert!(err.to_string().contains("alias"), "{short}: {err}");
            }
            // Letter case alone is the same file.
            let upper = BatchInput {
                path: WorkspacePath::parse("DATA/LONG_OBSERVATION_TABLE.CSV").unwrap(),
                sha256: format!(
                    "sha256:{}",
                    sha256_hex(
                        b"1
"
                    )
                ),
            };
            let other = tempfile::tempdir().unwrap();
            stage(
                &root,
                &other.path().join("w"),
                NO_CANCEL,
                &request(BatchCode::Inline { source: String::new() }, vec![upper]),
            )
            .unwrap();
        }
        // Earlier batch outputs (artifacts) may be inputs of later steps.
        let artifact = input(&root, ".exposition/artifacts/run-abc/table.csv", b"1\n");
        stage(&root, &work, NO_CANCEL, &request(BatchCode::Inline { source: String::new() }, vec![artifact])).unwrap();
    }

    #[test]
    fn the_inline_entry_name_is_reserved_and_hashes_are_checked() {
        let (_tmp, root, work) = dirs();
        let clash = input(&root, "__exposition_main__.py", b"x");
        let err = stage(&root, &work, NO_CANCEL, &request(BatchCode::Inline { source: String::new() }, vec![clash]))
            .unwrap_err();
        assert_eq!(err.code(), "research.inputRefused");
        let mut bad = input(&root, "a.csv", b"x");
        bad.sha256 = "md5:abc".into();
        let err = stage(&root, &work, NO_CANCEL, &request(BatchCode::Inline { source: String::new() }, vec![bad]))
            .unwrap_err();
        assert_eq!(err.code(), "research.invalidRequest");
        assert_eq!(normalize_sha256(&"A".repeat(64)), Some("a".repeat(64)));
    }

    #[cfg(windows)]
    fn junction(link: &Path, target: &Path) -> bool {
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .is_ok_and(|o| o.status.success())
    }

    #[cfg(windows)]
    #[test]
    fn inputs_through_a_junction_are_refused() {
        let (tmp, root, work) = dirs();
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.csv"), "s").unwrap();
        assert!(junction(&root.join("linked"), &outside), "mklink /J");
        let declared =
            BatchInput { path: WorkspacePath::parse("linked/secret.csv").unwrap(), sha256: sha256_hex(b"s") };
        let err = stage(&root, &work, NO_CANCEL, &request(BatchCode::Inline { source: String::new() }, vec![declared]))
            .unwrap_err();
        assert_eq!(err.code(), "research.inputRefused", "{err}");
        assert!(!work.join("linked").exists());
    }

    #[test]
    fn symlinked_inputs_are_refused_when_links_can_be_made() {
        let (tmp, root, work) = dirs();
        let outside = tmp.path().join("outside.csv");
        std::fs::write(&outside, "s").unwrap();
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_file(&outside, root.join("link.csv"));
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&outside, root.join("link.csv"));
        if let Err(e) = made {
            eprintln!("SKIPPED symlink case: creating a symbolic link is not permitted here ({e})");
            return;
        }
        let declared = BatchInput { path: WorkspacePath::parse("link.csv").unwrap(), sha256: sha256_hex(b"s") };
        let err = stage(&root, &work, NO_CANCEL, &request(BatchCode::Inline { source: String::new() }, vec![declared]))
            .unwrap_err();
        assert_eq!(err.code(), "research.inputRefused", "{err}");
    }
}
