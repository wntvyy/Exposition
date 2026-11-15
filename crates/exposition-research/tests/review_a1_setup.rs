//! R-a1 (independent review of A1): guided environment setup (lane E, D5).
//!
//! Tests marked `#[ignore = "R-a1 F<n>: ..."]` demonstrate a CURRENT defect:
//! they assert the expected behavior and fail on the product. Run them with
//! `cargo test -p exposition-research --test review_a1_setup -- --include-ignored --nocapture`.
//!
//! The real-pip test needs a discovered Python outside the project; it does
//! not need network access (pip prepares a local archive before it contacts
//! the index). Without a Python it prints SKIPPED and passes.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use exposition_execution::{ExecutionConfig, ExecutionService};
use exposition_research::launcher::ExecutionLauncher;
use exposition_research::setup::{SetupRequest, install_list, validate_packages};
use exposition_research::types::{RuntimeEnvironment, RuntimeLanguage, SessionOwner};
use exposition_research::{ResearchConfig, ResearchService};
use exposition_workspace::DocumentService;

fn refused(spec: &str) -> bool {
    validate_packages(&[spec.to_owned()]).is_err()
}

/// Held: hostile specifications are refused before anything is planned.
#[test]
fn review_a1_held_hostile_package_specs_are_refused() {
    let hostile = [
        "https://example.invalid/pkg.whl",
        "git+https://github.com/x/y",
        "--index-url",
        "--index-url=https://evil.invalid/simple",
        "-r",
        "-rrequirements.txt",
        "-e .",
        "pkg @ file:///C:/x",
        "pkg@https://x",
        "pkg; python_version < '3'",
        "pkg;sys_platform=='win32'",
        "./local",
        "..\\local",
        "C:\\temp\\x.whl",
        "/tmp/x.whl",
        "~/x",
        "n\u{0578}mpy",  // Armenian letter that looks like u
        "numpy\u{200b}", // zero-width space
        "nump\u{0443}",  // Cyrillic u
        "-numpy",
        "numpy --hash=sha256:00",
        "numpy==1.0 --pre",
        "numpy===1.0",
        "numpy==",
        "numpy>=1,",
        "numpy\n--index-url https://evil.invalid",
        "numpy\t-r x",
        "numpy[a,b",
        "numpy==1.*.2",
        "numpy>=1.*",
        "",
    ];
    for spec in hostile {
        if spec.trim().is_empty() {
            continue;
        }
        assert!(refused(spec), "accepted a hostile spec: {spec:?} -> {:?}", validate_packages(&[spec.to_owned()]));
    }
    // 51 packages are refused; 50 are not (before the kernel packages).
    let many: Vec<String> = (0..51).map(|i| format!("pkg{i}")).collect();
    assert!(validate_packages(&many).is_err());
    assert!(validate_packages(&many[..50]).is_ok());
    // The kernel packages count toward the limit.
    assert!(install_list(&many[..50]).is_err(), "50 + ipykernel + jupyter_client exceed 50");
    // Ordinary specs survive exactly.
    assert_eq!(
        validate_packages(&["numpy".into(), "pandas >= 2.2, <3".into(), "torch==2.4.0+cpu".into()]).unwrap(),
        vec!["numpy", "pandas>=2.2,<3", "torch==2.4.0+cpu"]
    );
}

/// R-a1 F2 (part 1): names that pip resolves as LOCAL ARCHIVE FILES (relative
/// to the setup job's working directory, the project folder) pass the
/// validator: they are valid PEP 508 names. D5 says "no URLs, paths".
#[test]
#[ignore = "R-a1 F2: archive file names (.tar, .whl, .zip, .tar.gz) pass package validation and pip installs them from the project folder"]
fn review_a1_f2_archive_file_names_are_refused() {
    for spec in ["evilsdist-1.0.tar", "evilsdist-1.0.tar.gz", "evil.zip", "evilpkg-1.0-py3-none-any.whl", "x.tgz"] {
        assert!(
            refused(spec),
            "accepted {spec:?}: pip treats it as a local file -> {:?}",
            validate_packages(&[spec.into()])
        );
    }
}

/// An sdist that is plain text (ustar headers + NUL padding are valid UTF-8,
/// so an agent's create_file could write it) whose in-tree build backend
/// writes `marker` when pip asks it for build requirements.
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
         def prepare_metadata_for_build_wheel(d, config_settings=None):\n    _mark(); raise RuntimeError('review probe')\n\
         def build_wheel(d, config_settings=None, metadata_directory=None):\n    _mark(); raise RuntimeError('review probe')\n"
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

fn fixture() -> (tempfile::TempDir, PathBuf, ExecutionService, ResearchService) {
    let dir = tempfile::Builder::new().prefix("exp-review-a1-setup-").tempdir().unwrap();
    let outer = std::fs::canonicalize(dir.path()).unwrap();
    let root = outer.join("project");
    std::fs::create_dir_all(&root).unwrap();
    let data = outer.join("appdata");
    let execution = ExecutionService::with_config(ExecutionConfig::new(&data)).unwrap();
    let config = ResearchConfig::new(
        data,
        Arc::new(ExecutionLauncher::new(execution.clone())),
        Arc::new(DocumentService::new()),
    );
    let service = ResearchService::new(config);
    let root = std::fs::canonicalize(&root).unwrap();
    execution.grant_trust(&root.display().to_string()).unwrap();
    assert!(service.set_workspace(&root, false).unwrap().trusted);
    (dir, root, execution, service)
}

async fn base_python(service: &ResearchService, root: &Path) -> Option<RuntimeEnvironment> {
    let report = service.environments(true).await.unwrap();
    report.runtimes.into_iter().find(|r| {
        r.language == RuntimeLanguage::Python
            && r.project_folder.is_none()
            && r.executable.as_deref().is_some_and(|e| !Path::new(e).starts_with(root))
    })
}

/// R-a1 F2 (part 2), end to end with the real setup service and real pip: a
/// plain-text sdist in the project folder named as a "package" makes the
/// approved setup run that archive's build-backend code natively — although
/// the plan says binary wheels from https://pypi.org/simple only
/// (`--only-binary=:all:` does not apply to a local archive's build).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "R-a1 F2: setup runs build code from a project archive named as a package"]
async fn review_a1_f2_setup_runs_build_code_from_a_project_archive() {
    let (dir, root, execution, service) = fixture();
    let Some(base) = base_python(&service, &root).await else {
        eprintln!("SKIPPED: no Python interpreter was discovered");
        return;
    };
    let marker = dir.path().join("sdist-build-code-ran.txt");
    let sdist = hostile_sdist(&marker);
    assert!(std::str::from_utf8(&sdist).is_ok(), "the archive is valid UTF-8 text");
    std::fs::write(root.join("evilsdist-1.0.tar"), &sdist).unwrap();
    let request = SetupRequest::CreateEnvironment {
        base_environment: base.id.clone(),
        packages: vec!["evilsdist-1.0.tar".into()],
    };
    let plan = match service.setup_plan(&root, &request).await {
        Ok(plan) => plan,
        Err(e) => {
            eprintln!("refused at planning (expected): {e}");
            return;
        }
    };
    eprintln!("plan: {}\npackages: {:?}\nindex: {}", plan.summary, plan.packages, plan.index_url);
    for step in &plan.steps {
        eprintln!("  {} {}", step.program, step.args.join(" "));
    }
    let status = tokio::time::timeout(Duration::from_secs(900), service.setup_run(&plan, SessionOwner::User))
        .await
        .expect("setup finished in time")
        .unwrap();
    eprintln!("setup: {:?} {:?}", status.state, status.message);
    let ran = marker.exists();
    eprintln!("build code from the project archive ran: {ran}");
    drop(execution);
    assert!(!ran, "DEFECT: setup ran build code from a project archive (plan said PyPI wheels only)");
}
