//! Restricted (Docker) backend enforcement tests.
//!
//! These run only when `EXPOSITION_TEST_DOCKER_IMAGE` names a locally present
//! image that provides `node` (e.g. `node:26-trixie-slim`); otherwise each test
//! prints `SKIPPED` and passes. They never pull images.

mod common;

use std::time::{Duration, Instant};

use common::*;
use exposition_core::job::{ExecutionPolicy, JobOutcome, JobSpec};
use exposition_execution::{ExecutionSettings, RefusalCode};

const IMAGE_ENV: &str = "EXPOSITION_TEST_DOCKER_IMAGE";

fn image() -> Option<String> {
    match std::env::var(IMAGE_ENV) {
        Ok(image) if !image.is_empty() => Some(image),
        _ => {
            eprintln!("SKIPPED: set {IMAGE_ENV} to a local image with node (e.g. node:26-trixie-slim)");
            None
        }
    }
}

/// Put a fake provider key into this test process's real environment so the
/// test proves it does not reach the container through any path.
#[allow(unsafe_code)]
fn set_fake_host_secret() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // SAFETY: done once, before the service under test is constructed; on
        // Windows the environment block is guarded by the OS/std lock, and these
        // tests do not read the environment concurrently from foreign code.
        unsafe { std::env::set_var("ANTHROPIC_API_KEY", FAKE_ANTHROPIC_KEY) };
    });
    assert_eq!(std::env::var("ANTHROPIC_API_KEY").as_deref(), Ok(FAKE_ANTHROPIC_KEY));
}

fn docker_fixture(image: &str) -> Fixture {
    set_fake_host_secret();
    let settings = ExecutionSettings {
        docker: exposition_execution::DockerSettings { image: Some(image.into()), ..Default::default() },
        ..Default::default()
    };
    // Restricted execution does not require workspace trust.
    fixture_with(settings, false)
}

fn node(f: &Fixture, script: &str) -> JobSpec {
    let mut spec = f.spec(&[]);
    spec.program = "node".into();
    spec.args = vec!["-e".into(), script.into()];
    spec.policy = ExecutionPolicy::Restricted { backend: "docker".into(), image_id: None };
    spec.timeout_ms = Some(120_000);
    spec
}

fn container_listed(job_id: &str) -> bool {
    let name = format!("name=^exposition-{job_id}$");
    let out = std::process::Command::new("docker").args(["ps", "-aq", "--filter", &name]).output().expect("docker ps");
    !String::from_utf8_lossy(&out.stdout).trim().is_empty()
}

#[tokio::test(flavor = "multi_thread")]
async fn docker_network_is_blocked() {
    let Some(image) = image() else { return };
    let f = docker_fixture(&image);
    let script = r#"
const net = require('net'), dns = require('dns');
const r = {};
const done = () => {
  if (!('tcp' in r) || !('dns' in r)) return;
  console.log(JSON.stringify(r));
  process.exit(r.tcp === 'blocked' && r.dns === 'blocked' ? 0 : 1);
};
const s = net.connect({ host: '1.1.1.1', port: 443, timeout: 4000 });
s.on('connect', () => { r.tcp = 'connected'; s.destroy(); done(); });
s.on('error', e => { r.tcp = 'blocked'; r.tcpError = e.code; done(); });
s.on('timeout', () => { r.tcp = 'blocked'; r.tcpError = 'timeout'; s.destroy(); done(); });
dns.lookup('example.com', e => { r.dns = e ? 'blocked' : 'resolved'; r.dnsError = e && e.code; done(); });
"#;
    let id = f.service.launch(node(&f, script));
    let result = f.service.wait(&id).await.unwrap();
    let out = f.stdout(&id);
    eprintln!("network probe: {result:?} {out}");
    assert_eq!(result.outcome, JobOutcome::Exited { code: 0 }, "{out}");
    assert!(out.contains(r#""tcp":"blocked""#) && out.contains(r#""dns":"blocked""#), "{out}");
}

#[tokio::test(flavor = "multi_thread")]
async fn docker_workspace_is_read_only_and_out_is_writable() {
    let Some(image) = image() else { return };
    let f = docker_fixture(&image);
    std::fs::write(f.workspace.join("input.txt"), "project data").unwrap();
    let sub = f.workspace.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let script = r#"
const fs = require('fs');
const r = { cwd: process.cwd(), uid: process.getuid() };
r.read = fs.readFileSync('/workspace/input.txt', 'utf8');
const attempt = (name, fn) => { try { fn(); r[name] = 'ok'; } catch (e) { r[name] = e.code; } };
attempt('writeWorkspace', () => fs.writeFileSync('/workspace/escaped.txt', 'x'));
attempt('writeWorkspaceSub', () => fs.writeFileSync('/workspace/sub/escaped.txt', 'x'));
attempt('writeRootfs', () => fs.writeFileSync('/etc/escaped.txt', 'x'));
attempt('writeTmp', () => fs.writeFileSync('/tmp/scratch.txt', 'x'));
attempt('writeOut', () => fs.writeFileSync('/out/result.txt', 'result from container'));
console.log(JSON.stringify(r));
"#;
    let mut spec = node(&f, script);
    spec.working_directory = sub.display().to_string();
    let id = f.service.launch(spec);
    let result = f.service.wait(&id).await.unwrap();
    let out = f.stdout(&id);
    eprintln!("filesystem probe: {result:?} {out}");
    assert_eq!(result.outcome, JobOutcome::Exited { code: 0 }, "{out}");
    let r: serde_json::Value = serde_json::from_str(out.trim()).expect("json");
    assert_eq!(r["cwd"], "/workspace/sub");
    assert_eq!(r["uid"], 65534, "runs as a non-root user");
    assert_eq!(r["read"], "project data");
    assert_eq!(r["writeWorkspace"], "EROFS");
    assert_eq!(r["writeWorkspaceSub"], "EROFS");
    assert_eq!(r["writeRootfs"], "EROFS");
    assert_eq!(r["writeTmp"], "ok");
    assert_eq!(r["writeOut"], "ok");
    assert!(!f.workspace.join("escaped.txt").exists() && !sub.join("escaped.txt").exists());
    let produced = f.service.job_dir(&id).join("out").join("result.txt");
    assert_eq!(std::fs::read_to_string(&produced).unwrap(), "result from container");
}

#[tokio::test(flavor = "multi_thread")]
async fn docker_container_never_sees_host_secrets() {
    let Some(image) = image() else { return };
    let f = docker_fixture(&image);
    let mut spec = node(&f, "console.log(JSON.stringify(process.env))");
    spec.environment.set = [("EXPOSITION_VISIBLE".to_string(), "yes".to_string())].into();
    let id = f.service.launch(spec);
    let result = f.service.wait(&id).await.unwrap();
    let out = f.stdout(&id);
    eprintln!("container env: {out}");
    assert_eq!(result.outcome, JobOutcome::Exited { code: 0 }, "{out}");
    let env: serde_json::Value = serde_json::from_str(out.trim()).expect("json");
    let env = env.as_object().unwrap();
    assert_eq!(env.get("EXPOSITION_VISIBLE").and_then(|v| v.as_str()), Some("yes"));
    for name in env.keys() {
        assert!(!name.contains("ANTHROPIC") && !name.contains("GITHUB_TOKEN"), "{name} leaked");
    }
    assert!(!out.contains("FAKEFAKE") && !out.contains(FAKE_GITHUB_TOKEN));
    // Asking for the credential explicitly refuses the job before docker runs.
    let mut leak = node(&f, "console.log(process.env.ANTHROPIC_API_KEY)");
    leak.environment.inherit = vec!["ANTHROPIC_API_KEY".into()];
    let id = f.service.launch(leak);
    let result = f.service.wait(&id).await.unwrap();
    assert!(matches!(result.outcome, JobOutcome::FailedToStart { .. }));
    assert_eq!(f.service.summary(&id).unwrap().refusal.unwrap().code, RefusalCode::EnvironmentRefused);
}

#[tokio::test(flavor = "multi_thread")]
async fn docker_cancel_and_timeout_kill_the_container() {
    let Some(image) = image() else { return };
    let f = docker_fixture(&image);
    let id = f.service.launch(node(&f, "setInterval(() => console.log('tick'), 200)"));
    f.wait_stdout(&id, Duration::from_secs(60), |t| t.contains("tick")).await;
    assert!(container_listed(&id.0), "container should be running");
    assert!(f.service.cancel(&id));
    assert_eq!(f.service.wait(&id).await.unwrap().outcome, JobOutcome::Canceled);
    let deadline = Instant::now() + Duration::from_secs(20);
    while container_listed(&id.0) {
        assert!(Instant::now() < deadline, "container survived cancel");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    let mut spec = node(&f, "setInterval(() => console.log('tick'), 200)");
    spec.timeout_ms = Some(6_000);
    let id = f.service.launch(spec);
    assert_eq!(f.service.wait(&id).await.unwrap().outcome, JobOutcome::TimedOut);
    let deadline = Instant::now() + Duration::from_secs(20);
    while container_listed(&id.0) {
        assert!(Instant::now() < deadline, "container survived timeout");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn docker_interactive_session_round_trips() {
    let Some(image) = image() else { return };
    let f = docker_fixture(&image);
    let script =
        "require('readline').createInterface({input: process.stdin}).on('line', l => console.log('echo: ' + l))";
    let mut job =
        f.service.launch_interactive(node(&f, script), Default::default()).await.expect("interactive container starts");
    for i in 0..3 {
        job.input.write_line(&format!("message {i}")).await.unwrap();
        let line = tokio::time::timeout(Duration::from_secs(30), job.output.next_line()).await.unwrap().unwrap();
        assert_eq!(line, format!("echo: message {i}"));
    }
    assert!(f.service.cancel(&job.id));
    assert_eq!(f.service.wait(&job.id).await.unwrap().outcome, JobOutcome::Canceled);
    let deadline = Instant::now() + Duration::from_secs(20);
    while container_listed(&job.id.0) {
        assert!(Instant::now() < deadline, "interactive container survived cancel");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// The application dies (here: only its docker client is killed, as the job
/// object does when the application exits): the container must stop by
/// itself, without the service's cleanup.
#[tokio::test(flavor = "multi_thread")]
async fn docker_container_stops_when_its_client_dies() {
    let Some(image) = image() else { return };
    let f = docker_fixture(&image);
    let id = f.service.launch(node(&f, "setInterval(() => console.log('tick'), 200)"));
    f.wait_stdout(&id, Duration::from_secs(60), |t| t.contains("tick")).await;
    assert!(container_listed(&id.0), "container should be running");
    let client = f.service.summary(&id).and_then(|s| s.pid).expect("docker client pid");
    // TerminateProcess on the client alone: no `docker kill`, no `docker rm`.
    let killed = if cfg!(windows) {
        std::process::Command::new("taskkill").args(["/PID", &client.to_string(), "/F"]).status()
    } else {
        std::process::Command::new("kill").args(["-9", &client.to_string()]).status()
    };
    assert!(killed.map(|s| s.success()).unwrap_or(false), "killed the docker client");
    let started = Instant::now();
    while container_running(&id.0) {
        assert!(started.elapsed() < Duration::from_secs(15), "the container outlived its client");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    eprintln!("container stopped {:?} after its client died", started.elapsed());
    let _ = f.service.wait(&id).await;
}

fn container_running(job_id: &str) -> bool {
    let name = format!("name=^exposition-{job_id}$");
    let out = std::process::Command::new("docker").args(["ps", "-q", "--filter", &name]).output().expect("docker ps");
    !String::from_utf8_lossy(&out.stdout).trim().is_empty()
}

/// The time limit holds inside the container, even with nobody left outside
/// to enforce it: the docker client is run directly, its stdin held open, and
/// nothing ever kills it.
#[tokio::test(flavor = "multi_thread")]
async fn docker_time_limit_is_enforced_inside_the_container() {
    use exposition_execution::docker::{ContainerStdin, RunPlan, TIME_LIMIT_GRACE, run_args};
    let Some(image) = image() else { return };
    let f = docker_fixture(&image);
    let out_dir = f.tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();
    let env_file = f.tmp.path().join("container.env");
    std::fs::write(&env_file, "").unwrap();
    let job_id = format!("timelimit-{}", std::process::id());
    let settings = exposition_execution::DockerSettings::default();
    let args = run_args(&RunPlan {
        job_id: &job_id,
        instance: "test-time-limit",
        image: &image,
        settings: &settings,
        mount_root: &f.workspace,
        workdir_components: Vec::new(),
        out_dir: &out_dir,
        env_file: &env_file,
        program: "node",
        args: &["-e".into(), "setInterval(() => {}, 1000)".into()],
        stdin: ContainerStdin::Batch { input_dir: None },
        time_limit: Some(Duration::from_secs(1)),
    });
    let started = Instant::now();
    let mut child = std::process::Command::new("docker")
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("docker run");
    let _stdin = child.stdin.take();
    let limit = Duration::from_secs(1) + TIME_LIMIT_GRACE;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            let elapsed = started.elapsed();
            eprintln!("container ended after {elapsed:?} with {status:?}");
            assert!(elapsed >= limit, "ended before its time limit");
            assert!(!status.success(), "the program was killed, not finished");
            break;
        }
        if started.elapsed() > limit + Duration::from_secs(30) {
            let _ = std::process::Command::new("docker").args(["rm", "-f", &format!("exposition-{job_id}")]).status();
            panic!("the container outlived its time limit");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(!container_listed(&job_id));
}

/// Containers an earlier session of the same data directory left running are
/// removed by the startup sweep; other containers are left alone.
#[tokio::test(flavor = "multi_thread")]
async fn docker_startup_sweep_removes_containers_left_by_an_earlier_session() {
    let Some(image) = image() else { return };
    let f = docker_fixture(&image);
    let stale = format!("exposition-stale-{}", std::process::id());
    let other = format!("exposition-other-{}", std::process::id());
    for (name, instance) in [(&stale, f.service.docker_instance().to_owned()), (&other, "another-instance".to_owned())]
    {
        let status = std::process::Command::new("docker")
            .args(["run", "-d", "--rm", "--pull=never", "--network=none", &format!("--name={name}")])
            .arg("--label=org.exposition.managed=1")
            .arg(format!("--label=org.exposition.instance={instance}"))
            .args(["--entrypoint=node", &image, "-e", "setInterval(() => {}, 1000)"])
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
    }
    let listed = |name: &str| {
        let out = std::process::Command::new("docker")
            .args(["ps", "-aq", "--filter", &format!("name=^{name}$")])
            .output()
            .unwrap();
        !String::from_utf8_lossy(&out.stdout).trim().is_empty()
    };
    assert!(listed(&stale) && listed(&other));
    assert_eq!(f.service.sweep_stale_containers(), Ok(1));
    let gone = !listed(&stale);
    let kept = listed(&other);
    let _ = std::process::Command::new("docker").args(["rm", "-f", &other]).status();
    assert!(gone, "the earlier session's container was removed");
    assert!(kept, "another data directory's container was left alone");
    // Swept once: a later call does nothing (a running job is never touched).
    assert_eq!(f.service.sweep_stale_containers(), Ok(0));
}

/// A restricted job's input (its stdin, e.g. approved agent code) is a file
/// in the job folder; it is removed when the job ends, whatever the outcome.
#[tokio::test(flavor = "multi_thread")]
async fn docker_job_input_is_removed_on_every_outcome() {
    let Some(image) = image() else { return };
    let f = docker_fixture(&image);
    let jobs = std::fs::canonicalize(f.data_dir.join("execution")).unwrap().join("jobs");
    let with_input = |script: &str, timeout_ms: u64| {
        let mut spec = node(&f, "");
        spec.args = vec!["-e".into(), script.into()];
        spec.stdin = Some("INPUT-MARKER approved code\n".into());
        spec.timeout_ms = Some(timeout_ms);
        spec
    };
    let input_of = |id: &exposition_core::job::JobId| jobs.join(&id.0).join("in");

    // Finished: the program read its input.
    let id = f.service.launch(with_input("process.stdin.on('data', d => process.stdout.write(d))", 120_000));
    let result = f.service.wait(&id).await.unwrap();
    assert_eq!(result.outcome, JobOutcome::Exited { code: 0 }, "{}", f.stdout(&id));
    assert!(f.stdout(&id).contains("INPUT-MARKER"), "the program got its input");
    assert!(!input_of(&id).exists(), "finished: input removed");

    // Canceled while running.
    let id = f.service.launch(with_input("setInterval(() => console.log('tick'), 200)", 120_000));
    f.wait_stdout(&id, Duration::from_secs(60), |t| t.contains("tick")).await;
    assert!(input_of(&id).join("stdin").exists(), "the input exists while the job runs");
    assert!(f.service.cancel(&id));
    assert_eq!(f.service.wait(&id).await.unwrap().outcome, JobOutcome::Canceled);
    assert!(!input_of(&id).exists(), "canceled: input removed");

    // Timed out.
    let id = f.service.launch(with_input("setInterval(() => console.log('tick'), 200)", 6_000));
    assert_eq!(f.service.wait(&id).await.unwrap().outcome, JobOutcome::TimedOut);
    assert!(!input_of(&id).exists(), "timed out: input removed");
}

/// A job pinned to an image identity runs only while the configured image has
/// that identity, and then starts from it (R-m8-F1).
#[tokio::test(flavor = "multi_thread")]
async fn docker_pinned_image_identity_is_enforced() {
    let Some(image) = image() else { return };
    let f = docker_fixture(&image);
    let out = std::process::Command::new("docker")
        .args(["image", "inspect", "--format", "{{.Id}}", &image])
        .output()
        .expect("docker image inspect");
    let actual = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    assert!(actual.starts_with("sha256:"), "{actual}");

    // The configured image's identity: runs.
    let mut spec = node(&f, "console.log('pinned ok')");
    spec.policy = ExecutionPolicy::Restricted { backend: "docker".into(), image_id: Some(actual.clone()) };
    let id = f.service.launch(spec);
    let result = f.service.wait(&id).await.unwrap();
    assert_eq!(result.outcome, JobOutcome::Exited { code: 0 }, "{}", f.stdout(&id));
    assert!(f.stdout(&id).contains("pinned ok"));

    // Another identity: refused before any container starts, never native.
    let other = format!("sha256:{}", "0".repeat(64));
    let mut spec = node(&f, "console.log('must not run')");
    spec.policy = ExecutionPolicy::Restricted { backend: "docker".into(), image_id: Some(other) };
    let id = f.service.launch(spec);
    let result = f.service.wait(&id).await.unwrap();
    assert!(matches!(result.outcome, JobOutcome::FailedToStart { .. }), "{:?}", result.outcome);
    assert_eq!(f.service.summary(&id).unwrap().refusal.unwrap().code, RefusalCode::RestrictedBackendUnavailable);
    assert!(!f.stdout(&id).contains("must not run"));
    assert!(!container_listed(&id.0));

    // Not an image identity at all: refused.
    let mut spec = node(&f, "console.log('must not run')");
    spec.policy = ExecutionPolicy::Restricted { backend: "docker".into(), image_id: Some(image.clone()) };
    let id = f.service.launch(spec);
    let result = f.service.wait(&id).await.unwrap();
    assert!(matches!(result.outcome, JobOutcome::FailedToStart { .. }), "{:?}", result.outcome);
}
