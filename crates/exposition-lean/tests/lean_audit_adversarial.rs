//! Adversarial files against the axiom audit, through the real
//! `LeanService::check` (the path of `lean_check` and the agent tool
//! `check_lean`) and the pinned toolchain.
//!
//! Each file tries to make the audited text, its imports, or its compiled code
//! decide the verdict: hijacked elaborators, printed results, forged
//! completion, kernel checking disabled through syntax the text never spells,
//! and axioms named like the standard ones. Every status must follow from the
//! declarations Lean actually produced, re-checked by the kernel in a process
//! that runs none of the file's code.

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

fn assert_unverified_by_kernel(path: &str, report: &LeanCheckReport) {
    assert!(!report.checked, "{path}: reported checked");
    match &report.status {
        LeanStatus::Unverified { reason, .. } => assert!(reason.contains("kernel rejected"), "{path}: {reason}"),
        other => panic!("{path}: expected Unverified (kernel rejected), got {other:?}"),
    }
}

fn assert_axioms(path: &str, report: &LeanCheckReport, expected: &[&str]) {
    assert!(!report.checked, "{path}: reported checked");
    match &report.status {
        LeanStatus::CheckedWithAxioms { axioms, .. } => assert_eq!(axioms, expected, "{path}"),
        other => panic!("{path}: expected CheckedWithAxioms {expected:?}, got {other:?}"),
    }
}

/// The reviewer's `#eval` hijacker, moved into a module the checked file
/// imports. The old audit imported the checked module (and so this one) into
/// its own program, whose `#eval` this elaborator replaced with a clean result
/// built from the nonce in the audit module's name.
const HIJACK_DEP: &str = r##"import Lean
open Lean Elab Command

@[command_elab Lean.Parser.Command.eval] def auditShim : CommandElab := fun _ => do
  let env ← getEnv
  let some m := env.header.moduleNames.find? (fun n => n.toString.startsWith "ExpositionAudit.T")
    | throwUnsupportedSyntax
  let nonce := m.toString.drop "ExpositionAudit.T".length
  logInfo m!"{nonce}-BEGIN\n{nonce} \{\"n\":\"exposition_audit_end_{nonce}\",\"i\":false,\"ax\":[],\"r\":null}\n{nonce}-END"
"##;

const USES_HIJACK: &str = r##"import LeanBasic.HijackDep

axiom cheat : False
theorem one_eq_two : (1 : Nat) = 2 := cheat.elim
"##;

/// Was DEFECT (fails on the pre-fix audit: `Checked`): an imported module's
/// elaborator decides the verdict.
#[tokio::test(flavor = "multi_thread")]
async fn adversarial_imported_hijacker_cannot_forge_the_audit() {
    if !lean_available() {
        return;
    }
    let h = harness("lean-basic", Options::default()).await;
    write(&h, "LeanBasic/HijackDep.lean", HIJACK_DEP);
    let r = report(&h, "LeanBasic/UsesHijack.lean", USES_HIJACK).await;
    h.lean.shutdown().await;
    assert_axioms("UsesHijack", &r, &["cheat"]);
}

/// A macro that turns every `theorem` into an `axiom` of the same statement.
const MACRO_AXIOM: &str = r##"macro_rules
  | `(theorem $id:declId $sig:declSig := $_) => `(axiom $id $sig)

theorem one_eq_two : (1 : Nat) = 2 := rfl
"##;

/// An elaborator for `trusted theorem …` that adds the stated theorem with a
/// proof of something else, with kernel checking off (option name computed).
const ELAB_NO_KERNEL: &str = r##"import Lean
open Lean Elab Command Term

elab "trusted " "theorem " n:ident " : " t:term " := " v:term : command => liftTermElabM do
  let type ← instantiateMVars (← elabType t)
  let value ← instantiateMVars (← elabTerm v none)
  let add : CoreM Unit := withOptions (fun o => o.setBool (Name.mkStr2 "debug" ("skip" ++ "KernelTC")) true) <|
    addDecl <| .thmDecl { name := n.getId, levelParams := [], type, value }
  add

trusted theorem one_eq_two : (1 : Nat) = 2 := (rfl : 1 = 1)
"##;

/// A macro rewriting `theorem` to `axiom` is reported as the axiom it is (held
/// before the fix too); an elaborator that adds the theorem without the kernel
/// was DEFECT (pre-fix: `Checked`) and is now rejected by the kernel replay.
#[tokio::test(flavor = "multi_thread")]
async fn adversarial_theorem_rewriting_macros_and_elaborators() {
    if !lean_available() {
        return;
    }
    let h = harness("lean-basic", Options::default()).await;
    let macro_axiom = report(&h, "LeanBasic/MacroAxiom.lean", MACRO_AXIOM).await;
    let elab = report(&h, "LeanBasic/ElabNoKernel.lean", ELAB_NO_KERNEL).await;
    h.lean.shutdown().await;
    assert_axioms("MacroAxiom", &macro_axiom, &["one_eq_two"]);
    assert_unverified_by_kernel("ElabNoKernel", &elab);
}

/// `set_option debug.skipKernelTC true in …` produced by a `scoped` macro
/// opened with `open … in`; the option name is computed, never written.
const SKIP_SCOPED: &str = r##"import Lean
open Lean Elab Command

namespace Evil
scoped macro "unchecked " c:command : command => do
  let opt := mkIdent (Name.mkStr2 "debug" ("skip" ++ "KernelTC"))
  `(command| set_option $opt:ident true in $c:command)
end Evil

open Evil in
unchecked run_cmd liftCoreM <| addDecl <| .thmDecl { name := `kbadScoped, levelParams := [], type := mkConst ``False, value := mkConst ``True.intro }

theorem viaScoped : False := kbadScoped
"##;

/// The same through a `local` macro.
const SKIP_LOCAL: &str = r##"import Lean
open Lean Elab Command

section
local macro "unchecked " c:command : command => do
  let opt := mkIdent (Name.mkStr2 "debug" ("skip" ++ "KernelTC"))
  `(command| set_option $opt:ident true in $c:command)

unchecked run_cmd liftCoreM <| addDecl <| .thmDecl { name := `kbadLocal, levelParams := [], type := mkConst ``False, value := mkConst ``True.intro }
end

theorem viaLocal : False := kbadLocal
"##;

/// Written out, under `open … in` (the pre-fix text search found this one).
const SKIP_OPEN_IN: &str = r##"import Lean

open Lean Elab Command in
set_option debug.skipKernelTC true in
run_cmd liftCoreM <| addDecl <| .thmDecl { name := `kbadOpenIn, levelParams := [], type := mkConst ``False, value := mkConst ``True.intro }

theorem viaOpenIn : False := kbadOpenIn
"##;

/// A project module that adds `False` without the kernel; the checked file
/// only imports and uses it.
const KERNEL_DEP: &str = r##"import Lean
open Lean Elab Command

run_cmd liftCoreM <| withOptions (fun o => o.setBool (Name.mkStr2 "debug" ("skip" ++ "KernelTC")) true) <|
  addDecl <| .thmDecl { name := `LeanBasic.kbadDep, levelParams := [], type := mkConst ``False, value := mkConst ``True.intro }
"##;

const USES_KERNEL_DEP: &str = "import LeanBasic.KernelDep\n\ntheorem viaDep : False := LeanBasic.kbadDep\n";

/// Kernel checking disabled through `scoped`/`local` macros and `open … in`,
/// or in an imported project module: whatever the syntax, the kernel replay
/// rejects the unchecked declaration. The scoped, local, and imported cases
/// were DEFECT (pre-fix: `Checked`, the text never names the option).
#[tokio::test(flavor = "multi_thread")]
async fn adversarial_kernel_checking_disabled_through_any_syntax_or_import() {
    if !lean_available() {
        return;
    }
    let h = harness("lean-basic", Options::default()).await;
    write(&h, "LeanBasic/KernelDep.lean", KERNEL_DEP);
    let cases = [
        ("LeanBasic/SkipScoped.lean", SKIP_SCOPED),
        ("LeanBasic/SkipLocal.lean", SKIP_LOCAL),
        ("LeanBasic/SkipOpenIn.lean", SKIP_OPEN_IN),
        ("LeanBasic/UsesKernelDep.lean", USES_KERNEL_DEP),
    ];
    let mut reports = Vec::new();
    for (path, text) in cases {
        reports.push((path, report(&h, path, text).await));
    }
    h.lean.shutdown().await;
    for (path, r) in &reports {
        assert_unverified_by_kernel(path, r);
    }
}

/// An `initialize` block (run whenever the module is imported) that prints a
/// clean audit result carrying the sentinel, with the nonce baked in from the
/// audit module's name at compile time; plus forged result lines printed
/// while the file is compiled, using whatever is left on stdin.
const INIT_FORGE: &str = r##"import Lean
open Lean Elab Term

axiom cheat : False
theorem one_eq_two : (1 : Nat) = 2 := cheat.elim

elab "audit_nonce%" : term => do
  return mkStrLit ((← getMainModule).toString.drop "ExpositionAudit.T".length).copy

initialize do
  let nonce : String := audit_nonce%
  let entry := "{\"n\":\"exposition_audit_end_" ++ nonce ++ "\",\"i\":false,\"ax\":[],\"r\":null}"
  let data := s!"{nonce}-BEGIN\n{nonce} {entry}\n{nonce}-END"
  IO.println (Json.compress (Json.mkObj [("severity", "information"), ("data", data)]))
  let rest ← (← IO.getStdin).getLine
  IO.println (rest.trimAscii.copy ++ " {\"complete\":true,\"stop\":[0,0],\"ranges\":[]}")

run_cmd do
  let rest ← (← IO.getStdin).getLine
  IO.println ("\n" ++ rest.trimAscii.copy ++ " {\"complete\":true,\"stop\":[0,0],\"ranges\":[]}")
"##;

/// Was DEFECT (pre-fix: `Checked`): the module's own `initialize` printed the
/// audit result when the audit program imported it.
#[tokio::test(flavor = "multi_thread")]
async fn adversarial_initialize_and_printed_results_cannot_forge_the_audit() {
    if !lean_available() {
        return;
    }
    let h = harness("lean-basic", Options::default()).await;
    let r = report(&h, "LeanBasic/InitForge.lean", INIT_FORGE).await;
    h.lean.shutdown().await;
    assert_axioms("InitForge", &r, &["cheat"]);
}

/// `implemented_by` gives a definition a lying implementation. The kernel
/// (`decide`) evaluates the definition and is sound; native evaluation trusts
/// the implementation, through `native_decide` behind a macro or through
/// `Lean.Meta.nativeEqTrue` under a tactic name of its own.
const IMPLEMENTED_BY: &str = r##"def trueImpl (_ : Unit) : Bool := true
@[implemented_by trueImpl] def alwaysFalse (_ : Unit) : Bool := false

theorem sound : alwaysFalse () = false := by decide
"##;

const HIDDEN_NATIVE: &str = r##"import Lean
open Lean Meta Elab Tactic

macro "by_computation" : tactic => `(tactic| native_decide)

elab "by_evaluation" : tactic => liftMetaFinishingTactic fun g => do
  let d ← mkDecide (← g.getType)
  match ← nativeEqTrue `by_evaluation d with
  | .success prf => g.assign (← mkAppM ``of_decide_eq_true #[prf])
  | .notTrue => throwError "the proposition evaluated to false"

def trueImpl (_ : Unit) : Bool := true
@[implemented_by trueImpl] def alwaysFalse (_ : Unit) : Bool := false

theorem viaMacro : alwaysFalse () = true := by by_computation
theorem viaEvaluation : alwaysFalse () = true := by by_evaluation
"##;

/// `implemented_by` under `decide` stays Checked (held before); hidden
/// `native_decide` is compiled-code trust (held before); native evaluation
/// under another tactic name was misreported as an ordinary axiom
/// (pre-fix: `CheckedWithAxioms`) and is now compiled-code trust too.
#[tokio::test(flavor = "multi_thread")]
async fn adversarial_implemented_by_and_hidden_native_evaluation() {
    if !lean_available() {
        return;
    }
    let h = harness("lean-basic", Options::default()).await;
    let sound = report(&h, "LeanBasic/ImplementedBy.lean", IMPLEMENTED_BY).await;
    let native = report(&h, "LeanBasic/HiddenNative.lean", HIDDEN_NATIVE).await;
    h.lean.shutdown().await;
    assert!(sound.checked, "ImplementedBy: {:?}", sound.status);
    assert!(matches!(sound.status, LeanStatus::Checked { .. }));
    assert!(!native.checked);
    match &native.status {
        LeanStatus::ReliesOnCompiledCode { declarations, axioms, .. } => {
            for name in ["viaMacro", "viaEvaluation"] {
                assert!(declarations.iter().any(|d| d.declaration == name), "{name}: {declarations:?}");
            }
            assert!(axioms.iter().any(|a| a.contains("._native.by_evaluation.")), "{axioms:?}");
        }
        other => panic!("HiddenNative: expected ReliesOnCompiledCode, got {other:?}"),
    }
}

/// Declares the old audit's end-of-file sentinel (its name follows from the
/// audit module's name), then stops at `#exit` before a bogus theorem.
const EXIT_FORGE: &str = r##"import Lean
open Lean Elab Command

theorem fine : (1 : Nat) = 1 := rfl

run_cmd do
  let nonce := (← getMainModule).toString.drop "ExpositionAudit.T".length
  liftCoreM <| addDecl <| .defnDecl {
    name := Name.mkSimple s!"exposition_audit_end_{nonce}", levelParams := [],
    type := mkConst ``Unit, value := mkConst ``Unit.unit, hints := .abbrev, safety := .safe }

#exit

theorem bogus : False := by exact nonsense
"##;

/// Was DEFECT (pre-fix: `Checked`): the file faked reaching its end. Where
/// elaboration stopped now comes from Lean's frontend in the compile step.
#[tokio::test(flavor = "multi_thread")]
async fn adversarial_early_stop_cannot_claim_completion() {
    if !lean_available() {
        return;
    }
    let h = harness("lean-basic", Options::default()).await;
    let r = report(&h, "LeanBasic/ExitForge.lean", EXIT_FORGE).await;
    h.lean.shutdown().await;
    let exit_line = EXIT_FORGE.lines().position(|l| l.starts_with("#exit")).unwrap() as u32;
    assert!(!r.checked);
    match &r.status {
        LeanStatus::PartiallyChecked { line, .. } => assert_eq!(*line, Some(exit_line)),
        other => panic!("ExitForge: expected PartiallyChecked, got {other:?}"),
    }
}

/// Without `Init`, nothing stops a file from naming its own axiom `propext`.
const FAKE_PROPEXT: &str = r##"prelude

inductive Absurd : Prop
axiom propext : Absurd
theorem boom : Absurd := propext
"##;

/// Was DEFECT (pre-fix: `Checked`, axioms ["propext"]): a standard axiom must
/// be the toolchain's own constant, not just one with the same name.
#[tokio::test(flavor = "multi_thread")]
async fn adversarial_axiom_named_like_a_standard_axiom() {
    if !lean_available() {
        return;
    }
    let h = harness("lean-basic", Options::default()).await;
    let r = report(&h, "LeanBasic/FakePropext.lean", FAKE_PROPEXT).await;
    h.lean.shutdown().await;
    assert_axioms("FakePropext", &r, &["propext (declared outside the toolchain)"]);
}

/// Ordinary Lean that the kernel replay must accept: structures, derived
/// instances, nested and mutual inductives, well-founded and partial
/// definitions, classes, private theorems, an imported project definition,
/// classical logic, quotients, `decide`, `simp`, `omega`, `grind`.
const LEGIT: &str = r##"import LeanBasic.Defs

namespace Legit

structure Point where
  x : Nat
  y : Nat
deriving Repr, DecidableEq, BEq, Hashable

inductive Tree (α : Type) where
  | leaf : Tree α
  | node : Tree α → α → Tree α → Tree α
deriving Repr

def Tree.size {α : Type} : Tree α → Nat
  | .leaf => 0
  | .node l _ r => l.size + 1 + r.size

mutual
  inductive Even : Nat → Prop
    | zero : Even 0
    | succ {n : Nat} : Odd n → Even (n + 1)
  inductive Odd : Nat → Prop
    | succ {n : Nat} : Even n → Odd (n + 1)
end

theorem even_two : Even 2 := .succ (.succ .zero)

def ack : Nat → Nat → Nat
  | 0, n => n + 1
  | m + 1, 0 => ack m 1
  | m + 1, n + 1 => ack m (ack (m + 1) n)
termination_by m n => (m, n)

theorem ack_zero (n : Nat) : ack 0 n = n + 1 := by simp [ack]

partial def collatz (n : Nat) : Nat :=
  if n ≤ 1 then 0 else collatz (if n % 2 = 0 then n / 2 else 3 * n + 1) + 1

class Size (α : Type) where
  size : α → Nat

instance : Size Point := ⟨fun p => p.x + p.y⟩

private theorem helper (a b : Nat) : a + b = b + a := Nat.add_comm a b

theorem uses_import (n : Nat) : LeanBasic.double n = 2 * n := by
  unfold LeanBasic.double
  omega

theorem classical_em (p : Prop) : p ∨ ¬p := Classical.em p

theorem quot_ok : Quot.mk (fun _ _ : Nat => True) 1 = Quot.mk _ 2 := Quot.sound trivial

theorem decided : 10 < 20 := by decide

def sumTo : Nat → Nat
  | 0 => 0
  | n + 1 => sumTo n + (n + 1)

theorem two_mul_sumTo (n : Nat) : 2 * sumTo n = n * (n + 1) := by
  induction n with
  | zero => rfl
  | succ k ih =>
    simp only [sumTo, Nat.mul_add, ih]
    grind

end Legit
"##;

/// A file in Lean's module system (its `.olean` is split into parts).
const MODULE_SYSTEM: &str = r##"module

public theorem pub_thm : 1 + 1 = 2 := rfl
theorem priv_thm : 2 + 2 = 4 := rfl
public def bump (n : Nat) : Nat := n + 1
public theorem bump_pos (n : Nat) : 0 < bump n := Nat.succ_pos n
"##;

/// The statuses that must survive the stricter audit (held before the fix):
/// standard axioms → Checked; sorry → Incomplete; an axiom → checked with
/// axioms; `#exit` → partially checked; and ordinary Lean is Checked.
#[tokio::test(flavor = "multi_thread")]
async fn positive_statuses_survive_the_kernel_replay() {
    if !lean_available() {
        return;
    }
    let h = harness("lean-basic", Options::default()).await;
    let legit = report(&h, "LeanBasic/Legit.lean", LEGIT).await;
    let module = report(&h, "LeanBasic/ModuleSystem.lean", MODULE_SYSTEM).await;
    let sorry = report(&h, "LeanBasic/WithSorry.lean", "theorem later (n : Nat) : n + 0 = n := by\n  sorry\n").await;
    let axiom = report(&h, "LeanBasic/WithAxiom.lean", "axiom choice' : 1 = 1\ntheorem t : 1 = 1 := choice'\n").await;
    let exit =
        report(&h, "LeanBasic/WithExit.lean", "theorem a : 1 = 1 := rfl\n#exit\ntheorem b : 2 = 2 := rfl\n").await;
    h.lean.shutdown().await;
    for (name, r) in [("Legit", &legit), ("ModuleSystem", &module)] {
        assert!(r.checked, "{name}: {:?}", r.status);
        match &r.status {
            LeanStatus::Checked { axioms, .. } => {
                assert!(
                    axioms.iter().all(|a| exposition_lean::audit::STANDARD_AXIOMS.contains(&a.as_str())),
                    "{axioms:?}"
                )
            }
            other => panic!("{name}: {other:?}"),
        }
    }
    let LeanStatus::Checked { axioms, .. } = &legit.status else { unreachable!() };
    assert_eq!(axioms, &["Classical.choice", "Quot.sound", "propext"]);
    assert!(matches!(sorry.status, LeanStatus::CheckedWithSorry { count: 1, .. }), "{:?}", sorry.status);
    assert_axioms("WithAxiom", &axiom, &["choice'"]);
    assert!(matches!(exit.status, LeanStatus::PartiallyChecked { line: Some(1), .. }), "{:?}", exit.status);
}
