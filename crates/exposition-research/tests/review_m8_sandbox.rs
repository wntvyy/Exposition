//! R-m8 independent review: the workflow batch sandbox against the real
//! execution service (no fakes).
//!
//! Trusted-native cases run when a Python interpreter is discovered (skipped
//! otherwise). Restricted (Docker) cases follow the crate convention and are
//! `#[ignore]`d: run them with
//! `EXPOSITION_DOCKER_TESTS=1 cargo test -p exposition-research --test review_m8_sandbox -- --include-ignored`
//! (Docker Desktop with the local image `python:3.13-slim`, never pulled).
//!
//! Findings reproduced here (fail while the defect exists):
//! - R-m8-F2: an output whose extension is upper case (`notes.TXT`,
//!   `figure.SVG`, `table.CSV`) is accepted and realizes ports, but its media
//!   type is looked up case-sensitively (`runs::media_type_for`), so it is
//!   recorded as `application/octet-stream` and text outputs skip the secret
//!   redaction that `outputs::register` applies to text types.
//!
//! Held checks (pass): container capabilities/sockets/mounts, hostile output
//! names and contents, a writer that outlives the batch's main process.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use exposition_core::artifact::{ArtifactRecord, RunStatus};
use exposition_core::path::WorkspacePath;
use exposition_execution::env::HostEnv;
use exposition_execution::{DockerSettings, ExecutionConfig, ExecutionService, ExecutionSettings};
use exposition_research::batch::{
    BatchCode, BatchControl, BatchInput, BatchMode, BatchOwner, BatchPort, BatchRequest, BatchResult, BatchRunner,
};
use exposition_research::helper::sha256_hex;
use exposition_research::launcher::ExecutionLauncher;
use exposition_research::sandbox::batch_runner;
use exposition_research::{ResearchConfig, ResearchError, ResearchService};
use exposition_workspace::DocumentService;

const IMAGE: &str = "python:3.13-slim";
/// A secret value the execution service's redactor knows (host environment).
const FAKE_KEY: &str = "sk-ant-api03-REVIEWM8FAKE-redaction-0123456789abcdef";

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
    runner: Arc<dyn BatchRunner>,
}

fn fixture(image: Option<&str>, trusted: bool) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let root = base.join("project");
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let data = base.join("appdata");
    let host = HostEnv::from_pairs(
        std::env::vars()
            .filter(|(k, _)| k != "ANTHROPIC_API_KEY")
            .chain([("ANTHROPIC_API_KEY".to_string(), FAKE_KEY.to_string())]),
    );
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
    let runner = batch_runner(research, execution.clone());
    Fixture { _tmp: tmp, root, data, execution, runner }
}

fn request(mode: BatchMode, code: &str, inputs: Vec<BatchInput>, outputs: Vec<BatchPort>) -> BatchRequest {
    BatchRequest {
        mode,
        owner: BatchOwner { run: "wrun-review".into(), step: "probe".into() },
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

fn write(root: &Path, rel: &str, bytes: &[u8]) -> BatchInput {
    let path = WorkspacePath::parse(rel).unwrap();
    let host = path.components().fold(root.to_path_buf(), |p, c| p.join(c));
    std::fs::create_dir_all(host.parent().unwrap()).unwrap();
    std::fs::write(&host, bytes).unwrap();
    BatchInput { path, sha256: format!("sha256:{}", sha256_hex(bytes)) }
}

fn run(f: &Fixture, request: &BatchRequest) -> Result<BatchResult, ResearchError> {
    let dispatched: Mutex<Option<String>> = Mutex::new(None);
    let canceled = || false;
    let on_dispatched = |id: &str| *dispatched.lock().unwrap() = Some(id.to_owned());
    f.runner.run(&f.root, request, &BatchControl { canceled: &canceled, on_dispatched: &on_dispatched })
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    names.sort();
    names
}

fn artifact_dir(f: &Fixture, run: &str) -> PathBuf {
    f.root.join(".exposition").join("artifacts").join(run)
}

fn containers_of(f: &Fixture) -> String {
    let filter = format!("label=org.exposition.instance={}", f.execution.docker_instance());
    let out = std::process::Command::new("docker").args(["ps", "-aq", "--filter", &filter]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn all_records(result: &BatchResult) -> Vec<ArtifactRecord> {
    result.ports.iter().map(|p| p.artifact.clone()).chain(result.extra.iter().cloned()).collect()
}

fn record<'a>(records: &'a [ArtifactRecord], name: &str) -> &'a ArtifactRecord {
    records
        .iter()
        .find(|r| r.id.0.rsplit('/').next() == Some(name))
        .unwrap_or_else(|| panic!("{name} was not accepted: {records:#?}"))
}

// ------------------------------------------------------------- R-m8-F2 ----

/// The batch writes the same kinds of outputs with lower- and upper-case
/// extensions; each contains a value the redactor knows.
fn case_probe_code() -> String {
    format!(
        r#"
import os
out = os.environ["EXPOSITION_OUTPUT_DIR"]
KEY = "{FAKE_KEY}"
open(os.path.join(out, "lower.txt"), "w").write("key=" + KEY + "\n")
open(os.path.join(out, "upper.TXT"), "w").write("key=" + KEY + "\n")
open(os.path.join(out, "figure.SVG"), "w").write('<svg xmlns="http://www.w3.org/2000/svg"><text>' + KEY + '</text></svg>')
open(os.path.join(out, "table.CSV"), "w").write("k\n" + KEY + "\n")
print("written")
"#
    )
}

fn check_case_probe(f: &Fixture, result: &BatchResult) {
    assert_eq!(result.status, RunStatus::Succeeded, "{}", result.log);
    let records = all_records(result);
    let dir = artifact_dir(f, &result.research_run);
    let stored = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap_or_default();
    // Control: the lower-case text output is redacted.
    assert!(!stored("lower.txt").contains(FAKE_KEY), "lower.txt is redacted (control)");
    let mut problems = Vec::new();
    for (name, media) in [("upper.TXT", "text/plain"), ("figure.SVG", "image/svg+xml"), ("table.CSV", "text/csv")] {
        let r = record(&records, name);
        if r.media_type != media {
            problems.push(format!("{name} recorded as {} (expected {media})", r.media_type));
        }
        if stored(name).contains(FAKE_KEY) {
            problems.push(format!("{name} was stored without redaction (the secret is in the project file)"));
        }
    }
    // The ports were realized by the upper-case files (so they flow on).
    assert!(result.ports.iter().any(|p| p.port == "figure"), "figure port realized: {:?}", result.ports);
    assert!(problems.is_empty(), "R-m8-F2 reproduced:\n{}", problems.join("\n"));
}

#[test]
fn review_m8_f2_native_upper_case_extensions_keep_type_and_redaction() {
    let f = fixture(None, true);
    if let Err(reason) = f.runner.availability(&f.root, BatchMode::TrustedNative) {
        eprintln!("SKIPPED: no Python interpreter for trusted native batches ({reason})");
        return;
    }
    let req = request(
        BatchMode::TrustedNative,
        &case_probe_code(),
        Vec::new(),
        vec![port("figure", &["svg", "png"]), port("table", &["csv"])],
    );
    let result = run(&f, &req).unwrap();
    check_case_probe(&f, &result);
}

#[test]
#[ignore = "needs Docker Desktop and the local image python:3.13-slim; run with EXPOSITION_DOCKER_TESTS=1 -- --include-ignored"]
fn review_m8_f2_restricted_upper_case_extensions_keep_type_and_redaction() {
    if !docker_enabled() {
        return;
    }
    let f = fixture(Some(IMAGE), false);
    let req = request(
        BatchMode::Restricted,
        &case_probe_code(),
        Vec::new(),
        vec![port("figure", &["svg", "png"]), port("table", &["csv"])],
    );
    let result = run(&f, &req).unwrap();
    check_case_probe(&f, &result);
}

// --------------------------------------------------------- held checks ----

/// The container has no capabilities, cannot gain privileges, sees no Docker
/// or other host sockets, and has exactly two host bind mounts: the read-only
/// working copy and the output area.
#[test]
#[ignore = "needs Docker Desktop and the local image python:3.13-slim; run with EXPOSITION_DOCKER_TESTS=1 -- --include-ignored"]
fn review_m8_held_container_privileges_sockets_and_mounts() {
    if !docker_enabled() {
        return;
    }
    let f = fixture(Some(IMAGE), false);
    let data = write(&f.root, "data/in.csv", b"a\n1\n");
    std::fs::write(f.root.join("undeclared.txt"), "not declared").unwrap();
    let code = r#"
import os, glob, json
status = dict(l.split(":\t", 1) for l in open("/proc/self/status").read().splitlines() if ":\t" in l)
print("CapEff", status.get("CapEff", "?").strip())
print("CapBnd", status.get("CapBnd", "?").strip())
print("NoNewPrivs", status.get("NoNewPrivs", "?").strip())
socks = [p for p in ["/var/run/docker.sock", "/run/docker.sock", "/var/run/containerd/containerd.sock"] if os.path.exists(p)]
for root, dirs, files in os.walk("/run"):
    socks += [os.path.join(root, n) for n in files if n.endswith(".sock")]
print("SOCKETS", json.dumps(socks))
mounts = []
for line in open("/proc/self/mountinfo"):
    parts = line.split()
    mounts.append((parts[4], parts[5]))
print("MOUNTS", json.dumps(mounts))
ws = []
for root, dirs, files in os.walk("/workspace"):
    ws += [os.path.relpath(os.path.join(root, n), "/workspace") for n in files]
print("WORKSPACE", json.dumps(sorted(ws)))
try:
    open("/workspace/new.txt", "w").write("x"); print("WORKSPACE_WRITABLE")
except OSError as e:
    print("workspace read-only", type(e).__name__)
try:
    open("/etc/xp", "w").write("x"); print("ROOTFS_WRITABLE")
except OSError as e:
    print("rootfs read-only", type(e).__name__)
"#;
    let result = run(&f, &request(BatchMode::Restricted, code, vec![data], Vec::new())).unwrap();
    assert_eq!(result.status, RunStatus::Succeeded, "{}", result.log);
    let line = |prefix: &str| {
        result
            .log
            .lines()
            .find(|l| l.starts_with(prefix))
            .map(|l| l[prefix.len()..].trim().to_owned())
            .unwrap_or_else(|| panic!("{prefix} missing: {}", result.log))
    };
    assert_eq!(line("CapEff"), "0000000000000000", "no effective capabilities");
    assert_eq!(line("NoNewPrivs"), "1", "no-new-privileges");
    let sockets: Vec<String> = serde_json::from_str(&line("SOCKETS")).unwrap();
    assert!(sockets.is_empty(), "host sockets reachable: {sockets:?}");
    let mounts: Vec<(String, String)> = serde_json::from_str(&line("MOUNTS")).unwrap();
    eprintln!("mounts: {mounts:#?}");
    for target in ["/workspace", "/out"] {
        assert!(mounts.iter().any(|(t, _)| t == target), "{target} mounted");
    }
    let ws_opts = &mounts.iter().find(|(t, _)| t == "/workspace").unwrap().1;
    assert!(ws_opts.split(',').any(|o| o == "ro"), "/workspace is read-only: {ws_opts}");
    let workspace: Vec<String> = serde_json::from_str(&line("WORKSPACE")).unwrap();
    assert!(workspace.iter().all(|p| p == "data/in.csv" || p == "__exposition_main__.py"), "{workspace:?}");
    assert!(!result.log.contains("WORKSPACE_WRITABLE") && !result.log.contains("ROOTFS_WRITABLE"), "{}", result.log);
    assert_eq!(containers_of(&f), "");
}

/// Hostile names and contents in the output area are refused (never copied
/// into the project); a mislabelled active document cannot pass as an image.
#[test]
#[ignore = "needs Docker Desktop and the local image python:3.13-slim; run with EXPOSITION_DOCKER_TESTS=1 -- --include-ignored"]
fn review_m8_held_hostile_output_names_and_contents() {
    if !docker_enabled() {
        return;
    }
    let f = fixture(Some(IMAGE), false);
    let code = r#"
import os
out = os.environ["EXPOSITION_OUTPUT_DIR"]
def attempt(name, data):
    try:
        with open(os.path.join(out, name), "w", encoding="utf-8") as fh:
            fh.write(data)
        print("made", repr(name))
    except Exception as e:
        print("failed", repr(name), type(e).__name__)
attempt("tаble.csv", "a\n1\n")          # Cyrillic a (confusable)
attempt("CON.csv", "a\n")
attempt("nul.txt", "a\n")
attempt("com1.json", "{}")
attempt("lpt9.csv", "a\n")
attempt("figure.png:evil", "x")               # NTFS alternate stream syntax
attempt("table.csv.", "a\n")
attempt("dataset.csv ", "a\n")
attempt("x" * 120 + ".csv", "a\n")
attempt("figure.png", "<svg xmlns='http://www.w3.org/2000/svg'><script>alert(1)</script></svg>")
attempt("figure.gif", "<html><script>alert(1)</script></html>")
attempt("dataset.json", "[" * 100000 + "]" * 100000)
attempt("dataset.txt", "ok\x00binary")
attempt("transcript.json", "{}")
attempt("ok.csv", "a\n1\n")
print("listing", sorted(os.listdir(out)))
"#;
    let req = request(
        BatchMode::Restricted,
        code,
        Vec::new(),
        vec![port("figure", &["png", "gif"]), port("dataset", &["json", "txt", "csv"]), port("table", &["csv"])],
    );
    let result = run(&f, &req).unwrap();
    eprintln!("log:\n{}\nrejected: {:#?}", result.log, result.rejected);
    assert_eq!(result.status, RunStatus::Succeeded, "{}", result.log);
    let accepted: Vec<String> =
        all_records(&result).iter().map(|r| r.id.0.rsplit('/').next().unwrap().to_owned()).collect();
    assert_eq!(accepted, ["ok.csv"], "only the plain safe file is accepted: {:?}", result.rejected);
    assert!(result.ports.is_empty(), "no port realized by a hostile file: {:?}", result.ports);
    let dir = artifact_dir(&f, &result.research_run);
    assert_eq!(names_in(&dir), ["ok.csv", "transcript.json"]);
    let transcript = std::fs::read_to_string(dir.join("transcript.json")).unwrap();
    assert!(!transcript.contains("alert(1)") || transcript.contains("not exported"), "{transcript}");
    assert_eq!(containers_of(&f), "");
}

/// A process the batch leaves behind (new session, delayed write) cannot add
/// or swap outputs after the batch's outcome is decided: the container ends
/// with its main process.
#[test]
#[ignore = "needs Docker Desktop and the local image python:3.13-slim; run with EXPOSITION_DOCKER_TESTS=1 -- --include-ignored"]
fn review_m8_held_late_writer_cannot_add_outputs() {
    if !docker_enabled() {
        return;
    }
    let f = fixture(Some(IMAGE), false);
    let code = r#"
import os, subprocess, sys
out = os.environ["EXPOSITION_OUTPUT_DIR"]
open(os.path.join(out, "table.csv"), "w").write("a\n1\n")
late = "import os, time\ntime.sleep(3)\nopen(os.path.join(%r, 'late.csv'), 'w').write('late\\n')\nos.remove(os.path.join(%r, 'table.csv'))\nos.symlink('/etc/passwd', os.path.join(%r, 'table.csv'))\n" % (out, out, out)
subprocess.Popen([sys.executable, "-c", late], start_new_session=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
print("main exits")
"#;
    let started = Instant::now();
    let result = run(&f, &request(BatchMode::Restricted, code, Vec::new(), vec![port("table", &["csv"])])).unwrap();
    assert_eq!(result.status, RunStatus::Succeeded, "{}", result.log);
    assert!(started.elapsed() < Duration::from_secs(60));
    std::thread::sleep(Duration::from_secs(6));
    let dir = artifact_dir(&f, &result.research_run);
    assert_eq!(names_in(&dir), ["table.csv", "transcript.json"], "nothing was added later");
    let table = std::fs::read_to_string(dir.join("table.csv")).unwrap();
    assert_eq!(table, "a\n1\n");
    let jobs = f.data.join("execution").join("jobs");
    assert!(names_in(&jobs).is_empty(), "job output areas removed: {:?}", names_in(&jobs));
    assert_eq!(containers_of(&f), "", "no container outlived the batch");
}
