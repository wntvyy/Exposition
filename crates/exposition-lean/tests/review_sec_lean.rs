//! R-sec (M6 security review): Lean checking-status integrity against the real
//! language server and the real two-step axiom audit (pinned toolchain).
//!
//! Both tests failed on the product at the review (they were `#[ignore]`d with
//! the findings) and pass since the audit stopped importing the audited module
//! and re-checks its declarations with the kernel (`exposition_lean::audit`).

mod common;

use common::*;
use exposition_core::path::WorkspacePath;
use exposition_lean::{LeanCheckReport, LeanCheckRequest, LeanCheckTarget, LeanStatus};

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

/// A file that proves `1 = 2` from a user axiom and also declares a command
/// elaborator for `#eval`. The audit compiles the file as module
/// `ExpositionAudit.T<nonce>` and then imports it into the audit program, whose
/// one command is an `#eval`: the imported elaborator replaces it, reads the
/// nonce from its own module name, and logs a "clean" audit that lists only the
/// sentinel. The Lean server reports no error or warning for the file.
const FORGER: &str = r#"import Lean
open Lean Elab Command

axiom cheat : False
theorem one_eq_two : (1 : Nat) = 2 := cheat.elim

@[command_elab Lean.Parser.Command.eval] def auditShim : CommandElab := fun _ => do
  let env ← getEnv
  let some m := env.header.moduleNames.find? (fun n => n.toString.startsWith "ExpositionAudit.T")
    | throwUnsupportedSyntax
  let nonce := m.toString.drop "ExpositionAudit.T".length
  logInfo m!"{nonce}-BEGIN\n{nonce} \{\"n\":\"exposition_audit_end_{nonce}\",\"i\":false,\"ax\":[],\"r\":null}\n{nonce}-END"
"#;

/// Adds a declaration of `False` with the kernel check disabled, naming the
/// option `debug.skipKernelTC` at run time so the text never contains it.
const SKIP_KERNEL_COMPUTED: &str = r#"import Lean
open Lean Elab Command

run_cmd liftCoreM <| withOptions (fun o => o.setBool (Name.mkStr2 "debug" ("skip" ++ "KernelTC")) true) <|
  addDecl <| .thmDecl { name := `kbad, levelParams := [], type := mkConst ``False, value := mkConst ``True.intro }
theorem viaKernelSkip : False := kbad
"#;

/// R-sec F-lean-skipkernel: the kernel-check bypass was detected by a text
/// search for `skipKernelTC` (now: kernel replay). Expected: a file that
/// adds `False` without kernel checking is `Unverified` (as the literal
/// `set_option debug.skipKernelTC true` form is). Observed: `Checked` with no
/// axioms (`#print axioms` in plain Lean: "does not depend on any axioms").
#[tokio::test(flavor = "multi_thread")]
async fn review_sec_f_lean_skip_kernel_through_computed_option_name() {
    if !lean_available() {
        return;
    }
    let h = harness("lean-basic", Options::default()).await;
    write(&h, "LeanBasic/SecSkipKernel.lean", SKIP_KERNEL_COMPUTED);
    let report = check_path(&h, "LeanBasic/SecSkipKernel.lean").await;
    println!(
        "skip-kernel (computed): checked={} status={:?} axioms={:?} diagnostics={:?}",
        report.checked,
        report.status,
        report.axioms,
        report.diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    h.lean.shutdown().await;
    assert!(
        !report.checked && !matches!(report.status, LeanStatus::Checked { .. }),
        "DEFECT: `theorem viaKernelSkip : False` (added without kernel checking) was reported Checked: {:?}",
        report.status
    );
}

/// R-sec F-lean-audit: the audited text can impersonate the axiom audit.
/// Expected: a file whose theorem rests on `axiom cheat : False` is never
/// `Checked` (it was `CheckedWithAxioms` before the shim was added — see the
/// control). Observed: `Checked`, `checked = true`, no axioms reported.
#[tokio::test(flavor = "multi_thread")]
async fn review_sec_f_lean_audit_forged_by_imported_elaborator() {
    if !lean_available() {
        return;
    }
    let h = harness("lean-basic", Options::default()).await;
    // Control: the same unsound proof without the shim is caught.
    write(&h, "LeanBasic/SecControl.lean", "axiom cheat : False\ntheorem one_eq_two : (1 : Nat) = 2 := cheat.elim\n");
    let control = check_path(&h, "LeanBasic/SecControl.lean").await;
    println!("control: checked={} status={:?} axioms={:?}", control.checked, control.status, control.axioms);
    assert!(matches!(control.status, LeanStatus::CheckedWithAxioms { .. }), "control: {:?}", control.status);

    write(&h, "LeanBasic/SecForged.lean", FORGER);
    let report = check_path(&h, "LeanBasic/SecForged.lean").await;
    println!(
        "forged: checked={} status={:?} axioms={:?} diagnostics={:?}",
        report.checked,
        report.status,
        report.axioms,
        report.diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    h.lean.shutdown().await;
    assert!(
        !report.checked && !matches!(report.status, LeanStatus::Checked { .. }),
        "DEFECT: a proof of 1 = 2 from `axiom cheat : False` was reported Checked: {:?}",
        report.status
    );
}
