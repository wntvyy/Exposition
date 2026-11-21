//! Independent review of the single-query restricted preflight: the pin is
//! decided from the identity the engine printed in that one answer, for
//! every job anew. A stand-in client (a small program compiled by the test,
//! because the service rightly refuses batch files) prints whatever identity
//! the test wrote beside it.

mod common;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use common::*;
use exposition_core::job::{ExecutionPolicy, JobOutcome};
use exposition_execution::{DockerSettings, ExecutionSettings};

const PINNED: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OTHER: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// The stand-in client: logs each invocation beside itself; `image inspect`
/// prints `identity.txt` byte for byte (exit 0), or fails like an engine
/// without the image when a file `missing` exists; anything else exits 0.
const STAND_IN: &str = r#"
use std::io::Write;
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let me = std::env::current_exe().unwrap();
    let dir = me.parent().unwrap();
    let mut log = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("invocations.log")).unwrap();
    let _ = writeln!(log, "docker {}", args.join(" "));
    if args.first().map(String::as_str) == Some("image") {
        if dir.join("missing").exists() {
            eprintln!("Error response from daemon: No such image");
            std::process::exit(1);
        }
        let identity = std::fs::read(dir.join("identity.txt")).unwrap_or_default();
        let _ = std::io::stdout().write_all(&identity);
    }
}
"#;

/// Compiled once per test run.
fn stand_in_program() -> &'static Path {
    static BUILT: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
    let (_, exe) = BUILT.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("standin.rs");
        std::fs::write(&source, STAND_IN).unwrap();
        let exe = dir.path().join(if cfg!(windows) { "standin.exe" } else { "standin" });
        let status = std::process::Command::new("rustc")
            .args(["--edition", "2021", "-o"])
            .arg(&exe)
            .arg(&source)
            .status()
            .expect("rustc builds the stand-in client");
        assert!(status.success(), "the stand-in client did not compile");
        (dir, exe)
    });
    exe
}

fn stand_in(dir: &Path) -> PathBuf {
    let exe = dir.join(if cfg!(windows) { "docker.exe" } else { "docker" });
    std::fs::copy(stand_in_program(), &exe).unwrap();
    exe
}

fn invocations(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("invocations.log"))
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().to_owned())
        .collect()
}

struct Engine {
    tools: tempfile::TempDir,
    f: Fixture,
}

fn engine() -> Engine {
    let tools = tempfile::tempdir().unwrap();
    let mut settings = ExecutionSettings::default();
    settings.tool_paths.insert("docker".into(), stand_in(tools.path()).display().to_string());
    settings.docker = DockerSettings { image: Some("python:3.13-slim".into()), ..Default::default() };
    let f = fixture_with(settings, false);
    Engine { tools, f }
}

impl Engine {
    fn reports(&self, identity: &str) {
        std::fs::write(self.tools.path().join("identity.txt"), identity).unwrap();
    }

    /// Launch a job pinned to [`PINNED`]; returns why it did not start (if it
    /// did not) and the client invocations it made.
    async fn pinned_job(&self) -> (Option<String>, Vec<String>) {
        let before = invocations(self.tools.path()).len();
        let mut spec = self.f.spec(&["exit", "0"]);
        spec.policy = ExecutionPolicy::Restricted { backend: "docker".into(), image_id: Some(PINNED.into()) };
        let id = self.f.service.launch(spec);
        let result = self.f.service.wait(&id).await.unwrap();
        let reason = match &result.outcome {
            JobOutcome::FailedToStart { reason } => Some(reason.clone()),
            _ => None,
        };
        (reason, invocations(self.tools.path())[before..].to_vec())
    }
}

fn pin_refused(reason: &Option<String>) -> bool {
    reason.as_deref().is_some_and(|r| r.contains("may run only in") && r.contains("not run natively"))
}

fn inspects(lines: &[String]) -> usize {
    lines.iter().filter(|l| l.starts_with("docker image inspect")).count()
}

fn runs(lines: &[String]) -> usize {
    lines.iter().filter(|l| l.starts_with("docker run")).count()
}

#[tokio::test(flavor = "multi_thread")]
async fn review_the_stand_in_answers_and_a_matching_identity_passes_the_pin() {
    // Guards the tests below: if the batch-file client could not be started
    // at all, every job would be refused for another reason.
    let e = engine();
    e.reports(&format!("{PINNED}\r\n"));
    let target = e.f.service.restricted_preflight().expect("the stand-in engine answers");
    assert_eq!(target.image_id.as_deref(), Some(PINNED));
    let (reason, lines) = e.pinned_job().await;
    assert!(!pin_refused(&reason), "{reason:?}");
    assert_eq!(inspects(&lines), 1, "{lines:?}");
    assert!(
        reason.as_deref().is_none_or(|r| !r.contains("restricted execution is unavailable")),
        "the pin check passed, so any failure is from starting the stand-in container: {reason:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn review_a_malformed_or_missing_identity_refuses_a_pinned_job_without_starting_a_container() {
    let e = engine();
    let malformed = [
        String::new(),
        "\r\n".to_owned(),
        // Right length, wrong alphabet.
        format!("sha256:{}", "A".repeat(64)),
        // Too short, too long, no prefix.
        format!("sha256:{}", "a".repeat(63)),
        format!("{PINNED}a"),
        "a".repeat(64),
        // The pinned identity followed or preceded by something else.
        format!("{PINNED}\r\n{OTHER}\r\n"),
        format!("{OTHER}\r\n{PINNED}\r\n"),
        format!("{PINNED} {PINNED}"),
        format!("x{PINNED}"),
        // A different, well-formed identity.
        OTHER.to_owned(),
    ];
    for identity in &malformed {
        e.reports(identity);
        let (reason, lines) = e.pinned_job().await;
        assert!(pin_refused(&reason), "identity {identity:?} was not refused for the pin: {reason:?}");
        assert_eq!(inspects(&lines), 1, "{identity:?}: {lines:?}");
        assert_eq!(runs(&lines), 0, "{identity:?}: a container was started: {lines:?}");
    }
    // An image that is not present: refused as unavailable, nothing started.
    e.reports(PINNED);
    std::fs::write(e.tools.path().join("missing"), "").unwrap();
    let (reason, lines) = e.pinned_job().await;
    let reason = reason.expect("refused");
    assert!(reason.contains("not present locally") && reason.contains("not run natively"), "{reason}");
    assert_eq!(runs(&lines), 0, "{lines:?}");
    assert!(e.f.service.restricted_preflight().is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn review_every_job_asks_the_engine_again_so_a_changed_image_is_refused_at_once() {
    let e = engine();
    for round in 0..3 {
        e.reports(PINNED);
        let (reason, lines) = e.pinned_job().await;
        assert!(!pin_refused(&reason), "round {round}: {reason:?}");
        assert_eq!(inspects(&lines), 1, "round {round}: {lines:?}");
        // The tag now names another image: the very next job is refused.
        e.reports(OTHER);
        let (reason, lines) = e.pinned_job().await;
        assert!(pin_refused(&reason), "round {round}: an earlier answer was reused: {reason:?}");
        assert!(reason.as_deref().unwrap().contains(OTHER), "{reason:?}");
        assert_eq!(inspects(&lines), 1, "round {round}: {lines:?}");
        assert_eq!(runs(&lines), 0, "round {round}: {lines:?}");
        // And the preflight reports what the engine says now.
        assert_eq!(e.f.service.restricted_preflight().unwrap().image_id.as_deref(), Some(OTHER));
    }
}
