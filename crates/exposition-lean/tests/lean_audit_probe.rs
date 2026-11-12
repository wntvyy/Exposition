//! Latency probe for the axiom audit on the M6 paper fixture (Paper/Gauss.lean).
//! Run with `cargo test -p exposition-lean --test lean_audit_probe -- --ignored --nocapture`.

mod common;

use std::time::{Duration, Instant};

use common::*;
use exposition_lean::{LeanStatus, audit, project, toolchain};

#[tokio::test(flavor = "multi_thread")]
#[ignore = "latency probe; run with --ignored --nocapture"]
async fn probe_audit_latency_m6_paper() {
    if !lean_available() {
        return;
    }
    let mut h = harness("m6-paper", Options::default()).await;
    let doc = h.open("Paper/Gauss.lean");
    let started = Instant::now();
    let (_, _) = h
        .wait_for(&doc, Duration::from_secs(300), "Lean done", |s| {
            matches!(s.status, LeanStatus::Checking { auditing: true, .. }) || s.status.finished_revision().is_some()
        })
        .await;
    let lean_done = started.elapsed();
    let state = h.finished(&doc, 1, Duration::from_secs(300)).await;
    let total = started.elapsed();
    println!(
        "open: Lean done {:.2} s, final {:?} at {:.2} s (added {:.2} s)",
        lean_done.as_secs_f64(),
        status_name(&state.status),
        total.as_secs_f64(),
        (total - lean_done).as_secs_f64()
    );
    // Rechecks: append a comment and wait for each final status.
    for i in 0..3 {
        let r = h.replace(&doc, "end Paper", &format!("-- edit {i}\nend Paper"));
        let started = Instant::now();
        let (_, _) = h
            .wait_for(&doc, Duration::from_secs(300), "Lean done", |s| {
                matches!(s.status, LeanStatus::Checking { revision, auditing: true, .. } if revision == r)
                    || s.status.finished_revision() == Some(r)
            })
            .await;
        let lean_done = started.elapsed();
        let state = h.finished(&doc, r, Duration::from_secs(300)).await;
        let total = started.elapsed();
        println!(
            "recheck {i}: Lean done {:.2} s, final {} at {:.2} s (added {:.2} s)",
            lean_done.as_secs_f64(),
            status_name(&state.status),
            total.as_secs_f64(),
            (total - lean_done).as_secs_f64()
        );
    }
    // The audit alone, sequentially.
    let text: std::sync::Arc<str> = h.text(&doc).into();
    let file = h.root.join("Paper").join("Gauss.lean");
    let proj = project::project_for(&h.root, &file);
    let home = toolchain::elan_home(|n| std::env::var(n).ok());
    let toolchain::Resolution::Installed(t) = toolchain::resolve(home.as_deref(), TOOLCHAIN) else { unreachable!() };
    let auditor = audit::Auditor::new(h.tmp.path().join("audit"));
    for i in 0..3 {
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let started = Instant::now();
        let outcome = auditor
            .run(
                &h.exec,
                audit::AuditRequest {
                    project: &proj,
                    toolchain: &t,
                    text: text.clone(),
                    file_name: "Gauss.lean".into(),
                },
                rx,
            )
            .await;
        println!(
            "audit alone {i}: {:.2} s -> {}",
            started.elapsed().as_secs_f64(),
            match &outcome {
                audit::AuditOutcome::Complete(d) => format!("complete ({} decls)", d.len()),
                other => format!("{other:?}"),
            }
        );
    }
    h.lean.shutdown().await;
}
