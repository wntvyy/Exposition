#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use exposition_core::job::{EnvironmentSpec, ExecutionPolicy, JobId, JobSpec, OutputStream};
use exposition_execution::env::HostEnv;
use exposition_execution::{ExecutionConfig, ExecutionService, ExecutionSettings};

/// A fake provider key placed in the service's host environment.
pub const FAKE_ANTHROPIC_KEY: &str = "sk-ant-api03-FAKEFAKEFAKE-test-only-0123456789";
pub const FAKE_GITHUB_TOKEN: &str = "fake-github-token-value-abcdef";

pub fn testproc() -> String {
    env!("CARGO_BIN_EXE_exposition-testproc").to_string()
}

pub struct Fixture {
    pub tmp: tempfile::TempDir,
    pub service: ExecutionService,
    pub workspace: PathBuf,
    pub data_dir: PathBuf,
}

pub fn host_env() -> HostEnv {
    HostEnv::from_pairs(std::env::vars().filter(|(k, _)| k != "ANTHROPIC_API_KEY" && k != "GITHUB_TOKEN").chain([
        ("ANTHROPIC_API_KEY".to_string(), FAKE_ANTHROPIC_KEY.to_string()),
        ("GITHUB_TOKEN".to_string(), FAKE_GITHUB_TOKEN.to_string()),
        ("EXPOSITION_TEST_HOST_ONLY".to_string(), "host-only-value".to_string()),
    ]))
}

pub fn fixture_with(settings: ExecutionSettings, trusted: bool) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let service = ExecutionService::with_config(ExecutionConfig {
        data_dir: data_dir.clone(),
        host_env: host_env(),
        settings: Some(settings),
        too_broad_to_trust: Some(vec![]),
        // Shared across test runs so self-extracting tools (biber) stay warm.
        tool_cache_dir: Some(PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("exposition-tool-cache")),
    })
    .unwrap();
    let workspace = service.register_workspace_root(&workspace).unwrap();
    if trusted {
        service.grant_trust(&workspace.display().to_string()).unwrap();
    }
    Fixture { tmp, service, workspace, data_dir }
}

pub fn fixture() -> Fixture {
    fixture_with(ExecutionSettings::default(), true)
}

pub fn spec_in(dir: &Path, args: &[&str]) -> JobSpec {
    JobSpec {
        label: format!("testproc {}", args.first().copied().unwrap_or("")),
        program: testproc(),
        args: args.iter().map(|s| s.to_string()).collect(),
        environment: EnvironmentSpec::default(),
        working_directory: dir.display().to_string(),
        policy: ExecutionPolicy::TrustedNative,
        timeout_ms: Some(60_000),
        log_limit_bytes: None,
        stdin: None,
    }
}

impl Fixture {
    pub fn spec(&self, args: &[&str]) -> JobSpec {
        spec_in(&self.workspace, args)
    }

    pub fn stdout(&self, id: &JobId) -> String {
        self.service.output_text(id, OutputStream::Stdout).unwrap_or_default()
    }

    /// Wait until stdout satisfies `pred` (or panic after `timeout`).
    pub async fn wait_stdout(&self, id: &JobId, timeout: Duration, pred: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + timeout;
        loop {
            let text = self.stdout(id);
            if pred(&text) {
                return text;
            }
            assert!(Instant::now() < deadline, "timed out waiting for output; got {text:?}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// Parse `pid=<n>` / `orphan=<n>` values from testproc output.
pub fn pids(text: &str) -> Vec<u32> {
    text.lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter_map(|w| w.strip_prefix("pid=").or_else(|| w.strip_prefix("orphan=")))
        .filter_map(|n| n.parse().ok())
        .collect()
}

#[cfg(windows)]
pub fn alive(pid: u32) -> bool {
    let out = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
        .expect("tasklist");
    String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
}

#[cfg(unix)]
pub fn alive(pid: u32) -> bool {
    std::process::Command::new("kill").args(["-0", &pid.to_string()]).status().map(|s| s.success()).unwrap_or(false)
}

/// Wait until none of the pids are alive.
pub async fn assert_all_dead(pids: &[u32]) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let living: Vec<u32> = pids.iter().copied().filter(|p| alive(*p)).collect();
        if living.is_empty() {
            return;
        }
        assert!(Instant::now() < deadline, "processes still alive: {living:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
