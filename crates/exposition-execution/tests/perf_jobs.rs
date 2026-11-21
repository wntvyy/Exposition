//! Scale measurements of the execution service (run on purpose, never in a
//! validation plan):
//!
//! `cargo test -p exposition-execution -p exposition --test perf_jobs -- --ignored --nocapture --test-threads=1`
//!
//! Each prints `PERF {...}` lines (`scripts/perf/rust-bench.mjs` collects
//! them). Scale: `EXPOSITION_PERF_SCALE` = small (default) | large | stress.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::*;
use exposition_core::job::{JobEvent, JobOutcome};
use exposition_execution::{DockerSettings, ExecutionSettings, InteractiveOptions};

fn scale() -> String {
    std::env::var("EXPOSITION_PERF_SCALE").unwrap_or_else(|_| "small".into())
}

fn pick<T: Copy>(small: T, large: T, stress: T) -> T {
    match scale().as_str() {
        "large" => large,
        "stress" => stress,
        _ => small,
    }
}

fn perf(bench: &str, scale: serde_json::Value, samples_ms: &[f64], counters: serde_json::Value) {
    println!(
        "PERF {}",
        serde_json::json!({"bench": bench, "scale": scale, "samples_ms": samples_ms, "counters": counters})
    );
}

fn ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1000.0
}

/// CPU time (user + kernel, ms) this process has used so far.
fn process_cpu_ms() -> Option<f64> {
    if !cfg!(windows) {
        return None;
    }
    let script = format!("(Get-Process -Id {}).TotalProcessorTime.TotalMilliseconds", std::process::id());
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().replace(',', ".").parse().ok()
}

fn slots(n: u32) -> ExecutionSettings {
    ExecutionSettings { max_concurrent_jobs: Some(n), ..Default::default() }
}

/// A long output stream: events emitted, bytes carried by those events (what
/// would cross IPC to the webview), the cost of one job listing with a
/// realistic number of retained jobs, and that the authoritative paged read
/// returns everything retained.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "perf"]
async fn long_output_stream() {
    let retained = pick(50usize, 300, 500);
    let flood = pick(5_000_000usize, 50_000_000, 200_000_000);
    let f = fixture_with(slots(8), true);
    let ids: Vec<_> = (0..retained).map(|i| f.service.launch(f.spec(&["print", &format!("job {i}")]))).collect();
    for id in &ids {
        f.service.wait(id).await.unwrap();
    }
    let mut events = f.service.subscribe();
    let started = Instant::now();
    let id = f.service.launch(f.spec(&["flood", &flood.to_string()]));
    let (mut output_events, mut output_event_bytes, mut text_bytes, mut lifecycle, mut lagged) =
        (0u64, 0u64, 0u64, 0u64, 0u64);
    let mut lifecycle_bytes = 0u64;
    loop {
        match tokio::time::timeout(Duration::from_secs(120), events.recv()).await.expect("job events stalled") {
            Ok(event) => {
                let bytes = serde_json::to_string(&event).unwrap().len() as u64;
                match &event {
                    JobEvent::Output { text, .. } => {
                        output_events += 1;
                        output_event_bytes += bytes;
                        text_bytes += text.len() as u64;
                    }
                    JobEvent::Finished { result } if result.id == id => {
                        lifecycle += 1;
                        lifecycle_bytes += bytes;
                        assert_eq!(result.outcome, JobOutcome::Exited { code: 0 });
                        break;
                    }
                    _ => {
                        lifecycle += 1;
                        lifecycle_bytes += bytes;
                    }
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => lagged += n,
            Err(e) => panic!("{e}"),
        }
    }
    let stream_ms = ms(started);
    // One job listing as the webview receives it.
    let mut list_samples = Vec::new();
    let mut list_bytes = 0usize;
    for _ in 0..21 {
        let t = Instant::now();
        list_bytes = serde_json::to_vec(&f.service.list()).unwrap().len();
        list_samples.push(ms(t));
    }
    list_samples.remove(0);
    // The authoritative read: every retained chunk, by sequence number.
    let t = Instant::now();
    let (mut pages, mut read_bytes, mut after, mut truncated) = (0u64, 0u64, None, false);
    loop {
        let page = f.service.output(&id, after, 128 * 1024).unwrap();
        pages += 1;
        truncated |= page.truncated;
        read_bytes += page.chunks.iter().map(|c| c.text.len() as u64).sum::<u64>();
        after = page.chunks.last().map(|c| c.seq).or(after);
        if !page.has_more {
            assert!(page.complete);
            break;
        }
    }
    let read_ms = ms(t);
    let scale = serde_json::json!({"retained_jobs": retained, "flood_bytes": flood});
    perf(
        "execution.output_stream",
        scale.clone(),
        &[stream_ms],
        serde_json::json!({
            "output_events": output_events,
            "output_event_bytes": output_event_bytes,
            "output_text_bytes_in_events": text_bytes,
            "lifecycle_events": lifecycle,
            "lifecycle_event_bytes": lifecycle_bytes,
            "lagged_events": lagged,
            "output_events_per_second": (output_events as f64 / (stream_ms / 1000.0)).round(),
        }),
    );
    perf("execution.job_list", scale.clone(), &list_samples, serde_json::json!({"list_json_bytes": list_bytes}));
    perf(
        "execution.output_read_all",
        scale,
        &[read_ms],
        serde_json::json!({"pages": pages, "bytes": read_bytes, "truncated": truncated}),
    );
}

/// CPU the service uses while interactive jobs sit idle (each has a
/// supervisor thread that polls).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "perf"]
async fn idle_supervision() {
    let window = Duration::from_secs(pick(5, 10, 20));
    for sessions in [0usize, 4, 12] {
        let f = fixture();
        let mut jobs = Vec::new();
        for _ in 0..sessions {
            jobs.push(
                f.service.launch_interactive(f.spec(&["echo-lines"]), InteractiveOptions::default()).await.unwrap(),
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        let before = process_cpu_ms();
        tokio::time::sleep(window).await;
        let after = process_cpu_ms();
        let cpu = before.zip(after).map(|(b, a)| a - b);
        perf(
            "execution.idle_supervision",
            serde_json::json!({"idle_interactive_jobs": sessions, "window_ms": window.as_millis() as u64}),
            &[],
            serde_json::json!({
                "process_cpu_ms": cpu,
                "cpu_percent_of_one_core": cpu.map(|c| (c / window.as_millis() as f64 * 1000.0).round() / 10.0),
            }),
        );
        for job in &jobs {
            f.service.cancel(&job.id);
        }
        for job in &jobs {
            f.service.wait(&job.id).await;
        }
    }
}

fn entries(dir: &Path) -> usize {
    std::fs::read_dir(dir).map(|d| d.count()).unwrap_or(0)
}

/// Cancel and timeout of process trees: latency until the job is reported
/// finished, processes left alive, and job directories left behind.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "perf"]
async fn cancel_leaves_nothing_behind() {
    let cycles = pick(5usize, 20, 50);
    let f = fixture_with(slots(4), true);
    let jobs_dir = f.data_dir.join("execution").join("jobs");
    let mut samples = Vec::new();
    let mut all_pids = Vec::new();
    for i in 0..cycles {
        // Alternate cancellation and a timeout kill.
        let mut spec = f.spec(&["tree", "2", "120"]);
        let by_timeout = i % 2 == 1;
        let id = f.service.launch(spec.clone());
        let tree = pids(&f.wait_stdout(&id, Duration::from_secs(30), |t| pids(t).len() == 3).await);
        if by_timeout {
            // A second job that is killed by its own time limit.
            spec.timeout_ms = Some(1500);
            let timed = f.service.launch(spec);
            let timed_tree = pids(&f.wait_stdout(&timed, Duration::from_secs(30), |t| pids(t).len() == 3).await);
            assert_eq!(f.service.wait(&timed).await.unwrap().outcome, JobOutcome::TimedOut);
            all_pids.extend(timed_tree);
        }
        let t = Instant::now();
        assert!(f.service.cancel(&id));
        assert_eq!(f.service.wait(&id).await.unwrap().outcome, JobOutcome::Canceled);
        samples.push(ms(t));
        all_pids.extend(tree);
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let alive_after: Vec<u32> = all_pids.iter().copied().filter(|p| alive(*p)).collect();
    perf(
        "execution.cancel_cleanup",
        serde_json::json!({"cycles": cycles, "tree_processes": 3}),
        &samples,
        serde_json::json!({
            "processes_started": all_pids.len(),
            "processes_left_alive": alive_after.len(),
            "job_dirs_left": entries(&jobs_dir),
        }),
    );
}

/// A stand-in docker client that logs each invocation next to itself.
fn fake_docker(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let exe = dir.join(if cfg!(windows) { "docker.exe" } else { "docker" });
    std::fs::copy(testproc(), &exe).unwrap();
    exe
}

fn invocations(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("invocations.log")).unwrap_or_default().lines().map(str::to_owned).collect()
}

/// Docker client processes spawned by one restricted preflight and by one
/// restricted job dispatch (counted with a stand-in client; with
/// `EXPOSITION_DOCKER_TESTS=1` the real engine's preflight is also timed).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "perf"]
async fn restricted_preflight_spawns() {
    let tools = tempfile::tempdir().unwrap();
    let docker = fake_docker(tools.path());
    let mut settings = slots(2);
    settings.tool_paths.insert("docker".into(), docker.display().to_string());
    settings.docker = DockerSettings { image: Some("python:3.13-slim".into()), ..Default::default() };
    let f = fixture_with(settings, false);
    let mut samples = Vec::new();
    for _ in 0..6 {
        let t = Instant::now();
        let target = f.service.restricted_preflight().expect("the stand-in engine has the image");
        samples.push(ms(t));
        assert!(target.image_id.is_some());
    }
    samples.remove(0);
    let per_preflight = invocations(tools.path()).len() as f64 / 6.0;
    let before = invocations(tools.path()).len();
    let mut spec = f.spec(&["exit", "0"]);
    spec.policy = exposition_core::job::ExecutionPolicy::Restricted {
        backend: "docker".into(),
        image_id: Some(format!("sha256:{}", "a".repeat(64))),
    };
    let id = f.service.launch(spec);
    let _ = f.service.wait(&id).await;
    let dispatch: Vec<String> = invocations(tools.path())[before..].to_vec();
    let inspects = dispatch.iter().filter(|l| l.starts_with("docker image inspect")).count();
    perf(
        "execution.restricted_preflight",
        serde_json::json!({"client": "stand-in"}),
        &samples,
        serde_json::json!({
            "client_spawns_per_preflight": per_preflight,
            "client_spawns_per_job_dispatch": dispatch.len(),
            "image_inspects_per_job_dispatch": inspects,
        }),
    );
    if !std::env::var("EXPOSITION_DOCKER_TESTS").is_ok_and(|v| v == "1") {
        println!("SKIPPED real engine: set EXPOSITION_DOCKER_TESTS=1 (Docker Desktop with local python:3.13-slim)");
        return;
    }
    let real = fixture_with(
        ExecutionSettings {
            docker: DockerSettings { image: Some("python:3.13-slim".into()), ..Default::default() },
            ..slots(2)
        },
        false,
    );
    let mut samples = Vec::new();
    for _ in 0..6 {
        let t = Instant::now();
        if let Err(reason) = real.service.restricted_preflight() {
            println!("SKIPPED real engine: {reason}");
            return;
        }
        samples.push(ms(t));
    }
    samples.remove(0);
    perf("execution.restricted_preflight", serde_json::json!({"client": "docker"}), &samples, serde_json::json!({}));
}
