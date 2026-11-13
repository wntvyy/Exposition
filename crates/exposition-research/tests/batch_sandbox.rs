//! Workflow batch sandbox against the real execution service.
//!
//! Restricted (Docker) tests are `#[ignore]`d: they need Docker Desktop with
//! the local image `python:3.13-slim` (never pulled). Run them with
//! `EXPOSITION_DOCKER_TESTS=1 cargo test -p exposition-research --test batch_sandbox -- --ignored`.
//! Trusted native tests run when a Python interpreter is discovered.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use exposition_core::artifact::RunStatus;
use exposition_core::path::WorkspacePath;
use exposition_execution::env::HostEnv;
use exposition_execution::{DockerSettings, ExecutionConfig, ExecutionService, ExecutionSettings};
use exposition_research::batch::{
    BatchCode, BatchControl, BatchInput, BatchMode, BatchOwner, BatchPort, BatchRequest, BatchResult, BatchRunner,
};
use exposition_research::helper::sha256_hex;
use exposition_research::launcher::ExecutionLauncher;
use exposition_research::sandbox::batch_runner;
use exposition_research::types::{RunExecution, SessionOwner};
use exposition_research::{ResearchConfig, ResearchError, ResearchService};
use exposition_workspace::DocumentService;

const IMAGE: &str = "python:3.13-slim";
const FAKE_KEY: &str = "sk-ant-api03-FAKEFAKEFAKE-batch-test-0123456789";

fn docker_enabled() -> bool {
    let on = std::env::var("EXPOSITION_DOCKER_TESTS").is_ok_and(|v| v == "1");
    if !on {
        eprintln!("SKIPPED: set EXPOSITION_DOCKER_TESTS=1 (Docker Desktop with local {IMAGE})");
    }
    on
}

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    data: PathBuf,
    execution: ExecutionService,
    research: Arc<ResearchService>,
    runner: Arc<dyn BatchRunner>,
}

fn fixture_with(image: Option<&str>, trusted: bool) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let root = base.join("project");
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let data = base.join("appdata");
    let host = HostEnv::from_pairs(std::env::vars().filter(|(k, _)| k != "ANTHROPIC_API_KEY").chain([
        ("ANTHROPIC_API_KEY".to_string(), FAKE_KEY.to_string()),
        ("EXPOSITION_TEST_HOST_ONLY".to_string(), "host-only-value".to_string()),
    ]));
    let settings = ExecutionSettings {
        docker: DockerSettings { image: image.map(str::to_owned), ..Default::default() },
        ..Default::default()
    };
    let execution = ExecutionService::with_config(ExecutionConfig {
        data_dir: data.clone(),
        host_env: host,
        settings: Some(settings),
        too_broad_to_trust: Some(vec![]),
        tool_cache_dir: None,
    })
    .unwrap();
    if trusted {
        execution.grant_trust(&root.display().to_string()).unwrap();
    }
    let documents = Arc::new(DocumentService::new());
    let research = Arc::new(ResearchService::new(ResearchConfig::new(
        data.clone(),
        Arc::new(ExecutionLauncher::new(execution.clone())),
        documents,
    )));
    research.set_workspace(&root, trusted).unwrap();
    let runner = batch_runner(research.clone(), execution.clone());
    Fixture { _tmp: tmp, root, data, execution, research, runner }
}

fn write(root: &Path, rel: &str, bytes: &[u8]) -> BatchInput {
    let path = WorkspacePath::parse(rel).unwrap();
    let host = path.components().fold(root.to_path_buf(), |p, c| p.join(c));
    std::fs::create_dir_all(host.parent().unwrap()).unwrap();
    std::fs::write(&host, bytes).unwrap();
    BatchInput { path, sha256: format!("sha256:{}", sha256_hex(bytes)) }
}

fn request(mode: BatchMode, code: &str, inputs: Vec<BatchInput>, outputs: Vec<BatchPort>) -> BatchRequest {
    BatchRequest {
        mode,
        owner: BatchOwner { run: "wfrun-1".into(), step: "analyze".into() },
        code: BatchCode::Inline { source: code.into() },
        inputs,
        outputs,
        time_limit: Duration::from_secs(120),
        max_output_bytes: 8 * 1024 * 1024,
    }
}

fn port(name: &str, exts: &[&str]) -> BatchPort {
    BatchPort { port: name.into(), extensions: exts.iter().map(|e| e.to_string()).collect() }
}

/// Run a batch; `cancel_after` cancels that long after dispatch.
fn run(
    f: &Fixture,
    request: &BatchRequest,
    cancel_after: Option<Duration>,
) -> (Result<BatchResult, ResearchError>, Option<String>) {
    let dispatched: Mutex<Option<(String, Instant)>> = Mutex::new(None);
    let canceled = || match (cancel_after, dispatched.lock().unwrap().as_ref()) {
        (Some(after), Some((_, at))) => at.elapsed() >= after,
        _ => false,
    };
    let on_dispatched = |id: &str| {
        let mut slot = dispatched.lock().unwrap();
        assert!(slot.is_none(), "on_dispatched is called once");
        *slot = Some((id.to_owned(), Instant::now()));
    };
    let result = f.runner.run(&f.root, request, &BatchControl { canceled: &canceled, on_dispatched: &on_dispatched });
    let id = dispatched.lock().unwrap().as_ref().map(|(id, _)| id.clone());
    (result, id)
}

fn containers_of(f: &Fixture) -> String {
    let filter = format!("label=org.exposition.instance={}", f.execution.docker_instance());
    let out = std::process::Command::new("docker").args(["ps", "-aq", "--filter", &filter]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn artifact_dir(f: &Fixture, run: &str) -> PathBuf {
    f.root.join(".exposition").join("artifacts").join(run)
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    names.sort();
    names
}

fn assert_clean(f: &Fixture) {
    assert_eq!(containers_of(f), "", "no container is left behind");
    let batches = f.data.join("research").join("batches");
    assert!(names_in(&batches).is_empty(), "staging folders are removed: {:?}", names_in(&batches));
}

const ANALYSIS: &str = r#"
import csv, os
out = os.environ["EXPOSITION_OUTPUT_DIR"]
with open("data/obs.csv", newline="") as f:
    rows = list(csv.DictReader(f))
total = sum(float(r["y"]) for r in rows)
with open(os.path.join(out, "table.csv"), "w", newline="") as f:
    w = csv.writer(f)
    w.writerow(["n", "total"])
    w.writerow([len(rows), total])
bars = "".join(f'<rect x="{i*20}" y="0" width="15" height="{float(r["y"])*10}"/>' for i, r in enumerate(rows))
with open(os.path.join(out, "figure.svg"), "w") as f:
    f.write(f'<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">{bars}</svg>')
with open(os.path.join(out, "notes.txt"), "w") as f:
    f.write("extra output")
print("rows", len(rows), "total", total)
"#;

#[test]
#[ignore = "needs Docker Desktop and the local image python:3.13-slim; run with EXPOSITION_DOCKER_TESTS=1 -- --ignored"]
fn restricted_batch_reads_declared_input_and_registers_outputs() {
    if !docker_enabled() {
        return;
    }
    let f = fixture_with(Some(IMAGE), false);
    let data = write(&f.root, "data/obs.csv", b"x,y\n1,2.5\n2,3.5\n");
    std::fs::write(f.root.join("secret.txt"), "undeclared").unwrap();
    assert!(f.runner.availability(&f.root, BatchMode::Restricted).is_ok());
    let req = request(
        BatchMode::Restricted,
        ANALYSIS,
        vec![data.clone()],
        vec![port("table", &["csv"]), port("figure", &["svg", "png"])],
    );
    let (result, dispatched) = run(&f, &req, None);
    let result = result.unwrap();
    assert_eq!(result.status, RunStatus::Succeeded, "{}", result.log);
    assert_eq!(dispatched.as_deref(), Some(result.research_run.as_str()));
    assert_eq!(result.exit_code, Some(0));
    assert!(result.log.contains("rows 2 total 6.0"), "{}", result.log);
    assert!(result.rejected.is_empty(), "{:?}", result.rejected);
    let ports: Vec<(&str, &str)> =
        result.ports.iter().map(|p| (p.port.as_str(), p.artifact.id.0.rsplit('/').next().unwrap())).collect();
    assert_eq!(ports, [("table", "table.csv"), ("figure", "figure.svg")]);
    assert_eq!(result.extra.len(), 1);
    let dir = artifact_dir(&f, &result.research_run);
    assert_eq!(names_in(&dir), ["figure.svg", "notes.txt", "table.csv", "transcript.json"]);
    for p in &result.ports {
        let path = p.artifact.path.as_ref().unwrap();
        let host = path.components().fold(f.root.clone(), |acc, c| acc.join(c));
        let bytes = std::fs::read(&host).unwrap();
        assert_eq!(p.artifact.content_hash, format!("sha256:{}", sha256_hex(&bytes)));
        assert_eq!(p.artifact.size_bytes, bytes.len() as u64);
    }
    let table = std::fs::read_to_string(dir.join("table.csv")).unwrap();
    assert!(table.contains("2,6.0"), "{table}");
    // The manifest records the actual mode, the workflow owner, and pinned inputs.
    let manifest = f.research.run_manifest(&result.research_run).unwrap();
    assert_eq!(manifest.session.owner, SessionOwner::Workflow { run: "wfrun-1".into(), step: "analyze".into() });
    match manifest.execution.as_ref().unwrap() {
        RunExecution::RestrictedContainer { backend, image, image_id, network, user, .. } => {
            assert_eq!((backend.as_str(), image.as_str(), network.as_str()), ("docker", IMAGE, "none"));
            assert!(image_id.as_deref().is_some_and(|id| id.starts_with("sha256:")), "{image_id:?}");
            assert_ne!(user, "0");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(manifest.record.inputs.len(), 1);
    assert_eq!(manifest.record.inputs[0].content_hash.as_deref(), Some(data.sha256.as_str()));
    assert_eq!(manifest.record.outputs.len(), 3);
    assert!(result.environment.iter().any(|(k, v)| k == "network" && v == "none"));
    // Reachable through the research service like any run artifact.
    let (_, bytes) = f.research.artifact_bytes(&result.research_run, &result.ports[0].artifact.id.0).unwrap();
    assert_eq!(bytes, table.as_bytes());
    assert!(f.research.runs().unwrap().iter().any(|r| r.id.0 == result.research_run));
    assert_clean(&f);
}

#[test]
#[ignore = "needs Docker Desktop and the local image python:3.13-slim; run with EXPOSITION_DOCKER_TESTS=1 -- --ignored"]
fn restricted_inputs_are_read_only_and_the_project_is_absent() {
    if !docker_enabled() {
        return;
    }
    let f = fixture_with(Some(IMAGE), false);
    let data = write(&f.root, "data/input.csv", b"a\n1\n");
    std::fs::write(f.root.join("secret.txt"), "undeclared").unwrap();
    std::fs::write(f.root.join("paper.tex"), "manuscript").unwrap();
    let code = r#"
import os
print("visible:", sorted(os.listdir(".")), sorted(os.listdir("data")))
assert not os.path.exists("secret.txt") and not os.path.exists("paper.tex"), "undeclared project files are visible"
try:
    open("new.txt", "w").write("x")
    print("WROTE new.txt")
except OSError as e:
    print("new file refused:", e)
open("data/input.csv", "w").write("tampered")
"#;
    let (result, _) = run(&f, &request(BatchMode::Restricted, code, vec![data], Vec::new()), None);
    let result = result.unwrap();
    assert!(matches!(result.status, RunStatus::Failed { .. }), "{:?}\n{}", result.status, result.log);
    assert!(result.log.contains("visible: ['__exposition_main__.py', 'data'] ['input.csv']"), "{}", result.log);
    assert!(result.log.contains("new file refused") && !result.log.contains("WROTE"), "{}", result.log);
    assert!(result.log.contains("Read-only file system"), "{}", result.log);
    assert_eq!(std::fs::read(f.root.join("data/input.csv")).unwrap(), b"a\n1\n", "the project file is untouched");
    assert!(!f.root.join("new.txt").exists());
    assert_clean(&f);
}

#[test]
#[ignore = "needs Docker Desktop and the local image python:3.13-slim; run with EXPOSITION_DOCKER_TESTS=1 -- --ignored"]
fn restricted_undeclared_import_fails_visibly() {
    if !docker_enabled() {
        return;
    }
    let f = fixture_with(Some(IMAGE), false);
    std::fs::write(f.root.join("helpers.py"), "VALUE = 42\n").unwrap();
    let declared = write(&f.root, "lib/declared_mod.py", b"VALUE = 7\n");
    let ok = "import sys\nsys.path.insert(0, 'lib')\nimport declared_mod\nprint('declared', declared_mod.VALUE)\n";
    let (result, _) = run(&f, &request(BatchMode::Restricted, ok, vec![declared], Vec::new()), None);
    let result = result.unwrap();
    assert_eq!(result.status, RunStatus::Succeeded, "{}", result.log);
    assert!(result.log.contains("declared 7"));
    let (result, _) = run(&f, &request(BatchMode::Restricted, "import helpers\n", Vec::new(), Vec::new()), None);
    let result = result.unwrap();
    match &result.status {
        RunStatus::Failed { message } => assert!(message.contains("ModuleNotFoundError"), "{message}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(result.exit_code, Some(1));
    assert!(result.log.contains("No module named 'helpers'"), "{}", result.log);
    let manifest = f.research.run_manifest(&result.research_run).unwrap();
    assert_eq!(manifest.error.as_ref().unwrap().ename, "ModuleNotFoundError");
    assert_clean(&f);
}

#[test]
#[ignore = "needs Docker Desktop and the local image python:3.13-slim; run with EXPOSITION_DOCKER_TESTS=1 -- --ignored"]
fn restricted_escaping_outputs_are_rejected_and_never_copied() {
    if !docker_enabled() {
        return;
    }
    let f = fixture_with(Some(IMAGE), false);
    let data = write(&f.root, "data/input.csv", b"a\n1\n");
    let code = r#"
import os
out = os.environ["EXPOSITION_OUTPUT_DIR"]
def attempt(label, fn):
    try:
        fn()
        print("made", label)
    except Exception as e:
        print("failed", label, type(e).__name__, e)
attempt("abs-symlink", lambda: os.symlink("/etc/passwd", os.path.join(out, "table.csv")))
attempt("rel-symlink", lambda: os.symlink("../workspace/data/input.csv", os.path.join(out, "rel.csv")))
attempt("dir-symlink", lambda: os.symlink("/etc", os.path.join(out, "etc.csv")))
def nested():
    os.mkdir(os.path.join(out, "nested"))
    open(os.path.join(out, "nested", "inner.csv"), "w").write("x")
attempt("directory", nested)
attempt("fifo", lambda: os.mkfifo(os.path.join(out, "pipe.csv")))
attempt("parent", lambda: open(os.path.join(out, "..", "x.csv"), "w").write("x"))
def hard():
    open(os.path.join(out, "a.csv"), "w").write("1\n")
    os.link(os.path.join(out, "a.csv"), os.path.join(out, "b.csv"))
attempt("hardlink", hard)
attempt("oversize", lambda: open(os.path.join(out, "big.csv"), "w").write("1" * 200000))
attempt("mislabelled", lambda: open(os.path.join(out, "fake.png"), "w").write("<html><script>alert(1)</script>"))
attempt("hidden", lambda: open(os.path.join(out, ".hidden.csv"), "w").write("x"))
attempt("exe", lambda: open(os.path.join(out, "tool.exe"), "w").write("MZ"))
attempt("ok", lambda: open(os.path.join(out, "ok.csv"), "w").write("a\n1\n"))
print("listing", sorted(os.listdir(out)))
"#;
    let mut req = request(BatchMode::Restricted, code, vec![data], vec![port("table", &["csv"])]);
    req.max_output_bytes = 64 * 1024;
    let (result, _) = run(&f, &req, None);
    let result = result.unwrap();
    eprintln!("container log:\n{}", result.log);
    eprintln!("rejected: {:#?}", result.rejected);
    assert_eq!(result.status, RunStatus::Succeeded, "{}", result.log);
    assert!(result.log.contains("failed parent"), "writing outside /out is impossible: {}", result.log);
    // Only the plain file was accepted; `table` is not realized by a link.
    let accepted: Vec<&str> = result.extra.iter().map(|a| a.id.0.rsplit('/').next().unwrap()).collect();
    assert_eq!(accepted, ["ok.csv"], "{:?}", result.rejected);
    assert!(result.ports.is_empty(), "{:?}", result.ports);
    let dir = artifact_dir(&f, &result.research_run);
    assert_eq!(names_in(&dir), ["ok.csv", "transcript.json"]);
    let rejected = |name: &str| result.rejected.iter().find(|r| r.name == name).map(|r| r.reason.clone());
    for name in ["big.csv", "fake.png", ".hidden.csv", "tool.exe", "nested"] {
        assert!(rejected(name).is_some(), "{name} must be rejected: {:?}", result.rejected);
    }
    // Whatever the bind mount made of links, FIFOs, and hard links: if the
    // entry exists on the host it was rejected, and nothing it points to was copied.
    for name in ["table.csv", "rel.csv", "etc.csv", "pipe.csv", "a.csv", "b.csv"] {
        if result.log.contains(&format!("made {}", name)) || result.rejected.iter().any(|r| r.name == name) {
            assert!(rejected(name).is_some() || !names_in(&dir).contains(&name.to_owned()), "{name}");
        }
        assert!(!dir.join(name).exists(), "{name} is not in the project");
    }
    for entry in names_in(&dir) {
        let text = std::fs::read_to_string(dir.join(&entry)).unwrap_or_default();
        assert!(!text.contains("root:x:0:0"), "/etc/passwd content leaked into {entry}");
    }
    assert_clean(&f);
}

#[test]
#[ignore = "needs Docker Desktop and the local image python:3.13-slim; run with EXPOSITION_DOCKER_TESTS=1 -- --ignored"]
fn restricted_has_no_network_no_host_environment_and_runs_as_non_root() {
    if !docker_enabled() {
        return;
    }
    let f = fixture_with(Some(IMAGE), false);
    let code = r#"
import json, os, socket
try:
    socket.create_connection(("1.1.1.1", 53), timeout=3)
    print("NETWORK connected")
except OSError as e:
    print("network blocked:", type(e).__name__)
try:
    socket.getaddrinfo("example.com", 443)
    print("DNS resolved")
except OSError as e:
    print("dns blocked:", type(e).__name__)
print("uid", os.getuid(), "gid", os.getgid())
print("ENV " + json.dumps(dict(os.environ), sort_keys=True))
"#;
    let (result, _) = run(&f, &request(BatchMode::Restricted, code, Vec::new(), Vec::new()), None);
    let result = result.unwrap();
    assert_eq!(result.status, RunStatus::Succeeded, "{}", result.log);
    assert!(result.log.contains("network blocked") && !result.log.contains("NETWORK connected"), "{}", result.log);
    assert!(result.log.contains("dns blocked") && !result.log.contains("DNS resolved"), "{}", result.log);
    assert!(!result.log.contains("uid 0 "), "{}", result.log);
    assert!(result.log.contains("uid 65534"), "{}", result.log);
    let env_line = result.log.lines().find(|l| l.starts_with("ENV ")).unwrap();
    let env: std::collections::BTreeMap<String, String> = serde_json::from_str(&env_line[4..]).unwrap();
    assert_eq!(env.get("EXPOSITION_OUTPUT_DIR").map(String::as_str), Some("/out"));
    for name in ["ANTHROPIC_API_KEY", "EXPOSITION_TEST_HOST_ONLY", "USERPROFILE", "USERNAME", "APPDATA", "GITHUB_TOKEN"]
    {
        assert!(!env.contains_key(name), "{name} reached the container: {env:?}");
    }
    assert!(!result.log.contains(FAKE_KEY));
    assert!(!env.values().any(|v| v.contains(r"C:\") || v.contains("Users")), "host paths leaked: {env:?}");
    assert_clean(&f);
}

#[test]
#[ignore = "needs Docker Desktop and the local image python:3.13-slim; run with EXPOSITION_DOCKER_TESTS=1 -- --ignored"]
fn restricted_cancel_removes_the_container() {
    if !docker_enabled() {
        return;
    }
    let f = fixture_with(Some(IMAGE), false);
    let code = "import time\nprint('started', flush=True)\ntime.sleep(120)\nprint('NOT CANCELED')\n";
    let started = Instant::now();
    let (result, dispatched) =
        run(&f, &request(BatchMode::Restricted, code, Vec::new(), Vec::new()), Some(Duration::from_secs(4)));
    let result = result.unwrap();
    assert!(dispatched.is_some());
    assert_eq!(result.status, RunStatus::Canceled, "{}", result.log);
    assert!(started.elapsed() < Duration::from_secs(60), "canceled promptly: {:?}", started.elapsed());
    assert!(!result.log.contains("NOT CANCELED"));
    let manifest = f.research.run_manifest(&result.research_run).unwrap();
    assert_eq!(manifest.record.status, RunStatus::Canceled);
    assert_clean(&f);
}

#[test]
#[ignore = "needs Docker Desktop and the local image python:3.13-slim; run with EXPOSITION_DOCKER_TESTS=1 -- --ignored"]
fn restricted_time_limit_stops_the_batch() {
    if !docker_enabled() {
        return;
    }
    let f = fixture_with(Some(IMAGE), false);
    let mut req = request(BatchMode::Restricted, "import time\ntime.sleep(120)\n", Vec::new(), Vec::new());
    req.time_limit = Duration::from_secs(5);
    let started = Instant::now();
    let (result, _) = run(&f, &req, None);
    let result = result.unwrap();
    match &result.status {
        RunStatus::Failed { message } => assert!(message.contains("time limit"), "{message}"),
        other => panic!("{other:?}"),
    }
    assert!(started.elapsed() < Duration::from_secs(60), "{:?}", started.elapsed());
    assert_clean(&f);
}

#[test]
#[ignore = "needs Docker Desktop; run with EXPOSITION_DOCKER_TESTS=1 -- --ignored"]
fn restricted_with_a_missing_image_is_refused_without_fallback() {
    if !docker_enabled() {
        return;
    }
    let f = fixture_with(Some("exposition-test-missing-image:0.0"), true);
    let reason = f.runner.availability(&f.root, BatchMode::Restricted).unwrap_err();
    assert!(reason.contains("not present locally"), "{reason}");
    let (result, dispatched) = run(&f, &request(BatchMode::Restricted, "print('ran')\n", Vec::new(), Vec::new()), None);
    let err = result.unwrap_err();
    assert_eq!(err.code(), "research.environmentUnavailable", "{err}");
    assert!(dispatched.is_none(), "nothing was dispatched");
    assert!(names_in(&f.root.join(".exposition").join("runs")).is_empty(), "no run was recorded");
}

#[test]
fn restricted_without_an_image_is_unavailable() {
    let f = fixture_with(None, false);
    let reason = f.runner.availability(&f.root, BatchMode::Restricted).unwrap_err();
    assert!(reason.contains("no container image is configured"), "{reason}");
    let (result, dispatched) = run(&f, &request(BatchMode::Restricted, "print('ran')\n", Vec::new(), Vec::new()), None);
    assert_eq!(result.unwrap_err().code(), "research.environmentUnavailable");
    assert!(dispatched.is_none());
}

#[test]
fn batches_are_refused_for_a_project_that_is_not_open() {
    let f = fixture_with(None, false);
    let other = f.root.parent().unwrap().join("other");
    std::fs::create_dir_all(&other).unwrap();
    let req = request(BatchMode::Restricted, "", Vec::new(), Vec::new());
    let canceled = || false;
    let on_dispatched = |_: &str| panic!("must not dispatch");
    let result = f.runner.run(&other, &req, &BatchControl { canceled: &canceled, on_dispatched: &on_dispatched });
    assert_eq!(result.unwrap_err().code(), "research.projectNotCurrent");
    let mut bad = request(BatchMode::Restricted, "", Vec::new(), vec![port("../x", &["csv"])]);
    let (result, _) = run(&f, &bad, None);
    assert_eq!(result.unwrap_err().code(), "research.invalidRequest");
    bad.outputs = vec![port("model", &["pkl"])];
    let (result, _) = run(&f, &bad, None);
    assert_eq!(result.unwrap_err().code(), "research.invalidRequest");
}

#[test]
fn trusted_native_needs_a_trusted_folder() {
    let f = fixture_with(None, false);
    let reason = f.runner.availability(&f.root, BatchMode::TrustedNative).unwrap_err();
    assert!(reason.contains("trusted"), "{reason}");
    let (result, dispatched) =
        run(&f, &request(BatchMode::TrustedNative, "print('ran')\n", Vec::new(), Vec::new()), None);
    assert_eq!(result.unwrap_err().code(), "research.environmentUnavailable");
    assert!(dispatched.is_none());
}

#[test]
fn trusted_native_batch_runs_in_a_fresh_process_and_registers_outputs() {
    let f = fixture_with(None, true);
    if let Err(reason) = f.runner.availability(&f.root, BatchMode::TrustedNative) {
        eprintln!("SKIPPED: no Python interpreter for trusted native batches ({reason})");
        return;
    }
    let data = write(&f.root, "data/obs.csv", b"x,y\n1,2.5\n2,3.5\n");
    std::fs::write(f.root.join("helpers.py"), "VALUE = 1\n").unwrap();
    let code = format!(
        "{ANALYSIS}\nimport os\nprint('cwd-has-secret', os.path.exists('helpers.py'))\nprint('out-env', bool(os.environ.get('EXPOSITION_OUTPUT_DIR')))\nprint('key-visible', 'ANTHROPIC_API_KEY' in os.environ)\n"
    );
    let req =
        request(BatchMode::TrustedNative, &code, vec![data], vec![port("table", &["csv"]), port("figure", &["svg"])]);
    let (result, dispatched) = run(&f, &req, None);
    let result = result.unwrap();
    assert_eq!(result.status, RunStatus::Succeeded, "{}", result.log);
    assert_eq!(dispatched.as_deref(), Some(result.research_run.as_str()));
    assert!(result.log.contains("cwd-has-secret False"), "the working copy has only declared files: {}", result.log);
    assert!(result.log.contains("out-env True") && result.log.contains("key-visible False"), "{}", result.log);
    assert_eq!(result.ports.len(), 2, "{:?}", result.rejected);
    let manifest = f.research.run_manifest(&result.research_run).unwrap();
    assert!(matches!(manifest.execution, Some(RunExecution::TrustedNative { .. })), "{:?}", manifest.execution);
    assert_eq!(manifest.session.owner, SessionOwner::Workflow { run: "wfrun-1".into(), step: "analyze".into() });
    assert!(result.environment.iter().any(|(k, v)| k == "mode" && v.contains("trusted native")));
    // Never the user's kernel: no research session was started.
    assert!(f.research.sessions().is_empty());
    let batches = f.data.join("research").join("batches");
    assert!(names_in(&batches).is_empty(), "staging folders are removed");
}

#[test]
fn trusted_native_time_limit_and_cancel_stop_the_process() {
    let f = fixture_with(None, true);
    if f.runner.availability(&f.root, BatchMode::TrustedNative).is_err() {
        eprintln!("SKIPPED: no Python interpreter for trusted native batches");
        return;
    }
    let mut req = request(BatchMode::TrustedNative, "import time\ntime.sleep(120)\n", Vec::new(), Vec::new());
    req.time_limit = Duration::from_secs(3);
    let started = Instant::now();
    let (result, _) = run(&f, &req, None);
    match result.unwrap().status {
        RunStatus::Failed { message } => assert!(message.contains("time limit"), "{message}"),
        other => panic!("{other:?}"),
    }
    req.time_limit = Duration::from_secs(120);
    let (result, _) = run(&f, &req, Some(Duration::from_secs(2)));
    assert_eq!(result.unwrap().status, RunStatus::Canceled);
    assert!(started.elapsed() < Duration::from_secs(60), "{:?}", started.elapsed());
}
