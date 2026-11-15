//! Guided environment setup: project-local Python environments.
//!
//! [`ResearchService::setup_plan`] describes exactly what a setup will do —
//! the argument array of every command, the folder it creates, the packages,
//! whether it needs the network — and runs nothing.
//! [`ResearchService::setup_start`] runs a plan's steps as trusted project
//! jobs of the shared execution service (cancellable, bounded and redacted
//! output, refused unless the folder is trusted), then discovers environments
//! again.
//!
//! Guarantees:
//! - Only a discovered interpreter outside the project can be a base; only
//!   `<root>/.venv` is ever created; packages are installed only into a
//!   project environment (`.venv`/`venv`), never into a system Python.
//! - Package specifications are validated ([`validate_packages`]): a name,
//!   optional extras, optional version clauses from a restricted set — never
//!   URLs, paths, options, markers, direct references (`@`), or files (`-r`).
//! - pip runs isolated (`-I`, `--isolated`), only inside the virtual
//!   environment (`--require-virtualenv`), binary wheels only (no package
//!   build code runs during the install), from PyPI only.
//! - An environment the run created is removed when the run fails or is
//!   canceled: the marker written at creation (and the app-local record)
//!   prove it is the run's own. An existing environment is never deleted or
//!   replaced; installing into it only adds packages.
//! - An environment whose setup did not finish is never reported ready
//!   (discovery reads the marker before it runs anything there).
//!
//! Package installation runs code from the internet with the user's account
//! privileges; the plan says so and the caller (the user's explicit action, or
//! an agent's setup authority) decides. A virtual environment is not a sandbox.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use exposition_core::job::{ExecutionPolicy, JobSpec};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, watch};
use ts_rs::TS;

use crate::discovery::now_ms;
use crate::error::ResearchError;
use crate::helper::sha256_hex;
use crate::launcher::{KernelProcessLauncher, ProjectJobEnd, research_environment};
use crate::service::{ResearchService, display_path};
use crate::types::{Availability, EnvironmentReport, RuntimeLanguage, SessionOwner};

/// Most packages one setup installs (the kernel packages included).
pub const MAX_PACKAGES: usize = 50;
/// Longest accepted package specification.
pub const MAX_SPEC_LEN: usize = 200;
/// Packages every research environment needs (always installed).
pub const KERNEL_PACKAGES: [&str; 2] = ["ipykernel", "jupyter_client"];
/// The only package index setup uses.
pub const PYPI_INDEX: &str = "https://pypi.org/simple";
/// The project environment folder setup creates.
pub const ENVIRONMENT_FOLDER: &str = ".venv";
/// Project environment folders setup may install into (as discovery offers them).
pub const PROJECT_ENVIRONMENT_FOLDERS: [&str; 2] = [".venv", "venv"];
/// Marker written into an environment setup creates.
pub const MARKER_FILE: &str = ".exposition-setup.json";
const MARKER_FORMAT: &str = "exposition.environmentSetup/1";
const RECORDS_FILE: &str = "setup-records.json";
const RECORDS_FORMAT: &str = "exposition.setupRecords/1";
const VENV_TIMEOUT_MS: u64 = 5 * 60 * 1000;
const PIP_TIMEOUT_MS: u64 = 30 * 60 * 1000;
const JOB_LOG_LIMIT: u64 = 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(150);
/// Output lines kept in a setup's status (the whole bounded log stays in the job).
const LOG_TAIL_LINES: usize = 80;
const LOG_LINE_CHARS: usize = 500;
/// Largest output chunk sent in one event.
const OUTPUT_EVENT_CHARS: usize = 16 * 1024;

// ==========================================================================
// Contract types

/// What to set up in the open project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum SetupRequest {
    /// Create the project environment `.venv` from a discovered Python
    /// interpreter (outside the project), then install the kernel packages
    /// and `packages` into it.
    CreateEnvironment { base_environment: String, packages: Vec<String> },
    /// Add packages to an existing project environment (`.venv` or `venv`).
    /// The kernel packages are always included; nothing is removed.
    InstallPackages { environment: String, packages: Vec<String> },
}

/// One command a setup runs, exactly as it is launched (no shell).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SetupStep {
    pub label: String,
    pub program: String,
    pub args: Vec<String>,
    /// The step downloads from the package index.
    pub network: bool,
    pub timeout_ms: u64,
}

/// Everything a setup will do. Nothing runs until it is started.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SetupPlan {
    pub request: SetupRequest,
    /// Project folder.
    pub root: String,
    /// Environment folder (absolute).
    pub target: String,
    /// Environment folder name inside the project (`.venv`).
    pub target_name: String,
    /// The setup creates the environment (it is removed if the setup fails or is canceled).
    pub creates_environment: bool,
    /// An unfinished environment an interrupted setup by Exposition left in
    /// the target folder is removed first (proved by its app-local record).
    pub removes_unfinished: bool,
    /// The interpreter the environment is created from.
    pub base_interpreter: Option<String>,
    /// e.g. "Python 3.12.10 (CPython)".
    pub base_display_name: Option<String>,
    /// The environment's interpreter (runs pip).
    pub interpreter: String,
    /// Exact requirement arguments pip receives, kernel packages first.
    pub packages: Vec<String>,
    pub index_url: String,
    /// Some step downloads from the package index.
    pub network: bool,
    pub steps: Vec<SetupStep>,
    /// Plain description of the change.
    pub summary: String,
    /// SHA-256 over the root, target, and steps: starting re-plans and
    /// refuses a plan whose hash no longer matches (`setup.planStale`).
    pub plan_hash: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum SetupState {
    Running,
    Succeeded,
    Failed,
    Canceled,
}

/// What a running setup is doing now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum SetupPhase {
    Preparing,
    /// Running `step`.
    Step,
    /// Removing the environment this setup created (failed or canceled).
    CleaningUp,
    /// Discovering environments again.
    Checking,
    Finished,
}

/// A setup run (one at a time).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SetupStatus {
    pub id: String,
    /// Who started it (the user, or an agent task through its setup authority).
    pub owner: SessionOwner,
    pub plan: SetupPlan,
    pub state: SetupState,
    pub phase: SetupPhase,
    /// Index of the step running now (or the last one that ran).
    pub step: Option<u32>,
    /// Why it failed or was canceled, and what happened to the environment.
    pub message: Option<String>,
    /// The environment this setup created was removed again.
    pub removed: bool,
    /// The target environment as discovery reports it afterwards.
    pub environment_id: Option<String>,
    /// Discovery reports that environment ready for sessions.
    pub ready: bool,
    /// Execution-service jobs of the steps (their full bounded logs).
    pub jobs: Vec<String>,
    /// The last lines of output (redacted).
    pub log: Vec<String>,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
}

/// Events emitted to the webview as `research:setup`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum SetupEvent {
    Changed {
        status: Box<SetupStatus>,
    },
    /// Output of the running step (bounded, redacted).
    Output {
        setup_id: String,
        step: u32,
        text: String,
    },
}

// ==========================================================================
// Package validation

/// One validated requirement: a name, optional extras, optional version clauses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageSpec {
    pub name: String,
    pub extras: Vec<String>,
    /// `(operator, version)` pairs.
    pub clauses: Vec<(String, String)>,
}

impl PackageSpec {
    /// The exact argument pip receives (no spaces).
    pub fn to_arg(&self) -> String {
        let mut out = self.name.clone();
        if !self.extras.is_empty() {
            out.push('[');
            out.push_str(&self.extras.join(","));
            out.push(']');
        }
        let clauses: Vec<String> = self.clauses.iter().map(|(op, v)| format!("{op}{v}")).collect();
        out.push_str(&clauses.join(","));
        out
    }

    /// PEP 503 normalized name (`Jupyter_Client` → `jupyter-client`).
    pub fn normalized_name(&self) -> String {
        normalize_name(&self.name)
    }
}

/// PEP 503 name normalization.
pub fn normalize_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut separator = false;
    for c in name.chars() {
        if matches!(c, '-' | '_' | '.') {
            separator = true;
            continue;
        }
        if separator && !out.is_empty() {
            out.push('-');
        }
        separator = false;
        out.push(c.to_ascii_lowercase());
    }
    out
}

const OPERATORS: [&str; 7] = ["==", "!=", "<=", ">=", "~=", "<", ">"];

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')
}

/// A PEP 508 name: `[A-Za-z0-9]([A-Za-z0-9._-]*[A-Za-z0-9])?`, at most 100 characters.
fn valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !name.is_empty()
        && name.len() <= 100
        && name.chars().all(is_name_char)
        && bytes[0].is_ascii_alphanumeric()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
}

/// A version from a restricted character set: starts with a digit; letters,
/// digits, `.`, `!` (epoch), `+` (local); `.*` only at the end.
fn valid_version(version: &str, operator: &str) -> Result<(), String> {
    if version.is_empty() {
        return Err("a version is missing after the operator".into());
    }
    if version.len() > 64 {
        return Err("the version is longer than 64 characters".into());
    }
    let core = match version.strip_suffix(".*") {
        Some(core) => {
            if operator != "==" && operator != "!=" {
                return Err("a wildcard version (.*) works only with == or !=".into());
            }
            core
        }
        None => version,
    };
    if !core.starts_with(|c: char| c.is_ascii_digit()) {
        return Err("a version must start with a digit".into());
    }
    if let Some(bad) = core.chars().find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '!' | '+'))) {
        return Err(format!("the version contains “{bad}”"));
    }
    Ok(())
}

/// Parse one package specification (see [`validate_packages`]). The error is a
/// plain reason.
pub fn parse_package(spec: &str) -> Result<PackageSpec, String> {
    let text = spec.trim();
    if text.is_empty() {
        return Err("it is empty".into());
    }
    if text.chars().count() > MAX_SPEC_LEN {
        return Err(format!("it is longer than {MAX_SPEC_LEN} characters"));
    }
    if text.contains("://") {
        return Err("URLs are not accepted".into());
    }
    if text.contains('@') {
        return Err("direct references (@) are not accepted".into());
    }
    if text.contains(';') {
        return Err("environment markers (;) are not accepted".into());
    }
    if text.starts_with('-') {
        return Err("options are not accepted".into());
    }
    if text.contains('/') || text.contains('\\') || text.starts_with('.') || text.starts_with('~') {
        return Err("paths are not accepted".into());
    }
    if text.contains("===") {
        return Err("arbitrary equality (===) is not accepted".into());
    }
    if let Some(bad) = text.chars().find(|c| c.is_control() || (!c.is_ascii() && !c.is_whitespace())) {
        return Err(format!("it contains the character {:?}", bad));
    }

    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let skip_space = |i: &mut usize| {
        while *i < chars.len() && chars[*i].is_whitespace() {
            *i += 1;
        }
    };
    let take_name = |i: &mut usize| {
        let start = *i;
        while *i < chars.len() && is_name_char(chars[*i]) {
            *i += 1;
        }
        chars[start..*i].iter().collect::<String>()
    };

    let name = take_name(&mut i);
    if !valid_name(&name) {
        return Err(if name.is_empty() {
            "it does not start with a package name".into()
        } else {
            format!("“{name}” is not a valid package name")
        });
    }
    skip_space(&mut i);

    let mut extras = Vec::new();
    if i < chars.len() && chars[i] == '[' {
        i += 1;
        loop {
            skip_space(&mut i);
            let extra = take_name(&mut i);
            if !valid_name(&extra) {
                return Err("the extras are not valid (use names like [plot,io])".into());
            }
            extras.push(extra);
            skip_space(&mut i);
            match chars.get(i) {
                Some(',') => i += 1,
                Some(']') => {
                    i += 1;
                    break;
                }
                _ => return Err("the extras are not closed with ]".into()),
            }
        }
        if extras.len() > 10 {
            return Err("at most 10 extras are accepted".into());
        }
        skip_space(&mut i);
    }

    let mut clauses = Vec::new();
    while i < chars.len() {
        let rest: String = chars[i..].iter().collect();
        let Some(op) = OPERATORS.iter().find(|op| rest.starts_with(**op)) else {
            return Err(format!("unexpected “{}” (expected a version such as >=1.2)", chars[i]));
        };
        i += op.len();
        skip_space(&mut i);
        let start = i;
        while i < chars.len() && !chars[i].is_whitespace() && chars[i] != ',' {
            i += 1;
        }
        let version: String = chars[start..i].iter().collect();
        valid_version(&version, op)?;
        clauses.push(((*op).to_owned(), version));
        skip_space(&mut i);
        match chars.get(i) {
            None => break,
            Some(',') => {
                i += 1;
                skip_space(&mut i);
                if i >= chars.len() {
                    return Err("a version is missing after the comma".into());
                }
            }
            Some(other) => return Err(format!("unexpected “{other}”")),
        }
    }
    if clauses.len() > 8 {
        return Err("at most 8 version clauses are accepted".into());
    }
    Ok(PackageSpec { name, extras, clauses })
}

/// Text of a refused specification that is safe to show.
fn shown(spec: &str) -> String {
    let clean: String = spec.trim().chars().filter(|c| !c.is_control()).take(80).collect();
    if spec.trim().chars().count() > 80 { format!("{clean}…") } else { clean }
}

/// Validate package specifications the user (or an agent) asked for and
/// return the exact pip arguments, duplicates removed. At most
/// [`MAX_PACKAGES`]. Refusals: `setup.invalidPackage`, `setup.tooManyPackages`.
pub fn validate_packages(specs: &[String]) -> Result<Vec<String>, ResearchError> {
    let specs: Vec<&String> = specs.iter().filter(|s| !s.trim().is_empty()).collect();
    if specs.len() > MAX_PACKAGES {
        return Err(ResearchError::coded(
            "setup.tooManyPackages",
            format!("At most {MAX_PACKAGES} packages can be installed at once ({} were given).", specs.len()),
        ));
    }
    let mut seen: HashMap<String, String> = HashMap::new();
    let mut out = Vec::new();
    for spec in specs {
        let parsed = parse_package(spec).map_err(|reason| {
            ResearchError::coded(
                "setup.invalidPackage",
                format!(
                    "“{}” is not accepted: {reason}. Use a package name, optionally with extras and versions, such as numpy or pandas>=2.2.",
                    shown(spec)
                ),
            )
        })?;
        let arg = parsed.to_arg();
        match seen.get(&parsed.normalized_name()) {
            Some(previous) if *previous == arg => continue,
            Some(previous) => {
                return Err(ResearchError::coded(
                    "setup.invalidPackage",
                    format!("{} is listed twice ({previous} and {arg}); list it once.", parsed.name),
                ));
            }
            None => {
                seen.insert(parsed.normalized_name(), arg.clone());
                out.push(arg);
            }
        }
    }
    Ok(out)
}

/// The packages a setup installs: the kernel packages (unless the request
/// names them itself) followed by the validated request.
pub fn install_list(requested: &[String]) -> Result<Vec<String>, ResearchError> {
    let requested = validate_packages(requested)?;
    let names: HashSet<String> =
        requested.iter().filter_map(|arg| parse_package(arg).ok()).map(|p| p.normalized_name()).collect();
    let mut out: Vec<String> =
        KERNEL_PACKAGES.iter().filter(|p| !names.contains(&normalize_name(p))).map(|p| (*p).to_owned()).collect();
    out.extend(requested);
    if out.len() > MAX_PACKAGES {
        return Err(ResearchError::coded(
            "setup.tooManyPackages",
            format!(
                "At most {MAX_PACKAGES} packages can be installed at once, including ipykernel and jupyter_client."
            ),
        ));
    }
    Ok(out)
}

// ==========================================================================
// Markers and app-local records

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum MarkerState {
    InProgress,
    Complete,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Marker {
    format: String,
    token: String,
    state: MarkerState,
    created_at_ms: u64,
}

/// The setup marker of an environment folder, if it has a readable one.
pub(crate) fn read_marker(env_dir: &Path) -> Option<Marker> {
    let bytes = std::fs::read(env_dir.join(MARKER_FILE)).ok()?;
    let marker: Marker = serde_json::from_slice(&bytes).ok()?;
    (marker.format == MARKER_FORMAT).then_some(marker)
}

/// An environment folder whose setup by Exposition did not finish. Discovery
/// never runs its interpreter and never reports it ready.
pub fn setup_unfinished(env_dir: &Path) -> bool {
    env_dir.join(MARKER_FILE).exists() && read_marker(env_dir).is_none_or(|m| m.state != MarkerState::Complete)
}

fn write_marker(env_dir: &Path, token: &str, state: MarkerState, created_at_ms: u64) -> std::io::Result<()> {
    let marker = Marker { format: MARKER_FORMAT.into(), token: token.into(), state, created_at_ms };
    let bytes = serde_json::to_vec_pretty(&marker).map_err(std::io::Error::other)?;
    write_atomic(&env_dir.join(MARKER_FILE), &bytes)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let partial = path.with_extension(format!("partial-{}", std::process::id()));
    std::fs::write(&partial, bytes)?;
    std::fs::rename(&partial, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&partial);
    })
}

/// A link, junction, or other reparse point (never followed or removed by setup).
fn is_link(path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return false };
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

/// A Python virtual environment (it has `pyvenv.cfg`).
fn is_virtual_environment(dir: &Path) -> bool {
    dir.join("pyvenv.cfg").is_file()
}

fn path_key(path: &str) -> String {
    let text = display_path(Path::new(path));
    if cfg!(windows) { text.to_lowercase() } else { text }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UnfinishedRecord {
    target: String,
    token: String,
    created_at_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Records {
    format: String,
    #[serde(default)]
    unfinished: Vec<UnfinishedRecord>,
}

/// App-local records of environments a setup created and has not finished
/// (`<data>/research/setup-records.json`). A project cannot plant one, so an
/// unfinished environment is removed only when its marker matches a record.
pub(crate) struct RecordStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl RecordStore {
    pub(crate) fn new(data_dir: &Path) -> Self {
        Self { path: data_dir.join("research").join(RECORDS_FILE), lock: Mutex::new(()) }
    }

    fn guard(&self) -> MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn read(&self) -> Records {
        std::fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Records>(&bytes).ok())
            .filter(|r| r.format == RECORDS_FORMAT)
            .unwrap_or_default()
    }

    fn write(&self, records: &Records) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let records = Records { format: RECORDS_FORMAT.into(), unfinished: records.unfinished.clone() };
        write_atomic(&self.path, &serde_json::to_vec_pretty(&records).map_err(std::io::Error::other)?)
    }

    fn add(&self, target: &str, token: &str) -> std::io::Result<()> {
        let _guard = self.guard();
        let mut records = self.read();
        records.unfinished.retain(|r| path_key(&r.target) != path_key(target));
        records.unfinished.push(UnfinishedRecord {
            target: target.into(),
            token: token.into(),
            created_at_ms: now_ms(),
        });
        self.write(&records)
    }

    fn remove(&self, token: &str) -> std::io::Result<()> {
        let _guard = self.guard();
        let mut records = self.read();
        let before = records.unfinished.len();
        records.unfinished.retain(|r| r.token != token);
        if records.unfinished.len() == before {
            return Ok(());
        }
        self.write(&records)
    }

    /// The token recorded for an unfinished environment at `target`.
    fn token_for(&self, target: &str) -> Option<String> {
        let _guard = self.guard();
        self.read().unfinished.into_iter().find(|r| path_key(&r.target) == path_key(target)).map(|r| r.token)
    }
}

/// Remove an environment folder only when its marker carries `token` and it
/// is a real folder (never a link). Returns why it was not removed.
fn remove_owned(target: &Path, token: &str) -> Result<(), String> {
    if !target.exists() {
        return Ok(());
    }
    if is_link(target) {
        return Err("it is a link, which setup never removes".into());
    }
    match read_marker(target) {
        Some(marker) if marker.token == token => {}
        _ => return Err("its setup marker does not match this setup, so it was left in place".into()),
    }
    // A just-killed process can hold files for a moment on Windows: retry briefly.
    let mut last = None;
    for attempt in 0..6 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(500));
        }
        match std::fs::remove_dir_all(target) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => last = Some(e),
        }
    }
    Err(last.map(|e| e.to_string()).unwrap_or_default())
}

// ==========================================================================
// Planning

fn venv_python(env_dir: &Path) -> PathBuf {
    if cfg!(windows) { env_dir.join("Scripts").join("python.exe") } else { env_dir.join("bin").join("python") }
}

/// Discovery's id of the interpreter of a project environment folder.
pub(crate) fn environment_id_for(env_dir: &Path) -> String {
    format!("python:{}", venv_python(env_dir).display())
}

fn pip_args(packages: &[String]) -> Vec<String> {
    let mut args: Vec<String> = [
        "-I",
        "-m",
        "pip",
        "--isolated",
        "install",
        "--disable-pip-version-check",
        "--no-input",
        "--require-virtualenv",
        "--only-binary=:all:",
        "--index-url",
        PYPI_INDEX,
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    args.extend(packages.iter().cloned());
    args
}

fn list_text(packages: &[String]) -> String {
    match packages.len() {
        0 => String::new(),
        1 => packages[0].clone(),
        n => format!("{} and {}", packages[..n - 1].join(", "), packages[n - 1]),
    }
}

fn plan_hash(plan: &SetupPlan) -> String {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Hashed<'a> {
        schema: &'a str,
        root: String,
        target: String,
        creates_environment: bool,
        removes_unfinished: bool,
        steps: &'a [SetupStep],
    }
    let hashed = Hashed {
        schema: "exposition.setupPlan/1",
        root: path_key(&plan.root),
        target: path_key(&plan.target),
        creates_environment: plan.creates_environment,
        removes_unfinished: plan.removes_unfinished,
        steps: &plan.steps,
    };
    sha256_hex(&serde_json::to_vec(&hashed).unwrap_or_default())
}

/// What setup knows about the open project when it plans.
pub(crate) struct PlanContext<'a> {
    /// Canonical project root.
    pub root: &'a Path,
    pub trusted: bool,
    pub report: &'a EnvironmentReport,
    pub records: &'a RecordStore,
}

fn refused(code: &str, message: impl Into<String>) -> ResearchError {
    ResearchError::coded(code, message)
}

/// Build the plan for `request` (runs nothing).
pub(crate) fn build_plan(context: &PlanContext<'_>, request: &SetupRequest) -> Result<SetupPlan, ResearchError> {
    if !context.trusted {
        return Err(ResearchError::NotTrusted);
    }
    let root = context.root;
    let root_text = display_path(root);
    let find = |id: &str| context.report.runtimes.iter().find(|r| r.id == id);
    match request {
        SetupRequest::CreateEnvironment { base_environment, packages } => {
            let packages = install_list(packages)?;
            let base =
                find(base_environment).ok_or_else(|| ResearchError::UnknownEnvironment(base_environment.clone()))?;
            let executable = match (&base.language, &base.executable) {
                (RuntimeLanguage::Python, Some(exe)) => PathBuf::from(exe),
                _ => {
                    return Err(refused(
                        "setup.unsupported",
                        format!(
                            "{} cannot be the base of a project environment; choose a discovered Python.",
                            base.display_name
                        ),
                    ));
                }
            };
            let in_project = |p: &Path| {
                let canonical = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
                canonical.starts_with(root) || p.starts_with(root) || p.starts_with(&root_text)
            };
            if in_project(&executable) || base.host_interpreter.as_deref().is_some_and(|h| in_project(Path::new(h))) {
                return Err(refused(
                    "setup.unsupported",
                    "A project environment is created from a Python installed on this computer, not from one inside the project.",
                ));
            }
            let target = root.join(ENVIRONMENT_FOLDER);
            let target_text = display_path(&target);
            let mut removes_unfinished = false;
            if std::fs::symlink_metadata(&target).is_ok() {
                if is_link(&target) {
                    return Err(refused(
                        "setup.targetOccupied",
                        format!(
                            "{ENVIRONMENT_FOLDER} in the project is a link to another folder; setup creates environments only inside the project."
                        ),
                    ));
                }
                let unfinished = read_marker(&target).filter(|m| m.state == MarkerState::InProgress);
                match unfinished {
                    Some(marker)
                        if context.records.token_for(&target_text).as_deref() == Some(marker.token.as_str()) =>
                    {
                        removes_unfinished = true;
                    }
                    Some(_) => {
                        return Err(refused(
                            "setup.targetOccupied",
                            format!(
                                "{ENVIRONMENT_FOLDER} holds an environment whose setup did not finish, and Exposition has no record of creating it. Delete the {ENVIRONMENT_FOLDER} folder, then set up again."
                            ),
                        ));
                    }
                    None if is_virtual_environment(&target) => {
                        return Err(refused(
                            "setup.environmentExists",
                            format!(
                                "The project already has an environment in {ENVIRONMENT_FOLDER}; install packages into it instead."
                            ),
                        ));
                    }
                    None => {
                        return Err(refused(
                            "setup.targetOccupied",
                            format!(
                                "The project has a {ENVIRONMENT_FOLDER} folder that is not a Python environment; rename or remove it, then set up again."
                            ),
                        ));
                    }
                }
            }
            let python = venv_python(&target);
            let python_text = display_path(&python);
            let steps = vec![
                SetupStep {
                    label: format!("Create the environment in {ENVIRONMENT_FOLDER}"),
                    program: display_path(&executable),
                    args: vec!["-I".into(), "-m".into(), "venv".into(), target_text.clone()],
                    network: false,
                    timeout_ms: VENV_TIMEOUT_MS,
                },
                SetupStep {
                    label: "Install packages from PyPI".into(),
                    program: python_text.clone(),
                    args: pip_args(&packages),
                    network: true,
                    timeout_ms: PIP_TIMEOUT_MS,
                },
            ];
            let base_name = base.display_name.clone();
            let summary = format!(
                "{}Creates a Python environment in {ENVIRONMENT_FOLDER} from {base_name}, then installs {} from PyPI into it. Runs on this computer with your account's privileges; if setup fails or is canceled, the new environment is removed.",
                if removes_unfinished {
                    format!("Removes the unfinished environment an interrupted setup left in {ENVIRONMENT_FOLDER}. ")
                } else {
                    String::new()
                },
                list_text(&packages),
            );
            let mut plan = SetupPlan {
                request: request.clone(),
                root: root_text,
                target: target_text,
                target_name: ENVIRONMENT_FOLDER.into(),
                creates_environment: true,
                removes_unfinished,
                base_interpreter: Some(display_path(&executable)),
                base_display_name: Some(base_name),
                interpreter: python_text,
                packages,
                index_url: PYPI_INDEX.into(),
                network: true,
                steps,
                summary,
                plan_hash: String::new(),
            };
            plan.plan_hash = plan_hash(&plan);
            Ok(plan)
        }
        SetupRequest::InstallPackages { environment, packages } => {
            let packages = install_list(packages)?;
            let env = find(environment).ok_or_else(|| ResearchError::UnknownEnvironment(environment.clone()))?;
            let folder = PROJECT_ENVIRONMENT_FOLDERS
                .iter()
                .find(|name| environment_id_for(&root.join(name)) == env.id)
                .ok_or_else(|| {
                    refused(
                        "setup.unsupported",
                        format!(
                            "{} is not a project environment. Packages are installed only into the project's .venv or venv, never into a Python installed for the whole computer.",
                            env.display_name
                        ),
                    )
                })?;
            let target = root.join(folder);
            if is_link(&target) {
                return Err(refused(
                    "setup.targetOccupied",
                    format!(
                        "{folder} in the project is a link to another folder; setup installs only into an environment inside the project."
                    ),
                ));
            }
            if setup_unfinished(&target) {
                return Err(refused(
                    "setup.unfinished",
                    format!("The setup of {folder} did not finish; set up the environment again instead."),
                ));
            }
            if !is_virtual_environment(&target) {
                return Err(refused(
                    "setup.unsupported",
                    format!("{folder} is not a Python virtual environment (it has no pyvenv.cfg)."),
                ));
            }
            let python = venv_python(&target);
            let python_text = display_path(&python);
            let steps = vec![SetupStep {
                label: "Install packages from PyPI".into(),
                program: python_text.clone(),
                args: pip_args(&packages),
                network: true,
                timeout_ms: PIP_TIMEOUT_MS,
            }];
            let summary = format!(
                "Installs {} from PyPI into the project environment {folder}. Packages already there stay; nothing is removed. Runs on this computer with your account's privileges.",
                list_text(&packages)
            );
            let mut plan = SetupPlan {
                request: request.clone(),
                root: root_text,
                target: display_path(&target),
                target_name: (*folder).to_owned(),
                creates_environment: false,
                removes_unfinished: false,
                base_interpreter: None,
                base_display_name: None,
                interpreter: python_text,
                packages,
                index_url: PYPI_INDEX.into(),
                network: true,
                steps,
                summary,
                plan_hash: String::new(),
            };
            plan.plan_hash = plan_hash(&plan);
            Ok(plan)
        }
    }
}

// ==========================================================================
// Running

/// One setup run.
pub(crate) struct SetupRun {
    id: String,
    root: PathBuf,
    canceled: AtomicBool,
    status: Mutex<SetupStatus>,
    job: Mutex<Option<String>>,
    done: watch::Sender<Option<SetupStatus>>,
}

/// Setup state shared by the research service.
pub(crate) struct SetupSlot {
    events: broadcast::Sender<SetupEvent>,
    active: Mutex<Option<Arc<SetupRun>>>,
    /// The last finished setup per project root (key: [`path_key`]).
    last: Mutex<HashMap<String, SetupStatus>>,
    pub(crate) records: RecordStore,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

impl SetupSlot {
    pub(crate) fn new(data_dir: &Path) -> Self {
        let (events, _) = broadcast::channel(1024);
        Self { events, active: Mutex::new(None), last: Mutex::new(HashMap::new()), records: RecordStore::new(data_dir) }
    }

    fn emit(&self, event: SetupEvent) {
        let _ = self.events.send(event);
    }

    fn changed(&self, run: &SetupRun) {
        let status = lock(&run.status).clone();
        self.emit(SetupEvent::Changed { status: Box::new(status) });
    }

    /// Cancel the running setup unless it belongs to `keep` (a project switch or close).
    pub(crate) fn cancel_outside(&self, keep: Option<&Path>, launcher: &dyn KernelProcessLauncher) {
        let active = lock(&self.active).clone();
        if let Some(run) = active
            && keep != Some(run.root.as_path())
        {
            cancel_run(&run, launcher);
        }
    }
}

fn cancel_run(run: &SetupRun, launcher: &dyn KernelProcessLauncher) {
    run.canceled.store(true, Ordering::SeqCst);
    if let Some(job) = lock(&run.job).clone() {
        launcher.cancel_project_job(&job);
    }
}

enum Stop {
    Canceled(String),
    Failed(String),
}

fn busy(run: &SetupRun) -> ResearchError {
    let what = lock(&run.status).plan.target_name.clone();
    refused("setup.busy", format!("An environment setup ({what}) is running; wait for it or cancel it."))
}

impl ResearchService {
    /// Plan a setup for the project at `root` (it must be the open project).
    /// Runs nothing; uses the current discovery report.
    pub async fn setup_plan(&self, root: &Path, request: &SetupRequest) -> Result<SetupPlan, ResearchError> {
        let (canonical, trusted) = self.setup_project(root)?;
        if let Some(running) = lock(&self.setup_slot().active).clone() {
            return Err(busy(&running));
        }
        let report = self.environments(false).await?;
        build_plan(
            &PlanContext { root: &canonical, trusted, report: &report, records: &self.setup_slot().records },
            request,
        )
    }

    /// Start a planned setup on behalf of `owner`. The plan is built again
    /// and must match (`setup.planStale` otherwise); one setup runs at a time
    /// (`setup.busy`). Returns at once; progress arrives as [`SetupEvent`]s.
    pub async fn setup_start(&self, plan: &SetupPlan, owner: SessionOwner) -> Result<SetupStatus, ResearchError> {
        let (canonical, trusted) = self.setup_project(Path::new(&plan.root))?;
        if !trusted {
            return Err(ResearchError::NotTrusted);
        }
        let slot = self.setup_slot();
        if let Some(running) = lock(&slot.active).clone() {
            return Err(busy(&running));
        }
        let report = self.environments(false).await?;
        let fresh = build_plan(
            &PlanContext { root: &canonical, trusted, report: &report, records: &slot.records },
            &plan.request,
        )?;
        if fresh.plan_hash != plan.plan_hash || fresh != *plan {
            return Err(refused(
                "setup.planStale",
                "The project or its environments changed since this setup was planned; review the new plan.",
            ));
        }
        let run = {
            let mut active = lock(&slot.active);
            if let Some(running) = active.as_ref() {
                return Err(busy(running));
            }
            let id = format!("setup-{}", &uuid::Uuid::new_v4().simple().to_string()[..16]);
            let status = SetupStatus {
                id: id.clone(),
                owner,
                plan: fresh.clone(),
                state: SetupState::Running,
                phase: SetupPhase::Preparing,
                step: None,
                message: None,
                removed: false,
                environment_id: None,
                ready: false,
                jobs: Vec::new(),
                log: Vec::new(),
                started_at_ms: now_ms(),
                finished_at_ms: None,
            };
            let (done, _) = watch::channel(None);
            let run = Arc::new(SetupRun {
                id,
                root: canonical.clone(),
                canceled: AtomicBool::new(false),
                status: Mutex::new(status),
                job: Mutex::new(None),
                done,
            });
            *active = Some(run.clone());
            run
        };
        slot.changed(&run);
        let service = self.handle();
        let worker = run.clone();
        let spawned = std::thread::Builder::new().name(format!("research-{}", run.id)).spawn(move || {
            execute_setup(&service, &worker, &fresh);
        });
        if let Err(e) = spawned {
            *lock(&slot.active) = None;
            return Err(ResearchError::Io(format!("could not start the setup: {e}")));
        }
        Ok(lock(&run.status).clone())
    }

    /// Start a planned setup and wait until it finishes (Rust callers such as
    /// agent tools; cancel it with [`Self::setup_cancel`]).
    pub async fn setup_run(&self, plan: &SetupPlan, owner: SessionOwner) -> Result<SetupStatus, ResearchError> {
        let status = self.setup_start(plan, owner).await?;
        self.setup_wait(&status.id)
            .await
            .ok_or_else(|| ResearchError::Io("the setup ended without reporting its outcome".into()))
    }

    /// Wait for a setup to finish; its final status (`None` for an unknown id).
    pub async fn setup_wait(&self, setup_id: &str) -> Option<SetupStatus> {
        let slot = self.setup_slot();
        let active = lock(&slot.active).clone().filter(|run| run.id == setup_id);
        if let Some(run) = active {
            let mut rx = run.done.subscribe();
            return rx.wait_for(|s| s.is_some()).await.ok().and_then(|s| s.clone());
        }
        lock(&slot.last).values().find(|s| s.id == setup_id).cloned()
    }

    /// Cancel a running setup: the running step is stopped and an
    /// environment the setup created is removed. False when it is not running.
    pub fn setup_cancel(&self, setup_id: &str) -> bool {
        let slot = self.setup_slot();
        let active = lock(&slot.active).clone().filter(|run| run.id == setup_id);
        match active {
            Some(run) => {
                cancel_run(&run, self.launcher().as_ref());
                true
            }
            None => false,
        }
    }

    /// The running setup of the open project, else its last finished one.
    pub fn setup_status(&self) -> Option<SetupStatus> {
        let root = self.workspace_root()?;
        let slot = self.setup_slot();
        if let Some(run) = lock(&slot.active).clone()
            && run.root == root
        {
            return Some(lock(&run.status).clone());
        }
        lock(&slot.last).get(&path_key(&display_path(&root))).cloned()
    }

    pub fn subscribe_setup(&self) -> broadcast::Receiver<SetupEvent> {
        self.setup_slot().events.subscribe()
    }

    /// The open project's canonical root (which must be `root`) and its trust.
    fn setup_project(&self, root: &Path) -> Result<(PathBuf, bool), ResearchError> {
        let current = self.workspace_root().ok_or(ResearchError::NoWorkspace)?;
        let requested = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        if path_key(&display_path(&requested)) != path_key(&display_path(&current)) {
            return Err(ResearchError::coded(
                "research.workspaceMismatch",
                "Setup works only in the open project; open it first.",
            ));
        }
        let trusted = self.workspace().is_some_and(|w| w.trusted);
        Ok((current, trusted))
    }
}

/// The run's own bookkeeping, then the steps, then cleanup or the marker
/// update, then discovery. Runs on its own thread.
fn execute_setup(service: &ResearchService, run: &Arc<SetupRun>, plan: &SetupPlan) {
    let slot = service.setup_slot();
    let target = PathBuf::from(&plan.target);
    // The token of an environment this run created (set only after it made the folder).
    let mut created: Option<String> = None;
    let outcome = run_steps(service, run, plan, &target, &mut created);

    let update = |f: &dyn Fn(&mut SetupStatus)| {
        f(&mut lock(&run.status));
        slot.changed(run);
    };
    let mut removal_problem: Option<String> = None;
    let mut removed = false;
    match &outcome {
        Ok(()) => {
            if let Some(token) = &created {
                let created_at = read_marker(&target).map(|m| m.created_at_ms).unwrap_or_else(now_ms);
                if let Err(e) = write_marker(&target, token, MarkerState::Complete, created_at) {
                    removal_problem = Some(format!("The setup marker could not be updated: {e}."));
                } else {
                    let _ = slot.records.remove(token);
                }
            }
        }
        Err(_) => {
            if let Some(token) = &created {
                update(&|s| s.phase = SetupPhase::CleaningUp);
                match remove_owned(&target, token) {
                    Ok(()) => {
                        removed = true;
                        let _ = slot.records.remove(token);
                    }
                    Err(reason) => {
                        removal_problem = Some(format!(
                            "The unfinished environment in {} could not be removed ({reason}); it is never offered as ready, and Set up environment removes it next time.",
                            plan.target_name
                        ));
                    }
                }
            }
        }
    }

    update(&|s| s.phase = SetupPhase::Checking);
    let report = service.rediscover_blocking().ok();
    let env_id = environment_id_for(&canonical_target(service, plan));
    let found = report.as_ref().and_then(|r| r.runtimes.iter().find(|e| e.id == env_id)).cloned();
    let ready = found.as_ref().is_some_and(|e| e.availability == Availability::Available);

    let (state, message) = match outcome {
        Ok(()) if ready && removal_problem.is_none() => (SetupState::Succeeded, None),
        Ok(()) => {
            let reason = match found.as_ref().map(|e| &e.availability) {
                Some(Availability::Unavailable { reason, .. }) => reason.clone(),
                _ => format!("discovery did not find the environment in {}", plan.target_name),
            };
            let mut message = format!("The steps finished, but the environment is not ready: {reason}");
            if let Some(problem) = &removal_problem {
                message.push(' ');
                message.push_str(problem);
            }
            (SetupState::Failed, Some(message))
        }
        Err(stop) => {
            let (state, mut message) = match stop {
                Stop::Canceled(m) => (SetupState::Canceled, m),
                Stop::Failed(m) => (SetupState::Failed, m),
            };
            if removed {
                message.push_str(&format!(" The new environment in {} was removed.", plan.target_name));
            } else if let Some(problem) = &removal_problem {
                message.push(' ');
                message.push_str(problem);
            } else if !plan.creates_environment {
                message.push_str(&format!(" Nothing was removed from {}.", plan.target_name));
            }
            (state, Some(message))
        }
    };
    let final_status = {
        let mut status = lock(&run.status);
        status.state = state;
        status.phase = SetupPhase::Finished;
        status.message = message;
        status.removed = removed;
        status.environment_id = found.map(|e| e.id);
        status.ready = ready && state == SetupState::Succeeded;
        status.finished_at_ms = Some(now_ms());
        status.clone()
    };
    lock(&slot.last).insert(path_key(&final_status.plan.root), final_status.clone());
    {
        let mut active = lock(&slot.active);
        if active.as_ref().is_some_and(|a| a.id == run.id) {
            *active = None;
        }
    }
    *lock(&run.job) = None;
    slot.emit(SetupEvent::Changed { status: Box::new(final_status.clone()) });
    let _ = run.done.send(Some(final_status));
}

/// The target as discovery names it (under the canonical project root).
fn canonical_target(service: &ResearchService, plan: &SetupPlan) -> PathBuf {
    match service.workspace_root() {
        Some(root) => root.join(&plan.target_name),
        None => PathBuf::from(&plan.target),
    }
}

fn run_steps(
    service: &ResearchService,
    run: &Arc<SetupRun>,
    plan: &SetupPlan,
    target: &Path,
    created: &mut Option<String>,
) -> Result<(), Stop> {
    let slot = service.setup_slot();
    let launcher = service.launcher();
    let canceled = || Stop::Canceled("Setup was canceled.".into());

    if plan.removes_unfinished {
        let token = slot.records.token_for(&plan.target).ok_or_else(|| {
            Stop::Failed("The record of the unfinished environment is gone; nothing was removed.".into())
        })?;
        remove_owned(target, &token)
            .map_err(|reason| Stop::Failed(format!("The unfinished environment could not be removed ({reason}).")))?;
        let _ = slot.records.remove(&token);
    }
    if run.canceled.load(Ordering::SeqCst) {
        return Err(canceled());
    }
    if plan.creates_environment {
        let token = uuid::Uuid::new_v4().simple().to_string();
        slot.records
            .add(&plan.target, &token)
            .map_err(|e| Stop::Failed(format!("The setup record could not be written: {e}.")))?;
        if let Err(e) = std::fs::create_dir(target) {
            let _ = slot.records.remove(&token);
            return Err(Stop::Failed(if e.kind() == std::io::ErrorKind::AlreadyExists {
                format!("{} appeared in the project while setup started; nothing was changed.", plan.target_name)
            } else {
                format!("{} could not be created: {e}.", plan.target_name)
            }));
        }
        *created = Some(token.clone());
        write_marker(target, &token, MarkerState::InProgress, now_ms())
            .map_err(|e| Stop::Failed(format!("The setup marker could not be written: {e}.")))?;
    }

    for (index, step) in plan.steps.iter().enumerate() {
        if run.canceled.load(Ordering::SeqCst) {
            return Err(canceled());
        }
        {
            let mut status = lock(&run.status);
            status.phase = SetupPhase::Step;
            status.step = Some(index as u32);
        }
        slot.changed(run);
        let spec = JobSpec {
            label: format!("environment setup: {}", step.label),
            program: step.program.clone(),
            args: step.args.clone(),
            environment: research_environment(BTreeMap::new()),
            working_directory: plan.root.clone(),
            policy: ExecutionPolicy::TrustedNative,
            timeout_ms: Some(step.timeout_ms),
            log_limit_bytes: Some(JOB_LOG_LIMIT),
            stdin: None,
        };
        let job = launcher
            .start_project_job(&spec)
            .map_err(|e| Stop::Failed(format!("{} could not start: {e}", step.label)))?;
        *lock(&run.job) = Some(job.clone());
        lock(&run.status).jobs.push(job.clone());
        slot.changed(run);
        if run.canceled.load(Ordering::SeqCst) {
            launcher.cancel_project_job(&job);
        }
        let mut cursor = None;
        let mut tail = LineTail::default();
        let mut cancel_sent = false;
        let end = loop {
            let progress = launcher.poll_project_job(&job, cursor);
            cursor = progress.cursor.or(cursor);
            let text: String = progress.output.concat();
            if !text.is_empty() {
                tail.push(&text);
                lock(&run.status).log = tail.lines();
                for chunk in chunks(&text, OUTPUT_EVENT_CHARS) {
                    slot.emit(SetupEvent::Output { setup_id: run.id.clone(), step: index as u32, text: chunk });
                }
            }
            if let Some(end) = progress.end {
                break end;
            }
            if run.canceled.load(Ordering::SeqCst) && !cancel_sent {
                launcher.cancel_project_job(&job);
                cancel_sent = true;
            }
            std::thread::sleep(POLL_INTERVAL);
        };
        *lock(&run.job) = None;
        match end {
            ProjectJobEnd::Exited(0) => {}
            ProjectJobEnd::Exited(code) => {
                let detail = tail.error_summary();
                return Err(Stop::Failed(format!(
                    "{} failed (exit code {code}){}",
                    step.label,
                    if detail.is_empty() { ".".to_owned() } else { format!(": {detail}") }
                )));
            }
            ProjectJobEnd::Canceled => {
                if run.canceled.load(Ordering::SeqCst) {
                    return Err(canceled());
                }
                let untrusted = launcher.workspace_trusted(&run.root) == Some(false);
                return Err(Stop::Canceled(if untrusted {
                    "Workspace trust was revoked; setup was stopped.".into()
                } else {
                    format!("{} was stopped.", step.label)
                }));
            }
            ProjectJobEnd::TimedOut => {
                return Err(Stop::Failed(format!(
                    "{} did not finish within {} minutes.",
                    step.label,
                    step.timeout_ms / 60_000
                )));
            }
            ProjectJobEnd::NotStarted { message, untrusted } => {
                return Err(Stop::Failed(if untrusted {
                    "The project folder is not trusted; trust it, then set up again.".into()
                } else {
                    format!("{} could not start: {message}", step.label)
                }));
            }
            ProjectJobEnd::Terminated(description) => {
                return Err(Stop::Failed(format!("{} ended abnormally: {description}", step.label)));
            }
        }
    }
    Ok(())
}

fn chunks(text: &str, size: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for c in text.chars() {
        if current.len() + c.len_utf8() > size {
            out.push(std::mem::take(&mut current));
        }
        current.push(c);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// The last lines of a step's output.
#[derive(Default)]
struct LineTail {
    lines: std::collections::VecDeque<String>,
    partial: String,
}

impl LineTail {
    fn push(&mut self, text: &str) {
        self.partial.push_str(text);
        while let Some(pos) = self.partial.find('\n') {
            let line: String = self.partial[..pos].trim_end_matches('\r').to_owned();
            self.partial.drain(..=pos);
            self.add(line);
        }
        if self.partial.len() > LOG_LINE_CHARS * 4 {
            let line = std::mem::take(&mut self.partial);
            self.add(line);
        }
    }

    fn add(&mut self, line: String) {
        let line: String = line.chars().take(LOG_LINE_CHARS).collect();
        self.lines.push_back(line);
        while self.lines.len() > LOG_TAIL_LINES {
            self.lines.pop_front();
        }
    }

    fn lines(&self) -> Vec<String> {
        let mut out: Vec<String> = self.lines.iter().cloned().collect();
        if !self.partial.trim().is_empty() {
            out.push(self.partial.chars().take(LOG_LINE_CHARS).collect());
        }
        out
    }

    /// pip's `ERROR:` lines, else the last non-empty lines.
    fn error_summary(&self) -> String {
        let all = self.lines();
        let errors: Vec<&String> = all.iter().filter(|l| l.trim_start().starts_with("ERROR:")).collect();
        let picked: Vec<&String> =
            if errors.is_empty() { all.iter().filter(|l| !l.trim().is_empty()).collect() } else { errors };
        let start = picked.len().saturating_sub(3);
        picked[start..].iter().map(|l| l.trim()).collect::<Vec<_>>().join(" ")
    }
}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod tests;
