//! R-a1 F7 regression, with the real execution service and a real virtual
//! environment: planning a setup never starts the project's interpreter.
//!
//! An agent's `install_packages` call is planned before its approval is
//! decided. Planning used to run environment discovery, which probes the
//! project `.venv` interpreter natively (its `site` runs every `*.pth` line
//! that starts with `import`). Planning now uses the cached report or
//! discovers interpreters outside the project only, and recognizes the
//! project environment by its files. (Adapted from the independent
//! reviewer's repro, review/a1
//! `exposition-agents/tests/review_a1_venv.rs::review_a1_f7_…`.)
//!
//! Needs a host Python that can create a virtual environment (`py -3.12`,
//! `py -3`, or `python`); no network. Without one it prints SKIPPED and
//! passes, unless EXPOSITION_REQUIRE_SETUP_TOOLS is set.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use exposition_execution::{ExecutionConfig, ExecutionService};
use exposition_research::launcher::ExecutionLauncher;
use exposition_research::setup::SetupRequest;
use exposition_research::{ResearchConfig, ResearchService};
use exposition_workspace::DocumentService;

/// Create `<root>/.venv` with a host Python; false when none works.
fn make_venv(root: &Path) -> bool {
    let target = root.join(".venv");
    for (program, args) in [("py", vec!["-3.12"]), ("py", vec!["-3"]), ("python", vec![])] {
        let ok = std::process::Command::new(program)
            .args(&args)
            .args(["-I", "-m", "venv", "--without-pip"])
            .arg(&target)
            .output()
            .is_ok_and(|o| o.status.success());
        if ok && target.join("pyvenv.cfg").is_file() {
            return true;
        }
    }
    false
}

fn site_packages(root: &Path) -> Option<PathBuf> {
    let windows = root.join(".venv").join("Lib").join("site-packages");
    if windows.is_dir() {
        return Some(windows);
    }
    std::fs::read_dir(root.join(".venv").join("lib"))
        .ok()?
        .flatten()
        .map(|e| e.path().join("site-packages"))
        .find(|p| p.is_dir())
}

#[tokio::test(flavor = "multi_thread")]
async fn planning_a_setup_never_runs_the_project_interpreter() {
    let dir = tempfile::Builder::new().prefix("exp-setup-plan-").tempdir().unwrap();
    let outer = std::fs::canonicalize(dir.path()).unwrap();
    let root = outer.join("project");
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    if !make_venv(&root) {
        assert!(
            std::env::var_os("EXPOSITION_REQUIRE_SETUP_TOOLS").is_none_or(|v| v.is_empty()),
            "EXPOSITION_REQUIRE_SETUP_TOOLS is set but no host Python could create a virtual environment"
        );
        eprintln!("SKIPPED: no host Python could create a virtual environment");
        return;
    }
    // The environment's own startup code stands for anything the project's
    // interpreter runs when it starts.
    let marker = outer.join("interpreter-ran.txt");
    let site = site_packages(&root).expect("the venv has site-packages");
    std::fs::write(
        site.join("startup_probe.pth"),
        format!("import os; open({:?}, \"a\").write(\"ran\\n\")\n", marker.display().to_string().replace('\\', "/")),
    )
    .unwrap();

    let data = outer.join("appdata");
    let execution = ExecutionService::with_config(ExecutionConfig::new(&data)).unwrap();
    let config = ResearchConfig::new(
        data,
        Arc::new(ExecutionLauncher::new(execution.clone())),
        Arc::new(DocumentService::new()),
    );
    let service = ResearchService::new(config);
    execution.grant_trust(&root.display().to_string()).unwrap();
    assert!(service.set_workspace(&root, false).unwrap().trusted);
    let canonical = service.workspace_root().unwrap();
    let python = if cfg!(windows) {
        canonical.join(".venv").join("Scripts").join("python.exe")
    } else {
        canonical.join(".venv").join("bin").join("python")
    };
    let venv_id = format!("python:{}", python.display());

    // Nothing was discovered yet (as for an agent's first install_packages call).
    let install = SetupRequest::InstallPackages { environment: venv_id.clone(), packages: vec!["tomli".into()] };
    let plan = service.setup_plan(&root, &install).await.expect("the project environment is recognized by its files");
    assert!(!plan.creates_environment);
    assert_eq!(plan.target_name, ".venv");
    let create = SetupRequest::CreateEnvironment { base_environment: "python:none".into(), packages: vec![] };
    let refused = service.setup_plan(&root, &create).await.unwrap_err();
    eprintln!("create while .venv exists: {} ({})", refused.code(), refused);
    assert!(!marker.exists(), "DEFECT: planning ran the project interpreter");

    // Control: the person refreshing Research does probe the environment,
    // which runs its startup code (so the marker above is meaningful).
    service.environments(true).await.unwrap();
    assert!(marker.exists(), "the discovery probe runs the project interpreter");
    // With a cached report, planning still runs nothing new.
    std::fs::remove_file(&marker).unwrap();
    service.setup_plan(&root, &install).await.unwrap();
    assert!(!marker.exists(), "planning from the cached report runs nothing");
}
