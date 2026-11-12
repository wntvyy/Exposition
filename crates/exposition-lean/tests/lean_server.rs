//! Lean checking against the real language server (`lake serve` of the
//! pinned toolchain) on tests/fixtures/lean-basic.

mod common;

use std::time::Duration;

use common::*;
use exposition_core::build::Severity;
use exposition_core::path::WorkspacePath;
use exposition_execution::JobKind;
use exposition_lean::{LeanCheckRequest, LeanCheckTarget, LeanStatus};
use sha2::Digest as _;

const FIRST: Duration = Duration::from_secs(300);
const NEXT: Duration = Duration::from_secs(120);

#[tokio::test(flavor = "multi_thread")]
async fn checks_the_unsaved_buffer_with_goals_sorry_errors_navigation_and_fixes() {
    if !lean_available() {
        return;
    }
    let mut h = harness("lean-basic", Options::default()).await;
    let doc = h.open("LeanBasic/Proofs.lean");

    // Revision 1 (as on disk): one unsolved-goals error, one sorry.
    let (state, seen) =
        h.wait_for(&doc, FIRST, "first finished check", |s| s.status.finished_revision() == Some(1)).await;
    assert!(seen.iter().any(|s| matches!(s.status, LeanStatus::Checking { revision: 1, .. })), "{seen:?}");
    match &state.status {
        LeanStatus::Errors { revision: 1, toolchain, count: 1, sorry_count: 1 } => assert_eq!(toolchain, TOOLCHAIN),
        other => panic!("expected errors at revision 1, got {other:?}"),
    }
    assert_eq!(state.diagnostics_revision, Some(1));
    let error = state.diagnostics.iter().find(|d| d.severity == Severity::Error).expect("error diagnostic");
    assert!(error.message.starts_with("unsolved goals"), "{}", error.message);
    assert!(error.message.contains("⊢ a + b = b + a"), "{}", error.message);
    let skip = h.offset(&doc, "skip", 0);
    let range = error.location.as_ref().unwrap().range;
    assert!(range.start < skip && skip < range.end, "error range {range:?} covers the proof");
    assert_eq!(error.location.as_ref().unwrap().path.as_str(), "LeanBasic/Proofs.lean");
    let sorry = state.diagnostics.iter().find(|d| d.severity == Severity::Warning).expect("sorry warning");
    assert!(exposition_lean::convert::is_sorry_message(&sorry.message), "{}", sorry.message);

    // Goals inside tactic proofs, as plain text.
    let unfold = h.offset(&doc, "unfold double", 0);
    let goals = h.lean.goals(doc.clone(), 1, unfold).await.unwrap();
    let goals = goals.goals.expect("inside a tactic proof");
    assert_eq!(goals.len(), 1);
    assert!(goals[0].contains("⊢ double (a + b) = double a + double b"), "{goals:?}");
    assert!(goals[0].contains("a b : Nat"), "{goals:?}");
    let at_skip = h.lean.goals(doc.clone(), 1, skip).await.unwrap().goals.unwrap();
    assert!(at_skip[0].contains("⊢ a + b = b + a"), "{at_skip:?}");
    let outside = h.lean.goals(doc.clone(), 1, h.offset(&doc, "namespace", 0)).await.unwrap();
    assert!(outside.goals.is_none(), "no tactic goals outside proofs: {outside:?}");

    // Hover and cross-file definition.
    let use_site = h.offset(&doc, "double (a + b)", 0);
    let hover = h.lean.hover(doc.clone(), 1, use_site).await.unwrap().expect("hover");
    assert!(hover.markdown.contains("Twice a natural number"), "{}", hover.markdown);
    let targets = h.lean.definition(doc.clone(), 1, use_site).await.unwrap();
    assert_eq!(targets.len(), 1, "{targets:?}");
    assert_eq!(targets[0].path.as_ref().map(WorkspacePath::as_str), Some("LeanBasic/Defs.lean"));
    assert_eq!(targets[0].line, 3);
    let defs = std::fs::read_to_string(h.root.join("LeanBasic").join("Defs.lean")).unwrap();
    let def_at = defs.find("double (n").unwrap() as u32; // ASCII before it
    assert_eq!(targets[0].range.unwrap().start, def_at);

    // The exact toolchain version is recorded.
    let (state, _) = h
        .wait_for(&doc, NEXT, "toolchain version", |s| s.toolchain.as_ref().is_some_and(|t| t.version.is_some()))
        .await;
    let toolchain = state.toolchain.unwrap();
    assert_eq!(toolchain.name, TOOLCHAIN);
    assert!(toolchain.version.unwrap().starts_with("Lean (version 4.35.0"));
    assert!(toolchain.source.ends_with("lean-toolchain"));

    // Fix the error in the buffer (not on disk): incomplete material remains.
    let r2 = h.replace(&doc, "skip", "omega");
    assert_eq!(r2, 2);
    let (state, seen) =
        h.wait_for(&doc, NEXT, "revision 2 finished", |s| s.status.finished_revision() == Some(2)).await;
    assert!(seen.iter().any(|s| matches!(s.status, LeanStatus::Checking { revision: 2, .. })), "edit → Checking");
    match &state.status {
        LeanStatus::CheckedWithSorry { revision: 2, count: 1, locations, .. } => {
            assert_eq!(locations[0].declaration.as_deref(), Some("add_zero'"));
            let sorry_at = h.offset(&doc, "sorry", 0);
            assert_eq!(locations[0].tokens, vec![exposition_core::text::TextRange::new(sorry_at, sorry_at + 5)]);
        }
        other => panic!("expected incomplete (sorry) at revision 2, got {other:?}"),
    }
    let disk = std::fs::read_to_string(h.root.join("LeanBasic").join("Proofs.lean")).unwrap();
    let original = std::fs::read_to_string(fixture_dir("lean-basic").join("LeanBasic").join("Proofs.lean")).unwrap();
    assert_eq!(disk, original, "disk is untouched");
    assert!(
        disk.contains("b + a := by\n  skip") && h.text(&doc).contains("b + a := by\n  omega"),
        "the unsaved buffer was checked"
    );

    // Stale positions are refused.
    let stale = h.lean.goals(doc.clone(), 1, unfold).await.unwrap_err();
    assert_eq!(stale.code, "lean.stale");
    assert_eq!(stale.current_revision, Some(2));

    // Complete the proof: checked.
    let r3 = h.replace(&doc, "sorry", "simp");
    let state = h.finished(&doc, r3, NEXT).await;
    assert!(matches!(state.status, LeanStatus::Checked { revision, .. } if revision == r3), "{:?}", state.status);
    assert!(state.diagnostics.iter().all(|d| d.severity != Severity::Error));

    // The agent seam reports the exact text it checked.
    let report = h
        .lean
        .check(LeanCheckRequest {
            target: LeanCheckTarget::Document { document: doc.clone() },
            goals_at: vec![h.offset(&doc, "unfold double", 0)],
            timeout_ms: None,
        })
        .await
        .unwrap();
    assert!(report.checked, "{report:?}");
    assert_eq!(report.revision, Some(r3));
    let hash = sha2::Sha256::digest(h.text(&doc).as_bytes());
    assert_eq!(report.content_hash, hash.iter().map(|b| format!("{b:02x}")).collect::<String>());
    assert_eq!(report.toolchain.unwrap().name, TOOLCHAIN);
    assert!(report.goals[0].goals.as_ref().unwrap()[0].contains("⊢ double (a + b)"));

    // A check that cannot finish in time is not reported as checked.
    let r4 = h.replace(&doc, "theorem double_add", "-- edited\ntheorem double_add");
    let quick = h
        .lean
        .check(LeanCheckRequest {
            target: LeanCheckTarget::Document { document: doc.clone() },
            goals_at: vec![],
            timeout_ms: Some(1),
        })
        .await
        .unwrap();
    assert!(!quick.checked, "{quick:?}");
    assert_eq!(quick.revision, Some(r4));
    assert!(matches!(quick.status, LeanStatus::Checking { .. }), "{:?}", quick.status);
    h.finished(&doc, r4, NEXT).await;

    // A file that is not open is checked from disk (and closed again).
    let defs = h
        .lean
        .check(LeanCheckRequest {
            target: LeanCheckTarget::Path { path: WorkspacePath::parse("LeanBasic/Defs.lean").unwrap() },
            goals_at: vec![],
            timeout_ms: Some(120_000),
        })
        .await
        .unwrap();
    assert!(defs.checked, "{defs:?}");
    assert!(defs.document.is_none() && defs.revision.is_none());
    assert_eq!(h.lean.documents().await.len(), 1, "the temporary check document is not tracked");
    h.lean.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn untrusted_workspace_is_refused_until_trusted() {
    if !lean_available() {
        return;
    }
    let mut h = harness("lean-basic", Options { trusted: false, ..Options::default() }).await;
    let doc = h.open("LeanBasic/Proofs.lean");
    let (state, _) = h.wait_for(&doc, NEXT, "untrusted", |s| matches!(s.status, LeanStatus::Untrusted { .. })).await;
    let LeanStatus::Untrusted { workspace_root } = &state.status else { unreachable!() };
    assert!(exposition_lean::uri::same_path(std::path::Path::new(workspace_root), &h.root));
    // Nothing project-related was launched (the version probe needs no trust
    // and runs only once a server starts).
    tokio::time::sleep(Duration::from_millis(500)).await;
    let jobs = h.exec.list();
    assert!(jobs.iter().all(|j| j.kind != JobKind::Interactive), "{jobs:?}");
    // The seam reports the refusal instead of a result.
    let report = h
        .lean
        .check(LeanCheckRequest {
            target: LeanCheckTarget::Document { document: doc.clone() },
            goals_at: vec![],
            timeout_ms: Some(5_000),
        })
        .await
        .unwrap();
    assert!(!report.checked);
    assert!(matches!(report.status, LeanStatus::Untrusted { .. }), "{:?}", report.status);
    // An explicit trust grant, then a restart, checks the project.
    h.exec.grant_trust(&h.root.display().to_string()).unwrap();
    h.lean.restart(doc.clone()).await.unwrap();
    let state = h.finished(&doc, 1, FIRST).await;
    assert!(matches!(state.status, LeanStatus::Errors { .. }), "{:?}", state.status);
    h.lean.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn crashed_server_is_detected_and_restarted() {
    if !lean_available() {
        return;
    }
    let mut h = harness("lean-basic", Options::default()).await;
    let doc = h.open("LeanBasic/Proofs.lean");
    h.finished(&doc, 1, FIRST).await;
    let pid = h.lean.server_pid(doc.clone()).await.expect("server pid");
    // Kill the server process from outside, as a crash would.
    let status = std::process::Command::new("taskkill").args(["/F", "/PID", &pid.to_string()]).status().unwrap();
    assert!(status.success(), "taskkill failed");
    let (crashed, _) =
        h.wait_for(&doc, NEXT, "crash detected", |s| matches!(s.status, LeanStatus::ServerCrashed { .. })).await;
    match &crashed.status {
        LeanStatus::ServerCrashed { restarting: true, attempts: 1, message } => {
            assert!(message.contains("Lean server"), "{message}")
        }
        other => panic!("{other:?}"),
    }
    // Restarted automatically; the same revision is checked again.
    let state = h.finished(&doc, 1, FIRST).await;
    assert!(matches!(state.status, LeanStatus::Errors { revision: 1, .. }), "{:?}", state.status);
    let new_pid = h.lean.server_pid(doc.clone()).await.expect("new server pid");
    assert_ne!(pid, new_pid);
    let goals = h.lean.goals(doc.clone(), 1, h.offset(&doc, "skip", 0)).await.unwrap();
    assert!(goals.goals.unwrap()[0].contains("⊢ a + b = b + a"));
    // Edits after the restart are synchronized normally.
    let r2 = h.replace(&doc, "skip", "omega");
    let state = h.finished(&doc, r2, NEXT).await;
    assert!(matches!(state.status, LeanStatus::CheckedWithSorry { .. }), "{:?}", state.status);
    h.lean.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_toolchain_is_reported_and_never_installed_implicitly() {
    // Runs with or without Lean: resolution only reads elan's directory.
    let missing = "leanprover/lean4:v4.0.0";
    let mut h = harness("lean-basic", Options { toolchain: Some(missing.into()), ..Options::default() }).await;
    let home = exposition_lean::toolchain::elan_home(|n| std::env::var(n).ok());
    let before = home.as_ref().map(|h| std::fs::read_dir(h.join("toolchains")).map(|d| d.count()).unwrap_or(0));
    let doc = h.open("LeanBasic/Proofs.lean");
    let (state, _) =
        h.wait_for(&doc, NEXT, "toolchain missing", |s| matches!(s.status, LeanStatus::ToolchainMissing { .. })).await;
    match &state.status {
        LeanStatus::ToolchainMissing { toolchain, install, guidance } => {
            assert_eq!(toolchain, missing);
            assert_eq!(install.as_deref(), Some(missing));
            assert!(guidance.contains("elan toolchain install"), "{guidance}");
        }
        other => panic!("{other:?}"),
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(h.exec.list().is_empty(), "nothing ran: {:?}", h.exec.list());
    let after = home.as_ref().map(|h| std::fs::read_dir(h.join("toolchains")).map(|d| d.count()).unwrap_or(0));
    assert_eq!(before, after, "no toolchain was installed");
    // Installing is an explicit action, and only for a trusted workspace.
    h.exec.revoke_trust(&h.root.display().to_string()).unwrap();
    let refused = h.lean.install_toolchain(doc.clone()).await.unwrap_err();
    assert_eq!(refused.code, "lean.untrusted");
    assert!(h.exec.list().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn changing_the_pinned_toolchain_invalidates_the_status() {
    if !lean_available() {
        return;
    }
    let mut h = harness("lean-basic", Options::default()).await;
    let doc = h.open("LeanBasic/Defs.lean");
    let state = h.finished(&doc, 1, FIRST).await;
    assert!(matches!(state.status, LeanStatus::Checked { revision: 1, .. }), "{:?}", state.status);
    // Re-pin to a toolchain that is not installed: the earlier result no
    // longer describes this project, and nothing is installed.
    std::fs::write(h.root.join("lean-toolchain"), "leanprover/lean4:v4.0.0\n").unwrap();
    let (state, _) = h
        .wait_for(&doc, NEXT, "toolchain missing after re-pin", |s| {
            matches!(s.status, LeanStatus::ToolchainMissing { .. })
        })
        .await;
    assert!(state.toolchain.is_none(), "no toolchain is claimed: {:?}", state.toolchain);
    // Back to the installed toolchain: a new server checks it again.
    std::fs::write(h.root.join("lean-toolchain"), format!("{TOOLCHAIN}\n")).unwrap();
    let (state, _) = h.wait_for(&doc, FIRST, "checked again", |s| matches!(s.status, LeanStatus::Checked { .. })).await;
    assert_eq!(state.toolchain.unwrap().name, TOOLCHAIN);
    h.lean.shutdown().await;
}
