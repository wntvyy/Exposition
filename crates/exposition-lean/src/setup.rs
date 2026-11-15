//! Guided Mathlib package setup: a new Mathlib-capable Lake package, or the
//! pinned dependencies and prebuilt cache of an existing one.
//!
//! [`inspect_package`] reads what a folder holds (lakefile, `lean-toolchain`,
//! `lake-manifest.json`, dependency checkouts, Mathlib's build outputs) and
//! runs nothing. [`LeanSetup::plan`] describes exactly what a setup will do:
//! the files it creates, the argument array of every command, which steps
//! download. [`LeanSetup::start`] builds the plan again, refuses one that
//! changed, and runs the steps as trusted jobs of the shared execution
//! service in the package folder (cancellable, bounded and redacted output,
//! refused unless the folder is trusted).
//!
//! Guarantees:
//! - The package root is explicit: every command runs in exactly the folder
//!   the plan names, with the pinned toolchain's own `lake` (never elan's
//!   shims, which download toolchains implicitly). elan's default toolchain is
//!   never changed; a missing pinned toolchain is installed only as a visible
//!   step (`elan toolchain install <name>`).
//! - A new package pins an exact Lean release and requires Mathlib at the
//!   release tag of that same Lean version; the resolved revisions are in
//!   `lake-manifest.json`, which Lake writes. An existing lakefile,
//!   `lean-toolchain`, or manifest is never overwritten, a package is never
//!   created inside or around another one, and nothing existing is replaced.
//! - Preparing is not upgrading: with a manifest, no `lake update` runs at
//!   all; Lake materializes exactly the revisions the manifest pins. Without
//!   one (a new package, or an existing package that was never resolved),
//!   `lake --keep-toolchain update` resolves once and never rewrites
//!   `lean-toolchain`. Mathlib's toolchain is compared with the package's
//!   afterwards; a mismatch fails the setup instead of rebuilding Mathlib.
//! - The prebuilt cache (`lake exe cache get`) is a performance capability.
//!   It does not check anything: verdicts come only from the Lean service's
//!   audit of a file.
//! - A setup that fails or is canceled removes what it created (recorded
//!   before each creation); whatever existed stays. A package a setup may have
//!   left incomplete keeps an application-owned record and is never reported
//!   ready until a setup of it succeeds.
//!
//! `lake update` and `lake exe cache get` run Lake configuration and programs
//! downloaded from the internet with the user's account privileges; the plan
//! says so. Only the user's explicit action starts a setup: no agent tool
//! exists for it, and project content cannot start or authorize one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use exposition_core::job::{EnvironmentSpec, ExecutionPolicy, JobId, JobOutcome, JobSpec};
use exposition_core::path::WorkspacePath;
use exposition_execution::{ExecutionService, Located, RefusalCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{broadcast, watch};
use ts_rs::TS;

use crate::project::{self, LAKEFILES, TOOLCHAIN_FILE};
use crate::provenance::{self, LAKE_MANIFEST};
use crate::toolchain::{self, Resolution};
use crate::types::LeanError;

/// The Lake package name of Mathlib.
pub const MATHLIB: &str = "mathlib";
const MATHLIB_SCOPE: &str = "leanprover-community";
const MATHLIB_CACHE_URL: &str = "https://cache.mathlib.org";
const LAKE_DIR: &str = ".lake";
const DEFAULT_PACKAGES_DIR: &str = ".lake/packages";
const GITIGNORE: &str = ".gitignore";
const RECORDS_FORMAT: &str = "exposition.leanSetupRecords/1";
const INSTALL_TIMEOUT_MS: u64 = 60 * 60 * 1000;
const UPDATE_TIMEOUT_MS: u64 = 30 * 60 * 1000;
const CACHE_TIMEOUT_MS: u64 = 60 * 60 * 1000;
const VERIFY_TIMEOUT_MS: u64 = 30 * 60 * 1000;
const JOB_LOG_LIMIT: u64 = 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(150);
const LOG_TAIL_LINES: usize = 80;
const LOG_LINE_CHARS: usize = 500;
const OUTPUT_EVENT_CHARS: usize = 16 * 1024;
const MAX_NAME_LEN: usize = 40;
const MAX_LAKEFILE_BYTES: u64 = 1024 * 1024;

// ==========================================================================
// Inspection (files only)

/// One dependency of a package as its manifest pins it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct LeanDependencyInfo {
    pub name: String,
    /// `git` or `path`.
    pub kind: String,
    /// The exact revision (commit) the manifest pins.
    pub rev: Option<String>,
    /// The revision the lakefile asked for (tag, branch).
    pub input_rev: Option<String>,
    /// Its checkout is in the package's packages folder.
    pub present: bool,
}

/// Whether a package can be checked as it is. Decided from files only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum LeanPackageReadiness {
    /// The folder has no lakefile.
    NotAPackage,
    /// A setup by Exposition did not finish here and may have left partial
    /// downloads; preparing the package again completes or replaces them.
    Unfinished {
        message: String,
    },
    /// No `lean-toolchain` pins the Lean version.
    ToolchainUnpinned,
    /// The pinned toolchain is not installed.
    ToolchainMissing {
        toolchain: String,
    },
    /// The lakefile requires dependencies but no manifest pins their revisions.
    DependenciesUnresolved,
    /// The manifest pins dependencies whose checkouts are not there.
    DependenciesMissing {
        names: Vec<String>,
    },
    /// Mathlib's checkout needs another Lean version than the package pins.
    ToolchainMismatch {
        package: String,
        mathlib: String,
    },
    /// Mathlib's checkout is there but its build outputs are not: Lean would
    /// build Mathlib from source.
    CacheMissing,
    Ready,
}

/// What a folder holds as a Lake package. Reading it runs nothing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct LeanPackageInspection {
    /// The folder inside the project (`""` is the project folder itself).
    pub folder: String,
    /// The package root (absolute).
    pub root: String,
    /// `lakefile.lean` or `lakefile.toml`, when the folder is a package.
    pub lakefile: Option<String>,
    /// The toolchain `lean-toolchain` pins, exactly as written.
    pub toolchain: Option<String>,
    pub toolchain_installed: bool,
    /// Why the pin could not be read.
    pub toolchain_problem: Option<String>,
    /// SHA-256 (hex) of `lake-manifest.json`.
    pub manifest_digest: Option<String>,
    pub manifest_problem: Option<String>,
    pub dependencies: Vec<LeanDependencyInfo>,
    /// The manifest pins Mathlib, or (without a manifest) the lakefile names it.
    pub uses_mathlib: bool,
    /// The Mathlib commit the manifest pins.
    pub mathlib_revision: Option<String>,
    /// The Mathlib revision the lakefile asks for (a tag, usually).
    pub mathlib_input_revision: Option<String>,
    /// The toolchain Mathlib's checkout pins.
    pub mathlib_toolchain: Option<String>,
    /// Mathlib's build outputs are in its checkout.
    pub cache_present: bool,
    pub readiness: LeanPackageReadiness,
}

impl LeanPackageInspection {
    /// Why the Lean server must not start on this package as it is, if it
    /// must not: starting it would download dependencies or build Mathlib
    /// from source inside the language server, or run on a package a setup
    /// left incomplete. `local_build` is the user's explicit choice to build
    /// Mathlib from source instead of using the prebuilt cache.
    pub fn blocks_checking(&self, local_build: bool) -> Option<String> {
        match &self.readiness {
            LeanPackageReadiness::Unfinished { message } => Some(message.clone()),
            _ if !self.uses_mathlib => None,
            LeanPackageReadiness::DependenciesUnresolved => Some(
                "This package requires Mathlib, but its dependencies have not been resolved (there is no lake-manifest.json). Set Up Mathlib pins the revisions and downloads Mathlib's prebuilt cache."
                    .into(),
            ),
            LeanPackageReadiness::DependenciesMissing { names } => Some(format!(
                "The dependencies this package pins are not downloaded ({}). Set Up Mathlib fetches exactly the pinned revisions and Mathlib's prebuilt cache; nothing is upgraded.",
                list(names)
            )),
            LeanPackageReadiness::ToolchainMismatch { package, mathlib } => Some(format!(
                "This package pins Lean {package}, but its Mathlib revision is built for {mathlib}. Checking would rebuild Mathlib with another Lean version. Change lean-toolchain to {mathlib}, or require a Mathlib revision made for {package}."
            )),
            LeanPackageReadiness::CacheMissing if !local_build => Some(
                "Mathlib's prebuilt cache is not in this package, so Lean would build Mathlib from source (hours, several GB of memory). Set Up Mathlib downloads the cache for the pinned revision; building from source stays possible."
                    .into(),
            ),
            _ => None,
        }
    }
}

fn list(names: &[String]) -> String {
    const SHOWN: usize = 6;
    let mut out = names.iter().take(SHOWN).cloned().collect::<Vec<_>>().join(", ");
    if names.len() > SHOWN {
        out.push_str(&format!(", and {} more", names.len() - SHOWN));
    }
    out
}

fn lakefile_in(dir: &Path) -> Option<String> {
    LAKEFILES.iter().find(|name| dir.join(name).is_file()).map(|name| (*name).to_owned())
}

/// What a lakefile says about Mathlib (a hint only: the manifest decides when
/// there is one). `lakefile.toml` is read as data; a `lakefile.lean` is code
/// and is only searched for a `require` of Mathlib.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LakefileHint {
    /// Some `require` is declared.
    pub requires: bool,
    pub mathlib: bool,
    pub mathlib_rev: Option<String>,
}

fn toml_string(value: &str) -> Option<String> {
    let value = value.trim();
    let inner = value.strip_prefix('"')?;
    let end = inner.find('"')?;
    Some(inner[..end].to_owned())
}

pub(crate) fn lakefile_hint(name: &str, text: &str) -> LakefileHint {
    let mut hint = LakefileHint::default();
    if name.ends_with(".toml") {
        // `[[require]]` tables: name, rev (declarative; no code).
        let mut in_require = false;
        let mut current: (Option<String>, Option<String>) = (None, None);
        let finish = |current: &mut (Option<String>, Option<String>), hint: &mut LakefileHint| {
            if current.0.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(MATHLIB)) {
                hint.mathlib = true;
                hint.mathlib_rev = current.1.take();
            }
            *current = (None, None);
        };
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.starts_with('[') {
                if in_require {
                    finish(&mut current, &mut hint);
                }
                in_require = line.replace(' ', "") == "[[require]]";
                hint.requires |= in_require;
                continue;
            }
            if !in_require {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                match key.trim() {
                    "name" => current.0 = toml_string(value),
                    "rev" => current.1 = toml_string(value),
                    _ => {}
                }
            }
        }
        if in_require {
            finish(&mut current, &mut hint);
        }
    } else {
        for line in text.lines() {
            let line = line.trim_start();
            if let Some(rest) = line.strip_prefix("require ") {
                hint.requires = true;
                let lowered = rest.to_ascii_lowercase();
                let first = lowered.split_whitespace().next().unwrap_or("");
                if first.trim_matches(['"', '«', '»']) == MATHLIB || lowered.contains("/ \"mathlib\"") {
                    hint.mathlib = true;
                    hint.mathlib_rev =
                        rest.split_once('@').and_then(|(_, r)| toml_string(r.trim_start_matches([' ', 'g', 'i', 't'])));
                }
            }
        }
    }
    hint
}

fn read_small(path: &Path, limit: u64) -> Option<String> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > limit {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// A manifest's `packagesDir` when it is a plain relative folder.
fn packages_dir(manifest: &serde_json::Value) -> String {
    manifest
        .get("packagesDir")
        .and_then(|v| v.as_str())
        .filter(|d| {
            !d.is_empty()
                && d.len() <= 200
                && !d.starts_with(['/', '\\'])
                && !d.contains(':')
                && !d.split(['/', '\\']).any(|c| c == ".." || c.is_empty())
        })
        .unwrap_or(DEFAULT_PACKAGES_DIR)
        .to_owned()
}

fn join_relative(root: &Path, relative: &str) -> PathBuf {
    relative.split(['/', '\\']).filter(|c| !c.is_empty()).fold(root.to_path_buf(), |p, c| p.join(c))
}

/// Mathlib's build outputs are in its checkout (the cache was unpacked, or
/// Mathlib was built there).
fn mathlib_built(checkout: &Path) -> bool {
    checkout.join(LAKE_DIR).join("build").join("lib").join("lean").join("Mathlib.olean").is_file()
}

/// Inspect `package_root` (inside `workspace_root`; both absolute). Reads
/// files only.
pub fn inspect_package(
    workspace_root: &Path,
    package_root: &Path,
    elan_home: Option<&Path>,
    records: &SetupRecords,
) -> LeanPackageInspection {
    let folder = relative_folder(workspace_root, package_root);
    let mut out = LeanPackageInspection {
        folder,
        root: package_root.display().to_string(),
        lakefile: lakefile_in(package_root),
        toolchain: None,
        toolchain_installed: false,
        toolchain_problem: None,
        manifest_digest: None,
        manifest_problem: None,
        dependencies: Vec::new(),
        uses_mathlib: false,
        mathlib_revision: None,
        mathlib_input_revision: None,
        mathlib_toolchain: None,
        cache_present: false,
        readiness: LeanPackageReadiness::NotAPackage,
    };
    match project::read_pin(&package_root.join(TOOLCHAIN_FILE)) {
        Ok(Some(pin)) => {
            out.toolchain_installed = matches!(toolchain::resolve(elan_home, &pin.requested), Resolution::Installed(_));
            out.toolchain = Some(pin.requested);
        }
        Ok(None) => {}
        Err(e) => out.toolchain_problem = Some(e),
    }
    let Some(lakefile) = out.lakefile.clone() else { return out };
    let hint = read_small(&package_root.join(&lakefile), MAX_LAKEFILE_BYTES)
        .map(|text| lakefile_hint(&lakefile, &text))
        .unwrap_or_default();

    let mut has_manifest = false;
    let mut packages_folder = DEFAULT_PACKAGES_DIR.to_owned();
    match provenance::read_manifest(workspace_root, &package_root.join(LAKE_MANIFEST)) {
        Ok(None) => {}
        Ok(Some(bytes)) => {
            has_manifest = true;
            out.manifest_digest = Some(hex(&Sha256::digest(&bytes)));
            match serde_json::from_slice::<serde_json::Value>(&bytes) {
                Ok(value) => packages_folder = packages_dir(&value),
                Err(e) => out.manifest_problem = Some(format!("{LAKE_MANIFEST} is not valid JSON: {e}")),
            }
            match provenance::parse_packages(&bytes) {
                Ok(packages) => {
                    out.dependencies = packages
                        .into_iter()
                        .map(|p| {
                            let present =
                                p.kind != "git" || join_relative(package_root, &packages_folder).join(&p.name).is_dir();
                            LeanDependencyInfo {
                                name: p.name,
                                kind: p.kind,
                                rev: p.rev,
                                input_rev: p.input_rev,
                                present,
                            }
                        })
                        .collect();
                }
                Err(e) => out.manifest_problem = Some(e),
            }
        }
        Err(e) => out.manifest_problem = Some(e),
    }
    let mathlib = out.dependencies.iter().find(|d| d.name.eq_ignore_ascii_case(MATHLIB)).cloned();
    out.uses_mathlib = mathlib.is_some() || ((!has_manifest || out.manifest_problem.is_some()) && hint.mathlib);
    out.mathlib_revision = mathlib.as_ref().and_then(|m| m.rev.clone());
    out.mathlib_input_revision = mathlib.as_ref().and_then(|m| m.input_rev.clone()).or(hint.mathlib_rev.clone());
    if let Some(m) = &mathlib {
        let checkout = join_relative(package_root, &packages_folder).join(&m.name);
        if m.present {
            out.mathlib_toolchain =
                project::read_pin(&checkout.join(TOOLCHAIN_FILE)).ok().flatten().map(|p| p.requested);
            out.cache_present = mathlib_built(&checkout);
        }
    }

    let missing: Vec<String> = out.dependencies.iter().filter(|d| !d.present).map(|d| d.name.clone()).collect();
    out.readiness = if let Some(record) = records.unfinished(package_root) {
        LeanPackageReadiness::Unfinished { message: record }
    } else if out.toolchain_problem.is_none() && out.toolchain.is_none() {
        LeanPackageReadiness::ToolchainUnpinned
    } else if let (Some(toolchain), false) = (&out.toolchain, out.toolchain_installed) {
        LeanPackageReadiness::ToolchainMissing { toolchain: toolchain.clone() }
    } else if !has_manifest && hint.requires {
        LeanPackageReadiness::DependenciesUnresolved
    } else if !missing.is_empty() {
        LeanPackageReadiness::DependenciesMissing { names: missing }
    } else if let (Some(package), Some(theirs)) = (&out.toolchain, &out.mathlib_toolchain)
        && toolchain::qualify(package) != toolchain::qualify(theirs)
    {
        LeanPackageReadiness::ToolchainMismatch { package: package.clone(), mathlib: theirs.clone() }
    } else if mathlib.is_some() && !out.cache_present {
        LeanPackageReadiness::CacheMissing
    } else {
        LeanPackageReadiness::Ready
    };
    out
}

/// `dir` relative to `root` with forward slashes (`""` for the root itself).
fn relative_folder(root: &Path, dir: &Path) -> String {
    dir.strip_prefix(root)
        .map(|rel| rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/"))
        .unwrap_or_default()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ==========================================================================
// Application-owned records of setups that may have left a package incomplete

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct RecordsFile {
    format: String,
    records: Vec<Record>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Record {
    /// Package root (absolute, as planned).
    root: String,
    /// Paths (relative to the root) the setup created, in creation order.
    created: Vec<String>,
    /// The setup ended without completing or removing what it started.
    interrupted: bool,
}

/// `<data>/lean/setup-records.json`: never in the project, never imported.
pub struct SetupRecords {
    path: Option<PathBuf>,
    guard: Mutex<()>,
}

fn path_key(path: &Path) -> String {
    let text = path.display().to_string();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text).trim_end_matches(['/', '\\']).to_owned();
    if cfg!(windows) { text.replace('/', "\\").to_lowercase() } else { text }
}

impl SetupRecords {
    /// `path` is the records file; `None` keeps nothing (tests, no data folder).
    pub fn new(path: Option<PathBuf>) -> Self {
        Self { path, guard: Mutex::new(()) }
    }

    fn read(&self) -> RecordsFile {
        self.path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|bytes| serde_json::from_slice::<RecordsFile>(&bytes).ok())
            .filter(|f| f.format == RECORDS_FORMAT)
            .unwrap_or_default()
    }

    fn write(&self, mut file: RecordsFile) -> std::io::Result<()> {
        let Some(path) = &self.path else { return Ok(()) };
        file.format = RECORDS_FORMAT.to_owned();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&file).unwrap_or_default())?;
        std::fs::rename(&tmp, path)
    }

    fn update(&self, root: &Path, change: impl FnOnce(Option<Record>) -> Option<Record>) -> std::io::Result<()> {
        let _guard = self.guard.lock().unwrap_or_else(|e| e.into_inner());
        let mut file = self.read();
        let key = path_key(root);
        let existing = file.records.iter().position(|r| path_key(Path::new(&r.root)) == key);
        let current = existing.map(|i| file.records.remove(i));
        if let Some(record) = change(current) {
            file.records.push(record);
        }
        self.write(file)
    }

    fn get(&self, root: &Path) -> Option<Record> {
        let _guard = self.guard.lock().unwrap_or_else(|e| e.into_inner());
        let key = path_key(root);
        self.read().records.into_iter().find(|r| path_key(Path::new(&r.root)) == key)
    }

    /// The message of an unfinished setup of `root`, if one is recorded and
    /// the package folder still exists.
    pub fn unfinished(&self, root: &Path) -> Option<String> {
        let record = self.get(root)?;
        (record.interrupted && root.is_dir()).then(|| {
            "A Mathlib setup did not finish in this package and may have left partial downloads. Set Up Mathlib prepares it again."
                .to_owned()
        })
    }
}

// ==========================================================================
// Contract types

/// What to set up in the open project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum LeanSetupRequest {
    /// Create a Lake package that requires Mathlib in `folder` (inside the
    /// project; `""` is the project folder) with the library `name`.
    NewMathlibPackage { folder: String, name: String },
    /// Fetch the dependencies an existing package pins and Mathlib's
    /// prebuilt cache for the pinned revision. Never upgrades.
    PrepareMathlibPackage { folder: String },
}

/// One command a setup runs, exactly as it is launched (no shell).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct LeanSetupStep {
    pub label: String,
    pub program: String,
    pub args: Vec<String>,
    /// Variables set for this command only.
    pub environment: BTreeMap<String, String>,
    /// The step downloads from the internet.
    pub network: bool,
    pub timeout_ms: u64,
}

/// Everything a setup will do. Nothing runs until it is started.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct LeanSetupPlan {
    pub request: LeanSetupRequest,
    /// Project folder (absolute).
    pub root: String,
    /// The package root every command runs in (absolute).
    pub package_root: String,
    /// The package folder inside the project (`""` is the project folder).
    pub folder: String,
    /// The setup creates the package (its files are removed if it fails).
    pub creates_package: bool,
    /// The exact Lean toolchain the package pins.
    pub toolchain: String,
    pub toolchain_installed: bool,
    /// The Mathlib revision: the release tag a new package requires, or the
    /// commit an existing manifest pins.
    pub mathlib_revision: String,
    /// The existing manifest pins the revisions (nothing is resolved again).
    pub pinned_by_manifest: bool,
    /// Files the setup writes itself, relative to the package folder.
    pub writes: Vec<String>,
    /// Existing configuration the setup leaves untouched.
    pub keeps: Vec<String>,
    /// Paths (relative to the package folder) removed again if the setup
    /// fails or is canceled: only ones that do not exist now.
    pub removed_on_failure: Vec<String>,
    pub steps: Vec<LeanSetupStep>,
    /// Plain description of the change.
    pub summary: String,
    /// SHA-256 over the roots, toolchain, revision, files, and steps:
    /// starting re-plans and refuses a plan whose hash no longer matches.
    pub plan_hash: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum LeanSetupState {
    Running,
    Succeeded,
    Failed,
    Canceled,
}

/// What a running setup is doing now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum LeanSetupPhase {
    Preparing,
    /// Running `step`.
    Step,
    /// Comparing the result with the plan (toolchain, revisions, cache).
    Verifying,
    /// Removing what this setup created (failed or canceled).
    CleaningUp,
    Finished,
}

/// A setup run (one at a time).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct LeanSetupStatus {
    pub id: String,
    pub plan: LeanSetupPlan,
    pub state: LeanSetupState,
    pub phase: LeanSetupPhase,
    /// Index of the step running now (or the last one that ran).
    pub step: Option<u32>,
    /// Why it failed or was canceled, and what happened to the package.
    pub message: Option<String>,
    /// Paths (relative to the package folder) this setup removed again.
    pub removed: Vec<String>,
    /// The package as inspected after the setup ended.
    pub inspection: Option<LeanPackageInspection>,
    /// The package is ready for checking.
    pub ready: bool,
    /// Execution-service jobs of the steps (their full bounded logs).
    pub jobs: Vec<String>,
    /// The last lines of output (redacted).
    pub log: Vec<String>,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
}

/// Events emitted to the webview as `lean:setup`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum LeanSetupEvent {
    Changed {
        status: Box<LeanSetupStatus>,
    },
    /// Output of the running step (bounded, redacted).
    Output {
        setup_id: String,
        step: u32,
        text: String,
    },
}

// ==========================================================================
// Planning (files only)

fn refused(code: &str, message: impl Into<String>) -> LeanError {
    LeanError::new(&format!("lean.setup.{code}"), message)
}

/// A Lean library name: one upper-camel-case identifier.
pub fn validate_library_name(name: &str) -> Result<(), LeanError> {
    let mut chars = name.chars();
    let valid = name.len() <= MAX_NAME_LEN
        && chars.next().is_some_and(|c| c.is_ascii_uppercase())
        && chars.all(|c| c.is_ascii_alphanumeric());
    if !valid {
        return Err(refused(
            "invalidName",
            format!(
                "`{name}` is not a library name. Use letters and digits starting with a capital letter, for example Formal or OddSums (at most {MAX_NAME_LEN} characters)."
            ),
        ));
    }
    // Names Windows reserves for devices cannot be folders.
    const RESERVED: [&str; 8] = ["Con", "Prn", "Aux", "Nul", "Com1", "Com2", "Lpt1", "Lpt2"];
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(name)) || name.eq_ignore_ascii_case("Mathlib") {
        return Err(refused("invalidName", format!("`{name}` cannot be used as the library name.")));
    }
    Ok(())
}

/// The package folder a request names, inside `root` (absolute, canonical).
/// Links are not followed out of the project: every existing component must
/// be a real folder.
pub fn package_folder(root: &Path, folder: &str) -> Result<PathBuf, LeanError> {
    let folder = folder.trim();
    if folder.starts_with(['/', '\\']) {
        return Err(refused("invalidFolder", format!("`{folder}` is not a folder inside the project.")));
    }
    let folder = folder.trim_end_matches('/');
    if folder.is_empty() {
        return Ok(root.to_path_buf());
    }
    let path = WorkspacePath::parse(folder)
        .map_err(|e| refused("invalidFolder", format!("`{folder}` is not a folder inside the project: {e}")))?;
    let mut out = root.to_path_buf();
    for component in path.components() {
        let lowered = component.to_ascii_lowercase();
        if component.starts_with('.')
            || matches!(lowered.as_str(), "lake-packages" | "node_modules" | "target" | "venv")
            || component.ends_with(['.', ' '])
        {
            return Err(refused(
                "invalidFolder",
                format!("`{folder}` cannot hold a package: `{component}` is reserved for tools or hidden."),
            ));
        }
        out.push(component);
        match std::fs::symlink_metadata(&out) {
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
                return Err(refused(
                    "invalidFolder",
                    format!("`{folder}` cannot hold a package: `{component}` is not a regular folder."),
                ));
            }
            _ => {}
        }
    }
    // A junction or other link resolves elsewhere: compare the real location.
    if out.exists() {
        let real = std::fs::canonicalize(&out).map_err(|e| refused("invalidFolder", format!("`{folder}`: {e}")))?;
        let real_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        if !real.starts_with(&real_root) || path_key(&real) != path_key(&join_relative(&real_root, folder)) {
            return Err(refused("invalidFolder", format!("`{folder}` resolves outside the project folder.")));
        }
    }
    Ok(out)
}

/// The Mathlib release tag made for a Lean release toolchain
/// (`leanprover/lean4:v4.35.0` → `v4.35.0`): Mathlib tags every Lean release
/// and release candidate with the same name. Other toolchains have none.
pub fn mathlib_tag_for(toolchain_name: &str) -> Option<String> {
    let qualified = toolchain::qualify(toolchain_name);
    let (origin, release) = qualified.split_once(':')?;
    if origin != toolchain::DEFAULT_ORIGIN {
        return None;
    }
    let rest = release.strip_prefix('v')?;
    let (core, rc) = match rest.split_once("-rc") {
        Some((core, rc)) => (core, Some(rc)),
        None => (rest, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    let numeric = |s: &str| !s.is_empty() && s.len() <= 4 && s.chars().all(|c| c.is_ascii_digit());
    (parts.len() == 3 && parts.iter().all(|p| numeric(p)) && rc.is_none_or(numeric)).then(|| release.to_owned())
}

/// The newest installed Lean release under elan (`None` when there is none).
fn newest_installed_release(elan_home: &Path) -> Option<String> {
    let prefix = format!("{}---", toolchain::DEFAULT_ORIGIN.replace('/', "--"));
    let mut found: Vec<(Vec<u64>, String)> = std::fs::read_dir(elan_home.join("toolchains"))
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let release = name.strip_prefix(&prefix)?.to_owned();
            if release.contains("-rc") {
                return None;
            }
            let qualified = format!("{}:{release}", toolchain::DEFAULT_ORIGIN);
            mathlib_tag_for(&qualified)?;
            matches!(toolchain::resolve(Some(elan_home), &qualified), Resolution::Installed(_)).then(|| {
                let key = release.trim_start_matches('v').split('.').filter_map(|p| p.parse().ok()).collect();
                (key, qualified)
            })
        })
        .collect();
    found.sort();
    found.pop().map(|(_, name)| name)
}

/// The exact toolchain a new package pins: the folder's own `lean-toolchain`
/// when there is one (kept as it is), else elan's default when it resolves to
/// an installed release, else the newest installed release.
fn toolchain_for_new(package_root: &Path, elan_home: Option<&Path>) -> Result<(String, bool, bool), LeanError> {
    match project::read_pin(&package_root.join(TOOLCHAIN_FILE)) {
        Err(e) => return Err(refused("invalidToolchain", e)),
        Ok(Some(pin)) => {
            let installed = matches!(toolchain::resolve(elan_home, &pin.requested), Resolution::Installed(_));
            return Ok((pin.requested, installed, true));
        }
        Ok(None) => {}
    }
    let Some(home) = elan_home else {
        return Err(refused(
            "noElan",
            "elan, the Lean toolchain manager, was not found. Install it from https://lean-lang.org/install/ (on Windows: elan-init.ps1 from https://github.com/leanprover/elan), then set up Mathlib again.",
        ));
    };
    let default =
        toolchain::elan_default(home).and_then(|requested| match toolchain::resolve(Some(home), &requested) {
            Resolution::Installed(t) if mathlib_tag_for(&t.name).is_some() => Some(t.name),
            _ => None,
        });
    match default.or_else(|| newest_installed_release(home)) {
        Some(name) => Ok((name, true, false)),
        None => Err(refused(
            "noToolchain",
            "No Lean release is installed. Install one with elan (for example `elan toolchain install stable`), then set up Mathlib again. Exposition never changes elan's default toolchain.",
        )),
    }
}

fn lake_program(elan_home: Option<&Path>, toolchain_name: &str) -> Result<String, LeanError> {
    let home = elan_home.ok_or_else(|| {
        refused(
            "noElan",
            "elan, the Lean toolchain manager, was not found. Install it from https://lean-lang.org/install/.",
        )
    })?;
    let dir = home.join("toolchains").join(toolchain::dir_name(&toolchain::qualify(toolchain_name)));
    Ok(dir.join("bin").join(if cfg!(windows) { "lake.exe" } else { "lake" }).display().to_string())
}

fn lakefile_text(name: &str, tag: &str) -> String {
    // The layout of Lake's own `math` template, with Mathlib pinned to the
    // release made for the package's Lean version instead of a moving branch.
    format!(
        "name = \"{name}\"\nversion = \"0.1.0\"\nkeywords = [\"math\"]\ndefaultTargets = [\"{name}\"]\n\n[leanOptions]\npp.unicode.fun = true # pretty-prints `fun a ↦ b`\nrelaxedAutoImplicit = false\nweak.linter.mathlibStandardSet = true\nmaxSynthPendingDepth = 3\n\n[[require]]\nname = \"{MATHLIB}\"\nscope = \"{MATHLIB_SCOPE}\"\nrev = \"{tag}\"\n\n[[lean_lib]]\nname = \"{name}\"\n"
    )
}

const STARTER: &str = "import Mathlib.Tactic.Ring\n\n/-- A first statement checked with Mathlib. Replace it with your own. -/\ntheorem two_mul_eq_add (n : ℕ) : 2 * n = n + n := by\n  ring\n";

fn step(label: &str, program: &str, args: &[&str], network: bool, timeout_ms: u64) -> LeanSetupStep {
    LeanSetupStep {
        label: label.to_owned(),
        program: program.to_owned(),
        args: args.iter().map(|a| (*a).to_owned()).collect(),
        environment: BTreeMap::new(),
        network,
        timeout_ms,
    }
}

fn install_step(toolchain_name: &str) -> LeanSetupStep {
    let qualified = toolchain::qualify(toolchain_name);
    step(
        &format!("Install Lean toolchain {qualified}"),
        "elan",
        &["toolchain", "install", &qualified],
        true,
        INSTALL_TIMEOUT_MS,
    )
}

fn update_step(lake: &str) -> LeanSetupStep {
    let mut s = step(
        "Resolve the dependencies and write lake-manifest.json",
        lake,
        &["--keep-toolchain", "update"],
        true,
        UPDATE_TIMEOUT_MS,
    );
    // Mathlib's post-update hook would otherwise fetch the cache inside this
    // step; the cache is its own visible step.
    s.environment.insert("MATHLIB_NO_CACHE_ON_UPDATE".into(), "1".into());
    s
}

fn cache_step(lake: &str) -> LeanSetupStep {
    step(
        "Download Mathlib's prebuilt cache for the pinned revision",
        lake,
        &["exe", "cache", "get"],
        true,
        CACHE_TIMEOUT_MS,
    )
}

fn plan_hash(plan: &LeanSetupPlan) -> String {
    let basis = serde_json::json!({
        "root": plan.root,
        "packageRoot": plan.package_root,
        "createsPackage": plan.creates_package,
        "toolchain": plan.toolchain,
        "toolchainInstalled": plan.toolchain_installed,
        "mathlibRevision": plan.mathlib_revision,
        "pinnedByManifest": plan.pinned_by_manifest,
        "writes": plan.writes,
        "removedOnFailure": plan.removed_on_failure,
        "steps": plan.steps,
    });
    hex(&Sha256::digest(basis.to_string().as_bytes()))
}

/// Inputs of planning.
pub struct PlanContext<'a> {
    /// The open project folder (absolute, canonical).
    pub root: &'a Path,
    pub elan_home: Option<&'a Path>,
    pub records: &'a SetupRecords,
}

/// Describe what `request` would do. Reads files only.
pub fn build_plan(context: &PlanContext<'_>, request: &LeanSetupRequest) -> Result<LeanSetupPlan, LeanError> {
    let root = context.root;
    match request {
        LeanSetupRequest::NewMathlibPackage { folder, name } => {
            validate_library_name(name)?;
            let package_root = package_folder(root, folder)?;
            let folder = relative_folder(root, &package_root);
            let shown = if folder.is_empty() { "the project folder".to_owned() } else { format!("`{folder}`") };
            if package_root.exists() && !package_root.is_dir() {
                return Err(refused("invalidFolder", format!("{shown} is a file, not a folder.")));
            }
            if let Some(lakefile) = lakefile_in(&package_root) {
                return Err(refused(
                    "packageExists",
                    format!(
                        "{shown} already is a Lake package ({lakefile}). Nothing is overwritten: prepare the existing package instead."
                    ),
                ));
            }
            // Never a package inside another package, or around one.
            let mut above = package_root.parent();
            while let Some(dir) = above {
                if !dir.starts_with(root) {
                    break;
                }
                if let Some(lakefile) = lakefile_in(dir) {
                    let outer = relative_folder(root, dir);
                    return Err(refused(
                        "insidePackage",
                        format!(
                            "{shown} is inside the Lake package in {} ({lakefile}). A package inside another package is not created; add Mathlib to that package, or choose a folder outside it.",
                            if outer.is_empty() { "the project folder".to_owned() } else { format!("`{outer}`") }
                        ),
                    ));
                }
                if dir == root {
                    break;
                }
                above = dir.parent();
            }
            if package_root.is_dir()
                && let Some(inner) = project::find_projects(&package_root).first()
            {
                return Err(refused(
                    "containsPackage",
                    format!(
                        "{shown} contains the Lake package in `{}`. A package around another package is not created; prepare that package, or choose another folder.",
                        relative_folder(root, &inner.root)
                    ),
                ));
            }
            let lean_root = format!("{name}.lean");
            let starter = format!("{name}/Basic.lean");
            for existing in [LAKE_MANIFEST, LAKE_DIR, lean_root.as_str(), name.as_str()] {
                if std::fs::symlink_metadata(package_root.join(existing)).is_ok() {
                    return Err(refused(
                        "wouldOverwrite",
                        format!(
                            "{shown} already contains `{existing}`. Nothing is overwritten: choose another library name or folder."
                        ),
                    ));
                }
            }
            let (toolchain_name, toolchain_installed, keeps_toolchain) =
                toolchain_for_new(&package_root, context.elan_home)?;
            let Some(tag) = mathlib_tag_for(&toolchain_name) else {
                return Err(refused(
                    "noMathlibRelease",
                    format!(
                        "Mathlib has release tags for Lean releases only, and `{toolchain_name}` is not one. Pin a release such as leanprover/lean4:v4.35.0 in lean-toolchain, or set up the package yourself with the Mathlib revision you need."
                    ),
                ));
            };
            let lake = lake_program(context.elan_home, &toolchain_name)?;
            let mut writes = vec!["lakefile.toml".to_owned()];
            let mut keeps = Vec::new();
            if keeps_toolchain {
                keeps.push(TOOLCHAIN_FILE.to_owned());
            } else {
                writes.push(TOOLCHAIN_FILE.to_owned());
            }
            writes.extend([lean_root.clone(), starter]);
            if package_root.join(GITIGNORE).exists() {
                keeps.push(GITIGNORE.to_owned());
            } else {
                writes.push(GITIGNORE.to_owned());
            }
            let mut removed_on_failure: Vec<String> = writes.clone();
            // The starter's folder, Lake's outputs, and the manifest Lake writes.
            removed_on_failure.extend([name.clone(), LAKE_MANIFEST.to_owned(), LAKE_DIR.to_owned()]);
            if !package_root.exists() {
                removed_on_failure.push(String::new());
            }
            let mut steps = Vec::new();
            if !toolchain_installed {
                steps.push(install_step(&toolchain_name));
            }
            steps.push(update_step(&lake));
            steps.push(cache_step(&lake));
            steps.push(step(
                &format!("Build the starter library {name} with the cache"),
                &lake,
                &["build", name],
                false,
                VERIFY_TIMEOUT_MS,
            ));
            let mut plan = LeanSetupPlan {
                request: LeanSetupRequest::NewMathlibPackage { folder: folder.clone(), name: name.clone() },
                root: root.display().to_string(),
                package_root: package_root.display().to_string(),
                folder,
                creates_package: true,
                toolchain: toolchain::qualify(&toolchain_name),
                toolchain_installed,
                mathlib_revision: tag.clone(),
                pinned_by_manifest: false,
                writes,
                keeps,
                removed_on_failure,
                steps,
                summary: format!(
                    "Creates the Lake package {name} in {shown} with Lean {} and Mathlib {tag}, downloads Mathlib and its prebuilt cache (several GB) from GitHub and {MATHLIB_CACHE_URL}, and builds the starter library.",
                    toolchain::qualify(&toolchain_name)
                ),
                plan_hash: String::new(),
            };
            plan.plan_hash = plan_hash(&plan);
            Ok(plan)
        }
        LeanSetupRequest::PrepareMathlibPackage { folder } => {
            let package_root = package_folder(root, folder)?;
            let inspection = inspect_package(root, &package_root, context.elan_home, context.records);
            let folder = inspection.folder.clone();
            let shown = if folder.is_empty() { "the project folder".to_owned() } else { format!("`{folder}`") };
            let Some(lakefile) = inspection.lakefile.clone() else {
                return Err(refused(
                    "notAPackage",
                    format!("{shown} is not a Lake package (it has no lakefile.lean or lakefile.toml)."),
                ));
            };
            if let Some(problem) = &inspection.toolchain_problem {
                return Err(refused("invalidToolchain", problem.clone()));
            }
            let Some(toolchain_name) = inspection.toolchain.clone() else {
                return Err(refused(
                    "toolchainUnpinned",
                    format!(
                        "The package in {shown} has no lean-toolchain file, so the Lean version its Mathlib needs is not pinned. Add a lean-toolchain file with the Lean release of its Mathlib revision (Mathlib's own lean-toolchain names it), then prepare again."
                    ),
                ));
            };
            if let Some(problem) = &inspection.manifest_problem {
                return Err(refused("invalidManifest", problem.clone()));
            }
            if !inspection.uses_mathlib {
                return Err(refused(
                    "noMathlib",
                    format!(
                        "The package in {shown} does not require Mathlib ({} names no mathlib dependency). Add the requirement to the lakefile yourself; Exposition does not edit an existing lakefile.",
                        if inspection.manifest_digest.is_some() { LAKE_MANIFEST } else { lakefile.as_str() }
                    ),
                ));
            }
            match &inspection.readiness {
                LeanPackageReadiness::Ready => {
                    return Err(refused(
                        "alreadyPrepared",
                        format!(
                            "The package in {shown} is already prepared: Lean {toolchain_name} is installed, the pinned dependencies are downloaded, and Mathlib's build outputs are present. Nothing needs to be downloaded."
                        ),
                    ));
                }
                LeanPackageReadiness::ToolchainMismatch { package, mathlib } => {
                    return Err(refused(
                        "toolchainMismatch",
                        format!(
                            "The package pins Lean {package}, but its Mathlib revision is built for {mathlib}; the prebuilt cache would not match. Change lean-toolchain to {mathlib}, or require a Mathlib revision made for {package}. Nothing was changed."
                        ),
                    ));
                }
                _ => {}
            }
            let pinned_by_manifest = inspection.manifest_digest.is_some();
            let revision = if pinned_by_manifest {
                inspection.mathlib_revision.clone()
            } else {
                inspection.mathlib_input_revision.clone()
            }
            .unwrap_or_else(|| "the revision the lakefile requires".to_owned());
            let lake = lake_program(context.elan_home, &toolchain_name)?;
            let mut steps = Vec::new();
            if !inspection.toolchain_installed {
                steps.push(install_step(&toolchain_name));
            }
            if !pinned_by_manifest {
                steps.push(update_step(&lake));
            }
            steps.push(cache_step(&lake));
            steps.push(step(
                "Verify that Mathlib's build outputs are complete (builds nothing)",
                &lake,
                &["build", "--no-build", "Mathlib"],
                false,
                VERIFY_TIMEOUT_MS,
            ));
            let mut keeps = vec![lakefile, TOOLCHAIN_FILE.to_owned()];
            let mut removed_on_failure = Vec::new();
            if pinned_by_manifest {
                keeps.push(LAKE_MANIFEST.to_owned());
            } else {
                removed_on_failure.push(LAKE_MANIFEST.to_owned());
            }
            if !package_root.join(LAKE_DIR).exists() {
                removed_on_failure.push(LAKE_DIR.to_owned());
            }
            let mut plan = LeanSetupPlan {
                request: LeanSetupRequest::PrepareMathlibPackage { folder: folder.clone() },
                root: root.display().to_string(),
                package_root: package_root.display().to_string(),
                folder,
                creates_package: false,
                toolchain: toolchain::qualify(&toolchain_name),
                toolchain_installed: inspection.toolchain_installed,
                mathlib_revision: revision.clone(),
                pinned_by_manifest,
                writes: Vec::new(),
                keeps,
                removed_on_failure,
                steps,
                summary: format!(
                    "Prepares the Lake package in {shown}: {} and downloads Mathlib's prebuilt cache (several GB) from {MATHLIB_CACHE_URL} for Mathlib {revision}. Nothing is upgraded and no configuration file is changed.",
                    if pinned_by_manifest {
                        "fetches exactly the revisions lake-manifest.json pins"
                    } else {
                        "resolves the dependencies the lakefile requires once (writing lake-manifest.json)"
                    }
                ),
                plan_hash: String::new(),
            };
            plan.plan_hash = plan_hash(&plan);
            Ok(plan)
        }
    }
}

// ==========================================================================
// Running

struct Run {
    id: String,
    canceled: AtomicBool,
    job: Mutex<Option<JobId>>,
    status: Mutex<LeanSetupStatus>,
    done: watch::Sender<Option<LeanSetupStatus>>,
}

struct Inner {
    exec: ExecutionService,
    elan_home: Option<PathBuf>,
    records: Arc<SetupRecords>,
    active: Mutex<Option<Arc<Run>>>,
    last: Mutex<Option<LeanSetupStatus>>,
    events: broadcast::Sender<LeanSetupEvent>,
    /// Called with the package root when a setup ends (the Lean service
    /// re-resolves the documents of that package).
    finished: Box<dyn Fn(&Path) + Send + Sync>,
}

/// Plans and runs Mathlib package setups. Cheap to clone.
#[derive(Clone)]
pub struct LeanSetup {
    inner: Arc<Inner>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

enum Stop {
    Canceled(String),
    Failed(String),
}

impl LeanSetup {
    pub fn new(
        exec: ExecutionService,
        elan_home: Option<PathBuf>,
        records: Arc<SetupRecords>,
        finished: Box<dyn Fn(&Path) + Send + Sync>,
    ) -> Self {
        let (events, _) = broadcast::channel(1024);
        Self {
            inner: Arc::new(Inner {
                exec,
                elan_home,
                records,
                active: Mutex::new(None),
                last: Mutex::new(None),
                events,
                finished,
            }),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<LeanSetupEvent> {
        self.inner.events.subscribe()
    }

    pub fn records(&self) -> &Arc<SetupRecords> {
        &self.inner.records
    }

    pub fn elan_home(&self) -> Option<&Path> {
        self.inner.elan_home.as_deref()
    }

    /// Inspect the package in `folder` of the project `root` (files only).
    pub fn inspect(&self, root: &Path, folder: &str) -> Result<LeanPackageInspection, LeanError> {
        let package_root = package_folder(root, folder)?;
        Ok(inspect_package(root, &package_root, self.inner.elan_home.as_deref(), &self.inner.records))
    }

    /// Describe a setup for the project `root` (runs nothing).
    pub fn plan(&self, root: &Path, request: &LeanSetupRequest) -> Result<LeanSetupPlan, LeanError> {
        build_plan(
            &PlanContext { root, elan_home: self.inner.elan_home.as_deref(), records: &self.inner.records },
            request,
        )
    }

    /// The running setup, else the last finished one.
    pub fn status(&self) -> Option<LeanSetupStatus> {
        if let Some(run) = lock(&self.inner.active).as_ref() {
            return Some(lock(&run.status).clone());
        }
        lock(&self.inner.last).clone()
    }

    /// The package root of the running setup, if one runs.
    pub fn running_root(&self) -> Option<PathBuf> {
        lock(&self.inner.active).as_ref().map(|run| PathBuf::from(&lock(&run.status).plan.package_root))
    }

    /// Cancel a running setup (false when it is not running).
    pub fn cancel(&self, setup_id: &str) -> bool {
        let Some(run) = lock(&self.inner.active).clone() else { return false };
        if run.id != setup_id {
            return false;
        }
        run.canceled.store(true, Ordering::SeqCst);
        if let Some(job) = lock(&run.job).clone() {
            self.inner.exec.cancel(&job);
        }
        true
    }

    /// Wait for a setup to end.
    pub async fn wait(&self, setup_id: &str) -> Option<LeanSetupStatus> {
        let mut rx = {
            let active = lock(&self.inner.active);
            match active.as_ref() {
                Some(run) if run.id == setup_id => run.done.subscribe(),
                _ => return lock(&self.inner.last).clone().filter(|s| s.id == setup_id),
            }
        };
        let done = rx.wait_for(|s| s.is_some()).await.ok()?;
        done.clone()
    }

    /// Start a reviewed plan: the plan is built again and one that changed is
    /// refused (`lean.setup.planStale`). One setup runs at a time.
    pub fn start(&self, root: &Path, plan: &LeanSetupPlan) -> Result<LeanSetupStatus, LeanError> {
        let current = self.plan(root, &plan.request)?;
        if current.plan_hash != plan.plan_hash || current != *plan {
            return Err(refused(
                "planStale",
                "The project changed since this plan was made (files, toolchains, or the manifest). Review the new plan.",
            ));
        }
        match self.inner.exec.trust_status(&trust_scope(Path::new(&current.package_root)).display().to_string()) {
            Ok(status) if status.trusted => {}
            _ => {
                return Err(LeanError::new(
                    "lean.untrusted",
                    "Trust the project folder first: the setup runs Lake, the package's configuration, and downloaded programs on this computer.",
                ));
            }
        }
        if current.steps.iter().any(|s| s.program == "elan")
            && !matches!(self.inner.exec.locate_tool("elan"), Located::Found { .. })
        {
            return Err(refused(
                "noElan",
                "elan was not found, so the Lean toolchain cannot be installed. Install elan from https://lean-lang.org/install/, then set up again.",
            ));
        }
        let run = {
            let mut active = lock(&self.inner.active);
            if let Some(running) = active.as_ref() {
                let folder = lock(&running.status).plan.folder.clone();
                return Err(refused(
                    "busy",
                    format!(
                        "A Mathlib setup is already running in {}. Wait for it or cancel it.",
                        if folder.is_empty() { "the project folder".to_owned() } else { format!("`{folder}`") }
                    ),
                ));
            }
            let id = uuid::Uuid::new_v4().simple().to_string();
            let (done, _) = watch::channel(None);
            let run = Arc::new(Run {
                id: id.clone(),
                canceled: AtomicBool::new(false),
                job: Mutex::new(None),
                status: Mutex::new(LeanSetupStatus {
                    id,
                    plan: current.clone(),
                    state: LeanSetupState::Running,
                    phase: LeanSetupPhase::Preparing,
                    step: None,
                    message: None,
                    removed: Vec::new(),
                    inspection: None,
                    ready: false,
                    jobs: Vec::new(),
                    log: Vec::new(),
                    started_at_ms: now_ms(),
                    finished_at_ms: None,
                }),
                done,
            });
            *active = Some(Arc::clone(&run));
            run
        };
        let status = lock(&run.status).clone();
        self.changed(&run);
        let this = self.clone();
        let root = root.to_path_buf();
        std::thread::Builder::new()
            .name("lean-setup".into())
            .spawn(move || this.execute(&root, &run, &current))
            .map_err(|e| LeanError::new("lean.setup.failed", format!("The setup could not start: {e}")))?;
        Ok(status)
    }

    fn changed(&self, run: &Run) {
        let status = lock(&run.status).clone();
        let _ = self.inner.events.send(LeanSetupEvent::Changed { status: Box::new(status) });
    }

    fn execute(&self, root: &Path, run: &Arc<Run>, plan: &LeanSetupPlan) {
        let package_root = PathBuf::from(&plan.package_root);
        let mut created: Vec<String> = Vec::new();
        let mut ran_step = false;
        let outcome = self.run_steps(root, run, plan, &package_root, &mut created, &mut ran_step);

        let mut removed = Vec::new();
        let mut cleanup_problem = None;
        if outcome.is_err() {
            lock(&run.status).phase = LeanSetupPhase::CleaningUp;
            self.changed(run);
            // Newest first, so files go before the folders that hold them.
            for relative in created.iter().rev() {
                let target = join_relative(&package_root, relative);
                match remove_created(&package_root, &target) {
                    Ok(true) => removed.push(relative.clone()),
                    Ok(false) => {}
                    Err(e) => {
                        cleanup_problem.get_or_insert_with(|| {
                            format!(
                                "`{}` could not be removed ({e}).",
                                if relative.is_empty() { "." } else { relative.as_str() }
                            )
                        });
                    }
                }
            }
        }
        // A package whose `.lake` existed before may hold partial downloads
        // this run cannot tell from what was there: it stays recorded as
        // unfinished. A run whose own `.lake` was removed again leaves no
        // record, and neither does a run in which no command started.
        let lake_existed = !created.iter().any(|c| c == LAKE_DIR);
        let leftovers =
            outcome.is_err() && (cleanup_problem.is_some() || (ran_step && !plan.creates_package && lake_existed));
        let record = self.inner.records.update(&package_root, |_| {
            leftovers.then(|| Record { root: plan.package_root.clone(), created: Vec::new(), interrupted: true })
        });
        let inspection = inspect_package(root, &package_root, self.inner.elan_home.as_deref(), &self.inner.records);
        let ready = outcome.is_ok() && inspection.readiness == LeanPackageReadiness::Ready;
        let (state, message) = match outcome {
            Ok(()) if ready => (LeanSetupState::Succeeded, None),
            Ok(()) => (
                LeanSetupState::Failed,
                Some(format!(
                    "The steps finished, but the package is not ready: {}",
                    inspection.blocks_checking(false).unwrap_or_else(|| format!("{:?}", inspection.readiness))
                )),
            ),
            Err(stop) => {
                let (state, mut message) = match stop {
                    Stop::Canceled(m) => (LeanSetupState::Canceled, m),
                    Stop::Failed(m) => (LeanSetupState::Failed, m),
                };
                if plan.creates_package && cleanup_problem.is_none() {
                    message.push_str(" The new package's files were removed; nothing else in the project changed.");
                } else if let Some(problem) = &cleanup_problem {
                    message.push_str(&format!(
                        " {problem} The package is not offered as ready; set it up again to finish or replace it."
                    ));
                } else if !ran_step {
                    message.push_str(" Nothing was changed.");
                } else if !leftovers {
                    message.push_str(
                        " What this setup downloaded was removed; your configuration files were not changed.",
                    );
                } else {
                    message.push_str(
                        " Your configuration files were not changed. Downloads that were already complete stay; the package is not offered as ready until a setup of it succeeds.",
                    );
                }
                (state, Some(message))
            }
        };
        let message = match (message, record) {
            (Some(m), Err(e)) => Some(format!("{m} The setup record could not be written: {e}.")),
            (m, _) => m,
        };
        let final_status = {
            let mut status = lock(&run.status);
            status.state = state;
            status.phase = LeanSetupPhase::Finished;
            status.message = message;
            status.removed = removed;
            status.ready = ready;
            status.inspection = Some(inspection);
            status.finished_at_ms = Some(now_ms());
            status.clone()
        };
        *lock(&self.inner.last) = Some(final_status.clone());
        {
            let mut active = lock(&self.inner.active);
            if active.as_ref().is_some_and(|a| a.id == run.id) {
                *active = None;
            }
        }
        *lock(&run.job) = None;
        let _ = self.inner.events.send(LeanSetupEvent::Changed { status: Box::new(final_status.clone()) });
        let _ = run.done.send(Some(final_status));
        (self.inner.finished)(&package_root);
    }

    fn run_steps(
        &self,
        root: &Path,
        run: &Arc<Run>,
        plan: &LeanSetupPlan,
        package_root: &Path,
        created: &mut Vec<String>,
        ran_step: &mut bool,
    ) -> Result<(), Stop> {
        let canceled = || Stop::Canceled("Setup was canceled.".into());
        let check_canceled = || if run.canceled.load(Ordering::SeqCst) { Err(canceled()) } else { Ok(()) };
        check_canceled()?;
        // Everything this run may create is recorded before it exists, so an
        // interrupted run is never mistaken for a prepared package.
        let will_create: Vec<String> = plan
            .removed_on_failure
            .iter()
            .filter(|relative| std::fs::symlink_metadata(join_relative(package_root, relative)).is_err())
            .cloned()
            .collect();
        self.inner
            .records
            .update(package_root, |_| {
                Some(Record { root: plan.package_root.clone(), created: will_create.clone(), interrupted: true })
            })
            .map_err(|e| Stop::Failed(format!("The setup record could not be written: {e}.")))?;
        // Outermost first: removal goes in reverse.
        let mut ordered = will_create;
        ordered.sort_by_key(|p| (if p.is_empty() { 0 } else { 1 }, p.matches('/').count(), p.clone()));
        *created = ordered;

        if plan.creates_package {
            let LeanSetupRequest::NewMathlibPackage { name, .. } = &plan.request else {
                return Err(Stop::Failed("The plan does not create a package.".into()));
            };
            let io = |what: &str, e: std::io::Error| Stop::Failed(format!("{what} could not be created: {e}."));
            std::fs::create_dir_all(package_root).map_err(|e| io("The package folder", e))?;
            std::fs::create_dir(package_root.join(name)).map_err(|e| io(name, e))?;
            for relative in &plan.writes {
                let text = match relative.as_str() {
                    "lakefile.toml" => lakefile_text(name, &plan.mathlib_revision),
                    TOOLCHAIN_FILE => format!("{}\n", plan.toolchain),
                    GITIGNORE => format!("/{LAKE_DIR}\n"),
                    r if r.ends_with("/Basic.lean") => STARTER.to_owned(),
                    _ => format!("import {name}.Basic\n"),
                };
                write_new(&join_relative(package_root, relative), &text).map_err(|e| io(relative, e))?;
            }
        }

        for (index, step) in plan.steps.iter().enumerate() {
            check_canceled()?;
            {
                let mut status = lock(&run.status);
                status.phase = LeanSetupPhase::Step;
                status.step = Some(index as u32);
            }
            self.changed(run);
            let mut environment = EnvironmentSpec::default();
            environment.set.extend(step.environment.clone());
            let spec = JobSpec {
                label: format!("Mathlib setup: {}", step.label),
                program: step.program.clone(),
                args: step.args.clone(),
                environment,
                working_directory: plan.package_root.clone(),
                policy: ExecutionPolicy::TrustedNative,
                timeout_ms: Some(step.timeout_ms),
                log_limit_bytes: Some(JOB_LOG_LIMIT),
                stdin: None,
            };
            let job = self.inner.exec.launch(spec);
            *ran_step = true;
            *lock(&run.job) = Some(job.clone());
            lock(&run.status).jobs.push(job.0.clone());
            self.changed(run);
            if run.canceled.load(Ordering::SeqCst) {
                self.inner.exec.cancel(&job);
            }
            let mut cursor = None;
            let mut tail = LineTail::default();
            let mut cancel_sent = false;
            let result = loop {
                let Some(page) = self.inner.exec.output(&job, cursor, 256 * 1024) else {
                    break None;
                };
                cursor = page.chunks.last().map(|c| c.seq).or(cursor);
                let text: String = page.chunks.iter().map(|c| c.text.as_str()).collect();
                if !text.is_empty() {
                    tail.push(&text);
                    lock(&run.status).log = tail.lines();
                    for chunk in chunks(&text, OUTPUT_EVENT_CHARS) {
                        let _ = self.inner.events.send(LeanSetupEvent::Output {
                            setup_id: run.id.clone(),
                            step: index as u32,
                            text: chunk,
                        });
                    }
                }
                if page.complete && !page.has_more {
                    break self.inner.exec.result(&job);
                }
                if run.canceled.load(Ordering::SeqCst) && !cancel_sent {
                    self.inner.exec.cancel(&job);
                    cancel_sent = true;
                }
                std::thread::sleep(POLL_INTERVAL);
            };
            *lock(&run.job) = None;
            match result.map(|r| r.outcome) {
                Some(JobOutcome::Exited { code: 0 }) => {}
                Some(JobOutcome::Exited { code }) => {
                    let detail = tail.error_summary();
                    // Lake often cannot even load a Mathlib made for another
                    // Lean version: name that cause, not only Lake's error.
                    if is_update(step)
                        && let Some(mismatch) = self.mismatch(root, package_root)
                    {
                        return Err(Stop::Failed(format!(
                            "{mismatch} (Lake reported: {})",
                            detail.trim_end_matches('.')
                        )));
                    }
                    return Err(Stop::Failed(format!(
                        "{} failed (exit code {code}){}{}",
                        step.label,
                        if detail.is_empty() {
                            ".".to_owned()
                        } else {
                            format!(": {}", detail.trim_end_matches('.')) + "."
                        },
                        failure_hint(step, &detail)
                    )));
                }
                Some(JobOutcome::Canceled) => {
                    if run.canceled.load(Ordering::SeqCst) {
                        return Err(canceled());
                    }
                    let untrusted = self
                        .inner
                        .exec
                        .trust_status(&trust_scope(package_root).display().to_string())
                        .map(|s| !s.trusted)
                        .unwrap_or(false);
                    return Err(Stop::Canceled(if untrusted {
                        "Workspace trust was revoked; setup was stopped.".into()
                    } else {
                        format!("{} was stopped.", step.label)
                    }));
                }
                Some(JobOutcome::TimedOut) => {
                    return Err(Stop::Failed(format!(
                        "{} did not finish within {} minutes.",
                        step.label,
                        step.timeout_ms / 60_000
                    )));
                }
                Some(JobOutcome::FailedToStart { reason }) => {
                    let refusal = self.inner.exec.summary(&job).and_then(|s| s.refusal);
                    let untrusted = refusal.as_ref().is_some_and(|r| r.code == RefusalCode::UntrustedWorkspace);
                    if index == 0 {
                        *ran_step = false;
                    }
                    return Err(Stop::Failed(if untrusted {
                        "The project folder is not trusted; trust it, then set up again.".into()
                    } else {
                        format!("{} could not start: {}", step.label, refusal.map(|r| r.message).unwrap_or(reason))
                    }));
                }
                Some(JobOutcome::Terminated { description }) => {
                    return Err(Stop::Failed(format!("{} ended abnormally: {description}", step.label)));
                }
                None => return Err(Stop::Failed(format!("{} disappeared from the job list.", step.label))),
            }
            // Lake must not have rewritten the pin, and Mathlib must be built
            // for it, before the cache for that pair is fetched.
            if is_update(step) {
                lock(&run.status).phase = LeanSetupPhase::Verifying;
                self.changed(run);
                let after =
                    inspect_package(root, package_root, self.inner.elan_home.as_deref(), &SetupRecords::new(None));
                if after.toolchain.as_deref().map(toolchain::qualify) != Some(plan.toolchain.clone()) {
                    return Err(Stop::Failed(format!(
                        "lean-toolchain no longer pins {} after the dependencies were resolved.",
                        plan.toolchain
                    )));
                }
                if let Some(mismatch) = self.mismatch(root, package_root) {
                    return Err(Stop::Failed(mismatch));
                }
                if after.manifest_digest.is_none() {
                    return Err(Stop::Failed("Lake did not write lake-manifest.json.".into()));
                }
            }
        }
        check_canceled()?;
        lock(&run.status).phase = LeanSetupPhase::Verifying;
        self.changed(run);
        Ok(())
    }
}

fn is_update(step: &LeanSetupStep) -> bool {
    step.args.last().is_some_and(|a| a == "update")
}

impl LeanSetup {
    /// The failure message when Mathlib's checkout pins another Lean version
    /// than the package (read from the two `lean-toolchain` files).
    fn mismatch(&self, root: &Path, package_root: &Path) -> Option<String> {
        let none = SetupRecords::new(None);
        let after = inspect_package(root, package_root, self.inner.elan_home.as_deref(), &none);
        // Before a manifest exists the checkout is still where Lake puts it.
        let theirs = after.mathlib_toolchain.clone().or_else(|| {
            let checkout = join_relative(package_root, DEFAULT_PACKAGES_DIR).join(MATHLIB);
            project::read_pin(&checkout.join(TOOLCHAIN_FILE)).ok().flatten().map(|p| p.requested)
        })?;
        let package = after.toolchain?;
        (toolchain::qualify(&package) != toolchain::qualify(&theirs)).then(|| {
            format!(
                "Mathlib {} is built for Lean {theirs}, but the package pins {package}; the prebuilt cache would not match. Require a Mathlib revision made for {package}, or change lean-toolchain to {theirs}.",
                after.mathlib_input_revision.as_deref().unwrap_or("at the required revision")
            )
        })
    }
}

/// What usually causes a failed step, when the output shows it.
fn failure_hint(step: &LeanSetupStep, detail: &str) -> &'static str {
    let lowered = detail.to_ascii_lowercase();
    let offline = [
        "could not resolve host",
        "unable to access",
        "failed to connect",
        "curl",
        "network",
        "timed out",
        "could not connect",
    ]
    .iter()
    .any(|needle| lowered.contains(needle));
    if step.network && offline {
        " This step downloads from the internet; check the connection, then set up again. Without the prebuilt cache Lean can still build Mathlib from source (hours), which Exposition never starts on its own."
    } else if step.args.iter().any(|a| a == "--no-build") {
        " Mathlib's prebuilt outputs do not cover the pinned revision completely (the cache server may not have them for this revision or platform)."
    } else {
        ""
    }
}

/// Create `path` with `text`; never replaces an existing file.
fn write_new(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(text.as_bytes())
}

/// Remove something a setup created inside `package_root`. `Ok(false)` when
/// it is not there. The package folder itself goes only when it is empty.
fn remove_created(package_root: &Path, target: &Path) -> std::io::Result<bool> {
    if !target.starts_with(package_root) {
        return Ok(false);
    }
    let Ok(meta) = std::fs::symlink_metadata(target) else { return Ok(false) };
    if target == package_root {
        return match std::fs::remove_dir(target) {
            Ok(()) => Ok(true),
            // The folder holds something this setup did not create: it stays.
            Err(_) if std::fs::read_dir(target).is_ok_and(|mut d| d.next().is_some()) => Ok(false),
            Err(e) => Err(e),
        };
    }
    if meta.is_dir() && !meta.file_type().is_symlink() {
        // Git marks pack files read-only; clear that so they can be removed.
        clear_readonly(target);
        std::fs::remove_dir_all(target)?;
    } else {
        std::fs::remove_file(target)?;
    }
    Ok(true)
}

fn clear_readonly(dir: &Path) {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else { continue };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            let file_type = entry.file_type().ok();
            if file_type.is_some_and(|t| t.is_dir()) {
                stack.push(entry.path());
            } else if meta.permissions().readonly() {
                let mut permissions = meta.permissions();
                #[allow(clippy::permissions_set_readonly_false)]
                permissions.set_readonly(false);
                let _ = std::fs::set_permissions(entry.path(), permissions);
            }
        }
    }
}

fn chunks(text: &str, size: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for c in text.chars() {
        current.push(c);
        if current.len() >= size {
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// The last lines of a step's output. A carriage return restarts the line
/// (progress counters overwrite themselves).
#[derive(Default)]
struct LineTail {
    lines: std::collections::VecDeque<String>,
    partial: String,
}

impl LineTail {
    fn push(&mut self, text: &str) {
        for c in text.chars() {
            match c {
                '\n' => {
                    let line = std::mem::take(&mut self.partial);
                    self.add(line);
                }
                '\r' => self.partial.clear(),
                c if self.partial.chars().count() < LOG_LINE_CHARS => self.partial.push(c),
                _ => {}
            }
        }
    }

    fn add(&mut self, line: String) {
        let line = strip_ansi(&line);
        if line.trim().is_empty() {
            return;
        }
        if self.lines.len() == LOG_TAIL_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    fn lines(&self) -> Vec<String> {
        let mut out: Vec<String> = self.lines.iter().cloned().collect();
        let partial = strip_ansi(&self.partial);
        if !partial.trim().is_empty() {
            out.push(partial);
        }
        out
    }

    /// The last error line, else the last line.
    fn error_summary(&self) -> String {
        let lines = self.lines();
        lines
            .iter()
            .rev()
            .find(|l| {
                let lowered = l.to_ascii_lowercase();
                lowered.contains("error") || lowered.contains("fatal")
            })
            .or_else(|| lines.last())
            .map(|l| l.trim().chars().take(300).collect())
            .unwrap_or_default()
    }
}

/// `text` without ANSI escape sequences (Lake and cache print them).
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// The folder whose trust decides a setup of `package_root`: the package
/// folder itself, or, while a new package's folder does not exist yet, the
/// nearest folder above it that does. Trust is looked up on the real
/// (canonical) folder, so a missing folder has no answer of its own; what is
/// created there lands inside this one. Each step is checked again at launch,
/// when the folder exists.
fn trust_scope(package_root: &Path) -> &Path {
    let mut scope = package_root;
    while !scope.is_dir() {
        match scope.parent() {
            Some(parent) => scope = parent,
            None => break,
        }
    }
    scope
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_of_a_new_package_is_decided_by_the_nearest_existing_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        assert_eq!(trust_scope(&root), root);
        assert_eq!(trust_scope(&root.join("lean")), root);
        assert_eq!(trust_scope(&root.join("formal").join("lean")), root);
        std::fs::create_dir_all(root.join("formal")).unwrap();
        assert_eq!(trust_scope(&root.join("formal").join("lean")), root.join("formal"));
        std::fs::write(root.join("file"), b"").unwrap();
        assert_eq!(trust_scope(&root.join("file")), root);
    }

    const MANIFEST: &str = r#"{"version": "1.3.0", "packagesDir": ".lake/packages", "packages": [
      {"url": "https://github.com/leanprover-community/mathlib4", "type": "git", "rev": "56a005855ea17e0b0958f73ec801ae140b0a1679",
       "name": "mathlib", "inputRev": "v4.35.0"},
      {"url": "https://github.com/leanprover-community/batteries", "type": "git", "rev": "66e469188fe07963e5cf666c7d4433f45b8131c5",
       "name": "batteries", "inputRev": "main"}]}"#;
    const LAKEFILE: &str = "name = \"Formal\"\n\n[[require]]\nname = \"mathlib\"\nscope = \"leanprover-community\"\nrev = \"v4.35.0\"\n\n[[lean_lib]]\nname = \"Formal\"\n";
    const PIN: &str = "leanprover/lean4:v4.35.0";

    fn fake_toolchain(home: &Path, release: &str) {
        let bin = home.join("toolchains").join(format!("leanprover--lean4---{release}")).join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        for name in ["lean", "lake"] {
            std::fs::write(bin.join(if cfg!(windows) { format!("{name}.exe") } else { name.to_owned() }), b"").unwrap();
        }
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        home: PathBuf,
        records: SetupRecords,
    }

    impl Fixture {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let root = std::fs::canonicalize(tmp.path()).unwrap().join("project");
            let home = tmp.path().join("elan");
            std::fs::create_dir_all(&root).unwrap();
            fake_toolchain(&home, "v4.35.0");
            std::fs::write(home.join("settings.toml"), "default_toolchain = \"stable\"\n").unwrap();
            let records = SetupRecords::new(Some(tmp.path().join("data").join("lean").join("setup-records.json")));
            Fixture { _tmp: tmp, root, home, records }
        }

        fn plan(&self, request: &LeanSetupRequest) -> Result<LeanSetupPlan, LeanError> {
            build_plan(&PlanContext { root: &self.root, elan_home: Some(&self.home), records: &self.records }, request)
        }

        fn inspect(&self, folder: &str) -> LeanPackageInspection {
            inspect_package(&self.root, &join_relative(&self.root, folder), Some(&self.home), &self.records)
        }

        /// An existing package in `folder`: lakefile, pin, and optionally the
        /// manifest, the checkouts, and Mathlib's build outputs.
        fn package(&self, folder: &str, manifest: bool, checkouts: bool, cache: bool) -> PathBuf {
            let dir = join_relative(&self.root, folder);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("lakefile.toml"), LAKEFILE).unwrap();
            std::fs::write(dir.join("lean-toolchain"), format!("{PIN}\n")).unwrap();
            if manifest {
                std::fs::write(dir.join(LAKE_MANIFEST), MANIFEST).unwrap();
            }
            if checkouts {
                for name in ["mathlib", "batteries"] {
                    std::fs::create_dir_all(dir.join(".lake").join("packages").join(name)).unwrap();
                }
                std::fs::write(dir.join(".lake").join("packages").join("mathlib").join("lean-toolchain"), PIN).unwrap();
            }
            if cache {
                let lib = dir
                    .join(".lake")
                    .join("packages")
                    .join("mathlib")
                    .join(".lake")
                    .join("build")
                    .join("lib")
                    .join("lean");
                std::fs::create_dir_all(&lib).unwrap();
                std::fs::write(lib.join("Mathlib.olean"), b"").unwrap();
            }
            dir
        }
    }

    fn new_package(folder: &str, name: &str) -> LeanSetupRequest {
        LeanSetupRequest::NewMathlibPackage { folder: folder.into(), name: name.into() }
    }

    fn prepare(folder: &str) -> LeanSetupRequest {
        LeanSetupRequest::PrepareMathlibPackage { folder: folder.into() }
    }

    fn code(result: Result<LeanSetupPlan, LeanError>) -> String {
        result.expect_err("refused").code
    }

    #[test]
    fn mathlib_tags_exist_for_lean_releases_only() {
        assert_eq!(mathlib_tag_for("leanprover/lean4:v4.35.0").as_deref(), Some("v4.35.0"));
        assert_eq!(mathlib_tag_for("v4.36.0-rc2").as_deref(), Some("v4.36.0-rc2"));
        assert_eq!(mathlib_tag_for("4.9.0").as_deref(), Some("v4.9.0"));
        for none in [
            "stable",
            "leanprover/lean4:nightly-2026-01-01",
            "my-local",
            "leanprover/lean4-pr-releases:pr-release-1",
            "leanprover/lean4:v4.x.0",
            "other/lean4:v4.35.0",
        ] {
            assert_eq!(mathlib_tag_for(none), None, "{none}");
        }
    }

    #[test]
    fn lakefiles_are_read_for_a_mathlib_requirement_without_running_them() {
        let toml = lakefile_hint("lakefile.toml", LAKEFILE);
        assert_eq!(toml, LakefileHint { requires: true, mathlib: true, mathlib_rev: Some("v4.35.0".into()) });
        let other = lakefile_hint(
            "lakefile.toml",
            "name = \"x\"\n[[require]]\nname = \"batteries\" # mathlib\nrev = \"main\"\n[[lean_lib]]\nname = \"mathlib\"\n",
        );
        assert_eq!(other, LakefileHint { requires: true, mathlib: false, mathlib_rev: None });
        assert_eq!(lakefile_hint("lakefile.toml", "name = \"x\"\n"), LakefileHint::default());
        let lean = lakefile_hint(
            "lakefile.lean",
            "import Lake\nopen Lake DSL\nrequire mathlib from git\n  \"https://github.com/leanprover-community/mathlib4\" @ \"v4.35.0\"\n",
        );
        assert!(lean.requires && lean.mathlib);
        let scoped =
            lakefile_hint("lakefile.lean", "require \"leanprover-community\" / \"mathlib\" @ git \"v4.35.0\"\n");
        assert!(scoped.mathlib);
        assert_eq!(scoped.mathlib_rev.as_deref(), Some("v4.35.0"));
        assert!(!lakefile_hint("lakefile.lean", "require batteries from git \"x\"\n").mathlib);
    }

    #[test]
    fn a_new_package_plan_pins_the_installed_release_and_its_mathlib_tag() {
        let f = Fixture::new();
        std::fs::write(f.root.join("paper.typ"), "= Paper\n").unwrap();
        let plan = f.plan(&new_package("lean", "OddSums")).unwrap();
        assert!(plan.creates_package && !plan.pinned_by_manifest && plan.toolchain_installed);
        assert_eq!(plan.toolchain, PIN);
        assert_eq!(plan.mathlib_revision, "v4.35.0");
        assert_eq!(plan.folder, "lean");
        assert_eq!(Path::new(&plan.package_root), f.root.join("lean"));
        assert_eq!(
            plan.writes,
            ["lakefile.toml", "lean-toolchain", "OddSums.lean", "OddSums/Basic.lean", ".gitignore"]
        );
        assert!(plan.keeps.is_empty());
        // Every command is the pinned toolchain's own lake, in order; no shim, no `elan default`.
        let lake = f.home.join("toolchains").join("leanprover--lean4---v4.35.0").join("bin").join(if cfg!(windows) {
            "lake.exe"
        } else {
            "lake"
        });
        let commands: Vec<(&str, Vec<&str>)> =
            plan.steps.iter().map(|s| (s.program.as_str(), s.args.iter().map(String::as_str).collect())).collect();
        let lake = lake.display().to_string();
        assert_eq!(
            commands,
            [
                (lake.as_str(), vec!["--keep-toolchain", "update"]),
                (lake.as_str(), vec!["exe", "cache", "get"]),
                (lake.as_str(), vec!["build", "OddSums"]),
            ]
        );
        assert_eq!(plan.steps[0].environment.get("MATHLIB_NO_CACHE_ON_UPDATE").map(String::as_str), Some("1"));
        assert_eq!(plan.steps.iter().map(|s| s.network).collect::<Vec<_>>(), [true, true, false]);
        // The new folder itself is removed on failure, with everything in it.
        assert!(
            plan.removed_on_failure.contains(&String::new()) && plan.removed_on_failure.contains(&".lake".to_owned())
        );
        // Planning wrote nothing.
        assert!(!f.root.join("lean").exists());
        assert_eq!(f.plan(&new_package("lean", "OddSums")).unwrap().plan_hash, plan.plan_hash);

        // The project folder itself as the package root: existing content stays out of the removal list.
        let root_plan = f.plan(&new_package("", "Formal")).unwrap();
        assert_eq!(root_plan.folder, "");
        assert_eq!(Path::new(&root_plan.package_root), f.root);
        assert!(!root_plan.removed_on_failure.contains(&String::new()));
        assert!(lakefile_text("Formal", "v4.35.0").contains("rev = \"v4.35.0\""));
    }

    #[test]
    fn a_new_package_keeps_an_existing_pin_and_installs_a_missing_toolchain_visibly() {
        let f = Fixture::new();
        std::fs::create_dir_all(f.root.join("lean")).unwrap();
        std::fs::write(f.root.join("lean").join("lean-toolchain"), "leanprover/lean4:v4.34.0\n").unwrap();
        std::fs::write(f.root.join("lean").join(".gitignore"), "build\n").unwrap();
        let plan = f.plan(&new_package("lean", "Formal")).unwrap();
        assert_eq!(plan.toolchain, "leanprover/lean4:v4.34.0");
        assert_eq!(plan.mathlib_revision, "v4.34.0");
        assert!(!plan.toolchain_installed);
        assert_eq!(plan.keeps, ["lean-toolchain", ".gitignore"]);
        assert!(
            !plan.writes.contains(&"lean-toolchain".to_owned())
                && !plan.removed_on_failure.contains(&"lean-toolchain".to_owned())
        );
        assert_eq!(plan.steps[0].program, "elan");
        assert_eq!(plan.steps[0].args, ["toolchain", "install", "leanprover/lean4:v4.34.0"]);
        assert!(plan.steps[1].program.contains("leanprover--lean4---v4.34.0"));

        // A pin that is not a release has no Mathlib tag: refused, nothing guessed.
        std::fs::write(f.root.join("lean").join("lean-toolchain"), "leanprover/lean4:nightly-2026-11-01\n").unwrap();
        assert_eq!(code(f.plan(&new_package("lean", "Formal"))), "lean.setup.noMathlibRelease");
    }

    #[test]
    fn a_new_package_is_refused_where_it_would_overwrite_or_nest() {
        let f = Fixture::new();
        f.package("formal", true, false, false);
        assert_eq!(code(f.plan(&new_package("formal", "Formal"))), "lean.setup.packageExists");
        assert_eq!(code(f.plan(&new_package("formal/sub", "Formal"))), "lean.setup.insidePackage");
        assert_eq!(code(f.plan(&new_package("", "Formal"))), "lean.setup.containsPackage");
        std::fs::create_dir_all(f.root.join("free").join("Formal")).unwrap();
        assert_eq!(code(f.plan(&new_package("free", "Formal"))), "lean.setup.wouldOverwrite");
        std::fs::write(f.root.join("free").join("Other.lean"), "").unwrap();
        assert_eq!(code(f.plan(&new_package("free", "Other"))), "lean.setup.wouldOverwrite");
        f.plan(&new_package("free", "Fresh")).unwrap();
        for folder in ["../out", "/abs", "a/../b", ".lake/x", ".git", "x/.venv", "node_modules/p", "C:\\x"] {
            assert_eq!(code(f.plan(&new_package(folder, "Formal"))), "lean.setup.invalidFolder", "{folder}");
        }
        std::fs::write(f.root.join("file"), "").unwrap();
        assert_eq!(code(f.plan(&new_package("file", "Formal"))), "lean.setup.invalidFolder");
        for name in ["formal", "My Lib", "A.B", "", "Mathlib", "X-1", "Con", &"A".repeat(41)] {
            assert_eq!(code(f.plan(&new_package("new", name))), "lean.setup.invalidName", "{name}");
        }
        // Nothing was created by any refusal.
        assert!(!f.root.join("new").exists() && !f.root.join("formal").join("sub").exists());
    }

    #[test]
    fn without_an_installed_release_a_new_package_is_refused_with_guidance() {
        let f = Fixture::new();
        std::fs::remove_dir_all(f.home.join("toolchains")).unwrap();
        let error = f.plan(&new_package("lean", "Formal")).unwrap_err();
        assert_eq!(error.code, "lean.setup.noToolchain");
        assert!(error.message.contains("never changes elan's default"));
        let none = build_plan(
            &PlanContext { root: &f.root, elan_home: None, records: &f.records },
            &new_package("lean", "Formal"),
        );
        assert_eq!(code(none), "lean.setup.noElan");
    }

    #[test]
    fn inspection_tells_the_states_of_an_existing_package_apart() {
        let f = Fixture::new();
        assert_eq!(f.inspect("").readiness, LeanPackageReadiness::NotAPackage);
        f.package("a", false, false, false);
        let a = f.inspect("a");
        assert!(a.uses_mathlib && a.manifest_digest.is_none());
        assert_eq!(a.mathlib_input_revision.as_deref(), Some("v4.35.0"));
        assert_eq!(a.readiness, LeanPackageReadiness::DependenciesUnresolved);
        f.package("b", true, false, false);
        let b = f.inspect("b");
        assert_eq!(b.mathlib_revision.as_deref(), Some("56a005855ea17e0b0958f73ec801ae140b0a1679"));
        assert_eq!(b.manifest_digest.as_deref().map(str::len), Some(64));
        assert_eq!(
            b.readiness,
            LeanPackageReadiness::DependenciesMissing { names: vec!["batteries".into(), "mathlib".into()] }
        );
        f.package("c", true, true, false);
        assert_eq!(f.inspect("c").readiness, LeanPackageReadiness::CacheMissing);
        let d = f.package("d", true, true, true);
        let ready = f.inspect("d");
        assert_eq!(ready.readiness, LeanPackageReadiness::Ready);
        assert!(ready.cache_present && ready.toolchain_installed);
        assert_eq!(ready.mathlib_toolchain.as_deref(), Some(PIN));
        assert_eq!(ready.blocks_checking(false), None);
        // Each unprepared state keeps the Lean server from downloading or building Mathlib on its own.
        for folder in ["a", "b", "c"] {
            assert!(f.inspect(folder).blocks_checking(false).is_some(), "{folder}");
        }
        // Building from source is the user's explicit choice, and only when just the cache is missing.
        assert_eq!(f.inspect("c").blocks_checking(true), None);
        assert!(f.inspect("b").blocks_checking(true).is_some());

        std::fs::write(
            d.join(".lake").join("packages").join("mathlib").join("lean-toolchain"),
            "leanprover/lean4:v4.36.0-rc2\n",
        )
        .unwrap();
        let mismatch = f.inspect("d");
        assert_eq!(
            mismatch.readiness,
            LeanPackageReadiness::ToolchainMismatch {
                package: PIN.into(),
                mathlib: "leanprover/lean4:v4.36.0-rc2".into()
            }
        );
        assert!(mismatch.blocks_checking(true).unwrap().contains("v4.36.0-rc2"));
        std::fs::write(d.join("lean-toolchain"), "leanprover/lean4:v4.1.0\n").unwrap();
        assert_eq!(
            f.inspect("d").readiness,
            LeanPackageReadiness::ToolchainMissing { toolchain: "leanprover/lean4:v4.1.0".into() }
        );
        std::fs::remove_file(d.join("lean-toolchain")).unwrap();
        assert_eq!(f.inspect("d").readiness, LeanPackageReadiness::ToolchainUnpinned);

        // A package without Mathlib is never held back for missing dependencies.
        let plain = f.root.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::write(
            plain.join("lakefile.toml"),
            "name = \"P\"\n[[require]]\nname = \"batteries\"\nrev = \"main\"\n",
        )
        .unwrap();
        std::fs::write(plain.join("lean-toolchain"), PIN).unwrap();
        let plain = f.inspect("plain");
        assert_eq!(plain.readiness, LeanPackageReadiness::DependenciesUnresolved);
        assert_eq!(plain.blocks_checking(false), None);
    }

    #[test]
    fn preparing_an_existing_package_never_updates_a_pinned_manifest() {
        let f = Fixture::new();
        let dir = f.package("formal", true, false, false);
        let before: Vec<(String, Vec<u8>)> = ["lakefile.toml", "lean-toolchain", LAKE_MANIFEST]
            .iter()
            .map(|n| ((*n).to_owned(), std::fs::read(dir.join(n)).unwrap()))
            .collect();
        let plan = f.plan(&prepare("formal")).unwrap();
        assert!(!plan.creates_package && plan.pinned_by_manifest && plan.writes.is_empty());
        assert_eq!(plan.mathlib_revision, "56a005855ea17e0b0958f73ec801ae140b0a1679");
        assert_eq!(plan.keeps, ["lakefile.toml", "lean-toolchain", LAKE_MANIFEST]);
        let args: Vec<Vec<&str>> = plan.steps.iter().map(|s| s.args.iter().map(String::as_str).collect()).collect();
        assert_eq!(args, [vec!["exe", "cache", "get"], vec!["build", "--no-build", "Mathlib"]]);
        assert!(plan.steps.iter().all(|s| !s.args.iter().any(|a| a == "update" || a == "--update")));
        // Only what does not exist yet is removed on failure: here `.lake`.
        assert_eq!(plan.removed_on_failure, [".lake"]);
        for (name, bytes) in &before {
            assert_eq!(&std::fs::read(dir.join(name)).unwrap(), bytes, "{name} changed while planning");
        }

        // Never resolved: one resolution that keeps the toolchain file; the manifest it writes is removed on failure.
        f.package("fresh", false, false, false);
        let fresh = f.plan(&prepare("fresh")).unwrap();
        assert!(!fresh.pinned_by_manifest);
        assert_eq!(fresh.mathlib_revision, "v4.35.0");
        assert_eq!(fresh.steps[0].args, ["--keep-toolchain", "update"]);
        assert_eq!(fresh.removed_on_failure, [LAKE_MANIFEST, ".lake"]);

        // Checkouts present, cache missing: `.lake` exists and is never removed.
        f.package("partial", true, true, false);
        assert!(f.plan(&prepare("partial")).unwrap().removed_on_failure.is_empty());
    }

    #[test]
    fn preparing_is_refused_when_there_is_nothing_supported_to_do() {
        let f = Fixture::new();
        assert_eq!(code(f.plan(&prepare(""))), "lean.setup.notAPackage");
        f.package("ready", true, true, true);
        let error = f.plan(&prepare("ready")).unwrap_err();
        assert_eq!(error.code, "lean.setup.alreadyPrepared");
        assert!(error.message.contains("Nothing needs to be downloaded"));
        let mismatch = f.package("mismatch", true, true, true);
        std::fs::write(
            mismatch.join(".lake").join("packages").join("mathlib").join("lean-toolchain"),
            "leanprover/lean4:v4.36.0-rc2",
        )
        .unwrap();
        let error = f.plan(&prepare("mismatch")).unwrap_err();
        assert_eq!(error.code, "lean.setup.toolchainMismatch");
        assert!(error.message.contains("Nothing was changed"));
        let unpinned = f.package("unpinned", true, false, false);
        std::fs::remove_file(unpinned.join("lean-toolchain")).unwrap();
        assert_eq!(code(f.plan(&prepare("unpinned"))), "lean.setup.toolchainUnpinned");
        let plain = f.root.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::write(plain.join("lakefile.toml"), "name = \"P\"\n").unwrap();
        std::fs::write(plain.join("lean-toolchain"), PIN).unwrap();
        assert_eq!(code(f.plan(&prepare("plain"))), "lean.setup.noMathlib");
        let broken = f.package("broken", false, false, false);
        std::fs::write(broken.join(LAKE_MANIFEST), "{ not json").unwrap();
        assert_eq!(code(f.plan(&prepare("broken"))), "lean.setup.invalidManifest");
        assert_eq!(std::fs::read_to_string(broken.join(LAKE_MANIFEST)).unwrap(), "{ not json");
    }

    #[test]
    fn an_unfinished_setup_is_recorded_outside_the_project_and_never_ready() {
        let f = Fixture::new();
        let dir = f.package("formal", true, true, true);
        assert_eq!(f.inspect("formal").readiness, LeanPackageReadiness::Ready);
        f.records
            .update(&dir, |_| Some(Record { root: dir.display().to_string(), created: vec![], interrupted: true }))
            .unwrap();
        let unfinished = f.inspect("formal");
        assert!(matches!(unfinished.readiness, LeanPackageReadiness::Unfinished { .. }));
        assert!(unfinished.blocks_checking(true).is_some());
        // Preparing again is offered (it is not "already prepared").
        f.plan(&prepare("formal")).unwrap();
        // Another instance reading the same file sees it; a copy of the project elsewhere has no record.
        let again = SetupRecords::new(f.records.path.clone());
        assert!(again.unfinished(&dir).is_some());
        assert!(again.unfinished(&f.root.join("copy")).is_none());
        f.records.update(&dir, |_| None).unwrap();
        assert_eq!(f.inspect("formal").readiness, LeanPackageReadiness::Ready);
        // Nothing of the record is in the project.
        assert!(!dir.join(".lake").join("exposition-setup.json").exists());
    }

    #[test]
    fn cleanup_removes_only_what_the_setup_created() {
        let tmp = tempfile::tempdir().unwrap();
        let package = tmp.path().join("pkg");
        std::fs::create_dir_all(package.join(".lake").join("packages")).unwrap();
        std::fs::write(package.join("mine.lean"), "-- user file").unwrap();
        std::fs::write(package.join("lakefile.toml"), "x").unwrap();
        assert!(remove_created(&package, &package.join(".lake")).unwrap());
        assert!(remove_created(&package, &package.join("lakefile.toml")).unwrap());
        assert!(!remove_created(&package, &package.join("absent")).unwrap());
        // The folder still holds the user's file: it stays.
        assert!(!remove_created(&package, &package).unwrap());
        assert!(package.join("mine.lean").is_file());
        // Outside the package: never touched.
        std::fs::write(tmp.path().join("outside"), "").unwrap();
        assert!(!remove_created(&package, &tmp.path().join("outside")).unwrap());
        assert!(tmp.path().join("outside").exists());
        std::fs::remove_file(package.join("mine.lean")).unwrap();
        assert!(remove_created(&package, &package).unwrap());
        // `write_new` never replaces a file.
        let file = tmp.path().join("existing");
        std::fs::write(&file, "keep").unwrap();
        assert!(write_new(&file, "replace").is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "keep");
    }

    #[test]
    fn output_tails_follow_progress_counters_and_drop_escape_codes() {
        let mut tail = LineTail::default();
        tail.push("Downloaded: 1 file(s) [attempted 1/9031 = 0%]\rDownloaded: 70 file(s) [attempted 70/9031 = 0%]");
        assert_eq!(tail.lines(), ["Downloaded: 70 file(s) [attempted 70/9031 = 0%]"]);
        tail.push("\n\u{1b}[31merror:\u{1b}[0m mathlib: failed to fetch cache\nlast line\n");
        assert_eq!(tail.error_summary(), "error: mathlib: failed to fetch cache");
        assert_eq!(strip_ansi("\u{1b}[2K✔ built"), "✔ built");
    }
}
