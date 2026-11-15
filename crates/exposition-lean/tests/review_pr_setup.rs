//! R-pr: independent review probes of the guided Mathlib setup and the
//! package gate (change set e6eca9d..fa6c16e).
//!
//! `review_pr_f*` tests are ignored until fixed: each asserts the expected
//! behavior and fails on the reviewed build. `review_pr_held_*` tests pass.
//!
//! Nothing here uses the network or the real Lake: where a step has to run,
//! the pinned toolchain's `lake` is a copy of a small system program that
//! exits with an error for Lake's arguments.
//!
//!   cargo test -p exposition-lean --test review_pr_setup -- --include-ignored --nocapture --test-threads=1

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use exposition_execution::env::HostEnv;
use exposition_execution::{ExecutionConfig, ExecutionService, ExecutionSettings};
use exposition_lean::setup::{
    LeanPackageInspection, LeanPackageReadiness, LeanSetup, LeanSetupPlan, LeanSetupRequest, LeanSetupState,
    LeanSetupStatus, SetupRecords, inspect_package,
};

const PIN: &str = "leanprover/lean4:v4.35.0";
const MATHLIB_REV: &str = "56a005855ea17e0b0958f73ec801ae140b0a1679";

fn manifest(mathlib_url: &str, batteries_url: &str, batteries_rev: &str) -> String {
    format!(
        r#"{{"version": "1.3.0", "packagesDir": ".lake/packages", "packages": [
  {{"url": "{mathlib_url}", "type": "git", "rev": "{MATHLIB_REV}", "name": "mathlib", "inputRev": "v4.35.0"}},
  {{"url": "{batteries_url}", "type": "git", "rev": "{batteries_rev}", "name": "batteries", "inputRev": "main"}}]}}"#
    )
}

const OFFICIAL_MATHLIB: &str = "https://github.com/leanprover-community/mathlib4";
const OFFICIAL_BATTERIES: &str = "https://github.com/leanprover-community/batteries";
const BATTERIES_REV: &str = "66e469188fe07963e5cf666c7d4433f45b8131c5";
const LAKEFILE: &str = "name = \"Formal\"\n\n[[require]]\nname = \"mathlib\"\nscope = \"leanprover-community\"\nrev = \"v4.35.0\"\n\n[[lean_lib]]\nname = \"Formal\"\n";

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    exec: ExecutionService,
    records: Arc<SetupRecords>,
    setup: LeanSetup,
}

/// A program that starts, prints an error for Lake's arguments, and exits
/// with a failure code (never Lake, never the network).
fn failing_program() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into()))
            .join("System32")
            .join("whoami.exe")
    } else {
        PathBuf::from("/bin/false")
    }
}

impl Fixture {
    fn new(trusted: bool) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        let project = base.join("project");
        let home = base.join("elan");
        std::fs::create_dir_all(&project).unwrap();
        let bin = home.join("toolchains").join("leanprover--lean4---v4.35.0").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        for name in ["lean", "lake"] {
            let file = bin.join(if cfg!(windows) { format!("{name}.exe") } else { name.to_owned() });
            std::fs::copy(failing_program(), file).unwrap();
        }
        let exec = ExecutionService::with_config(ExecutionConfig {
            data_dir: base.join("data"),
            host_env: HostEnv::capture(),
            settings: Some(ExecutionSettings::default()),
            too_broad_to_trust: Some(vec![]),
            tool_cache_dir: None,
        })
        .unwrap();
        let root = exec.register_workspace_root(&project).unwrap();
        if trusted {
            exec.grant_trust(&root.display().to_string()).unwrap();
        }
        let records = Arc::new(SetupRecords::new(Some(base.join("data").join("lean").join("setup-records.json"))));
        let setup = LeanSetup::new(exec.clone(), Some(home.clone()), Arc::clone(&records), Box::new(|_| {}));
        Fixture { _tmp: tmp, root, home, exec, records, setup }
    }

    /// An existing Mathlib package in `folder`: lakefile, pin, manifest, and
    /// the dependency checkouts, without Mathlib's build outputs.
    fn package(&self, folder: &str) -> PathBuf {
        let dir = self.root.join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lakefile.toml"), LAKEFILE).unwrap();
        std::fs::write(dir.join("lean-toolchain"), format!("{PIN}\n")).unwrap();
        std::fs::write(
            dir.join("lake-manifest.json"),
            manifest(OFFICIAL_MATHLIB, OFFICIAL_BATTERIES, BATTERIES_REV),
        )
        .unwrap();
        for name in ["mathlib", "batteries"] {
            std::fs::create_dir_all(dir.join(".lake").join("packages").join(name)).unwrap();
        }
        std::fs::write(dir.join(".lake").join("packages").join("mathlib").join("lean-toolchain"), PIN).unwrap();
        dir
    }

    fn inspect(&self, folder: &str) -> LeanPackageInspection {
        inspect_package(&self.root, &self.root.join(folder), Some(&self.home), &self.records)
    }

    fn prepare(folder: &str) -> LeanSetupRequest {
        LeanSetupRequest::PrepareMathlibPackage { folder: folder.into() }
    }

    fn run(&self, plan: &LeanSetupPlan) -> LeanSetupStatus {
        let started = self.setup.start(&self.root, plan).expect("the setup starts");
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime
            .block_on(async { tokio::time::timeout(Duration::from_secs(120), self.setup.wait(&started.id)).await })
            .expect("the setup ends")
            .expect("its status")
    }
}

fn unprepared(root: &Path, files: &[(&str, &str)]) -> LeanPackageInspection {
    for (name, text) in files {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    inspect_package(root, root, None, &SetupRecords::new(None))
}

// ---------------------------------------------------------------------------
// Findings (ignored until fixed)

/// R-pr-F1. A package whose Mathlib cache is missing can be checked by
/// building from source (the user's explicit choice). A setup of it that
/// fails once (offline, no cache for the revision) takes that away: the
/// package is `Unfinished`, which the build-from-source choice does not lift,
/// until a setup succeeds.
#[test]
#[ignore = "R-pr-F1: a failed setup of an existing package removes the build-from-source route"]
fn review_pr_f1_failed_prepare_keeps_build_from_source_available() {
    let f = Fixture::new(true);
    f.package("lean");
    let before = f.inspect("lean");
    assert_eq!(before.readiness, LeanPackageReadiness::CacheMissing);
    assert!(before.blocks_checking(true).is_none(), "before: the user's choice to build from source lifts the gate");

    let plan = f.setup.plan(&f.root, &Fixture::prepare("lean")).unwrap();
    let status = f.run(&plan);
    println!("setup ended: {:?}: {:?}", status.state, status.message);
    assert_eq!(status.state, LeanSetupState::Failed);
    for kept in ["lakefile.toml", "lean-toolchain", "lake-manifest.json", ".lake/packages/mathlib/lean-toolchain"] {
        assert!(f.root.join("lean").join(kept).exists(), "{kept} was kept");
    }

    let after = f.inspect("lean");
    println!("after the failed setup: {:?}", after.readiness);
    assert!(
        after.blocks_checking(true).is_none(),
        "after a failed setup the package that could be built from source is blocked: {:?}",
        after.blocks_checking(true)
    );
}

/// R-pr-F2. After a failed setup the package stays recorded as unfinished.
/// When the user then drops the Mathlib requirement (an ordinary Lake
/// package), the record still blocks every Lean file in it, and no setup can
/// clear it: preparing refuses (`noMathlib`), a new package refuses
/// (`packageExists`). Only editing the application's data folder recovers.
#[test]
#[ignore = "R-pr-F2: an unfinished-setup record strands a package that no longer uses Mathlib"]
fn review_pr_f2_unfinished_record_does_not_strand_an_ordinary_package() {
    let f = Fixture::new(true);
    let dir = f.package("lean");
    let plan = f.setup.plan(&f.root, &Fixture::prepare("lean")).unwrap();
    assert_eq!(f.run(&plan).state, LeanSetupState::Failed);

    // The user gives up on Mathlib here: an ordinary package without dependencies.
    std::fs::write(dir.join("lakefile.toml"), "name = \"Formal\"\n\n[[lean_lib]]\nname = \"Formal\"\n").unwrap();
    std::fs::remove_file(dir.join("lake-manifest.json")).unwrap();
    std::fs::remove_dir_all(dir.join(".lake")).unwrap();

    let after = f.inspect("lean");
    let blocked = after.blocks_checking(false);
    let prepare = f.setup.plan(&f.root, &Fixture::prepare("lean")).map(|_| ()).map_err(|e| e.code);
    let create = f
        .setup
        .plan(&f.root, &LeanSetupRequest::NewMathlibPackage { folder: "lean".into(), name: "Formal".into() })
        .map(|_| ())
        .map_err(|e| e.code);
    println!("uses mathlib: {}; blocked: {blocked:?}; prepare: {prepare:?}; new: {create:?}", after.uses_mathlib);
    assert!(
        blocked.is_none() || prepare.is_ok() || create.is_ok(),
        "an ordinary Lake package is blocked with no supported way out: {blocked:?} / {prepare:?} / {create:?}"
    );
}

/// R-pr-F3. The gate decides "requires Mathlib" from the lakefile's text when
/// there is no manifest. Spellings Lake accepts (checked with Lake 4.35.0 on
/// a local path dependency) are not recognized, so the Lean server starts on
/// the unprepared package and Lake clones Mathlib and builds it from source
/// inside the language server: what the gate exists to prevent.
#[test]
#[ignore = "R-pr-F3: Mathlib requirements the gate does not recognize"]
fn review_pr_f3_the_gate_recognizes_mathlib_requirements_lake_accepts() {
    let cases: [(&str, &str, &str); 4] = [
        (
            "TOML literal string",
            "lakefile.toml",
            "name = \"Formal\"\n\n[[require]]\nname = 'mathlib'\nscope = 'leanprover-community'\n",
        ),
        (
            "TOML inline array of tables",
            "lakefile.toml",
            "name = \"Formal\"\nrequire = [{name = \"mathlib\", scope = \"leanprover-community\"}]\n",
        ),
        (
            "lakefile.lean, name on the next line",
            "lakefile.lean",
            "import Lake\nopen Lake DSL\npackage Formal\nrequire\n  mathlib from git \"https://github.com/leanprover-community/mathlib4\"\n",
        ),
        (
            "lakefile.lean, documented requirement",
            "lakefile.lean",
            "import Lake\nopen Lake DSL\npackage Formal\n/-- the library -/ require mathlib from git \"https://github.com/leanprover-community/mathlib4\"\n",
        ),
    ];
    let mut missed = Vec::new();
    for (what, lakefile, text) in cases {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let inspection = unprepared(&root, &[(lakefile, text), ("lean-toolchain", "leanprover/lean4:v4.35.0\n")]);
        // No elan home: the toolchain is "missing", which alone never blocks;
        // what matters is whether the package is seen to use Mathlib.
        println!("{what}: usesMathlib={} readiness={:?}", inspection.uses_mathlib, inspection.readiness);
        if !inspection.uses_mathlib {
            missed.push(what);
        }
    }
    assert!(missed.is_empty(), "Mathlib requirement not recognized (the Lean server would start): {missed:?}");
}

/// R-pr-F4a. The plan names the Mathlib revision and says the cache comes
/// from cache.mathlib.org, but not where the package called `mathlib` comes
/// from. A manifest (or lakefile) that takes `mathlib` from another
/// repository gets the same plan text; `lake exe cache get` then builds and
/// runs that repository's `cache` program.
#[test]
#[ignore = "R-pr-F4: the plan does not show where `mathlib` is fetched from"]
fn review_pr_f4_the_plan_shows_a_mathlib_that_is_not_the_official_repository() {
    let f = Fixture::new(false);
    let dir = f.package("lean");
    std::fs::remove_dir_all(dir.join(".lake")).unwrap();
    let official = f.setup.plan(&f.root, &Fixture::prepare("lean")).unwrap();
    std::fs::write(
        dir.join("lake-manifest.json"),
        manifest("https://github.com/attacker-example/mathlib4", OFFICIAL_BATTERIES, BATTERIES_REV),
    )
    .unwrap();
    match f.setup.plan(&f.root, &Fixture::prepare("lean")) {
        Err(e) => println!("refused: {}", e.code),
        Ok(other) => {
            let text = serde_json::to_string(&other).unwrap();
            println!("summary: {}", other.summary);
            assert!(
                text.contains("attacker-example"),
                "the plan for a `mathlib` from another repository reads exactly like the official one \
                 (same hash: {})",
                other.plan_hash == official.plan_hash
            );
        }
    }
}

/// R-pr-F4b. Starting re-plans and refuses a plan that changed, but the plan
/// (and its hash) does not cover the manifest or the lakefile: another
/// dependency can be redirected to another repository and revision between
/// review and start, and the reviewed plan still starts. (The folder is not
/// trusted here, so nothing runs: `lean.untrusted` shows the stale check was
/// passed.)
#[test]
#[ignore = "R-pr-F4: a manifest changed after the plan was reviewed is not a stale plan"]
fn review_pr_f4_a_changed_manifest_makes_the_reviewed_plan_stale() {
    let f = Fixture::new(false);
    let dir = f.package("lean");
    std::fs::remove_dir_all(dir.join(".lake")).unwrap();
    let plan = f.setup.plan(&f.root, &Fixture::prepare("lean")).unwrap();
    std::fs::write(
        dir.join("lake-manifest.json"),
        manifest(
            OFFICIAL_MATHLIB,
            "https://github.com/attacker-example/batteries",
            "0000000000000000000000000000000000000001",
        ),
    )
    .unwrap();
    let code = f.setup.start(&f.root, &plan).map(|s| s.id).map_err(|e| e.code);
    println!("start after the manifest changed: {code:?}");
    assert_eq!(code, Err("lean.setup.planStale".to_owned()));
}

// ---------------------------------------------------------------------------
// Held

/// A plan the webview altered is refused before trust is even looked at, and
/// an unaltered plan does not start in a folder that is not trusted.
#[test]
fn review_pr_held_altered_plans_and_untrusted_folders_do_not_start() {
    let f = Fixture::new(false);
    let dir = f.package("lean");
    let plan = f.setup.plan(&f.root, &Fixture::prepare("lean")).unwrap();
    let start = |plan: &LeanSetupPlan| f.setup.start(&f.root, plan).map(|s| s.id).map_err(|e| e.code);
    assert_eq!(start(&plan), Err("lean.untrusted".to_owned()));

    let stale = Err("lean.setup.planStale".to_owned());
    let mut program = plan.clone();
    program.steps[0].program = "cmd.exe".into();
    assert_eq!(start(&program), stale);
    let mut args = plan.clone();
    args.steps[0].args = vec!["script".into(), "evil".into()];
    assert_eq!(start(&args), stale);
    let mut environment = plan.clone();
    environment.steps[0].environment.insert("PATH".into(), "C:\\evil".into());
    assert_eq!(start(&environment), stale);
    let mut extra = plan.clone();
    extra.steps.push(extra.steps[0].clone());
    assert_eq!(start(&extra), stale);
    let mut elsewhere = plan.clone();
    elsewhere.package_root = f.home.display().to_string();
    assert_eq!(start(&elsewhere), stale);
    let mut root = plan.clone();
    root.root = f.home.display().to_string();
    assert_eq!(start(&root), stale);
    let mut cleanup = plan.clone();
    cleanup.removed_on_failure.push("lakefile.toml".into());
    assert_eq!(start(&cleanup), stale);
    let mut timeout = plan.clone();
    timeout.steps[0].timeout_ms = u64::MAX;
    assert_eq!(start(&timeout), stale);

    // The toolchain pin changed after the review.
    std::fs::write(dir.join("lean-toolchain"), "leanprover/lean4:v4.34.0\n").unwrap();
    assert_eq!(start(&plan), stale);
    assert!(f.setup.status().is_none(), "nothing ran");
    assert!(f.exec.trust_status(&f.root.display().to_string()).is_ok_and(|s| !s.trusted));
}

/// Folders that leave the project, hidden or tool folders, and hostile
/// toolchain pins are refused when planning.
#[test]
fn review_pr_held_hostile_folders_and_pins_are_refused() {
    let f = Fixture::new(false);
    f.package("lean");
    let outside = f.root.parent().unwrap().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("lakefile.toml"), LAKEFILE).unwrap();
    std::fs::write(outside.join("lean-toolchain"), format!("{PIN}\n")).unwrap();
    let code = |request: LeanSetupRequest| f.setup.plan(&f.root, &request).map(|p| p.package_root).map_err(|e| e.code);
    let invalid = Err("lean.setup.invalidFolder".to_owned());
    for folder in [
        "../outside",
        "lean/../../outside",
        "/outside",
        "\\outside",
        "C:/Windows",
        "C:\\Windows",
        "\\\\?\\C:\\Windows",
        "//server/share",
        ".lake",
        "lean/.lake/packages/mathlib",
        ".git",
        "lean.",
        "lean::$DATA",
    ] {
        assert_eq!(code(Fixture::prepare(folder)), invalid, "prepare {folder:?}");
        assert_eq!(
            code(LeanSetupRequest::NewMathlibPackage { folder: folder.into(), name: "Formal".into() }),
            invalid,
            "new {folder:?}"
        );
    }
    // A junction (Windows) or symbolic link out of the project.
    let link = f.root.join("link");
    #[cfg(windows)]
    let linked = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&link)
        .arg(&outside)
        .output()
        .is_ok_and(|o| o.status.success());
    #[cfg(not(windows))]
    let linked = std::os::unix::fs::symlink(&outside, &link).is_ok();
    assert!(linked, "the link was made");
    assert_eq!(code(Fixture::prepare("link")), invalid);
    assert_eq!(code(LeanSetupRequest::NewMathlibPackage { folder: "link/new".into(), name: "Formal".into() }), invalid);
    assert_eq!(code(LeanSetupRequest::NewMathlibPackage { folder: "link".into(), name: "Formal".into() }), invalid);

    for name in ["formal", "Formal.Evil", "Formal\"\nscope = \"x", "../Up", "Formal Name", "", "Mathlib", "Con", "A-b", "Fórmal"] {
        let got = code(LeanSetupRequest::NewMathlibPackage { folder: "fresh".into(), name: name.into() });
        assert_eq!(got, Err("lean.setup.invalidName".to_owned()), "{name:?}");
    }
    for pin in ["-v", "--help", "a b", "x;y", "x\ty", "../../evil", "/abs", "a\"b", "$(x)", "a|b", "a&b"] {
        std::fs::write(f.root.join("lean").join("lean-toolchain"), format!("{pin}\n")).unwrap();
        let got = code(Fixture::prepare("lean"));
        assert!(
            got.as_ref().err().map(String::as_str) == Some("lean.setup.invalidToolchain"),
            "pin {pin:?} must be refused as invalid, got {got:?}"
        );
    }
}

/// R-pr-F7. A package pinned to a channel (`stable`) is inspected as having
/// its toolchain installed (the newest installed release is used for
/// checking), but the plan's commands name `toolchains/leanprover--lean4---stable/bin/lake`,
/// which is not where that toolchain is: the setup cannot start its first
/// command.
#[test]
#[ignore = "R-pr-F7: the plan for a channel-pinned package names a lake that does not exist"]
fn review_pr_f7_a_channel_pin_plans_the_lake_that_is_installed() {
    let f = Fixture::new(false);
    let dir = f.package("lean");
    std::fs::write(dir.join("lean-toolchain"), "stable\n").unwrap();
    std::fs::write(dir.join(".lake").join("packages").join("mathlib").join("lean-toolchain"), "stable\n").unwrap();
    let inspection = f.inspect("lean");
    println!("toolchain installed: {}; readiness: {:?}", inspection.toolchain_installed, inspection.readiness);
    assert!(inspection.toolchain_installed, "the channel resolves to the installed release");
    match f.setup.plan(&f.root, &Fixture::prepare("lean")) {
        Err(e) => println!("refused with guidance: {} {}", e.code, e.message),
        Ok(plan) => {
            let lake = plan.steps.iter().map(|s| s.program.clone()).find(|p| p.contains("lake")).unwrap_or_default();
            println!("planned lake: {lake}");
            assert!(Path::new(&lake).is_file(), "the plan runs a lake that is not installed: {lake}");
        }
    }
}

/// R-pr-F0. A new package in a folder that does not exist yet (the dialog's
/// default, `lean`) cannot be started in a trusted project: trust is looked
/// up for the package folder, which has to exist to be looked up, so the
/// start answers `lean.untrusted` ("Trust the project folder first") and
/// trusting the project again changes nothing.
#[test]
#[ignore = "R-pr-F0: a new package in a folder that does not exist yet never starts"]
fn review_pr_f0_a_new_package_in_a_new_folder_starts_in_a_trusted_project() {
    let f = Fixture::new(true);
    let request = LeanSetupRequest::NewMathlibPackage { folder: "fresh".into(), name: "Formal".into() };
    let plan = f.setup.plan(&f.root, &request).unwrap();
    assert!(f.exec.trust_status(&f.root.display().to_string()).is_ok_and(|s| s.trusted), "the project is trusted");
    let started = f.setup.start(&f.root, &plan).map(|s| s.id).map_err(|e| format!("{}: {}", e.code, e.message));
    println!("start in a folder that does not exist yet: {started:?}");
    if let Ok(id) = &started {
        // The stand-in lake fails at once; let the run end and clean up.
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let _ = runtime.block_on(async { tokio::time::timeout(Duration::from_secs(120), f.setup.wait(id)).await });
    }
    assert!(started.is_ok(), "{started:?}");
    // The same request with the folder made first does start (control).
}
