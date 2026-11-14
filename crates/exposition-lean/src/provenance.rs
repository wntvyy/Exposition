//! Toolchain provenance of a Lean file, read from files only.
//!
//! Workflow runs record what a Lean audit ran with (M9 environment capture):
//! the toolchain the file's `lean-toolchain` pins, and the digest of the Lake
//! project's `lake-manifest.json` with the exact revision of every dependency
//! (mathlib, …). Nothing here runs Lean, Lake, elan, or git: a lakefile is
//! code, and capturing provenance must never execute anything.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::project::{ProjectKind, project_for};

/// The manifest Lake writes next to the lakefile.
pub const LAKE_MANIFEST: &str = "lake-manifest.json";
/// A manifest larger than this is not read (a real one is a few KiB).
pub const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
/// At most this many dependencies are listed.
pub const MAX_PACKAGES: usize = 1000;
const MAX_FIELD_CHARS: usize = 200;

/// One dependency of a Lake project as its manifest pins it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LakePackage {
    pub name: String,
    /// `git` or `path`.
    pub kind: String,
    /// The exact git revision (commit) for git dependencies.
    pub rev: Option<String>,
    /// The revision the lakefile asked for (branch, tag), if any.
    pub input_rev: Option<String>,
}

/// What the files around a Lean file say about its toolchain and dependencies.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LeanProvenance {
    /// The toolchain `lean-toolchain` pins (exactly as written).
    pub toolchain_pin: Option<String>,
    /// Why the pin could not be read, if it could not.
    pub pin_error: Option<String>,
    /// The file belongs to a Lake package.
    pub lake_project: bool,
    /// SHA-256 (hex) of the Lake project's `lake-manifest.json`, when present.
    pub manifest_digest: Option<String>,
    /// Dependencies the manifest pins (bounded).
    pub packages: Vec<LakePackage>,
    /// Why the manifest was not used, if it exists but could not be read.
    pub manifest_error: Option<String>,
}

impl LeanProvenance {
    /// The pinned revision of the `mathlib` dependency, if the project uses it.
    pub fn mathlib_rev(&self) -> Option<&str> {
        self.packages.iter().find(|p| p.name.eq_ignore_ascii_case("mathlib")).and_then(|p| p.rev.as_deref())
    }
}

/// Provenance of `file` (absolute, inside `workspace_root`; both canonical).
pub fn lean_provenance(workspace_root: &Path, file: &Path) -> LeanProvenance {
    let project = project_for(workspace_root, file);
    let mut out = LeanProvenance::default();
    match &project.pin {
        Ok(pin) => out.toolchain_pin = pin.as_ref().map(|p| p.requested.clone()),
        // Recorded with runs: never the absolute location.
        Err(e) => out.pin_error = Some(without_root(e, workspace_root)),
    }
    if !matches!(project.kind, ProjectKind::Lake { .. }) {
        return out;
    }
    out.lake_project = true;
    match read_manifest(workspace_root, &project.root.join(LAKE_MANIFEST)) {
        Ok(None) => {}
        Ok(Some(bytes)) => {
            out.manifest_digest = Some(Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect());
            match parse_packages(&bytes) {
                Ok(packages) => out.packages = packages,
                Err(e) => out.manifest_error = Some(e),
            }
        }
        Err(e) => out.manifest_error = Some(e),
    }
    out
}

/// `text` with the workspace root's absolute location removed (paths
/// become project-relative).
fn without_root(text: &str, root: &Path) -> String {
    let display = root.display().to_string();
    let mut out = text.to_owned();
    for form in [display.clone(), display.strip_prefix(r"\\?\").unwrap_or(&display).to_owned()] {
        if form.is_empty() {
            continue;
        }
        for sep in ['\\', '/'] {
            out = out.replace(&format!("{form}{sep}"), "");
        }
        out = out.replace(&form, ".");
    }
    out
}

/// The manifest's bytes: `Ok(None)` when there is none. A link, a folder, an
/// oversized file, or a path that resolves outside the workspace is refused.
fn read_manifest(workspace_root: &Path, path: &Path) -> Result<Option<Vec<u8>>, String> {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return Ok(None) };
    if !meta.is_file() {
        return Err(format!("{LAKE_MANIFEST} is not a regular file"));
    }
    if meta.len() > MAX_MANIFEST_BYTES {
        return Err(format!("{LAKE_MANIFEST} is larger than {MAX_MANIFEST_BYTES} bytes"));
    }
    let canonical: PathBuf = std::fs::canonicalize(path).map_err(|e| format!("{LAKE_MANIFEST}: {e}"))?;
    let root = std::fs::canonicalize(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf());
    if !canonical.starts_with(&root) {
        return Err(format!("{LAKE_MANIFEST} resolves outside the project"));
    }
    std::fs::read(&canonical).map(Some).map_err(|e| format!("{LAKE_MANIFEST} could not be read: {e}"))
}

fn field(value: &serde_json::Value, name: &str) -> Option<String> {
    value
        .get(name)
        .and_then(|v| v.as_str())
        .map(|s| s.chars().filter(|c| !c.is_control()).take(MAX_FIELD_CHARS).collect::<String>())
        .filter(|s| !s.is_empty())
}

/// Dependencies of a manifest (Lake manifest versions 7 and later: a
/// `packages` array of `{ type, name, rev, inputRev, … }`).
fn parse_packages(bytes: &[u8]) -> Result<Vec<LakePackage>, String> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| format!("{LAKE_MANIFEST} is not valid JSON: {e}"))?;
    let Some(packages) = value.get("packages").and_then(|p| p.as_array()) else {
        return Ok(Vec::new());
    };
    let mut out: Vec<LakePackage> = packages
        .iter()
        .take(MAX_PACKAGES)
        .filter_map(|p| {
            let name = field(p, "name")?;
            Some(LakePackage {
                name,
                kind: field(p, "type").unwrap_or_else(|| "git".to_owned()),
                rev: field(p, "rev"),
                input_rev: field(p, "inputRev"),
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()).then(a.name.cmp(&b.name)));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"{"version": "1.1.0",
 "packagesDir": ".lake/packages",
 "packages":
 [{"url": "https://github.com/leanprover-community/mathlib4",
   "type": "git",
   "subDir": null,
   "scope": "leanprover-community",
   "rev": "0123456789abcdef0123456789abcdef01234567",
   "name": "mathlib",
   "manifestFile": "lake-manifest.json",
   "inputRev": "master",
   "inherited": false,
   "configFile": "lakefile.lean"},
  {"type": "path", "scope": "", "name": "Local", "manifestFile": "lake-manifest.json", "inherited": false, "dir": "../Local", "configFile": "lakefile.toml"},
  {"url": "https://github.com/leanprover-community/batteries", "type": "git", "subDir": null, "scope": "leanprover-community",
   "rev": "fedcba9876543210fedcba9876543210fedcba98", "name": "batteries", "manifestFile": "lake-manifest.json",
   "inputRev": "main", "inherited": true, "configFile": "lakefile.toml"}],
 "name": "Paper",
 "lakeDir": ".lake"}"#;

    fn project(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        for (name, text) in files {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        (tmp, root)
    }

    #[test]
    fn a_lake_project_reports_its_pin_manifest_digest_and_dependency_revisions() {
        let (_tmp, root) = project(&[
            ("lakefile.lean", "import Lake\n"),
            ("lean-toolchain", "leanprover/lean4:v4.35.0\n"),
            ("lake-manifest.json", MANIFEST),
            ("Paper/Main.lean", "theorem t : True := trivial\n"),
        ]);
        let p = lean_provenance(&root, &root.join("Paper").join("Main.lean"));
        assert!(p.lake_project);
        assert_eq!(p.toolchain_pin.as_deref(), Some("leanprover/lean4:v4.35.0"));
        let digest: String = Sha256::digest(MANIFEST.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(p.manifest_digest.as_deref(), Some(digest.as_str()));
        let names: Vec<&str> = p.packages.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["batteries", "Local", "mathlib"], "sorted by name");
        assert_eq!(p.mathlib_rev(), Some("0123456789abcdef0123456789abcdef01234567"));
        let local = p.packages.iter().find(|p| p.name == "Local").unwrap();
        assert_eq!((local.kind.as_str(), local.rev.as_deref()), ("path", None));
        assert!(p.manifest_error.is_none());
    }

    #[test]
    fn a_standalone_file_has_its_pin_but_no_manifest() {
        let (_tmp, root) = project(&[
            ("lean-toolchain", "leanprover/lean4:v4.20.0"),
            ("lake-manifest.json", MANIFEST),
            ("A.lean", ""),
        ]);
        let p = lean_provenance(&root, &root.join("A.lean"));
        assert!(!p.lake_project, "no lakefile: the manifest beside it is not the file's project");
        assert_eq!(p.toolchain_pin.as_deref(), Some("leanprover/lean4:v4.20.0"));
        assert!(p.manifest_digest.is_none() && p.packages.is_empty());
    }

    #[test]
    fn a_lake_project_without_a_manifest_or_with_a_broken_one_says_so() {
        let (_tmp, root) = project(&[("lakefile.toml", "name = \"X\"\n"), ("X.lean", "")]);
        let p = lean_provenance(&root, &root.join("X.lean"));
        assert!(p.lake_project && p.manifest_digest.is_none() && p.manifest_error.is_none());
        assert!(p.toolchain_pin.is_none());

        std::fs::write(root.join(LAKE_MANIFEST), "{not json").unwrap();
        let p = lean_provenance(&root, &root.join("X.lean"));
        assert!(p.manifest_digest.is_some(), "the bytes are still identified");
        assert!(p.manifest_error.as_deref().is_some_and(|e| e.contains("not valid JSON")), "{p:?}");
        assert!(p.packages.is_empty());
    }

    #[test]
    fn a_pin_error_names_the_file_relative_to_the_project() {
        let (_tmp, root) = project(&[("lean-toolchain", &"x".repeat(5000)), ("A.lean", "")]);
        let p = lean_provenance(&root, &root.join("A.lean"));
        let error = p.pin_error.expect("an oversized pin is an error");
        assert!(error.starts_with("lean-toolchain"), "{error}");
        let bare = root.display().to_string();
        let bare = bare.strip_prefix(r"\\?\").unwrap_or(&bare).to_owned();
        assert!(!error.contains(&bare), "{error}");
    }

    #[test]
    fn a_manifest_that_is_a_folder_is_refused() {
        let (_tmp, root) = project(&[("lakefile.toml", "name = \"X\"\n"), ("X.lean", "")]);
        std::fs::create_dir_all(root.join(LAKE_MANIFEST)).unwrap();
        let p = lean_provenance(&root, &root.join("X.lean"));
        assert!(p.manifest_digest.is_none());
        assert!(p.manifest_error.as_deref().is_some_and(|e| e.contains("not a regular file")), "{p:?}");
    }
}
