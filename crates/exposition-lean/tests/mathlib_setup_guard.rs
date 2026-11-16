//! The guided Mathlib setup and the package gate against the real execution
//! service and the real setup service: what a failed setup leaves possible,
//! what the gate recognizes, what a plan shows and binds, and what is refused.
//!
//! Nothing here uses the network or the real Lake: where a step has to run,
//! the pinned toolchain's `lake` is a copy of a small system program that
//! exits with an error for Lake's arguments.

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
        std::fs::write(dir.join("lake-manifest.json"), manifest(OFFICIAL_MATHLIB, OFFICIAL_BATTERIES, BATTERIES_REV))
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
// After a failed setup

/// A package whose Mathlib cache is missing can be checked by building from
/// source (the user's explicit choice). A setup of it that fails (offline,
/// no cache for the revision) does not take that away: its steps wrote build
/// outputs only, so the choice still lifts the gate, while the package is
/// not offered as ready.
#[test]
fn a_failed_prepare_keeps_build_from_source_available() {
    let f = Fixture::new(true);
    f.package("lean");
    let before = f.inspect("lean");
    assert_eq!(before.readiness, LeanPackageReadiness::CacheMissing);
    assert!(before.blocks_checking(true).is_none(), "before: the user's choice to build from source lifts the gate");

    let plan = f.setup.plan(&f.root, &Fixture::prepare("lean")).unwrap();
    let status = f.run(&plan);
    assert_eq!(status.state, LeanSetupState::Failed, "{:?}", status.message);
    let message = status.message.clone().unwrap_or_default();
    assert!(message.contains("Building Mathlib from source stays possible"), "{message}");
    for kept in ["lakefile.toml", "lean-toolchain", "lake-manifest.json", ".lake/packages/mathlib/lean-toolchain"] {
        assert!(f.root.join("lean").join(kept).exists(), "{kept} was kept");
    }

    let after = f.inspect("lean");
    assert!(
        matches!(after.readiness, LeanPackageReadiness::Unfinished { can_build_from_source: true, .. }),
        "{:?}",
        after.readiness
    );
    assert!(after.blocks_checking(false).is_some(), "not ready, and never started without the user's choice");
    assert!(
        after.blocks_checking(true).is_none(),
        "after a failed setup the package that could be built from source is blocked: {:?}",
        after.blocks_checking(true)
    );
    // Preparing again is still offered.
    f.setup.plan(&f.root, &Fixture::prepare("lean")).unwrap();

    // A setup that had to fetch the pinned dependencies may leave partial
    // checkouts: there the choice to build from source does not lift the gate.
    let partial = f.package("partial");
    std::fs::remove_dir_all(partial.join(".lake").join("packages").join("batteries")).unwrap();
    let plan = f.setup.plan(&f.root, &Fixture::prepare("partial")).unwrap();
    assert_eq!(f.run(&plan).state, LeanSetupState::Failed);
    std::fs::create_dir_all(partial.join(".lake").join("packages").join("batteries")).unwrap();
    let after = f.inspect("partial");
    assert!(
        matches!(after.readiness, LeanPackageReadiness::Unfinished { can_build_from_source: false, .. }),
        "{:?}",
        after.readiness
    );
    assert!(after.blocks_checking(true).is_some());
}

/// After a failed setup the package stays recorded as unfinished. When the
/// user then drops the Mathlib requirement (an ordinary Lake package), the
/// record no longer holds the package: its Lean files are checked.
#[test]
fn an_unfinished_record_does_not_strand_a_package_that_dropped_mathlib() {
    let f = Fixture::new(true);
    let dir = f.package("lean");
    let plan = f.setup.plan(&f.root, &Fixture::prepare("lean")).unwrap();
    assert_eq!(f.run(&plan).state, LeanSetupState::Failed);

    // The user gives up on Mathlib here: an ordinary package without dependencies.
    std::fs::write(dir.join("lakefile.toml"), "name = \"Formal\"\n\n[[lean_lib]]\nname = \"Formal\"\n").unwrap();
    std::fs::remove_file(dir.join("lake-manifest.json")).unwrap();
    std::fs::remove_dir_all(dir.join(".lake")).unwrap();

    let after = f.inspect("lean");
    assert!(!after.uses_mathlib);
    assert!(!matches!(after.readiness, LeanPackageReadiness::Unfinished { .. }), "{:?}", after.readiness);
    assert_eq!(after.blocks_checking(false), None, "an ordinary Lake package is not held by the Mathlib record");
}

// ---------------------------------------------------------------------------
// The gate

/// The gate decides "requires Mathlib" from the lakefile when there is no
/// manifest. Every spelling Lake accepts (checked with Lake 4.35.0 on a
/// local path dependency) is recognized, so the Lean server never starts on
/// the unprepared package (where Lake would clone Mathlib and build it from
/// source inside the language server).
#[test]
fn the_gate_recognizes_mathlib_requirements_lake_accepts() {
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
        // No elan home, so the toolchain is "missing": that never hides that
        // the package uses Mathlib and is not prepared.
        if !inspection.uses_mathlib || inspection.blocks_checking(true).is_none() {
            missed.push((what, inspection.readiness));
        }
    }
    assert!(missed.is_empty(), "Mathlib requirement not recognized (the Lean server would start): {missed:?}");
}

/// A `lakefile.lean` is code: when it requires anything and nothing is
/// resolved, the server does not start, and the reason says what to do.
#[test]
fn unresolved_requirements_in_a_lean_lakefile_do_not_start_the_server() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let inspection = unprepared(
        &root,
        &[
            (
                "lakefile.lean",
                "import Lake\nopen Lake DSL\npackage Formal\nrequire helper from git \"https://example.org/helper\"\n",
            ),
            ("lean-toolchain", "leanprover/lean4:v4.35.0\n"),
        ],
    );
    assert!(!inspection.uses_mathlib);
    let reason = inspection.blocks_checking(false).expect("not started");
    assert!(reason.contains("lake update") && reason.contains("lakefile.lean"), "{reason}");
    // A package that requires nothing is checked as before.
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let plain = unprepared(
        &root,
        &[
            ("lakefile.lean", "import Lake\nopen Lake DSL\npackage Formal\nlean_lib Formal\n"),
            ("lean-toolchain", "leanprover/lean4:v4.35.0\n"),
        ],
    );
    assert_eq!(plain.blocks_checking(false), None);
}

// ---------------------------------------------------------------------------
// The plan

/// The plan lists where each dependency is fetched from, with the commit. A
/// manifest that takes `mathlib` from another repository is refused with
/// that address: `lake exe cache get` would build and run that repository's
/// `cache` program.
#[test]
fn the_plan_names_its_sources_and_refuses_a_mathlib_from_another_repository() {
    let f = Fixture::new(false);
    let dir = f.package("lean");
    std::fs::remove_dir_all(dir.join(".lake")).unwrap();
    let official = f.setup.plan(&f.root, &Fixture::prepare("lean")).unwrap();
    let sources: Vec<(&str, Option<&str>, Option<&str>)> =
        official.sources.iter().map(|s| (s.name.as_str(), s.url.as_deref(), s.rev.as_deref())).collect();
    assert_eq!(
        sources,
        [
            ("batteries", Some(OFFICIAL_BATTERIES), Some(BATTERIES_REV)),
            ("mathlib", Some(OFFICIAL_MATHLIB), Some(MATHLIB_REV))
        ]
    );
    assert!(official.manifest_digest.is_some() && official.lakefile_digest.is_some());
    std::fs::write(
        dir.join("lake-manifest.json"),
        manifest("https://github.com/attacker-example/mathlib4", OFFICIAL_BATTERIES, BATTERIES_REV),
    )
    .unwrap();
    let refused = f.setup.plan(&f.root, &Fixture::prepare("lean")).expect_err("not planned");
    assert_eq!(refused.code, "lean.setup.unofficialMathlib");
    assert!(refused.message.contains("https://github.com/attacker-example/mathlib4"), "{}", refused.message);
    // The plan reviewed before the manifest changed does not start either.
    let code = f.setup.start(&f.root, &official).map(|s| s.id).map_err(|e| e.code);
    assert_eq!(code, Err("lean.setup.unofficialMathlib".to_owned()));
    assert!(f.setup.status().is_none(), "nothing ran");
}

/// Starting re-plans and refuses a plan that changed, and the plan covers the
/// manifest and the lakefile: a dependency redirected to another repository
/// and revision between review and start makes the reviewed plan stale, and
/// so does an edited lakefile. (The folder is not trusted here: the stale
/// check comes before trust, and nothing runs.)
#[test]
fn a_changed_manifest_or_lakefile_makes_the_reviewed_plan_stale() {
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
    assert_eq!(code, Err("lean.setup.planStale".to_owned()));
    let replanned = f.setup.plan(&f.root, &Fixture::prepare("lean")).unwrap();
    assert_ne!(replanned.plan_hash, plan.plan_hash);
    assert!(serde_json::to_string(&replanned).unwrap().contains("https://github.com/attacker-example/batteries"));

    // The lakefile edited after the review (same manifest).
    std::fs::write(
        dir.join("lakefile.toml"),
        format!("{LAKEFILE}\n[[require]]\nname = \"extra\"\ngit = \"https://example.org/extra\"\n"),
    )
    .unwrap();
    let code = f.setup.start(&f.root, &replanned).map(|s| s.id).map_err(|e| e.code);
    assert_eq!(code, Err("lean.setup.planStale".to_owned()));
    assert!(f.setup.status().is_none(), "nothing ran");
}

/// A plan the webview altered is refused before trust is even looked at, and
/// an unaltered plan does not start in a folder that is not trusted.
#[test]
fn altered_plans_and_untrusted_folders_do_not_start() {
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
fn hostile_folders_and_pins_are_refused() {
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

    for name in [
        "formal",
        "Formal.Evil",
        "Formal\"\nscope = \"x",
        "../Up",
        "Formal Name",
        "",
        "Mathlib",
        "Con",
        "A-b",
        "Fórmal",
    ] {
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

/// A package pinned to a channel (`stable`) is inspected as having its
/// toolchain installed (the newest installed release is used for checking),
/// but a channel names no toolchain folder and no Lean version a Mathlib
/// revision is built for: planning refuses with guidance, and never names a
/// `lake` that does not exist.
#[test]
fn a_channel_pin_is_refused_with_guidance_and_no_plan_names_a_missing_lake() {
    let f = Fixture::new(false);
    let dir = f.package("lean");
    std::fs::write(dir.join("lean-toolchain"), "stable\n").unwrap();
    std::fs::write(dir.join(".lake").join("packages").join("mathlib").join("lean-toolchain"), "stable\n").unwrap();
    let inspection = f.inspect("lean");
    assert!(inspection.toolchain_installed, "the channel resolves to the installed release");
    let refused = f.setup.plan(&f.root, &Fixture::prepare("lean")).expect_err("a channel pin is not planned");
    assert_eq!(refused.code, "lean.setup.channelPin");
    assert!(refused.message.contains("`stable`") && refused.message.contains("lean-toolchain"), "{}", refused.message);
    // With the exact release written, every planned program is installed.
    std::fs::write(dir.join("lean-toolchain"), "v4.35.0\n").unwrap();
    std::fs::write(dir.join(".lake").join("packages").join("mathlib").join("lean-toolchain"), PIN).unwrap();
    let plan = f.setup.plan(&f.root, &Fixture::prepare("lean")).unwrap();
    for step in &plan.steps {
        assert!(Path::new(&step.program).is_file(), "the plan runs a program that is not installed: {}", step.program);
    }
}

/// A new package in a folder that does not exist yet (the dialog's default,
/// `lean`) starts in a trusted project: trust is decided by the nearest
/// existing folder. In a project that is not trusted it does not start.
#[test]
fn a_new_package_in_a_new_folder_starts_in_a_trusted_project() {
    let f = Fixture::new(true);
    let request = LeanSetupRequest::NewMathlibPackage { folder: "fresh".into(), name: "Formal".into() };
    let plan = f.setup.plan(&f.root, &request).unwrap();
    assert!(f.exec.trust_status(&f.root.display().to_string()).is_ok_and(|s| s.trusted), "the project is trusted");
    let started = f.setup.start(&f.root, &plan).map(|s| s.id).map_err(|e| format!("{}: {}", e.code, e.message));
    let id = started.expect("the setup starts");
    // The stand-in lake fails at once: the run ends and removes what it created.
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let ended = runtime
        .block_on(async { tokio::time::timeout(Duration::from_secs(120), f.setup.wait(&id)).await })
        .expect("the setup ends")
        .expect("its status");
    assert_eq!(ended.state, LeanSetupState::Failed, "{:?}", ended.message);
    assert!(ended.kept.is_empty(), "{:?}", ended.kept);
    assert!(!f.root.join("fresh").exists(), "everything the setup created was removed");
    let message = ended.message.unwrap_or_default();
    assert!(
        message.contains("The new package's files were removed; nothing else in the project changed."),
        "{message}"
    );

    let untrusted = Fixture::new(false);
    let plan = untrusted.setup.plan(&untrusted.root, &request).unwrap();
    let code = untrusted.setup.start(&untrusted.root, &plan).map(|s| s.id).map_err(|e| e.code);
    assert_eq!(code, Err("lean.untrusted".to_owned()));
    assert!(!untrusted.root.join("fresh").exists());
}
