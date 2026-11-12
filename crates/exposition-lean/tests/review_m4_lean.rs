//! R-m4 independent review: Lean checking-status honesty against the real
//! language server (`lake serve` of the pinned toolchain).
//!
//! The `regression_*` tests were the reviewer's `review_defect_*` tests
//! (they failed before the axiom audit: every file below was reported
//! `Checked`); they now assert the distinct status each file must get. The
//! large-file probe stays `#[ignore]`d (run with `--ignored --nocapture`).
//! The remaining tests record behavior that held up under attack.

mod common;

use std::time::Duration;

use common::*;
use exposition_core::path::WorkspacePath;
use exposition_lean::{LeanCheckReport, LeanCheckRequest, LeanCheckTarget, LeanStatus, SorrySource};

const FIRST: Duration = Duration::from_secs(300);

async fn check_path(h: &Harness, path: &str) -> LeanCheckReport {
    h.lean
        .check(LeanCheckRequest {
            target: LeanCheckTarget::Path { path: WorkspacePath::parse(path).unwrap() },
            goals_at: vec![],
            timeout_ms: Some(300_000),
        })
        .await
        .unwrap()
}

fn write(h: &Harness, path: &str, text: &str) {
    let full = path.split('/').fold(h.root.clone(), |p, c| p.join(c));
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, text).unwrap();
}

/// Files whose only theorem proves `False` (or is unfinished) while Lean prints
/// no error and no "declaration uses `sorry`" warning. `#print axioms` on each
/// shows `sorryAx`, a user axiom, a `native_decide` axiom over a lying
/// `implemented_by`, or a kernel-unchecked declaration.
const CHEATS: &[(&str, &str)] = &[
    (
        "LeanBasic/CheatGuardMsgs.lean",
        "/-- warning: declaration uses `sorry` -/\n#guard_msgs in\ntheorem guarded : False := sorry\n",
    ),
    ("LeanBasic/CheatGuardDrop.lean", "#guard_msgs (drop warning) in\ntheorem dropped : False := sorry\n"),
    ("LeanBasic/CheatWarnSorry.lean", "set_option warn.sorry false in\ntheorem quiet : False := sorry\n"),
    ("LeanBasic/CheatAxiom.lean", "axiom cheat : False\ntheorem byAxiom : False := cheat\n"),
    (
        "LeanBasic/CheatNative.lean",
        "def trueImpl (_ : Unit) : Bool := true\n@[implemented_by trueImpl] def alwaysFalse (_ : Unit) : Bool := false\ntheorem bad : alwaysFalse () = true := by native_decide\ntheorem boom : False := by\n  have h := bad\n  simp [alwaysFalse] at h\n",
    ),
    // `#exit` stops elaboration: the bogus theorem below it is never checked,
    // yet the file is reported checked (with one warning).
    ("LeanBasic/CheatExit.lean", "theorem fine : 1 = 1 := rfl\n#exit\ntheorem bogus : False := by exact nonsense\n"),
    (
        "LeanBasic/CheatSkipKernel.lean",
        "import Lean\nopen Lean Elab Command\nset_option debug.skipKernelTC true\nrun_cmd liftCoreM <| addDecl <| .thmDecl { name := `kbad, levelParams := [], type := mkConst ``False, value := mkConst ``True.intro }\ntheorem viaKernelSkip : False := kbad\n",
    ),
];

/// Was DEFECT (status honesty): every file below was reported `Checked` /
/// `checked: true`. Each now gets a distinct, honest status: hidden `sorry` →
/// Incomplete (found by the axiom audit), a user `axiom` → checked with
/// axioms, `native_decide` → relies on compiled code, `#exit` → partially
/// checked, `debug.skipKernelTC` → not verified.
#[tokio::test(flavor = "multi_thread")]
async fn regression_hidden_sorry_and_unsound_proofs_are_never_checked() {
    if !lean_available() {
        return;
    }
    let h = harness("lean-basic", Options::default()).await;
    for (path, text) in CHEATS {
        write(&h, path, text);
    }
    let mut claimed_checked = Vec::new();
    for (path, _) in CHEATS {
        let report = check_path(&h, path).await;
        println!(
            "{path}: checked={} status={:?} axioms={:?} diagnostics={:?}",
            report.checked,
            report.status,
            report.axioms,
            report.diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
        if report.checked {
            claimed_checked.push(*path);
        }
        let name = path.rsplit('/').next().unwrap();
        match (name, &report.status) {
            (
                "CheatGuardMsgs.lean" | "CheatGuardDrop.lean" | "CheatWarnSorry.lean",
                LeanStatus::CheckedWithSorry { count: 1, locations, .. },
            ) => {
                assert_eq!(locations[0].source, SorrySource::AxiomAudit, "{path}");
                assert!(report.axioms.contains(&"sorryAx".to_owned()), "{path}: {:?}", report.axioms);
            }
            ("CheatAxiom.lean", LeanStatus::CheckedWithAxioms { axioms, declarations, .. }) => {
                assert_eq!(axioms, &vec!["cheat".to_owned()]);
                assert!(
                    declarations.iter().any(|d| d.declaration == "byAxiom" && d.range.is_some()),
                    "{declarations:?}"
                );
            }
            ("CheatNative.lean", LeanStatus::ReliesOnCompiledCode { declarations, .. }) => {
                assert!(declarations.iter().any(|d| d.declaration == "bad"), "{declarations:?}");
            }
            ("CheatExit.lean", LeanStatus::PartiallyChecked { line: Some(1), .. }) => {}
            ("CheatSkipKernel.lean", LeanStatus::Unverified { reason, .. }) => {
                assert!(reason.contains("skipKernelTC"), "{reason}");
            }
            (_, other) => panic!("{path}: unexpected status {other:?}"),
        }
    }
    h.lean.shutdown().await;
    assert!(claimed_checked.is_empty(), "files proving False were reported checked: {claimed_checked:?}");
}

/// Was DEFECT (status honesty): a file whose theorem is proved by an imported
/// lemma that is itself `sorry` was reported `Checked`. The axiom audit
/// follows the dependency into the import: Incomplete, naming the declaration.
#[tokio::test(flavor = "multi_thread")]
async fn regression_sorry_in_imports_is_incomplete() {
    if !lean_available() {
        return;
    }
    let h = harness("lean-basic", Options::default()).await;
    write(&h, "LeanBasic/Shaky.lean", "namespace LeanBasic\ntheorem shaky : False := sorry\nend LeanBasic\n");
    write(
        &h,
        "LeanBasic/UsesShaky.lean",
        "import LeanBasic.Shaky\nnamespace LeanBasic\ntheorem relies : False := shaky\nend LeanBasic\n",
    );
    let shaky = check_path(&h, "LeanBasic/Shaky.lean").await;
    assert!(matches!(shaky.status, LeanStatus::CheckedWithSorry { .. }), "control: {:?}", shaky.status);
    // The importing file is checked against the built import (Lake builds it).
    let report = check_path(&h, "LeanBasic/UsesShaky.lean").await;
    println!("UsesShaky: checked={} status={:?}", report.checked, report.status);
    h.lean.shutdown().await;
    assert!(!report.checked, "a proof resting on an imported sorry was reported checked: {:?}", report.status);
    match &report.status {
        LeanStatus::CheckedWithSorry { count: 1, locations, .. } => {
            assert_eq!(locations[0].declaration.as_deref(), Some("LeanBasic.relies"));
            assert_eq!(locations[0].source, SorrySource::AxiomAudit);
            assert_eq!(locations[0].line, 2);
        }
        other => panic!("expected incomplete, got {other:?}"),
    }
    assert!(report.diagnostics.iter().any(|d| d.message.contains("`LeanBasic.relies` depends on `sorry`")));
}

/// Held up: explicit incomplete material in its usual forms is distinguished
/// (`admit`, a direct `sorryAx` term, a macro expanding to `sorry`), and files
/// whose imports are missing or do not compile are reported as errors.
#[tokio::test(flavor = "multi_thread")]
async fn review_incomplete_material_forms_are_distinguished() {
    if !lean_available() {
        return;
    }
    let h = harness("lean-basic", Options::default()).await;
    let cases = [
        ("LeanBasic/Admit.lean", "theorem adm : False := by admit\n"),
        ("LeanBasic/SorryAx.lean", "theorem direct : False := sorryAx False false\n"),
        (
            "LeanBasic/MacroSorry.lean",
            "macro \"trust_me\" : tactic => `(tactic| sorry)\ntheorem m : False := by trust_me\n",
        ),
    ];
    for (path, text) in cases {
        write(&h, path, text);
        let report = check_path(&h, path).await;
        assert!(!report.checked, "{path}: {:?}", report.status);
        assert!(
            matches!(report.status, LeanStatus::CheckedWithSorry { count: 1, .. }),
            "{path}: expected incomplete, got {:?}",
            report.status
        );
    }
    // Broken imports: a missing module, and a module that does not compile.
    write(&h, "LeanBasic/Broken.lean", "namespace LeanBasic\ntheorem broken : 1 = 2 := rfl\nend LeanBasic\n");
    write(&h, "LeanBasic/UsesBroken.lean", "import LeanBasic.Broken\ntheorem fineHere : 1 = 1 := rfl\n");
    write(&h, "LeanBasic/UsesMissing.lean", "import LeanBasic.DoesNotExist\ntheorem fineToo : 1 = 1 := rfl\n");
    for path in ["LeanBasic/UsesBroken.lean", "LeanBasic/UsesMissing.lean"] {
        let report = check_path(&h, path).await;
        println!("{path}: checked={} status={:?}", report.checked, report.status);
        assert!(!report.checked, "{path}: a file whose imports fail must not be checked: {:?}", report.status);
        assert!(matches!(report.status, LeanStatus::Errors { .. }), "{path}: {:?}", report.status);
    }
    h.lean.shutdown().await;
}

/// Performance probe (no webview): time until a 3000-declaration file with one
/// `sorry` each reaches its final status. `lean Big.lean` on the same
/// toolchain takes about 5 s on the review machine; the app's Lean panel took
/// 134 s (review-m4-lean-ui.mjs). Prints the service-only time for comparison.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "R-m4 performance probe; run with --ignored --nocapture"]
async fn review_probe_large_file_time_to_final_status() {
    if !lean_available() {
        return;
    }
    let n = 3000;
    let text: String = (0..n).map(|i| format!("theorem big{i} : {i} + 0 = {i} := by\n  sorry\n\n")).collect();
    // Baseline first, under the same machine load: a minimal client (same
    // framing, no service) on its own copy, until `waitForDiagnostics` answers.
    let raw = raw_client_seconds(&text).await;
    println!("Big.lean: raw client (lake serve + waitForDiagnostics) {raw:.1} s");
    let mut h = harness("lean-basic", Options::default()).await;
    write(&h, "LeanBasic/Big.lean", &text);
    let started = std::time::Instant::now();
    let doc = h.open("LeanBasic/Big.lean");
    let (_, mut seen) = h
        .wait_for(&doc, Duration::from_secs(600), "Lean finished, auditing", |s| {
            matches!(s.status, LeanStatus::Checking { auditing: true, .. }) || s.status.finished_revision().is_some()
        })
        .await;
    println!("Big.lean: Lean reported all diagnostics after {:.1} s", started.elapsed().as_secs_f64());
    let (state, rest) = h
        .wait_for(&doc, Duration::from_secs(600), "big file finished", |s| s.status.finished_revision() == Some(1))
        .await;
    seen.extend(rest);
    let events = seen.len();
    println!(
        "Big.lean: {:?} after {:.1} s ({events} document events observed)",
        match &state.status {
            LeanStatus::CheckedWithSorry { count, .. } => format!("checkedWithSorry count={count}"),
            other => format!("{other:?}"),
        },
        started.elapsed().as_secs_f64()
    );
    assert!(matches!(state.status, LeanStatus::CheckedWithSorry { count, .. } if count == n));
    println!(
        "Big.lean: service/raw = {:.2} (final status, including the axiom audit)",
        started.elapsed().as_secs_f64() / raw
    );
    h.lean.shutdown().await;
}

async fn raw_client_seconds(text: &str) -> f64 {
    use exposition_core::job::{EnvironmentSpec, ExecutionPolicy, JobSpec};
    use exposition_execution::InteractiveOptions;
    use exposition_lean::lsp::LspClient;
    use exposition_lean::toolchain::{self, Resolution};
    use exposition_lean::uri::path_to_uri;
    use serde_json::json;

    let h = harness("lean-basic", Options::default()).await;
    write(&h, "LeanBasic/Big.lean", text);
    let home = toolchain::elan_home(|n| std::env::var(n).ok());
    let Resolution::Installed(t) = toolchain::resolve(home.as_deref(), TOOLCHAIN) else { unreachable!() };
    let started = std::time::Instant::now();
    let spec = JobSpec {
        label: "raw lake serve (baseline)".into(),
        program: t.lake.display().to_string(),
        args: vec!["serve".into()],
        environment: EnvironmentSpec::default(),
        working_directory: h.root.display().to_string(),
        policy: ExecutionPolicy::TrustedNative,
        timeout_ms: None,
        log_limit_bytes: None,
        stdin: None,
    };
    let job = h.exec.launch_interactive(spec, InteractiveOptions::default()).await.unwrap();
    let id = job.id.clone();
    let (client, mut incoming) = LspClient::start(job.input, job.output);
    tokio::spawn(async move { while incoming.recv().await.is_some() {} });
    client
        .call(
            "initialize",
            json!({ "processId": null, "rootUri": path_to_uri(&h.root), "capabilities": {} }),
            Duration::from_secs(120),
        )
        .await
        .unwrap();
    client.notify("initialized", json!({})).unwrap();
    let uri = path_to_uri(&h.root.join("LeanBasic").join("Big.lean"));
    client
        .notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": "lean4", "version": 1, "text": text } }),
        )
        .unwrap();
    client
        .call("textDocument/waitForDiagnostics", json!({ "uri": uri, "version": 1 }), Duration::from_secs(900))
        .await
        .unwrap();
    let seconds = started.elapsed().as_secs_f64();
    h.exec.cancel(&id);
    let _ = h.exec.wait(&id).await;
    seconds
}

/// Held up: an edit while Lean is still elaborating the checked revision never
/// yields `checked: true` for the old text, and the report names the revision
/// and hash it was asked about.
#[tokio::test(flavor = "multi_thread")]
async fn review_edit_during_check_is_never_reported_checked() {
    if !lean_available() {
        return;
    }
    let mut h = harness("lean-basic", Options::default()).await;
    // A slow but valid file: elaboration sleeps for a few seconds.
    write(&h, "LeanBasic/Slow.lean", "#eval IO.sleep 8000\ntheorem ok : 2 + 2 = 4 := rfl\n");
    let doc = h.open("LeanBasic/Slow.lean");
    let lean = h.lean.clone();
    let doc_for_check = doc.clone();
    let check = tokio::spawn(async move {
        lean.check(LeanCheckRequest {
            target: LeanCheckTarget::Document { document: doc_for_check },
            goals_at: vec![],
            timeout_ms: Some(300_000),
        })
        .await
        .unwrap()
    });
    h.wait_for(&doc, FIRST, "checking revision 1", |s| matches!(s.status, LeanStatus::Checking { revision: 1, .. }))
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    // Break the proof while revision 1 is still being elaborated.
    let r2 = h.replace(&doc, "rfl", "by exact nonsense");
    let report = check.await.unwrap();
    println!("edit during check: checked={} revision={:?} status={:?}", report.checked, report.revision, report.status);
    assert!(!report.checked, "an edit during the check must not produce checked: {:?}", report.status);
    let state = h.finished(&doc, r2, FIRST).await;
    assert!(matches!(state.status, LeanStatus::Errors { .. }), "{:?}", state.status);
    h.lean.shutdown().await;
}
