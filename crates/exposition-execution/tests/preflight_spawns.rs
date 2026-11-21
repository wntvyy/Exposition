//! Docker client processes per restricted decision, counted with a stand-in
//! client (the test process fixture copied as `docker`, which logs each
//! invocation): one preflight asks the engine once, and one job dispatch
//! asks once for presence and identity together. The pin is still enforced
//! from that answer, at dispatch, for every job.

mod common;

use std::path::{Path, PathBuf};

use common::*;
use exposition_core::job::{ExecutionPolicy, JobOutcome};
use exposition_execution::{DockerSettings, ExecutionSettings, RefusalCode};

/// The identity the stand-in engine reports for every image.
const ENGINE_IMAGE: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OTHER_IMAGE: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn stand_in(dir: &Path) -> PathBuf {
    let exe = dir.join(if cfg!(windows) { "docker.exe" } else { "docker" });
    std::fs::copy(testproc(), &exe).unwrap();
    exe
}

fn invocations(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("invocations.log")).unwrap_or_default().lines().map(str::to_owned).collect()
}

fn inspects(lines: &[String]) -> usize {
    lines.iter().filter(|l| l.starts_with("docker image inspect")).count()
}

fn fixture_with_stand_in(tools: &Path) -> Fixture {
    let mut settings = ExecutionSettings::default();
    settings.tool_paths.insert("docker".into(), stand_in(tools).display().to_string());
    settings.docker = DockerSettings { image: Some("python:3.13-slim".into()), ..Default::default() };
    fixture_with(settings, false)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_preflight_asks_the_engine_once_and_reports_the_identity_it_read() {
    let tools = tempfile::tempdir().unwrap();
    let f = fixture_with_stand_in(tools.path());
    let target = f.service.restricted_preflight().expect("the stand-in engine has the image");
    assert_eq!(target.image, "python:3.13-slim");
    assert_eq!(target.image_id.as_deref(), Some(ENGINE_IMAGE));
    let lines = invocations(tools.path());
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(inspects(&lines), 1);
    // Each preflight is its own decision: nothing is remembered between them.
    f.service.restricted_preflight().unwrap();
    assert_eq!(inspects(&invocations(tools.path())), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_job_dispatch_reads_presence_and_identity_in_one_query_and_enforces_the_pin() {
    let tools = tempfile::tempdir().unwrap();
    let f = fixture_with_stand_in(tools.path());
    let restricted = |image_id: Option<&str>| {
        let mut spec = f.spec(&["exit", "0"]);
        spec.policy = ExecutionPolicy::Restricted { backend: "docker".into(), image_id: image_id.map(str::to_owned) };
        spec
    };
    // Pinned to another identity than the engine reports: refused at
    // dispatch, after exactly one identity query, and no container starts.
    let before = invocations(tools.path()).len();
    let id = f.service.launch(restricted(Some(OTHER_IMAGE)));
    let result = f.service.wait(&id).await.unwrap();
    let JobOutcome::FailedToStart { reason } = &result.outcome else { panic!("{result:?}") };
    assert!(reason.contains(ENGINE_IMAGE) && reason.contains(OTHER_IMAGE), "{reason}");
    assert!(reason.contains("not run natively"), "{reason}");
    assert_eq!(f.service.summary(&id).unwrap().refusal.unwrap().code, RefusalCode::RestrictedBackendUnavailable);
    let lines: Vec<String> = invocations(tools.path())[before..].to_vec();
    assert_eq!(inspects(&lines), 1, "{lines:?}");
    assert!(!lines.iter().any(|l| l.starts_with("docker run")), "{lines:?}");

    // Pinned to the identity the engine reports: one query, and the
    // container is started from the identity (never the tag).
    let before = invocations(tools.path()).len();
    let id = f.service.launch(restricted(Some(ENGINE_IMAGE)));
    f.service.wait(&id).await.unwrap();
    let lines: Vec<String> = invocations(tools.path())[before..].to_vec();
    assert_eq!(inspects(&lines), 1, "{lines:?}");
    let run = lines.iter().find(|l| l.starts_with("docker run")).unwrap_or_else(|| panic!("no container: {lines:?}"));
    assert!(run.contains(ENGINE_IMAGE) && !run.contains("python:3.13-slim"), "{run}");

    // Not pinned: presence is still checked for this job, once.
    let before = invocations(tools.path()).len();
    let id = f.service.launch(restricted(None));
    f.service.wait(&id).await.unwrap();
    let lines: Vec<String> = invocations(tools.path())[before..].to_vec();
    assert_eq!(inspects(&lines), 1, "{lines:?}");
}
