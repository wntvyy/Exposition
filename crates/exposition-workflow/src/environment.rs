//! The execution environment of one step attempt (M9: stronger
//! reproducibility). A recorded input hash alone does not establish
//! reproducibility (spec §8), so every attempt records what it ran with:
//! runner and containment, platform, application version, the restricted
//! image (name and identity), interpreter and its packages, the Lean
//! toolchain with the Lake manifest digest, the model of an agent step, and
//! the seed a batch step was given.
//!
//! Capture never runs an unapproved command. Sources:
//! - restricted batches: the approved image identity, and the package list
//!   listed inside exactly that image as part of the approved batch (cached per
//!   identity; see `exposition_research::sandbox::image_env`);
//! - native batches: the research discovery report already collected for the
//!   interpreter that ran (only the packages that report checks);
//! - Lean audits: the audit record's toolchain and `lean --version`, the
//!   `lean-toolchain` pin, and the `lake-manifest.json` digest and dependency
//!   revisions (files read, nothing run);
//! - agent reviews: the task's provider, configured model, and the model
//!   id the provider reported serving (recorded; only the configured model is
//!   compared for drift).
//!
//! Nothing here reads environment variables or records absolute paths, so
//! secrets and user folders never reach an environment (or a manifest).

use std::collections::BTreeMap;

use exposition_graph::WorkflowRunId;
use exposition_graph::workflow::registry::Containment;
use exposition_research::batch::environment_keys as keys;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::adapters::sha256_hex;
use crate::model::{WorkflowPlanStep, WorkflowRun, WorkflowRunCause, WorkflowRunStep};

/// The application version that runs steps.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
/// At most this many packages are kept per environment (the digest covers
/// every captured package).
pub const MAX_PACKAGES: usize = 1000;
/// At most this many notes are kept per environment.
const MAX_NOTES: usize = 16;
const MAX_TEXT: usize = 300;
/// Environment variable carrying a batch step's seed.
pub const SEED_VAR: &str = exposition_research::batch::SEED_VAR;

/// A container image by name and identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowEnvironmentImage {
    /// The configured reference, e.g. `python:3.13-slim`.
    pub name: String,
    /// The engine's identity (`sha256:…`) the step ran in.
    pub id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowEnvironmentInterpreter {
    /// `python`, `R`, `julia`.
    pub name: String,
    pub version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowEnvironmentPackage {
    pub name: String,
    /// Version, or the exact revision of a Lake dependency.
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowEnvironmentToolchain {
    /// E.g. `leanprover/lean4:v4.35.0`.
    pub name: String,
    /// E.g. `4.35.0`.
    pub version: Option<String>,
    /// SHA-256 (hex) of the Lake project's `lake-manifest.json`, if present.
    pub manifest_digest: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowEnvironmentModel {
    /// `anthropic`, `scripted`.
    pub provider: String,
    /// The configured model the step asked for (what drift compares).
    pub model: String,
    /// The model id the provider reported serving, when it said (may be a
    /// dated id or a fallback; recorded, never treated as drift).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub served_by: Option<String>,
}

/// What one attempt ran with (see the module docs).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowEnvironment {
    /// Runner id (`restrictedPython`, `nativePython`, `leanAudit`, …).
    pub runner: String,
    pub containment: Containment,
    /// Platform the step's code ran on (the image's for restricted steps).
    pub os: String,
    pub arch: String,
    /// The Exposition version that ran the step.
    pub app_version: String,
    #[serde(default)]
    pub image: Option<WorkflowEnvironmentImage>,
    #[serde(default)]
    pub interpreter: Option<WorkflowEnvironmentInterpreter>,
    /// Sorted by name, at most [`MAX_PACKAGES`] (a note says when more were captured).
    #[serde(default)]
    pub packages: Vec<WorkflowEnvironmentPackage>,
    /// SHA-256 over every captured package (`name==version` lines, sorted).
    #[serde(default)]
    pub packages_digest: Option<String>,
    #[serde(default)]
    pub toolchain: Option<WorkflowEnvironmentToolchain>,
    #[serde(default)]
    pub model: Option<WorkflowEnvironmentModel>,
    /// [`SEED_VAR`] as given to the step (always below 2^32).
    #[serde(default)]
    pub seed: Option<u64>,
    /// Where parts came from, and what is unknown and why.
    #[serde(default)]
    pub notes: Vec<String>,
}

impl WorkflowEnvironment {
    /// Runner, containment, host platform, and application version.
    pub fn base(runner: &str, containment: Containment) -> Self {
        Self {
            runner: runner.to_owned(),
            containment,
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            app_version: APP_VERSION.to_owned(),
            image: None,
            interpreter: None,
            packages: Vec::new(),
            packages_digest: None,
            toolchain: None,
            model: None,
            seed: None,
            notes: Vec::new(),
        }
    }

    /// The base environment of a planned step.
    pub fn of_step(step: &WorkflowPlanStep) -> Self {
        Self::base(&step.runner, step.containment)
    }

    /// Add a note (absolute paths are replaced: an environment is portable
    /// and never records user folders).
    pub fn note(&mut self, text: impl AsRef<str>) {
        let text = crate::manifest::scrub_absolute_paths(text.as_ref());
        let text: String = text.chars().filter(|c| !c.is_control()).take(MAX_TEXT).collect();
        if !text.is_empty() && self.notes.len() < MAX_NOTES && !self.notes.contains(&text) {
            self.notes.push(text);
        }
    }

    /// Record `packages` (sorted, digested, bounded).
    pub fn set_packages(&mut self, packages: impl IntoIterator<Item = (String, String)>) {
        let mut all: Vec<WorkflowEnvironmentPackage> = packages
            .into_iter()
            .map(|(name, version)| WorkflowEnvironmentPackage {
                name: name.chars().filter(|c| !c.is_control()).take(MAX_TEXT).collect(),
                version: version.chars().filter(|c| !c.is_control()).take(MAX_TEXT).collect(),
            })
            .filter(|p| !p.name.is_empty())
            .collect();
        all.sort_by(|a, b| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()).then_with(|| a.cmp(b)));
        all.dedup();
        if all.is_empty() {
            self.packages.clear();
            self.packages_digest = None;
            return;
        }
        self.packages_digest = Some(packages_digest(&all));
        if all.len() > MAX_PACKAGES {
            self.note(format!(
                "{} packages were captured; the first {MAX_PACKAGES} are listed (the digest covers all).",
                all.len()
            ));
            all.truncate(MAX_PACKAGES);
        }
        self.packages = all;
    }
}

/// SHA-256 (hex) over `name==version\n` lines in the given (sorted) order.
pub fn packages_digest(packages: &[WorkflowEnvironmentPackage]) -> String {
    let text: String = packages.iter().map(|p| format!("{}=={}\n", p.name, p.version)).collect();
    sha256_hex(text.as_bytes())
}

/// The seed a batch step receives: derived from the run's seed basis, the
/// step, and the instance index (deterministic; below 2^32 so every common
/// random generator accepts it, e.g. `numpy.random.seed`).
pub fn step_seed(basis: &str, step: &str, instance: u32) -> u64 {
    let digest = sha256_hex(format!("exposition.workflowSeed/1\n{basis}\n{step}\n{instance}").as_bytes());
    u64::from_str_radix(&digest[..8], 16).unwrap_or(0)
}

/// Bound on how far a chain of reproductions is followed.
const MAX_REPRODUCTION_CHAIN: usize = 32;

/// The run whose id seeds `run`'s steps: the run itself, or — for a
/// reproduction — the original it reproduces (following reproductions of
/// reproductions), so a reproduction gives every step the seed the original
/// step received. `lookup` reads a stored run.
pub fn seed_basis(run: &WorkflowRun, lookup: &dyn Fn(&WorkflowRunId) -> Option<WorkflowRun>) -> String {
    let mut basis = run.id.clone();
    let mut cause = run.cause.clone();
    for _ in 0..MAX_REPRODUCTION_CHAIN {
        let WorkflowRunCause::Reproduce { of } = cause else { break };
        basis = of.clone();
        match lookup(&of) {
            Some(original) => cause = original.cause,
            None => break,
        }
    }
    basis.to_string()
}

/// Apply what a batch runner reported (`exposition_research::batch::environment_keys`).
pub fn apply_batch_pairs(env: &mut WorkflowEnvironment, pairs: &[(String, String)]) {
    let get =
        |key: &str| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.trim().to_owned()).filter(|v| !v.is_empty());
    if let Some(os) = get(keys::OS) {
        env.os = os;
    }
    if let Some(arch) = get(keys::ARCH) {
        env.arch = arch;
    }
    if let Some(name) = get(keys::IMAGE) {
        env.image = Some(WorkflowEnvironmentImage { name, id: get(keys::IMAGE_ID) });
    }
    if let Some(version) = get(keys::PYTHON_VERSION) {
        env.interpreter = Some(WorkflowEnvironmentInterpreter { name: "python".into(), version: Some(version) });
        if let Some(implementation) = get(keys::IMPLEMENTATION).filter(|i| i != "cpython") {
            env.note(format!("Python implementation: {implementation}"));
        }
    } else if let Some(name) = get(keys::INTERPRETER_NAME) {
        // Any batch language (R, Julia): its name, and its version when known.
        env.interpreter = Some(WorkflowEnvironmentInterpreter { name, version: get(keys::INTERPRETER_VERSION) });
    }
    let packages: Vec<(String, String)> = pairs
        .iter()
        .filter(|(k, _)| k == keys::PACKAGE)
        .filter_map(|(_, v)| v.split_once("==").map(|(n, v)| (n.trim().to_owned(), v.trim().to_owned())))
        .collect();
    if !packages.is_empty() {
        env.set_packages(packages);
        if let Some(source) = get(keys::PACKAGES_SOURCE) {
            env.note(format!("Packages: {source}."));
        }
        if let Some(limit) = get(keys::PACKAGES_TRUNCATED) {
            env.note(format!("The runner listed at most {limit} packages; more are installed."));
        }
    }
    if let Some(seed) = get(keys::SEED).and_then(|s| s.parse::<u64>().ok()) {
        env.seed = Some(seed);
    }
    for (_, note) in pairs.iter().filter(|(k, _)| k == keys::NOTE) {
        env.note(note);
    }
}

/// `4.35.0` from `Lean (version 4.35.0, x86_64-…)` or from the toolchain
/// name `leanprover/lean4:v4.35.0`.
pub fn lean_version(toolchain: &str, lean_version_output: Option<&str>) -> Option<String> {
    let numeric = |s: &str| -> Option<String> {
        let v: String = s.chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-')).collect();
        (v.chars().next().is_some_and(|c| c.is_ascii_digit())).then_some(v)
    };
    if let Some(out) = lean_version_output
        && let Some(rest) = out.split("version ").nth(1)
        && let Some(v) = numeric(rest)
    {
        return Some(v);
    }
    toolchain.rsplit_once(':').and_then(|(_, tag)| numeric(tag.trim_start_matches('v')))
}

/// Apply a Lean toolchain (`name`, `lean --version` output) and what the
/// files around the audited file say.
pub fn apply_lean(
    env: &mut WorkflowEnvironment,
    toolchain: Option<(&str, Option<&str>)>,
    provenance: &exposition_lean::provenance::LeanProvenance,
) {
    let name = toolchain.map(|(n, _)| n.to_owned()).or_else(|| provenance.toolchain_pin.clone());
    if let Some(name) = name {
        let version = lean_version(&name, toolchain.and_then(|(_, v)| v));
        env.toolchain =
            Some(WorkflowEnvironmentToolchain { name, version, manifest_digest: provenance.manifest_digest.clone() });
    } else if let Some(error) = &provenance.pin_error {
        env.note(format!("Lean toolchain pin unreadable: {error}"));
    }
    if provenance.lake_project && provenance.manifest_digest.is_none() && provenance.manifest_error.is_none() {
        env.note("The Lake project has no lake-manifest.json.");
    }
    if let Some(error) = &provenance.manifest_error {
        env.note(format!("lake-manifest.json: {error}"));
    }
    if !provenance.packages.is_empty() {
        env.set_packages(
            provenance
                .packages
                .iter()
                .map(|p| (p.name.clone(), p.rev.clone().unwrap_or_else(|| format!("({})", p.kind)))),
        );
        env.note("Packages: Lake dependencies at the revisions lake-manifest.json pins.");
    }
    if let Some(rev) = provenance.mathlib_rev() {
        env.note(format!("mathlib revision {rev}"));
    }
}

/// The environment recorded by `attempts`: the latest attempt that has one
/// (a succeeded attempt first).
pub fn recorded_in(attempts: &[crate::model::WorkflowStepAttempt]) -> Option<&WorkflowEnvironment> {
    use crate::model::WorkflowAttemptState;
    attempts
        .iter()
        .rev()
        .filter(|a| a.state == WorkflowAttemptState::Succeeded)
        .find_map(|a| a.environment.as_ref())
        .or_else(|| attempts.iter().rev().find_map(|a| a.environment.as_ref()))
}

/// The environment recorded for a (non-repeated) step.
pub fn recorded(step: &WorkflowRunStep) -> Option<&WorkflowEnvironment> {
    recorded_in(&step.attempts)
}

/// Every environment a step recorded: its own (instance `None`), or one per
/// instance of a repeated step (`Some(index)`).
pub fn recorded_all(step: &WorkflowRunStep) -> Vec<(Option<u32>, &WorkflowEnvironment)> {
    let mut out: Vec<(Option<u32>, &WorkflowEnvironment)> = recorded(step).map(|e| (None, e)).into_iter().collect();
    out.extend(step.instances.iter().filter_map(|i| recorded_in(&i.attempts).map(|e| (Some(i.index), e))));
    out
}

fn short(id: &str) -> String {
    let bare = id.strip_prefix("sha256:").unwrap_or(id);
    bare.chars().take(12).collect()
}

fn image_text(image: Option<&WorkflowEnvironmentImage>) -> String {
    match image {
        Some(WorkflowEnvironmentImage { name, id: Some(id) }) => format!("{name} ({})", short(id)),
        Some(WorkflowEnvironmentImage { name, id: None }) => format!("{name} (identity unknown)"),
        None => "no image".to_owned(),
    }
}

/// At most this many package changes are named in one notice.
const MAX_NAMED_CHANGES: usize = 6;

/// What changed between two package lists (added, removed, changed).
fn package_changes(before: &[WorkflowEnvironmentPackage], after: &[WorkflowEnvironmentPackage]) -> String {
    let index = |list: &[WorkflowEnvironmentPackage]| -> BTreeMap<String, String> {
        list.iter().map(|p| (p.name.to_ascii_lowercase(), p.version.clone())).collect()
    };
    let (b, a) = (index(before), index(after));
    let mut changed = Vec::new();
    let mut added = Vec::new();
    let mut removed = Vec::new();
    for (name, version) in &a {
        match b.get(name) {
            None => added.push(format!("{name} {version}")),
            Some(old) if old != version => changed.push(format!("{name} {old} → {version}")),
            Some(_) => {}
        }
    }
    for (name, version) in &b {
        if !a.contains_key(name) {
            removed.push(format!("{name} {version}"));
        }
    }
    let mut parts = Vec::new();
    let mut named = 0;
    for (label, list) in [("changed", &changed), ("added", &added), ("removed", &removed)] {
        if list.is_empty() {
            continue;
        }
        let shown: Vec<&String> = list.iter().take(MAX_NAMED_CHANGES.saturating_sub(named)).collect();
        named += shown.len();
        let more = list.len() - shown.len();
        let mut text = format!("{} {label}", list.len());
        if !shown.is_empty() {
            text.push_str(&format!(
                " ({}{})",
                shown.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "),
                if more > 0 { format!(", {more} more") } else { String::new() }
            ));
        }
        parts.push(text);
    }
    if parts.is_empty() { "the lists differ".to_owned() } else { parts.join("; ") }
}

/// One difference between the environment a step recorded and the one it
/// would get now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowEnvironmentDrift {
    /// The plan step it concerns (`None`: the run as a whole).
    pub step: Option<String>,
    /// `image`, `platform`, `interpreter`, `toolchain`, `lakeManifest`,
    /// `packages`, `model`, `appVersion`, or `unrecorded`.
    pub aspect: String,
    /// The recorded value (`None`: none was recorded).
    pub recorded: Option<String>,
    /// The value now (`None`: unknown without running anything, or absent).
    pub current: Option<String>,
    /// One sentence for display.
    pub message: String,
}

fn item(aspect: &str, recorded: Option<String>, current: Option<String>, message: String) -> WorkflowEnvironmentDrift {
    WorkflowEnvironmentDrift { step: None, aspect: aspect.to_owned(), recorded, current, message }
}

fn image_value(image: Option<&WorkflowEnvironmentImage>) -> Option<String> {
    image.map(|i| match &i.id {
        Some(id) => format!("{}@{id}", i.name),
        None => i.name.clone(),
    })
}

fn packages_value(env: &WorkflowEnvironment) -> Option<String> {
    env.packages_digest.as_ref().map(|d| format!("{} packages, sha256 {}", env.packages.len(), short(d)))
}

/// How the environment a step would get now differs from the one recorded
/// (empty: no drift in what can be compared). `current` is assembled without
/// running anything, so parts of it may be unknown (no package list for an
/// image identity never listed); those are said, not guessed.
pub fn drift(recorded: &WorkflowEnvironment, current: &WorkflowEnvironment) -> Vec<WorkflowEnvironmentDrift> {
    let mut out = Vec::new();
    if recorded.image != current.image && (recorded.image.is_some() || current.image.is_some()) {
        let same_name = recorded.image.as_ref().map(|i| &i.name) == current.image.as_ref().map(|i| &i.name);
        out.push(item(
            "image",
            image_value(recorded.image.as_ref()),
            image_value(current.image.as_ref()),
            format!(
                "image {} → {}{}",
                image_text(recorded.image.as_ref()),
                image_text(current.image.as_ref()),
                if same_name { " (same name, different image)" } else { "" }
            ),
        ));
    }
    let known = |s: &str| s != "unknown";
    if (recorded.os != current.os || recorded.arch != current.arch) && known(&current.os) {
        out.push(item(
            "platform",
            Some(format!("{}/{}", recorded.os, recorded.arch)),
            Some(format!("{}/{}", current.os, current.arch)),
            format!("platform {}/{} → {}/{}", recorded.os, recorded.arch, current.os, current.arch),
        ));
    }
    let interpreter =
        |i: &WorkflowEnvironmentInterpreter| format!("{} {}", i.name, i.version.as_deref().unwrap_or("?"));
    match (&recorded.interpreter, &current.interpreter) {
        (Some(r), Some(c)) if r != c => out.push(item(
            "interpreter",
            Some(interpreter(r)),
            Some(interpreter(c)),
            format!("{} → {}", interpreter(r), interpreter(c)),
        )),
        (Some(r), None) => out.push(item(
            "interpreter",
            Some(interpreter(r)),
            None,
            format!("{}: the current interpreter is not known without running it", interpreter(r)),
        )),
        _ => {}
    }
    let toolchain = |t: &WorkflowEnvironmentToolchain| match &t.version {
        Some(v) => format!("{} ({v})", t.name),
        None => t.name.clone(),
    };
    match (&recorded.toolchain, &current.toolchain) {
        (Some(r), Some(c)) => {
            if r.name != c.name {
                out.push(item(
                    "toolchain",
                    Some(toolchain(r)),
                    Some(toolchain(c)),
                    format!("toolchain {} → {}", toolchain(r), toolchain(c)),
                ));
            }
            if r.manifest_digest != c.manifest_digest {
                let text = |d: &Option<String>| d.as_deref().map(short).unwrap_or_else(|| "absent".into());
                out.push(item(
                    "lakeManifest",
                    r.manifest_digest.clone(),
                    c.manifest_digest.clone(),
                    format!("lake-manifest.json {} → {}", text(&r.manifest_digest), text(&c.manifest_digest)),
                ));
            }
        }
        (Some(r), None) => {
            out.push(item("toolchain", Some(toolchain(r)), None, format!("toolchain {} → none found", toolchain(r))))
        }
        (None, Some(c)) => {
            out.push(item("toolchain", None, Some(toolchain(c)), format!("toolchain none recorded → {}", toolchain(c))))
        }
        (None, None) => {}
    }
    match (&recorded.packages_digest, &current.packages_digest) {
        (Some(r), Some(c)) if r != c => out.push(item(
            "packages",
            packages_value(recorded),
            packages_value(current),
            format!("packages {}", package_changes(&recorded.packages, &current.packages)),
        )),
        (Some(r), None) => out.push(item(
            "packages",
            packages_value(recorded),
            None,
            format!(
                "packages: the current list is not known until the step runs (recorded {} packages, sha256 {})",
                recorded.packages.len(),
                short(r)
            ),
        )),
        _ => {}
    }
    // The configured model is compared; the id a provider reports serving
    // (a dated id, a fallback) is recorded but is not drift.
    let model = |m: &WorkflowEnvironmentModel| format!("{}/{}", m.provider, m.model);
    match (&recorded.model, &current.model) {
        (Some(r), Some(c)) if (&r.provider, &r.model) != (&c.provider, &c.model) => {
            out.push(item("model", Some(model(r)), Some(model(c)), format!("model {} → {}", model(r), model(c))))
        }
        (Some(r), None) => out.push(item("model", Some(model(r)), None, format!("model {} → unknown", model(r)))),
        _ => {}
    }
    if recorded.app_version != current.app_version {
        out.push(item(
            "appVersion",
            Some(recorded.app_version.clone()),
            Some(current.app_version.clone()),
            format!("Exposition {} → {}", recorded.app_version, current.app_version),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkgs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter().map(|(n, v)| ((*n).to_owned(), (*v).to_owned())).collect()
    }

    #[test]
    fn seeds_are_deterministic_bounded_and_distinct_per_step_and_instance() {
        let a = step_seed("wrun-1", "fit", 0);
        assert_eq!(a, step_seed("wrun-1", "fit", 0));
        assert!(a < (1u64 << 32));
        assert_ne!(a, step_seed("wrun-1", "fit", 1));
        assert_ne!(a, step_seed("wrun-1", "plot", 0));
        assert_ne!(a, step_seed("wrun-2", "fit", 0));
    }

    #[test]
    fn packages_are_sorted_digested_and_bounded() {
        let mut env = WorkflowEnvironment::base("restrictedPython", Containment::RestrictedContainer);
        env.set_packages(pkgs(&[("pip", "25.0"), ("Numpy", "2.3.1"), ("attrs", "24.1")]));
        let names: Vec<&str> = env.packages.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["attrs", "Numpy", "pip"]);
        let digest = env.packages_digest.clone().unwrap();
        let mut again = WorkflowEnvironment::base("x", Containment::NativeAccount);
        again.set_packages(pkgs(&[("attrs", "24.1"), ("pip", "25.0"), ("Numpy", "2.3.1")]));
        assert_eq!(again.packages_digest.as_deref(), Some(digest.as_str()), "order-independent");

        let many: Vec<(String, String)> = (0..1500).map(|i| (format!("p{i:04}"), "1".to_owned())).collect();
        env.set_packages(many);
        assert_eq!(env.packages.len(), MAX_PACKAGES);
        assert!(env.notes.iter().any(|n| n.contains("1500 packages")), "{:?}", env.notes);
    }

    #[test]
    fn batch_pairs_become_image_interpreter_packages_and_seed() {
        let pairs: Vec<(String, String)> = [
            ("mode", "restricted container"),
            ("image", "python:3.13-slim"),
            ("imageId", "sha256:abc"),
            ("interpreter", "C:\\Users\\someone\\python.exe"),
            ("os", "linux"),
            ("arch", "x86_64"),
            ("implementation", "cpython"),
            ("pythonVersion", "3.13.9"),
            ("packagesSource", "every installed distribution"),
            ("package", "pip==25.2"),
            ("package", "numpy==2.3.1"),
            ("seed", "42"),
            ("note", "hello"),
        ]
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
        let mut env = WorkflowEnvironment::base("restrictedPython", Containment::RestrictedContainer);
        apply_batch_pairs(&mut env, &pairs);
        assert_eq!(env.os, "linux");
        assert_eq!(
            env.image,
            Some(WorkflowEnvironmentImage { name: "python:3.13-slim".into(), id: Some("sha256:abc".into()) })
        );
        assert_eq!(env.interpreter.as_ref().and_then(|i| i.version.as_deref()), Some("3.13.9"));
        assert_eq!(env.packages.len(), 2);
        assert_eq!(env.seed, Some(42));
        assert!(env.notes.iter().any(|n| n == "hello"));
        let json = serde_json::to_string(&env).unwrap();
        assert!(!json.contains("Users"), "no interpreter path is recorded: {json}");
    }

    /// R and Julia batches report their interpreter generically; Python's
    /// `pythonVersion` keeps working (and wins when both are present).
    #[test]
    fn r_and_julia_batches_record_their_interpreter() {
        let pairs = |p: &[(&str, &str)]| -> Vec<(String, String)> {
            p.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
        };
        let mut env = WorkflowEnvironment::base("nativeR", Containment::NativeAccount);
        apply_batch_pairs(&mut env, &pairs(&[("interpreterName", "R"), ("interpreterVersion", "4.6.2")]));
        assert_eq!(
            env.interpreter,
            Some(WorkflowEnvironmentInterpreter { name: "R".into(), version: Some("4.6.2".into()) })
        );
        let mut env = WorkflowEnvironment::base("restrictedJulia", Containment::RestrictedContainer);
        apply_batch_pairs(&mut env, &pairs(&[("image", "julia:1.13"), ("interpreterName", "julia")]));
        assert_eq!(env.interpreter, Some(WorkflowEnvironmentInterpreter { name: "julia".into(), version: None }));
        let mut env = WorkflowEnvironment::base("nativePython", Containment::NativeAccount);
        apply_batch_pairs(
            &mut env,
            &pairs(&[("pythonVersion", "3.14.0"), ("interpreterName", "python"), ("interpreterVersion", "3.14.0")]),
        );
        assert_eq!(
            env.interpreter,
            Some(WorkflowEnvironmentInterpreter { name: "python".into(), version: Some("3.14.0".into()) })
        );
    }

    #[test]
    fn lean_versions_come_from_the_version_output_or_the_toolchain_name() {
        assert_eq!(
            lean_version(
                "leanprover/lean4:v4.35.0",
                Some("Lean (version 4.35.0, x86_64-w64-windows-gnu, commit abc, Release)")
            )
            .as_deref(),
            Some("4.35.0")
        );
        assert_eq!(lean_version("leanprover/lean4:v4.20.0-rc2", None).as_deref(), Some("4.20.0-rc2"));
        assert_eq!(lean_version("leanprover/lean4:stable", None), None);
    }

    #[test]
    fn drift_names_image_versions_packages_and_model_changes() {
        let mut before = WorkflowEnvironment::base("restrictedPython", Containment::RestrictedContainer);
        before.image =
            Some(WorkflowEnvironmentImage { name: "py".into(), id: Some(format!("sha256:{}", "a".repeat(64))) });
        before.interpreter =
            Some(WorkflowEnvironmentInterpreter { name: "python".into(), version: Some("3.13.1".into()) });
        before.set_packages(pkgs(&[("numpy", "2.1.0"), ("pip", "25.0"), ("six", "1.16")]));
        before.model =
            Some(WorkflowEnvironmentModel { provider: "anthropic".into(), model: "m-1".into(), served_by: None });
        assert!(drift(&before, &before.clone()).is_empty(), "no drift against itself");
        let messages = |a: &WorkflowEnvironment, b: &WorkflowEnvironment| {
            drift(a, b).into_iter().map(|d| d.message).collect::<Vec<_>>().join(" | ")
        };

        let mut after = before.clone();
        after.image =
            Some(WorkflowEnvironmentImage { name: "py".into(), id: Some(format!("sha256:{}", "b".repeat(64))) });
        after.interpreter =
            Some(WorkflowEnvironmentInterpreter { name: "python".into(), version: Some("3.13.2".into()) });
        after.set_packages(pkgs(&[("numpy", "2.2.0"), ("pip", "25.0"), ("scipy", "1.15")]));
        after.model =
            Some(WorkflowEnvironmentModel { provider: "anthropic".into(), model: "m-2".into(), served_by: None });
        let d = messages(&before, &after);
        let aspects: Vec<String> = drift(&before, &after).into_iter().map(|d| d.aspect).collect();
        assert_eq!(aspects, ["image", "interpreter", "packages", "model"]);
        let image = drift(&before, &after).into_iter().next().unwrap();
        assert_eq!(image.recorded, Some(format!("py@sha256:{}", "a".repeat(64))));
        assert!(d.contains("same name, different image"), "{d}");
        assert!(d.contains("python 3.13.1 → python 3.13.2"), "{d}");
        assert!(d.contains("numpy 2.1.0 → 2.2.0") && d.contains("scipy 1.15") && d.contains("six 1.16"), "{d}");
        assert!(d.contains("model anthropic/m-1 → anthropic/m-2"), "{d}");

        let mut unknown = after.clone();
        unknown.packages.clear();
        unknown.packages_digest = None;
        let d = messages(&before, &unknown);
        assert!(d.contains("not known until the step runs"), "{d}");
    }
}

#[cfg(test)]
mod hook_tests {
    //! The runner records an environment on every dispatched attempt (the
    //! adapter's, or the step's base environment), and passes the seed basis
    //! (the original run for reproductions, following chains).

    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use exposition_core::path::WorkspacePath;
    use exposition_graph::workflow::presets::{NUMERICAL_EXPERIMENT, WorkflowPresetRequest, draft};
    use exposition_graph::workflow::registry::RESEARCH_NATIVE_PYTHON;
    use exposition_graph::workflow::{
        CodeLanguage, ExecutionCeiling, OutcomeKind, PortSource, StepOperation, StepPayload, StepPort,
        WorkflowConnection, WorkflowDefinition, WorkflowOutcome, WorkflowStep,
    };
    use exposition_graph::{
        GraphConfig, GraphService, OperationRef, Origin, SaveWorkflowRequest, SavedWorkflow, ValidationContext,
        WorkflowId,
    };

    use super::*;
    use crate::adapters::{Adapters, StepAdapter, StepControl, StepFailure, StepRequest, StepResult, runner_ids};
    use crate::model::{StartWorkflowRunRequest, WorkflowRunError};
    use crate::service::{PlanOptions, WorkflowHost, WorkflowRunConfig, WorkflowRunService};

    struct Host {
        root: PathBuf,
        graph: Arc<GraphService>,
    }

    impl WorkflowHost for Host {
        fn project_root(&self) -> Option<PathBuf> {
            Some(self.root.clone())
        }
        fn trusted(&self, _root: &Path) -> bool {
            true
        }
        fn restricted(&self) -> (Option<String>, Option<String>) {
            (None, None)
        }
        fn restricted_image_id(&self) -> Option<String> {
            None
        }
        fn workflow(
            &self,
            _root: &Path,
            id: &WorkflowId,
            revision: Option<u64>,
        ) -> Result<SavedWorkflow, WorkflowRunError> {
            self.graph.workflow(id, revision).map_err(|e| WorkflowRunError::new(&e.code, e.message))
        }
    }

    /// Reports an environment (or none) and remembers the seed basis it got.
    struct Reporting {
        report: Option<WorkflowEnvironment>,
        fail: bool,
        bases: Mutex<Vec<String>>,
    }

    impl StepAdapter for Reporting {
        fn availability(&self, _root: &Path) -> Result<(), String> {
            Ok(())
        }
        fn run(&self, request: &StepRequest<'_>, control: &StepControl<'_>) -> Result<StepResult, StepFailure> {
            self.bases.lock().unwrap().push(request.seed_basis.to_owned());
            let op = OperationRef { kind: "researchRun".into(), id: format!("op-{}", request.attempt) };
            (control.on_dispatched)(&op);
            if self.fail {
                return Err(StepFailure {
                    environment: self.report.clone(),
                    ..StepFailure::new("research.batchFailed", "failed on purpose")
                });
            }
            Ok(StepResult { operation: Some(op), environment: self.report.clone(), ..Default::default() })
        }
    }

    struct Fixture {
        _data: tempfile::TempDir,
        _project: tempfile::TempDir,
        graph: Arc<GraphService>,
        service: WorkflowRunService,
        adapter: Arc<Reporting>,
    }

    fn fixture(report: Option<WorkflowEnvironment>, fail: bool) -> Fixture {
        let data = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(project.path()).unwrap();
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::write(root.join("data/obs.csv"), "x\n1\n").unwrap();
        let graph = GraphService::new(GraphConfig { data_dir: data.path().to_path_buf() });
        assert!(graph.attach(&root).is_usable());
        let adapter = Arc::new(Reporting { report, fail, bases: Mutex::new(Vec::new()) });
        let mut adapters: Adapters = BTreeMap::new();
        adapters.insert(runner_ids::NATIVE_PYTHON.into(), adapter.clone());
        let service = WorkflowRunService::new(WorkflowRunConfig {
            database: None,
            host: Arc::new(Host { root, graph: graph.clone() }),
            adapters,
            lean: None,
            research: None,
        })
        .unwrap();
        Fixture { _data: data, _project: project, graph, service, adapter }
    }

    fn definition() -> WorkflowDefinition {
        let mut def = draft(&WorkflowPresetRequest {
            preset: NUMERICAL_EXPERIMENT.into(),
            data: Some(WorkspacePath::parse("data/obs.csv").unwrap()),
            ..Default::default()
        })
        .unwrap();
        def.steps = vec![WorkflowStep {
            id: "fit".into(),
            label: "Fit".into(),
            operation: StepOperation { id: RESEARCH_NATIVE_PYTHON.into(), version: 1 },
            parameters: BTreeMap::new(),
            payload: Some(StepPayload::Code { language: CodeLanguage::Python, source: "print(1)\n".into() }),
            composite: None,
            condition: None,
            repeat: None,
        }];
        def.connections = vec![WorkflowConnection::Data {
            from: PortSource::Input { input: "data".into() },
            to: StepPort { step: "fit".into(), port: "data".into() },
        }];
        def.ceiling.execution = ExecutionCeiling::TrustedNative;
        def.outcomes = vec![WorkflowOutcome {
            id: "o-fit".into(),
            kind: OutcomeKind::TransformedDataset,
            label: "Dataset".into(),
            from: StepPort { step: "fit".into(), port: "dataset".into() },
            optional: false,
        }];
        def
    }

    impl Fixture {
        fn save(&self) -> WorkflowId {
            self.graph
                .save_workflow(
                    &Origin::User,
                    &SaveWorkflowRequest { id: None, expected_revision: None, definition: definition() },
                    &ValidationContext::default(),
                )
                .unwrap()
                .id
        }

        fn run(&self, id: &WorkflowId, cause: WorkflowRunCause) -> WorkflowRun {
            let plan = self.service.plan_with(id, None, PlanOptions { cause, ..Default::default() }).unwrap();
            assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
            let run = self
                .service
                .start(&StartWorkflowRunRequest { plan: plan.plan, approval_hash: plan.approval_hash })
                .unwrap();
            assert!(self.service.wait_idle(Duration::from_secs(20)));
            self.service.get(&run.id).unwrap()
        }
    }

    fn attempt_environment(run: &WorkflowRun) -> Option<WorkflowEnvironment> {
        run.steps[0].attempts.last().unwrap().environment.clone()
    }

    #[test]
    fn an_attempt_records_the_environment_its_adapter_reported() {
        let mut reported = WorkflowEnvironment::base(runner_ids::NATIVE_PYTHON, Containment::NativeAccount);
        reported.seed = Some(7);
        reported.notes.push("from the adapter".into());
        for fail in [false, true] {
            let f = fixture(Some(reported.clone()), fail);
            let run = f.run(&f.save(), WorkflowRunCause::Manual);
            assert_eq!(attempt_environment(&run).as_ref(), Some(&reported), "fail={fail}");
        }
    }

    #[test]
    fn an_attempt_without_a_reported_environment_records_the_step_base() {
        for fail in [false, true] {
            let f = fixture(None, fail);
            let run = f.run(&f.save(), WorkflowRunCause::Manual);
            let env = attempt_environment(&run).expect("the base environment is recorded");
            assert_eq!(env, WorkflowEnvironment::of_step(&run.plan.steps[0]), "fail={fail}");
            assert_eq!((env.runner.as_str(), env.containment), (runner_ids::NATIVE_PYTHON, Containment::NativeAccount));
            assert_eq!((env.os.as_str(), env.app_version.as_str()), (std::env::consts::OS, APP_VERSION));
        }
    }

    #[test]
    fn reproductions_are_seeded_from_the_original_run_following_chains() {
        let f = fixture(None, false);
        let id = f.save();
        let original = f.run(&id, WorkflowRunCause::Manual);
        let copy = f.run(&id, WorkflowRunCause::Reproduce { of: original.id.clone() });
        let copy_of_copy = f.run(&id, WorkflowRunCause::Reproduce { of: copy.id.clone() });
        let bases = f.adapter.bases.lock().unwrap().clone();
        let o = original.id.to_string();
        assert_eq!(bases, vec![o.clone(), o.clone(), o], "every reproduction is seeded from the original");

        let lookup = |_: &WorkflowRunId| None;
        assert_eq!(seed_basis(&copy_of_copy, &lookup), copy.id.to_string(), "an unknown original ends the chain");
        assert_eq!(seed_basis(&original, &lookup), original.id.to_string());
    }
}
