//! What a restricted image contains: platform, interpreter, and installed
//! packages, captured inside that exact image (by identity) and cached per
//! image identity.
//!
//! Why a separate probe container rather than a wrapper around the batch's
//! own code: the batch's command, environment, and output area stay exactly
//! what the user approved (no injected prelude changing `__main__`, `sys.argv`,
//! or tracebacks, nothing extra in the output area), and the code under study
//! cannot influence what is recorded about its environment. The probe runs as
//! part of the approved restricted batch, in the image the batch was approved
//! for (started by identity), with the same containment and less: no network,
//! no inputs (an empty read-only working folder), no host environment (a
//! constructed one), non-root, read-only root file system, a short time limit.
//! An image identity names immutable content, so its package list never
//! changes: it is probed once per identity and cached in the application data
//! folder (`<data>/research/image-environments/<digest>.json`), never in the
//! project. Nothing runs natively.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use exposition_core::job::{EnvironmentSpec, ExecutionPolicy, JobOutcome, JobSpec, OutputStream};
use exposition_execution::ExecutionService;
use serde::{Deserialize, Serialize};

/// Interpreter inside the container image (as for batches).
const CONTAINER_PYTHON: &str = "python3";
const MARKER: &str = "EXPOSITION-IMAGE-PROBE";
/// At most this many packages are kept (a note says when more exist).
pub const MAX_PACKAGES: usize = 5000;
const MAX_NAME: usize = 200;
const MAX_VERSION: usize = 100;
const MAX_FIELD: usize = 64;
/// A cache file larger than this is ignored (and replaced).
const MAX_CACHE_BYTES: u64 = 2 * 1024 * 1024;
const PROBE_TIMEOUT_MS: u64 = 30_000;
const PROBE_LOG_LIMIT: u64 = 4 * 1024 * 1024;
const POLL: Duration = Duration::from_millis(50);
const CACHE_DIR: &str = "image-environments";
const CACHE_FORMAT: &str = "exposition.imageEnvironment/1";

/// Lists the image's platform, interpreter, and every installed distribution.
const PROBE: &str = r#"
import json, platform, sys
from importlib import metadata
seen = {}
for dist in metadata.distributions():
    try:
        name = dist.metadata["Name"]
        version = dist.version
    except Exception:
        continue
    if name and version and name.lower() not in seen:
        seen[name.lower()] = [name, version]
packages = sorted(seen.values(), key=lambda p: (p[0].lower(), p[1]))
print("EXPOSITION-IMAGE-PROBE" + json.dumps({
    "os": platform.system().lower(),
    "arch": platform.machine(),
    "implementation": sys.implementation.name,
    "pythonVersion": ".".join(str(p) for p in sys.version_info[:3]),
    "packages": packages[:5000],
    "truncated": len(packages) > 5000,
}))
"#;

/// The environment of one image identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageEnvironment {
    pub format: String,
    /// `sha256:…` identity the probe ran in.
    pub image_id: String,
    pub os: String,
    pub arch: String,
    pub implementation: String,
    pub python_version: String,
    /// `(name, version)`, sorted by name.
    pub packages: Vec<(String, String)>,
    pub truncated: bool,
    pub captured_at_ms: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProbeOutput {
    os: String,
    arch: String,
    implementation: String,
    python_version: String,
    packages: Vec<(String, String)>,
    #[serde(default)]
    truncated: bool,
}

fn clean(text: &str, max: usize) -> String {
    text.chars().filter(|c| !c.is_control()).take(max).collect::<String>().trim().to_owned()
}

/// The bare hex digest of a `sha256:<64 hex>` identity.
fn digest_of(image_id: &str) -> Option<&str> {
    let hex = image_id.strip_prefix("sha256:")?;
    (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))).then_some(hex)
}

/// Where the environment of `image_id` is cached (`None`: not a digest).
pub fn cache_path(data_dir: &Path, image_id: &str) -> Option<PathBuf> {
    let hex = digest_of(image_id)?;
    Some(data_dir.join("research").join(CACHE_DIR).join(format!("{hex}.json")))
}

/// Validate what the probe (or the cache) says; untrusted data either way.
fn checked(image_id: &str, out: ProbeOutput, captured_at_ms: u64) -> ImageEnvironment {
    let mut packages: Vec<(String, String)> = out
        .packages
        .into_iter()
        .take(MAX_PACKAGES)
        .map(|(n, v)| (clean(&n, MAX_NAME).replace('=', "-"), clean(&v, MAX_VERSION)))
        .filter(|(n, v)| !n.is_empty() && !v.is_empty())
        .collect();
    packages.sort_by(|a, b| a.0.to_ascii_lowercase().cmp(&b.0.to_ascii_lowercase()).then(a.cmp(b)));
    packages.dedup_by(|a, b| a.0.eq_ignore_ascii_case(&b.0));
    ImageEnvironment {
        format: CACHE_FORMAT.to_owned(),
        image_id: image_id.to_owned(),
        os: clean(&out.os, MAX_FIELD),
        arch: clean(&out.arch, MAX_FIELD),
        implementation: clean(&out.implementation, MAX_FIELD),
        python_version: clean(&out.python_version, MAX_FIELD),
        packages,
        truncated: out.truncated,
        captured_at_ms,
    }
}

/// The cached environment of `image_id`, if a valid one exists.
pub fn cached(data_dir: &Path, image_id: &str) -> Option<ImageEnvironment> {
    let path = cache_path(data_dir, image_id)?;
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > MAX_CACHE_BYTES {
        return None;
    }
    let env: ImageEnvironment = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
    if env.format != CACHE_FORMAT || env.image_id != image_id || env.packages.len() > MAX_PACKAGES {
        return None;
    }
    let ImageEnvironment { os, arch, implementation, python_version, packages, truncated, captured_at_ms, .. } = env;
    Some(checked(
        image_id,
        ProbeOutput { os, arch, implementation, python_version, packages, truncated },
        captured_at_ms,
    ))
}

fn store(path: &Path, env: &ImageEnvironment) {
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let Ok(bytes) = serde_json::to_vec(env) else { return };
    let temp = dir.join(format!(".{}.{}.tmp", std::process::id(), crate::store::new_run_id()));
    if std::fs::write(&temp, &bytes).is_ok() && std::fs::rename(&temp, path).is_ok() {
        return;
    }
    let _ = std::fs::remove_file(&temp);
}

/// Removes the probe's empty working folder and its registration.
struct ProbeDir<'a> {
    execution: &'a ExecutionService,
    dir: PathBuf,
    registered: Option<PathBuf>,
}

impl Drop for ProbeDir<'_> {
    fn drop(&mut self) {
        if let Some(root) = self.registered.take() {
            self.execution.unregister_root(&root);
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The environment of `image_id`: cached, or probed now inside exactly that
/// image (restricted, by identity) and cached. `owner` is the project the
/// batch belongs to (the empty working folder is registered for it, as the
/// batch's own working copy is).
pub fn capture(
    execution: &ExecutionService,
    data_dir: &Path,
    image_id: &str,
    owner: &Path,
    canceled: &(dyn Fn() -> bool + Sync),
) -> Result<ImageEnvironment, String> {
    if let Some(env) = cached(data_dir, image_id) {
        return Ok(env);
    }
    let cache = cache_path(data_dir, image_id).ok_or("the image identity is not a sha256 digest")?;
    // A batch staging folder name, so a leftover is removed at the next start.
    let dir = data_dir.join("research").join("batches").join(crate::store::new_run_id());
    let work = dir.join("work");
    let mut guard = ProbeDir { execution, dir: dir.clone(), registered: None };
    std::fs::create_dir_all(&work).map_err(|e| format!("the probe folder could not be created: {e}"))?;
    let registered = execution
        .register_app_dir(&work, Some(owner))
        .map_err(|e| format!("the probe folder could not be registered: {e}"))?;
    guard.registered = Some(registered.clone());
    let mut set = BTreeMap::new();
    set.insert("PYTHONDONTWRITEBYTECODE".to_owned(), "1".to_owned());
    set.insert("PYTHONIOENCODING".to_owned(), "utf-8".to_owned());
    let spec = JobSpec {
        label: format!("Restricted image environment probe ({})", &image_id[..image_id.len().min(19)]),
        program: CONTAINER_PYTHON.into(),
        args: vec!["-c".into(), PROBE.into()],
        environment: EnvironmentSpec { inherit: Vec::new(), set },
        working_directory: registered.display().to_string(),
        policy: ExecutionPolicy::Restricted {
            backend: exposition_execution::docker::BACKEND.into(),
            image_id: Some(image_id.to_owned()),
        },
        timeout_ms: Some(PROBE_TIMEOUT_MS),
        log_limit_bytes: Some(PROBE_LOG_LIMIT),
        stdin: None,
    };
    if canceled() {
        return Err("canceled".into());
    }
    let job = execution.launch(spec);
    let deadline = Instant::now() + Duration::from_millis(PROBE_TIMEOUT_MS + 30_000);
    let mut cancel_sent = false;
    let result = loop {
        if let Some(result) = execution.result(&job) {
            break Some(result);
        }
        if execution.summary(&job).is_none() {
            break None;
        }
        if !cancel_sent && (canceled() || Instant::now() > deadline) {
            cancel_sent = true;
            execution.cancel(&job);
        }
        std::thread::sleep(POLL);
    };
    let stdout = execution.output_text(&job, OutputStream::Stdout).unwrap_or_default();
    let stderr = execution.output_text(&job, OutputStream::Stderr).unwrap_or_default();
    let _ = std::fs::remove_dir_all(execution.job_dir(&job));
    drop(guard);
    let outcome = result.map(|r| r.outcome).ok_or("the execution service lost track of the probe")?;
    if outcome != (JobOutcome::Exited { code: 0 }) {
        let last = stderr.lines().rev().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
        return Err(
            format!("the probe ended with {outcome:?}{}", if last.is_empty() { "" } else { ": " }) + &clean(last, 300)
        );
    }
    let line = stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix(MARKER))
        .ok_or("the probe printed no environment (the image may lack importlib.metadata)")?;
    let out: ProbeOutput = serde_json::from_str(line).map_err(|e| format!("the probe output is invalid: {e}"))?;
    let env = checked(image_id, out, now_ms());
    store(&cache, &env);
    Ok(env)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn only_digest_identities_have_a_cache_file() {
        let data = Path::new("data");
        assert!(cache_path(data, ID).unwrap().ends_with(format!("{}.json", &ID[7..])));
        for bad in ["", "python:3.13-slim", "sha256:../../x", "sha256:ABC", &ID[..70]] {
            assert!(cache_path(data, bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn probe_output_is_bounded_cleaned_and_sorted() {
        let out = ProbeOutput {
            os: "linux\u{7}".into(),
            arch: "x86_64".into(),
            implementation: "cpython".into(),
            python_version: "3.13.9".into(),
            packages: vec![
                ("pip".into(), "25.2".into()),
                ("Numpy".into(), "2.3.1".into()),
                ("numpy".into(), "2.3.1".into()),
                ("evil==1".into(), "0\n".into()),
                ("".into(), "1".into()),
            ],
            truncated: false,
        };
        let env = checked(ID, out, 5);
        assert_eq!(env.os, "linux");
        assert_eq!(
            env.packages,
            vec![("evil--1".into(), "0".into()), ("Numpy".into(), "2.3.1".into()), ("pip".into(), "25.2".into())]
        );
    }

    #[test]
    fn the_cache_is_read_back_only_for_its_own_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let env = checked(
            ID,
            ProbeOutput {
                os: "linux".into(),
                arch: "aarch64".into(),
                implementation: "cpython".into(),
                python_version: "3.13.1".into(),
                packages: vec![("pip".into(), "25.0".into())],
                truncated: false,
            },
            7,
        );
        let path = cache_path(tmp.path(), ID).unwrap();
        store(&path, &env);
        assert_eq!(cached(tmp.path(), ID), Some(env.clone()));
        let other = format!("sha256:{}", "f".repeat(64));
        std::fs::copy(&path, cache_path(tmp.path(), &other).unwrap()).unwrap();
        assert_eq!(cached(tmp.path(), &other), None, "a file naming another identity is not used");
        std::fs::write(&path, "{").unwrap();
        assert_eq!(cached(tmp.path(), ID), None);
    }
}
