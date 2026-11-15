//! R-a1 F2 regression, with the real setup service, execution service, and
//! pip: setup never installs from files in the project.
//!
//! - A plain-text sdist written into the project (ustar headers and NUL
//!   padding are valid UTF-8, so an agent's create_file can write it) and
//!   named as a "package" is refused at planning; its build code never runs.
//!   (Adapted from the independent reviewer's repro, review/a1
//!   `tests/review_a1_setup.rs`.)
//! - Every setup command runs in an empty application-owned working folder,
//!   bound to the project only for trust, and removed afterwards; pip loads
//!   no configuration file, so a `pip.ini` in the project environment cannot
//!   add find-links or indexes.
//!
//! Needs a discovered Python outside the project; no network is needed for
//! the assertions (pip reports its find-links before it contacts an index).
//! Without a Python it prints SKIPPED and passes, unless
//! EXPOSITION_REQUIRE_SETUP_TOOLS is set. Set TMP/TEMP to a plain folder:
//! kernels are not started here, but the execution service refuses
//! environment values that look like tokens.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use exposition_execution::{ExecutionConfig, ExecutionService};
use exposition_research::launcher::ExecutionLauncher;
use exposition_research::setup::{SetupRequest, SetupState, validate_packages};
use exposition_research::types::{RuntimeEnvironment, RuntimeLanguage, SessionOwner};
use exposition_research::{ResearchConfig, ResearchService};
use exposition_workspace::DocumentService;

fn required() -> bool {
    std::env::var_os("EXPOSITION_REQUIRE_SETUP_TOOLS").is_some_and(|v| !v.is_empty())
}

/// An sdist whose in-tree build backend writes `marker` when pip asks it for
/// build requirements (the reviewer's probe).
fn hostile_sdist(marker: &Path) -> Vec<u8> {
    fn header(name: &str, size: usize) -> Vec<u8> {
        let mut h = vec![0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..107].copy_from_slice(b"0000644");
        h[108..115].copy_from_slice(b"0000000");
        h[116..123].copy_from_slice(b"0000000");
        let size = format!("{size:011o}");
        h[124..135].copy_from_slice(size.as_bytes());
        h[136..147].copy_from_slice(b"00000000000");
        h[148..156].copy_from_slice(b"        ");
        h[156] = b'0';
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        let sum: u32 = h.iter().map(|b| *b as u32).sum();
        let sum = format!("{sum:06o}\0 ");
        h[148..156].copy_from_slice(sum.as_bytes());
        h
    }
    let marker = marker.display().to_string().replace('\\', "/");
    let backend = format!(
        "def _mark():\n    open({marker:?}, 'w').write('sdist build code ran')\n\
         def get_requires_for_build_wheel(config_settings=None):\n    _mark(); return []\n\
         def prepare_metadata_for_build_wheel(d, config_settings=None):\n    _mark(); raise RuntimeError('probe')\n\
         def build_wheel(d, config_settings=None, metadata_directory=None):\n    _mark(); raise RuntimeError('probe')\n"
    );
    let pyproject = "[build-system]\nrequires = []\nbuild-backend = \"backend\"\nbackend-path = [\".\"]\n";
    let mut out = Vec::new();
    for (name, body) in [("evilsdist-1.0/backend.py", backend.as_str()), ("evilsdist-1.0/pyproject.toml", pyproject)] {
        out.extend(header(name, body.len()));
        out.extend(body.as_bytes());
        out.resize(out.len().div_ceil(512) * 512, 0);
    }
    out.resize(out.len() + 1024, 0);
    out
}

struct Fixture {
    dir: tempfile::TempDir,
    root: PathBuf,
    data: PathBuf,
    execution: ExecutionService,
    service: ResearchService,
}

fn fixture() -> Fixture {
    let dir = tempfile::Builder::new().prefix("exp-setup-files-").tempdir().unwrap();
    let outer = std::fs::canonicalize(dir.path()).unwrap();
    let root = outer.join("project");
    std::fs::create_dir_all(&root).unwrap();
    let data = outer.join("appdata");
    let execution = ExecutionService::with_config(ExecutionConfig::new(&data)).unwrap();
    let config = ResearchConfig::new(
        data.clone(),
        Arc::new(ExecutionLauncher::new(execution.clone())),
        Arc::new(DocumentService::new()),
    );
    let service = ResearchService::new(config);
    let root = std::fs::canonicalize(&root).unwrap();
    execution.grant_trust(&root.display().to_string()).unwrap();
    assert!(service.set_workspace(&root, false).unwrap().trusted);
    Fixture { dir, root, data, execution, service }
}

fn inside(path: &str, root: &Path) -> bool {
    std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path)).starts_with(root)
}

async fn base_python(f: &Fixture) -> Option<RuntimeEnvironment> {
    let report = f.service.environments(true).await.unwrap();
    let base = report.runtimes.into_iter().find(|r| {
        r.language == RuntimeLanguage::Python
            && r.project_folder.is_none()
            && r.executable.as_deref().is_some_and(|e| !inside(e, &f.root))
    });
    if base.is_none() {
        assert!(!required(), "EXPOSITION_REQUIRE_SETUP_TOOLS is set but no Python was discovered");
        eprintln!("SKIPPED: no Python interpreter was discovered");
    }
    base
}

/// The reviewer's end-to-end probe: a project archive named as a package is
/// refused at planning, nothing runs, and its build code never executes.
#[tokio::test(flavor = "multi_thread")]
async fn a_project_archive_named_as_a_package_never_runs() {
    let f = fixture();
    let Some(base) = base_python(&f).await else { return };
    let marker = f.dir.path().join("sdist-build-code-ran.txt");
    let sdist = hostile_sdist(&marker);
    assert!(std::str::from_utf8(&sdist).is_ok(), "the archive is valid UTF-8 text");
    std::fs::write(f.root.join("evilsdist-1.0.tar"), &sdist).unwrap();
    for spec in ["evilsdist-1.0.tar", "EvilSdist-1.0.TAR", "evilsdist-1.0.tar[x]", "evilsdist==1.0.tar"] {
        let request =
            SetupRequest::CreateEnvironment { base_environment: base.id.clone(), packages: vec![spec.into()] };
        let refused = f.service.setup_plan(&f.root, &request).await.unwrap_err();
        assert_eq!(refused.code(), "setup.invalidPackage", "{spec}: {refused}");
        assert!(refused.to_string().contains("only package names from PyPI"), "{refused}");
        assert!(validate_packages(&[spec.into()]).is_err());
    }
    let jobs = f.execution.list();
    assert!(jobs.iter().all(|j| !j.info.label.starts_with("environment setup")), "nothing ran: {jobs:?}");
    assert!(!f.root.join(".venv").exists());
    assert!(!marker.exists(), "the archive's build code never ran");
}

/// Setup commands run in an empty application-owned working folder (not the
/// project), bound to the project for trust and removed afterwards; pip
/// reads no configuration file, so a project environment's `pip.ini` cannot
/// point it at local find-links (or another index).
#[tokio::test(flavor = "multi_thread")]
async fn setup_runs_outside_the_project_and_ignores_pip_configuration() {
    let f = fixture();
    let Some(base) = base_python(&f).await else { return };
    let exe = base.executable.clone().unwrap();
    // An existing project environment (made directly, as a person would).
    let venv = f.root.join(".venv");
    let made = std::process::Command::new(&exe).args(["-I", "-m", "venv"]).arg(&venv).status().unwrap();
    assert!(made.success(), "python -m venv");
    // Project content pip must never use: a find-links folder named in the
    // environment's own configuration file, and archives in the project root.
    let links = f.root.join("wheels");
    std::fs::create_dir_all(&links).unwrap();
    let config = if cfg!(windows) { venv.join("pip.ini") } else { venv.join("pip.conf") };
    std::fs::write(&config, format!("[global]\nfind-links = {}\n", links.display())).unwrap();
    let marker = f.dir.path().join("sdist-build-code-ran.txt");
    std::fs::write(f.root.join("evilsdist-1.0.tar"), hostile_sdist(&marker)).unwrap();

    let report = f.service.environments(true).await.unwrap();
    let env = report
        .runtimes
        .iter()
        .find(|r| r.project_folder.as_deref() == Some(".venv"))
        .unwrap_or_else(|| panic!("the project environment is discovered: {:?}", report.runtimes))
        .clone();
    let request = SetupRequest::InstallPackages {
        environment: env.id.clone(),
        packages: vec!["exposition-a1-f2-no-such-package".into()],
    };
    let plan = f.service.setup_plan(&f.root, &request).await.unwrap();
    let status = tokio::time::timeout(Duration::from_secs(600), f.service.setup_run(&plan, SessionOwner::User))
        .await
        .expect("setup finished in time")
        .unwrap();
    eprintln!("setup: {:?} {:?}\n{}", status.state, status.message, status.log.join("\n"));
    assert_ne!(status.state, SetupState::Succeeded, "the package does not exist");
    assert!(!status.removed, "an existing environment is never removed");
    assert!(
        !status.log.iter().any(|l| l.contains("Looking in links") || l.contains(&links.display().to_string())),
        "pip used the project environment's configuration file: {:?}",
        status.log
    );
    assert!(!marker.exists(), "no project archive was built");

    let jobs: Vec<_> =
        f.execution.list().into_iter().filter(|j| j.info.label.starts_with("environment setup")).collect();
    assert_eq!(jobs.len(), 1, "{jobs:?}");
    let workdir = PathBuf::from(&jobs[0].working_directory);
    eprintln!("working folder: {}", workdir.display());
    let plain = |p: &Path| p.display().to_string().trim_start_matches(r"\\?\").to_lowercase();
    assert!(
        plain(&workdir).starts_with(&plain(&f.data)),
        "the working folder {} is application-owned (under {})",
        workdir.display(),
        f.data.display()
    );
    assert!(!plain(&workdir).starts_with(&plain(&f.root)), "never the project folder");
    assert!(workdir.display().to_string().contains("setup-work"), "{}", workdir.display());
    assert!(!workdir.exists(), "the working folder is removed afterwards");
}
