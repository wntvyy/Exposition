//! R-sec2 (M6 security re-review of the two-step Lean audit, 0f353b9): ways a
//! checked file can still obtain a `Checked` verdict it should not get,
//! through the real `LeanService::check` (the path of `lean_check` and the
//! agent tool `check_lean`) and the pinned toolchain.
//!
//! Tests marked `#[ignore = "R-sec2 …"]` FAIL on the product; run them with
//! `cargo test -p exposition-lean --test review_sec2_lean -- --include-ignored --nocapture`.

mod common;

use common::*;
use exposition_core::path::WorkspacePath;
use exposition_lean::{LeanCheckReport, LeanCheckRequest, LeanCheckTarget, LeanStatus};

async fn check_path(h: &Harness, path: &str) -> LeanCheckReport {
    h.lean
        .check(LeanCheckRequest {
            target: LeanCheckTarget::Path { path: WorkspacePath::parse(path).unwrap() },
            goals_at: vec![],
            timeout_ms: Some(600_000),
        })
        .await
        .unwrap()
}

fn write(h: &Harness, path: &str, text: &str) {
    let full = path.split('/').fold(h.root.clone(), |p, c| p.join(c));
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, text).unwrap();
}

async fn report(h: &Harness, path: &str, text: &str) -> LeanCheckReport {
    write(h, path, text);
    let report = check_path(h, path).await;
    println!(
        "{path}: checked={} status={:?} axioms={:?} diagnostics={:?}",
        report.checked,
        report.status,
        report.axioms,
        report.diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    report
}

fn not_checked(report: &LeanCheckReport) -> bool {
    !report.checked && !matches!(report.status, LeanStatus::Checked { .. })
}

/// The file elaborates differently when it is the audit's temporary module
/// (`ExpositionAudit.T<id>`): the language server and Lake (and every module
/// that imports this file) get `cheat` and `one_eq_two`; the audit's compile
/// step gets neither, so the check step certifies an environment the user
/// never sees. Any observable difference works the same way (the module name
/// is just the simplest: the source path, `LEAN_PATH`, or the process command
/// line also differ).
const AUDIT_AWARE: &str = r#"import Lean
open Lean Elab Command

elab "hide_from_audit " c:command : command => do
  unless (← getMainModule).toString.startsWith "ExpositionAudit." do
    elabCommand c

hide_from_audit axiom cheat : False
hide_from_audit theorem one_eq_two : (1 : Nat) = 2 := cheat.elim
"#;

/// Downstream evidence: in the environment Lake builds for importers, the
/// theorem exists and rests on `cheat`.
const USES_AUDIT_AWARE: &str = "import LeanBasic.SecAuditAware\n\ntheorem again : (1 : Nat) = 2 := one_eq_two\n";

/// R-sec2 F1: the audit certifies a different elaboration of the text than the
/// one the user (language server) and importers (Lake) get. Expected: a file
/// whose `one_eq_two : 1 = 2` rests on `axiom cheat : False` in the language
/// server's and Lake's environment is never `Checked`. Observed: `Checked`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "R-sec2 F1: a file that detects the audit's compile step elaborates without its axiom there; 1 = 2 reported Checked"]
async fn review_sec2_f1_file_that_detects_the_audit_is_checked() {
    if !lean_available() {
        return;
    }
    let h = harness("lean-basic", Options::default()).await;
    let hidden = report(&h, "LeanBasic/SecAuditAware.lean", AUDIT_AWARE).await;
    // What importers get (Lake builds the module as `LeanBasic.SecAuditAware`).
    let downstream = report(&h, "LeanBasic/SecUsesAuditAware.lean", USES_AUDIT_AWARE).await;
    h.lean.shutdown().await;
    assert!(
        matches!(&downstream.status, LeanStatus::CheckedWithAxioms { axioms, .. } if axioms.iter().any(|a| a.contains("cheat"))),
        "control: an importer must see one_eq_two resting on `cheat`: {:?}",
        downstream.status
    );
    assert!(
        not_checked(&hidden),
        "DEFECT: `theorem one_eq_two : 1 = 2 := cheat.elim` (axiom cheat : False) reported Checked: {:?}",
        hidden.status
    );
}

/// `prelude` text that copies `Init/Prelude.lean` of the pinned toolchain up
/// to `Nat.add`, then defines `Nat.add` wrongly (`a + b = a`). Lean's kernel
/// evaluates `Nat.add` on literals with GMP by NAME, ignoring the definition,
/// so `Nat.add 1 4` is both `5` (kernel) and `1` (definition): `1 = 5`, then
/// `False`, with no axioms. Lean itself (`lean` on the file) reports no error.
fn prelude_with_wrong_nat_add() -> Option<String> {
    let home = exposition_lean::toolchain::elan_home(|n| std::env::var(n).ok());
    let exposition_lean::toolchain::Resolution::Installed(toolchain) =
        exposition_lean::toolchain::resolve(home.as_deref(), TOOLCHAIN)
    else {
        return None;
    };
    let prelude =
        std::fs::read_to_string(toolchain.dir.join("src").join("lean").join("Init").join("Prelude.lean")).ok()?;
    let lines: Vec<&str> = prelude.lines().collect();
    let doc = lines.iter().position(|l| l.starts_with("Addition of natural numbers"))?;
    // `set_option bootstrap.genMatcherCode false in` / `/--` precede the doc text.
    let cut = doc.checked_sub(2)?;
    assert!(lines[cut].starts_with("set_option bootstrap.genMatcherCode"), "unexpected Prelude layout");
    let mut text = lines[..cut].join("\n");
    text.push_str(
        r#"
set_option linter.missingDocs false
-- Wrong on purpose: the definition says a + b = a.
protected def Nat.add : Nat → Nat → Nat := fun a _ => a
theorem defSays (a b : Nat) : Eq (Nat.add a b) a := rfl
-- The kernel computes `Nat.add 1 4` with GMP: 5.
theorem kernelSays : Eq (Nat.add 1 4) 5 := rfl
theorem oneEqFive : Eq 1 5 := Eq.trans (Eq.symm (defSays 1 4)) kernelSays
def D (n : Nat) : Prop :=
  Nat.rec (motive := fun _ => Prop) False (fun k _ => Nat.rec (motive := fun _ => Prop) True (fun _ _ => False) k) n
theorem boom : False := @Eq.rec Nat 1 (fun x _ => D x) True.intro 5 oneEqFive
"#,
    );
    Some(text)
}

/// The checked file: a `prelude` module whose one theorem is `False`, taken
/// from the project module above.
const USES_GMP: &str = "prelude
import LeanBasic.SecGmpBase

theorem main : False := boom
";

/// R-sec2 F2: the kernel replay inherits the kernel's trust in the built-in
/// `Nat` operations, which a non-toolchain `prelude` module can redefine.
/// (The copied Prelude also declares its own `propext`/`Quot.sound`/
/// `Classical.choice`/`sorryAx`, so the base module itself is reported with
/// those non-toolchain axioms; the checked file only uses `boom`, which needs
/// none.) Expected: not `Checked` (e.g. Unverified: a non-toolchain module
/// defines a kernel-accelerated constant, or uses `prelude`). Observed:
/// `Checked`, no axioms, for `theorem main : False`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "R-sec2 F2: a prelude file redefining Nat.add (kernel GMP trusts the name) proves False; reported Checked"]
async fn review_sec2_f2_prelude_redefining_kernel_accelerated_nat_add_is_checked() {
    if !lean_available() {
        return;
    }
    let Some(text) = prelude_with_wrong_nat_add() else {
        eprintln!("SKIPPED: the toolchain's Init/Prelude.lean source was not found");
        return;
    };
    let h = harness("lean-basic", Options::default()).await;
    let base = report(&h, "LeanBasic/SecGmpBase.lean", &text).await;
    assert!(base.diagnostics.iter().all(|d| !d.message.contains("error")), "control: Lean accepts the base module");
    let r = report(&h, "LeanBasic/SecGmpMain.lean", USES_GMP).await;
    h.lean.shutdown().await;
    assert!(
        not_checked(&r),
        "DEFECT: `theorem main : False` (kernel GMP vs. a redefined Nat.add) reported Checked: {:?}",
        r.status
    );
}

/// The file's code runs in two places: the audit's compile step (whose process
/// tree is gone before the programs are verified) and the language server's
/// worker for the file, which keeps running during the audit. In the compile
/// step the file records where the audit programs and its `.olean` are; in
/// the worker it starts one background thread that waits for that `.olean`
/// (the end of the compile step), sleeps `.sec2/delay.txt` ms, and replaces
/// the pinned check stub `RunCheck.lean` with a program that prints a clean
/// result for the tag it is given on its command line, then restores it.
const RACE: &str = r##"import Lean
open Lean Elab Command

axiom cheat : False
theorem one_eq_two : (1 : Nat) = 2 := cheat.elim

def fakeCheck : String :=
  "def main (args : List String) : IO UInt32 := do\n" ++
  "  let tag := args.headD \"\"\n" ++
  "  IO.println (tag ++ \" {\\\"kernel\\\":null,\\\"replayed\\\":0}\")\n" ++
  "  IO.println (tag ++ \" {\\\"end\\\":true}\")\n" ++
  "  return 0\n"

def readOr (p : System.FilePath) (d : String) : IO String := do
  try IO.FS.readFile p catch _ => pure d

partial def raceLoop (last : String) : IO Unit := do
  IO.sleep 1
  let cur ← readOr (".sec2" / "olean.txt") ""
  if cur == "" || cur == last then return (← raceLoop last)
  match cur.splitOn "\n" with
  | [olean, check] =>
    let mut n := 0
    while !(← System.FilePath.pathExists olean) && n < 120000 do
      IO.sleep 1
      n := n + 1
    let delay := (← readOr (".sec2" / "delay.txt") "50").trimAscii.copy.toNat!
    IO.sleep delay.toUInt32
    let stub : System.FilePath := (check : System.FilePath) / "RunCheck.lean"
    let tmp : System.FilePath := (check : System.FilePath) / "RunCheck.tmp"
    try
      let genuine ← IO.FS.readFile stub
      IO.FS.writeFile tmp fakeCheck
      IO.FS.rename tmp stub
      IO.FS.writeFile (".sec2" / "swapped.txt") s!"{delay}"
      IO.sleep 3000
      IO.FS.writeFile tmp genuine
      IO.FS.rename tmp stub
    catch e => IO.FS.writeFile (".sec2" / "race-error.txt") (toString e)
    raceLoop cur
  | _ => raceLoop cur

run_cmd do
  IO.FS.createDirAll ".sec2"
  let main ← getMainModule
  if main.toString.startsWith "ExpositionAudit." then
    -- Compile step: [toolchain lib, <tools>/compile, <run dir>, project ...].
    let some path ← IO.getEnv "LEAN_PATH" | return
    let entries := System.SearchPath.parse path
    let some i := entries.findIdx? (fun e => e.fileName == some "compile") | return
    let some compileDir := entries[i]? | return
    let some runDir := entries[i + 1]? | return
    let some tools := compileDir.parent | return
    let olean := runDir / "ExpositionAudit" / (main.getString! ++ ".olean")
    IO.FS.writeFile (".sec2" / "olean.txt") s!"{olean}\n{tools / "check"}"
  else
    let marker := (".sec2" : System.FilePath) / s!"worker-{← IO.Process.getPID}"
    unless ← marker.pathExists do
      IO.FS.writeFile marker ""
      -- Ignore a location left by an earlier audit.
      let _ ← IO.asTask (prio := .dedicated) (do raceLoop (← readOr (".sec2" / "olean.txt") ""))
"##;

/// R-sec2 F3: the programs are verified after the compile step's process tree
/// ends, but the file's code also runs in the language server's worker, which
/// lives through the audit and can replace the verified stub before the check
/// step reads it. Expected: never `Checked` for `one_eq_two` resting on
/// `cheat`. Observed: `Checked` once the delay lands between verification and
/// the check step reading the stub (90 ms on the review machine; earlier
/// delays give `Unverified` "programs were modified", later ones the genuine
/// verdict).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "R-sec2 F3: the language server's worker swaps the pinned check stub between verification and use; 1 = 2 reported Checked"]
async fn review_sec2_f3_worker_swaps_the_check_program_after_verification() {
    if !lean_available() {
        return;
    }
    let h = harness("lean-basic", Options::default()).await;
    let mut forged = None;
    for attempt in 0..30u32 {
        let delay = 10 + attempt * 10;
        std::fs::create_dir_all(h.root.join(".sec2")).unwrap();
        std::fs::write(h.root.join(".sec2").join("delay.txt"), delay.to_string()).unwrap();
        let _ = std::fs::remove_file(h.root.join(".sec2").join("swapped.txt"));
        // A new revision each time (a verdict is per revision).
        let text = format!("{RACE}\n-- attempt {attempt}\n");
        let r = report(&h, "LeanBasic/SecRace.lean", &text).await;
        let swapped = std::fs::read_to_string(h.root.join(".sec2").join("swapped.txt")).ok();
        let error = std::fs::read_to_string(h.root.join(".sec2").join("race-error.txt")).ok();
        println!("attempt {attempt}: delay {delay} ms, swapped={swapped:?}, race error={error:?}");
        if r.checked || matches!(r.status, LeanStatus::Checked { .. }) {
            forged = Some((attempt, delay, r.status));
            break;
        }
        // Let the worker restore the stub before the next attempt.
        tokio::time::sleep(std::time::Duration::from_millis(3500)).await;
    }
    h.lean.shutdown().await;
    assert!(forged.is_none(), "DEFECT: `one_eq_two : 1 = 2` (axiom cheat : False) reported Checked: {forged:?}");
}
