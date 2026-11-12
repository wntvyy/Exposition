//! R-sec (M6 security review): execution, trust, and the restricted backend,
//! exercised against the real execution service with negative cases.
//!
//! Tests marked `#[ignore = "R-sec ..."]` FAIL on the product (findings); the
//! others record behavior that held. Docker tests use only the locally present
//! image `python:3.13-slim` (never pulled) and print `SKIPPED` without it.
//! Run: `cargo test -p exposition-execution --test review_sec_exec -- --include-ignored --nocapture`.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::*;
use exposition_core::job::{ExecutionPolicy, JobOutcome, JobSpec};
use exposition_execution::{ExecutionSettings, RefusalCode, TrustError};

const IMAGE: &str = "python:3.13-slim";

fn image_present() -> bool {
    let present = std::process::Command::new("docker")
        .args(["image", "inspect", "--format", "{{.Id}}", IMAGE])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !present {
        eprintln!("SKIPPED: local image {IMAGE} not present (never pulled)");
    }
    present
}

fn docker_fixture(image: &str) -> Fixture {
    let settings = ExecutionSettings {
        docker: exposition_execution::DockerSettings { image: Some(image.into()), ..Default::default() },
        ..Default::default()
    };
    fixture_with(settings, false)
}

fn python_in_container(f: &Fixture, script: &str) -> JobSpec {
    let mut spec = f.spec(&[]);
    spec.program = "python3".into();
    spec.args = vec!["-c".into(), script.into()];
    spec.policy = ExecutionPolicy::Restricted { backend: "docker".into() };
    spec.timeout_ms = Some(120_000);
    spec
}

#[cfg(windows)]
fn junction(link: &Path, target: &Path) {
    let status = std::process::Command::new("cmd")
        .args(["/c", "mklink", "/J", &link.display().to_string(), &target.display().to_string()])
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "mklink /J failed");
}

/// HELD: the container runs as a non-root user without capabilities or new
/// privileges, has no network interface but loopback, no Docker socket, a
/// read-only root and workspace, and no host environment.
#[tokio::test(flavor = "multi_thread")]
async fn review_sec_docker_python_isolation_held() {
    if !image_present() {
        return;
    }
    let f = docker_fixture(IMAGE);
    std::fs::write(f.workspace.join("input.txt"), "project data").unwrap();
    let script = r#"
import json, os, socket
r = {"uid": os.getuid(), "gid": os.getgid(), "env": sorted(os.environ)}
status = open("/proc/self/status").read()
r["capeff"] = [l.split()[1] for l in status.splitlines() if l.startswith("CapEff")][0]
r["nonewprivs"] = [l.split()[1] for l in status.splitlines() if l.startswith("NoNewPrivs")][0]
r["ifaces"] = sorted(os.listdir("/sys/class/net"))
r["docker_sock"] = os.path.exists("/var/run/docker.sock") or os.path.exists("/run/docker.sock")
def attempt(name, fn):
    try:
        fn(); r[name] = "ok"
    except Exception as e:
        r[name] = type(e).__name__
attempt("net", lambda: socket.create_connection(("1.1.1.1", 53), timeout=3))
attempt("dns", lambda: socket.getaddrinfo("example.com", 80))
attempt("write_ws", lambda: open("/workspace/x.txt", "w").write("x"))
attempt("write_root", lambda: open("/etc/x.txt", "w").write("x"))
mounts = [l.split()[4] for l in open("/proc/self/mountinfo")]
r["mounts"] = sorted(m for m in mounts if not m.startswith(("/proc", "/sys", "/dev")))
print(json.dumps(r))
"#;
    let id = f.service.launch(python_in_container(&f, script));
    let result = f.service.wait(&id).await.unwrap();
    let out = f.stdout(&id);
    println!("docker isolation: {result:?}\n{out}");
    assert_eq!(result.outcome, JobOutcome::Exited { code: 0 }, "{out}");
    let r: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_ne!(r["uid"], 0, "root in container");
    assert_eq!(r["capeff"], "0000000000000000");
    assert_eq!(r["nonewprivs"], "1");
    assert_eq!(r["ifaces"], serde_json::json!(["lo"]));
    assert_eq!(r["docker_sock"], false);
    assert_ne!(r["net"], "ok");
    assert_ne!(r["dns"], "ok");
    assert_eq!(r["write_ws"], "OSError");
    assert_eq!(r["write_root"], "OSError");
    let env: Vec<String> = serde_json::from_value(r["env"].clone()).unwrap();
    for name in &env {
        assert!(
            [
                "PATH",
                "HOME",
                "TMPDIR",
                "LANG",
                "HOSTNAME",
                "PWD",
                "GPG_KEY",
                "PYTHON_VERSION",
                "PYTHON_SHA256",
                "LC_CTYPE"
            ]
            .contains(&name.as_str()),
            "unexpected container variable {name}"
        );
    }
}

/// R-sec F-docker-junction: a junction inside the (read-only) project mount
/// that points outside the project. Expected: the restricted container sees
/// only the project; a file outside it is not readable. Observed: see output.
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread")]
async fn review_sec_docker_junction_inside_project_does_not_expose_outside() {
    if !image_present() {
        return;
    }
    let f = docker_fixture(IMAGE);
    let outside = f.tmp.path().join("outside-secret");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "OUTSIDE-SECRET-4711").unwrap();
    junction(&f.workspace.join("escape"), &outside);
    let script = r#"
import os
for p in ["/workspace/escape/secret.txt"]:
    try:
        print("READ", p, open(p).read())
    except Exception as e:
        print("REFUSED", p, type(e).__name__, e)
try:
    print("LIST", os.listdir("/workspace/escape"), os.path.islink("/workspace/escape"), os.readlink("/workspace/escape") if os.path.islink("/workspace/escape") else "")
except Exception as e:
    print("LISTERR", type(e).__name__, e)
"#;
    let id = f.service.launch(python_in_container(&f, script));
    let result = f.service.wait(&id).await.unwrap();
    let out = f.stdout(&id);
    println!("junction probe: {result:?}\n{out}");
    assert!(
        !out.contains("OUTSIDE-SECRET-4711"),
        "DEFECT: restricted container read a file outside the project through a junction:\n{out}"
    );
}

/// HELD: a missing image refuses the job (no pull, no native fallback).
#[tokio::test(flavor = "multi_thread")]
async fn review_sec_docker_missing_image_is_refused_without_pull_or_fallback() {
    if !image_present() {
        return;
    }
    let image = "exposition-review-sec-absent:never";
    let f = docker_fixture(image);
    let marker = f.workspace.join("ran-natively.txt");
    let mut spec = python_in_container(&f, &format!("open(r'{}', 'w').write('x')", marker.display()));
    spec.program = "python".into(); // also a native tool id: a fallback would run it
    let id = f.service.launch(spec);
    let result = f.service.wait(&id).await.unwrap();
    println!("missing image: {result:?} {:?}", f.service.summary(&id).and_then(|s| s.refusal));
    assert!(matches!(result.outcome, JobOutcome::FailedToStart { .. }));
    assert_eq!(f.service.summary(&id).unwrap().refusal.unwrap().code, RefusalCode::RestrictedBackendUnavailable);
    assert!(!marker.exists(), "ran natively");
    let listed = std::process::Command::new("docker").args(["image", "ls", "-q", image]).output().unwrap();
    assert!(String::from_utf8_lossy(&listed.stdout).trim().is_empty(), "image was pulled");
}

/// HELD: a broken Docker client path refuses restricted jobs (no fallback).
#[tokio::test(flavor = "multi_thread")]
async fn review_sec_docker_missing_client_is_refused() {
    let mut settings = ExecutionSettings {
        docker: exposition_execution::DockerSettings { image: Some(IMAGE.into()), ..Default::default() },
        ..Default::default()
    };
    settings.tool_paths.insert("docker".into(), r"C:\definitely\not\docker.exe".into());
    let f = fixture_with(settings, true);
    let marker = f.workspace.join("ran-natively.txt");
    let mut spec = f.spec(&[]);
    spec.program = "python".into();
    spec.args = vec!["-c".into(), format!("open(r'{}', 'w').write('x')", marker.display())];
    spec.policy = ExecutionPolicy::Restricted { backend: "docker".into() };
    let id = f.service.launch(spec);
    let result = f.service.wait(&id).await.unwrap();
    assert!(matches!(result.outcome, JobOutcome::FailedToStart { .. }), "{result:?}");
    assert_eq!(f.service.summary(&id).unwrap().refusal.unwrap().code, RefusalCode::RestrictedBackendUnavailable);
    assert!(!marker.exists());
}

/// HELD: native children start from an empty environment: credential-like
/// names in any case and interpreter/TeX influence variables of the app are
/// not inherited.
#[tokio::test(flavor = "multi_thread")]
async fn review_sec_native_env_excludes_host_credentials_and_influence_vars() {
    let hostile = [
        ("ANTHROPIC_API_KEY", "sk-ant-api03-REVIEWSECFAKE-000000000000"),
        ("anthropic_api_key", "lower-case-variant-value-1"),
        ("Aws_Secret_Access_Key", "mixed-case-variant-value-2"),
        ("MY_PASSWORD", "hunter2-review-sec"),
        ("SENTRY_DSN", "https://user:pass@sentry.invalid/1"),
        ("PYTHONPATH", r"C:\evil\pythonpath"),
        ("PYTHONSTARTUP", r"C:\evil\startup.py"),
        ("PYTHONHOME", r"C:\evil\home"),
        ("PERL5OPT", "-Mevil"),
        ("PERL5LIB", r"C:\evil\perl"),
        ("TEXMFHOME", r"C:\evil\texmf"),
        ("TEXINPUTS", r"C:\evil\tex//;"),
        ("NODE_OPTIONS", "--require C:/evil.js"),
        ("LUA_PATH", r"C:\evil\?.lua"),
        ("R_PROFILE_USER", r"C:\evil\.Rprofile"),
        ("JULIA_DEPOT_PATH", r"C:\evil\julia"),
        ("LEAN_PATH", r"C:\evil\lean"),
        ("ELAN_TOOLCHAIN", "evil/lean4:v0"),
        ("COR_ENABLE_PROFILING", "1"),
        ("COR_PROFILER_PATH", r"C:\evil\prof.dll"),
    ];
    let host = exposition_execution::env::HostEnv::from_pairs(
        std::env::vars().chain(hostile.iter().map(|(k, v)| (k.to_string(), v.to_string()))),
    );
    let tmp = tempfile::tempdir().unwrap();
    let service = exposition_execution::ExecutionService::with_config(exposition_execution::ExecutionConfig {
        data_dir: tmp.path().join("data"),
        host_env: host,
        settings: Some(ExecutionSettings::default()),
        too_broad_to_trust: Some(vec![]),
        tool_cache_dir: None,
    })
    .unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let ws = service.register_workspace_root(&ws).unwrap();
    service.grant_trust(&ws.display().to_string()).unwrap();
    let id = service.launch(spec_in(&ws, &["env"]));
    let result = service.wait(&id).await.unwrap();
    assert_eq!(result.outcome, JobOutcome::Exited { code: 0 });
    let out = service.output_text(&id, exposition_core::job::OutputStream::Stdout).unwrap_or_default();
    let names: Vec<String> =
        out.lines().filter_map(|l| l.split_once('=').map(|(k, _)| k.to_ascii_uppercase())).collect();
    println!("child env names: {names:?}");
    for (name, value) in hostile {
        assert!(!names.contains(&name.to_ascii_uppercase()), "{name} reached the child");
        // Short values ("1") occur in ordinary variables; check distinctive ones.
        if value.len() >= 8 {
            assert!(!out.contains(value), "value of {name} reached the child");
        }
    }
    assert!(!out.to_ascii_lowercase().contains(r"c:\evil"));
}

/// HELD: a child cannot break away from the job object (no BREAKAWAY_OK),
/// and the whole tree dies on cancel.
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread")]
async fn review_sec_breakaway_from_job_is_refused() {
    let f = fixture();
    let script = r#"
import subprocess, sys, time
try:
    p = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(120)"], creationflags=0x01000000)
    print("BREAKAWAY-STARTED pid=%d" % p.pid, flush=True)
except OSError as e:
    print("BREAKAWAY-REFUSED", e.winerror, flush=True)
time.sleep(120)
"#;
    let mut spec = f.spec(&[]);
    spec.program = "python".into();
    spec.args = vec!["-E".into(), "-u".into(), "-c".into(), script.into()];
    let id = f.service.launch(spec);
    let out = f.wait_stdout(&id, Duration::from_secs(30), |t| t.contains("BREAKAWAY-")).await;
    println!("{out}");
    f.service.cancel(&id);
    let _ = f.service.wait(&id).await;
    if let Some(pid) = out.split("pid=").nth(1).and_then(|s| s.trim().parse::<u32>().ok()) {
        let alive_after = alive(pid);
        if alive_after {
            let _ = std::process::Command::new("taskkill").args(["/PID", &pid.to_string(), "/F"]).status();
        }
        panic!("DEFECT: a child broke away from the job object (pid {pid}, alive after cancel: {alive_after})");
    }
    assert!(out.contains("BREAKAWAY-REFUSED"));
}

/// HELD: revoking a parent folder's trust stops a running job of a project
/// that was trusted only through that parent, before the revoke returns.
#[tokio::test(flavor = "multi_thread")]
async fn review_sec_revoke_of_parent_stops_child_project_job() {
    let f = fixture_with(ExecutionSettings::default(), false);
    let parent = f.workspace.clone();
    let child = parent.join("child-project");
    std::fs::create_dir_all(&child).unwrap();
    let child = f.service.register_workspace_root(&child).unwrap();
    f.service.grant_trust(&parent.display().to_string()).unwrap();
    assert!(f.service.trust_status(&child.display().to_string()).unwrap().trusted);
    let id = f.service.launch(spec_in(&child, &["tree", "2", "120"]));
    let out = f.wait_stdout(&id, Duration::from_secs(20), |t| pids(t).len() >= 3).await;
    let tree = pids(&out);
    let started = Instant::now();
    // Revoke through a different spelling of the same folder (case + trailing separator).
    let spelled = format!("{}\\", parent.display().to_string().to_uppercase());
    f.service.revoke_trust(&spelled).unwrap();
    let took = started.elapsed();
    // Contract: when the revoke returns, the job's process tree is gone (its
    // result is recorded a moment later, after its output readers drain).
    let living: Vec<u32> = tree.iter().copied().filter(|p| alive(*p)).collect();
    println!("revoke returned after {took:?}; still alive at return: {living:?}");
    assert!(living.is_empty(), "DEFECT: processes alive when revoke returned: {living:?}");
    let result = f.service.wait(&id).await.unwrap();
    assert_eq!(result.outcome, JobOutcome::Canceled);
    let log =
        f.stdout(&id) + &f.service.output_text(&id, exposition_core::job::OutputStream::Stderr).unwrap_or_default();
    println!("log tail: {}", log.lines().rev().take(2).collect::<Vec<_>>().join(" | "));
    assert!(!f.service.trust_status(&child.display().to_string()).unwrap().trusted);
    // And an untrusted launch is refused afterwards.
    let id = f.service.launch(spec_in(&child, &["print", "x"]));
    let r = f.service.wait(&id).await.unwrap();
    assert!(matches!(r.outcome, JobOutcome::FailedToStart { .. }));
}

/// R-sec F-unc-alias: the broad-root rule is bypassed through the local admin
/// share alias of a refused folder. `C:\Users\<me>` is refused, but
/// `\\localhost\C$\Users\<me>` (the same folder) is accepted as a trust root.
/// Expected: `TooBroad`. (Skips if the admin share is not reachable.)
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread")]
async fn review_sec_trust_refuses_admin_share_alias_of_broad_folders() {
    let f = fixture_with(ExecutionSettings::default(), false);
    let profile = PathBuf::from(std::env::var("USERPROFILE").unwrap());
    let drive = profile.to_string_lossy().chars().next().unwrap();
    let rest = profile.to_string_lossy()[2..].to_owned(); // \Users\<me>
    let windows = std::env::var("SystemRoot").unwrap()[2..].to_owned();
    // Control: the direct form is refused.
    assert!(matches!(f.service.grant_trust(&profile.display().to_string()), Err(TrustError::TooBroad(_))));
    let mut accepted = Vec::new();
    for alias in [
        format!(r"\\localhost\{drive}$\Users"),
        format!(r"\\localhost\{drive}${rest}"),
        format!(r"\\127.0.0.1\{drive}${rest}"),
        format!(r"\\localhost\{drive}${windows}"),
        format!(r"\\localhost\{drive}${windows}\System32"),
    ] {
        if !Path::new(&alias).is_dir() {
            eprintln!("SKIPPED alias {alias}: not reachable");
            continue;
        }
        // workspace_open uses the same rule on the canonical path.
        let canonical = exposition_execution::paths::canonicalize(Path::new(&alias)).unwrap();
        match exposition_core::roots::too_broad_reason(&canonical) {
            Some(reason) => println!("ok: open rule refuses {alias}: {reason}"),
            None => println!("DEFECT: open rule accepts {alias} (canonical {})", canonical.display()),
        }
        match f.service.grant_trust(&alias) {
            Ok(status) => {
                println!("DEFECT: trusted {alias} -> {status:?}");
                accepted.push(alias.clone());
                let _ = f.service.revoke_trust(&alias);
            }
            Err(e) => println!("ok: {alias} refused: {e}"),
        }
    }
    assert!(accepted.is_empty(), "broad folders trusted through admin-share aliases: {accepted:?}");
}
