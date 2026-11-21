//! Scale measurement of the Lean service around the real language server:
//! document states sent per edit (count and serialized size) and processes
//! started per check. Run with `node scripts/perf/rust-bench.mjs exposition-lean`
//! (`EXPOSITION_PERF_SCALE` = small | large | stress). Skips with a message
//! when the fixture toolchain is not installed.
//!
//! The timings include Lean's own elaboration and the axiom audit; the
//! counters (events, bytes, processes) are the service's own share.

mod common;

use std::collections::HashSet;
use std::time::{Duration, Instant};

use common::{Options, harness, lean_available};
use exposition_lean::LeanDocumentState;

const TIMEOUT: Duration = Duration::from_secs(900);

fn declarations() -> usize {
    match std::env::var("EXPOSITION_PERF_SCALE").as_deref() {
        Ok("large") => 400,
        Ok("stress") => 2000,
        _ => 50,
    }
}

/// `n` small theorems, each followed by a `#check` (one information
/// diagnostic per declaration), and a trailing comment that the edits change.
fn source(n: usize) -> String {
    let mut text = String::new();
    for i in 0..n {
        text.push_str(&format!("theorem perf_t{i} (a : Nat) : a + 0 = a := rfl\n#check perf_t{i}\n\n"));
    }
    text.push_str("-- tail 0\n");
    text
}

fn perf(bench: &str, scale: &[(&str, f64)], samples: &[f64], counters: &[(&str, f64)]) {
    let object =
        |pairs: &[(&str, f64)]| pairs.iter().map(|(k, v)| format!("\"{k}\":{v}")).collect::<Vec<_>>().join(",");
    let samples = samples.iter().map(|s| format!("{s:.3}")).collect::<Vec<_>>().join(",");
    println!(
        "PERF {{\"bench\":\"{bench}\",\"scale\":{{{}}},\"samples_ms\":[{samples}],\"counters\":{{{}}}}}",
        object(scale),
        object(counters)
    );
}

fn bytes(state: &LeanDocumentState) -> usize {
    serde_json::to_vec(state).map(|v| v.len()).unwrap_or(0)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "perf"]
async fn lean_service_overhead() {
    if !lean_available() {
        return;
    }
    let n = declarations();
    let text = source(n);
    let scale = [("declarations", n as f64), ("source_bytes", text.len() as f64)];
    let mut h = harness("lean-basic", Options::default()).await;
    std::fs::write(h.root.join("Perf.lean"), &text).unwrap();
    let started = Instant::now();
    let doc = h.open("Perf.lean");
    let revision = h.documents.info(&doc).unwrap().revision;
    let state = h.finished(&doc, revision, TIMEOUT).await;
    perf(
        "lean.open_to_finished",
        &scale,
        &[started.elapsed().as_secs_f64() * 1000.0],
        &[("diagnostics", state.diagnostics.len() as f64), ("state_bytes", bytes(&state) as f64)],
    );

    let jobs = |h: &common::Harness| -> HashSet<String> {
        h.exec.list().into_iter().map(|job| format!("{:?}|{}", job.info.id, job.info.label)).collect()
    };

    // One edit, then wait for the finished check of that revision.
    let mut samples = Vec::new();
    let (mut events, mut event_bytes, mut max_bytes, mut processes, mut lake_env) = (0, 0, 0, 0, 0);
    for i in 0..6 {
        let before = jobs(&h);
        let started = Instant::now();
        let revision = h.replace(&doc, &format!("-- tail {i}"), &format!("-- tail {}", i + 1));
        let (_, seen) =
            h.wait_for(&doc, TIMEOUT, "finished check", |s| s.status.finished_revision() == Some(revision)).await;
        if i > 0 {
            samples.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        // The first state is the one read when the wait started, not an event.
        let sent: Vec<usize> = seen.iter().skip(1).map(bytes).collect();
        events = sent.len();
        event_bytes = sent.iter().sum();
        max_bytes = sent.iter().copied().max().unwrap_or(0);
        let started_jobs: Vec<String> = jobs(&h).difference(&before).cloned().collect();
        processes = started_jobs.len();
        lake_env = started_jobs.iter().filter(|j| j.contains("lake env")).count();
    }
    perf(
        "lean.edit_to_finished",
        &scale,
        &samples,
        &[
            ("states_sent", events as f64),
            ("state_bytes_total", event_bytes as f64),
            ("state_bytes_max", max_bytes as f64),
            ("processes_started", processes as f64),
            ("lake_env_processes", lake_env as f64),
        ],
    );

    // Ten edits at ten per second, then the finished check of the last one.
    let mut samples = Vec::new();
    let (mut events, mut event_bytes, mut processes) = (0, 0, 0);
    let mut tail = 6;
    for _ in 0..5 {
        let before = jobs(&h);
        let started = Instant::now();
        let mut revision = 0;
        for _ in 0..10 {
            revision = h.replace(&doc, &format!("-- tail {tail}"), &format!("-- tail {}", tail + 1));
            tail += 1;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let (_, seen) =
            h.wait_for(&doc, TIMEOUT, "finished check", |s| s.status.finished_revision() == Some(revision)).await;
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
        let sent: Vec<usize> = seen.iter().skip(1).map(bytes).collect();
        events = sent.len();
        event_bytes = sent.iter().sum();
        processes = jobs(&h).difference(&before).count();
    }
    perf(
        "lean.burst_10_edits",
        &scale,
        &samples,
        &[
            ("states_sent", events as f64),
            ("state_bytes_total", event_bytes as f64),
            ("processes_started", processes as f64),
        ],
    );
}
