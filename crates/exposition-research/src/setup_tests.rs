//! Setup unit tests: package validation, exact plans, and the runner's
//! cleanup rules against a scripted launcher (no real Python). Real-tool
//! evidence lives in `tests/environment_setup.rs`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::{Arc, Mutex};

use exposition_core::job::{ExecutionPolicy, JobSpec};
use exposition_workspace::DocumentService;

use super::*;
use crate::launcher::{LaunchError, LaunchFuture, ProbeOutput, ProjectJobProgress};
use crate::service::ResearchConfig;
use crate::types::{RuntimeEnvironment, SetupHint};

// ---- package validation ---------------------------------------------------

fn ok(spec: &str) -> String {
    parse_package(spec).unwrap_or_else(|e| panic!("{spec:?} refused: {e}")).to_arg()
}

#[test]
fn ordinary_requirements_are_accepted_exactly() {
    assert_eq!(ok("numpy"), "numpy");
    assert_eq!(ok("pandas>=2.2"), "pandas>=2.2");
    assert_eq!(ok("  scipy >= 1.11 , < 2  "), "scipy>=1.11,<2");
    assert_eq!(ok("Jupyter_Client"), "Jupyter_Client");
    assert_eq!(ok("matplotlib[qt, io]==3.9.*"), "matplotlib[qt,io]==3.9.*");
    assert_eq!(ok("torch==2.3.1+cpu"), "torch==2.3.1+cpu");
    assert_eq!(ok("legacy!=1.0,~=1.4"), "legacy!=1.0,~=1.4");
    assert_eq!(ok("pkg==1!2.0"), "pkg==1!2.0");
    assert_eq!(ok("a"), "a");
    assert_eq!(normalize_name("Jupyter_Client"), "jupyter-client");
    assert_eq!(normalize_name("zope.interface"), "zope-interface");
}

#[test]
fn hostile_or_unsupported_specifications_are_refused() {
    let refused = [
        "",
        "   ",
        "https://example.com/evil-1.0-py3-none-any.whl",
        "git+https://github.com/x/y",
        "file:///C:/x.whl",
        "pkg @ https://example.com/pkg.whl",
        "pkg@1.0",
        "pkg; python_version < '3.8'",
        "-r requirements.txt",
        "--index-url=https://evil.example/simple",
        "-e .",
        "--pre",
        "./local",
        "../x",
        "C:\\wheels\\x.whl",
        "/tmp/x.whl",
        "~/x",
        "pkg===1.0",
        "num py",
        "numpy\n--index-url=https://evil.example/simple",
        "numpy\t--extra-index-url",
        "nümpy",
        "pkg>=",
        ">=1.0",
        "pkg>=1.0,",
        "pkg[extra",
        "pkg[]",
        "pkg>=abc",
        "pkg>=1.*",
        "pkg==1.0 extra",
        "_private",
        "trailing-",
        "pkg$(calc)",
        "pkg|calc",
        "pkg&calc",
        "pkg`calc`",
        "pkg\"x",
        "pkg'x",
        "pkg%PATH%",
    ];
    for spec in refused {
        assert!(parse_package(spec).is_err(), "{spec:?} must be refused");
        if !spec.trim().is_empty() {
            let error = validate_packages(&[spec.to_owned()]).unwrap_err();
            assert_eq!(error.code(), "setup.invalidPackage", "{spec:?}");
        }
    }
    let long = format!("a{}", "b".repeat(MAX_SPEC_LEN));
    assert!(parse_package(&long).is_err());
}

/// R-a1 F2: text pip would resolve as a local archive or distribution file
/// (relative to its working directory) is never a package name.
#[test]
fn archive_and_distribution_file_names_are_refused() {
    let files = [
        "evilsdist-1.0.tar",
        "evilsdist-1.0.tar.gz",
        "EVILSDIST-1.0.TAR.GZ",
        "evil.zip",
        "evilpkg-1.0-py3-none-any.whl",
        "x.tgz",
        "x.tar.bz2",
        "x.tbz",
        "x.tbz2",
        "x.tar.xz",
        "x.txz",
        "x.tlz",
        "x.tar.lz",
        "x.tar.lzma",
        "x-1.0-py3.8.egg",
        "setup.exe",
        "installer.msi",
        "x.tar.foo",
        "x.whl.bak",
        "pkg==1.whl",
        "pkg>=1.0.zip",
        "evil.tar[extra]",
        "evil.tar[extra]==1.0",
    ];
    for spec in files {
        let reason = parse_package(spec).expect_err(spec);
        assert!(reason.contains("only package names from PyPI"), "{spec}: {reason}");
        let error = validate_packages(&[spec.to_owned()]).unwrap_err();
        assert_eq!(error.code(), "setup.invalidPackage", "{spec}");
        assert!(error.to_string().contains("only package names from PyPI"), "{error}");
    }
    // Real names that merely contain such letters stay accepted.
    for name in
        ["zope.interface", "backports.tarfile", "backports.lzma", "py2exe", "zipp", "tarsafe", "exepy", "msilib-x"]
    {
        assert!(parse_package(name).is_ok(), "{name} must stay accepted");
    }
    assert!(!looks_like_archive("pandas>=2.2,<3"));
    assert!(!looks_like_archive("torch==2.3.1+cpu"));
}

#[test]
fn package_lists_are_bounded_deduplicated_and_led_by_the_kernel_packages() {
    let many: Vec<String> = (0..=MAX_PACKAGES).map(|i| format!("pkg{i}")).collect();
    assert_eq!(validate_packages(&many).unwrap_err().code(), "setup.tooManyPackages");
    let exactly: Vec<String> = (0..MAX_PACKAGES).map(|i| format!("pkg{i}")).collect();
    assert_eq!(validate_packages(&exactly).unwrap().len(), MAX_PACKAGES);
    // The kernel packages make the install list longer than the request.
    assert_eq!(install_list(&exactly).unwrap_err().code(), "setup.tooManyPackages");

    let deduped = validate_packages(&["numpy".into(), "numpy".into(), "".into()]).unwrap();
    assert_eq!(deduped, vec!["numpy"]);
    let conflict = validate_packages(&["numpy".into(), "NumPy>=2".into()]).unwrap_err();
    assert_eq!(conflict.code(), "setup.invalidPackage");

    assert_eq!(install_list(&[]).unwrap(), vec!["ipykernel", "jupyter_client"]);
    assert_eq!(install_list(&["numpy".into()]).unwrap(), vec!["ipykernel", "jupyter_client", "numpy"]);
    // A request that names a kernel package itself keeps its version.
    assert_eq!(
        install_list(&["jupyter-client==8.6.3".into(), "numpy".into()]).unwrap(),
        vec!["ipykernel", "jupyter-client==8.6.3", "numpy"]
    );
}

// ---- plans ------------------------------------------------------------------

fn python_env(id: &str, executable: &str, available: bool) -> RuntimeEnvironment {
    RuntimeEnvironment {
        id: id.into(),
        language: RuntimeLanguage::Python,
        display_name: "Python 3.12.10 (CPython)".into(),
        executable: Some(executable.into()),
        version: Some("3.12.10".into()),
        kernel: None,
        host_interpreter: Some(executable.into()),
        packages: Vec::new(),
        kernel_environment: BTreeMap::new(),
        availability: if available {
            Availability::Available
        } else {
            Availability::Unavailable { reason: "lacks ipykernel".into(), guidance: String::new() }
        },
        setup: (!available).then(SetupHint::default),
        project_folder: None,
    }
}

struct PlanFixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    base: PathBuf,
    data: PathBuf,
}

impl PlanFixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let outer = std::fs::canonicalize(dir.path()).unwrap();
        let root = outer.join("project");
        std::fs::create_dir_all(&root).unwrap();
        let base = outer.join("python312").join("python.exe");
        std::fs::create_dir_all(base.parent().unwrap()).unwrap();
        std::fs::write(&base, b"").unwrap();
        let data = outer.join("appdata");
        Self { _dir: dir, root: std::fs::canonicalize(&root).unwrap(), base, data }
    }

    fn base_id(&self) -> String {
        format!("python:{}", self.base.display())
    }

    fn report(&self, extra: Vec<RuntimeEnvironment>) -> EnvironmentReport {
        let mut runtimes = vec![python_env(&self.base_id(), &self.base.display().to_string(), false)];
        runtimes.extend(extra);
        EnvironmentReport { runtimes, scanned_at_ms: 0 }
    }

    fn plan(
        &self,
        report: &EnvironmentReport,
        trusted: bool,
        request: &SetupRequest,
    ) -> Result<SetupPlan, ResearchError> {
        let records = RecordStore::new(&self.data);
        build_plan(&PlanContext { root: &self.root, trusted, report, records: &records }, request)
    }

    fn create(&self, packages: &[&str]) -> SetupRequest {
        SetupRequest::CreateEnvironment {
            base_environment: self.base_id(),
            packages: packages.iter().map(|p| (*p).to_owned()).collect(),
        }
    }

    /// A project venv as `python -m venv` leaves it (enough for discovery and setup).
    fn make_venv(&self, folder: &str) -> RuntimeEnvironment {
        let dir = self.root.join(folder);
        let python = venv_python(&dir);
        std::fs::create_dir_all(python.parent().unwrap()).unwrap();
        std::fs::write(&python, b"").unwrap();
        std::fs::write(dir.join("pyvenv.cfg"), b"home = x\n").unwrap();
        python_env(&environment_id_for(&dir), &python.display().to_string(), true)
    }
}

const PIP_PREFIX: [&str; 11] = [
    "-I",
    "-m",
    "pip",
    "--isolated",
    "install",
    "--disable-pip-version-check",
    "--no-input",
    "--require-virtualenv",
    "--only-binary=:all:",
    "--index-url",
    "https://pypi.org/simple",
];

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn a_create_plan_lists_the_exact_commands() {
    let f = PlanFixture::new();
    let plan = f.plan(&f.report(vec![]), true, &f.create(&["numpy", "pandas>=2.2"])).unwrap();
    let target = f.root.join(".venv");
    assert!(plan.creates_environment && !plan.removes_unfinished && plan.network);
    assert_eq!(plan.target, display_path(&target));
    assert_eq!(plan.target_name, ".venv");
    assert_eq!(plan.root, display_path(&f.root));
    assert_eq!(plan.packages, strings(&["ipykernel", "jupyter_client", "numpy", "pandas>=2.2"]));
    assert_eq!(plan.steps.len(), 2);
    assert_eq!(plan.steps[0].program, display_path(&f.base));
    assert_eq!(plan.steps[0].args, vec!["-I".to_owned(), "-m".into(), "venv".into(), display_path(&target)]);
    assert!(!plan.steps[0].network);
    assert_eq!(plan.steps[1].program, display_path(&venv_python(&target)));
    let mut pip = strings(&PIP_PREFIX);
    pip.extend(strings(&["ipykernel", "jupyter_client", "numpy", "pandas>=2.2"]));
    assert_eq!(plan.steps[1].args, pip);
    assert!(plan.steps[1].network);
    assert!(plan.summary.contains("Creates a Python environment in .venv"), "{}", plan.summary);
    assert!(plan.summary.contains("your account's privileges"), "{}", plan.summary);
    assert_eq!(plan.plan_hash.len(), 64);
    // Deterministic; a different package list is a different plan.
    let again = f.plan(&f.report(vec![]), true, &f.create(&["numpy", "pandas>=2.2"])).unwrap();
    assert_eq!(again, plan);
    let other = f.plan(&f.report(vec![]), true, &f.create(&["numpy"])).unwrap();
    assert_ne!(other.plan_hash, plan.plan_hash);
}

#[test]
fn an_install_plan_targets_only_the_project_environment() {
    let f = PlanFixture::new();
    let venv = f.make_venv("venv");
    let report = f.report(vec![venv.clone()]);
    let request = SetupRequest::InstallPackages { environment: venv.id.clone(), packages: vec!["scipy".into()] };
    let plan = f.plan(&report, true, &request).unwrap();
    assert!(!plan.creates_environment);
    assert_eq!(plan.target_name, "venv");
    assert_eq!(plan.steps.len(), 1);
    assert_eq!(plan.steps[0].program, display_path(&venv_python(&f.root.join("venv"))));
    let mut pip = strings(&PIP_PREFIX);
    pip.extend(strings(&["ipykernel", "jupyter_client", "scipy"]));
    assert_eq!(plan.steps[0].args, pip);
    assert!(plan.summary.contains("nothing is removed"), "{}", plan.summary);

    // A Python installed for the whole computer is never a target.
    let system = SetupRequest::InstallPackages { environment: f.base_id(), packages: vec![] };
    assert_eq!(f.plan(&report, true, &system).unwrap_err().code(), "setup.unsupported");
    // Unknown environments are refused.
    let unknown =
        SetupRequest::InstallPackages { environment: "python:C:\\nowhere\\python.exe".into(), packages: vec![] };
    assert_eq!(f.plan(&report, true, &unknown).unwrap_err().code(), "research.unknownEnvironment");
}

#[test]
fn plans_need_a_trusted_folder_and_a_base_outside_the_project() {
    let f = PlanFixture::new();
    assert_eq!(f.plan(&f.report(vec![]), false, &f.create(&[])).unwrap_err().code(), "research.notTrusted");
    let inside = f.root.join("tools").join("python.exe");
    let report = EnvironmentReport {
        runtimes: vec![python_env("python:inside", &inside.display().to_string(), true)],
        scanned_at_ms: 0,
    };
    let request = SetupRequest::CreateEnvironment { base_environment: "python:inside".into(), packages: vec![] };
    assert_eq!(f.plan(&report, true, &request).unwrap_err().code(), "setup.unsupported");
    let bad = f.create(&["https://evil.example/x.whl"]);
    assert_eq!(f.plan(&f.report(vec![]), true, &bad).unwrap_err().code(), "setup.invalidPackage");
}

#[test]
fn an_existing_folder_is_never_replaced_by_a_create_plan() {
    let f = PlanFixture::new();
    let venv = f.make_venv(".venv");
    let error = f.plan(&f.report(vec![venv]), true, &f.create(&[])).unwrap_err();
    assert_eq!(error.code(), "setup.environmentExists");

    let g = PlanFixture::new();
    std::fs::create_dir_all(g.root.join(".venv").join("notes")).unwrap();
    assert_eq!(g.plan(&g.report(vec![]), true, &g.create(&[])).unwrap_err().code(), "setup.targetOccupied");
}

#[test]
fn an_unfinished_environment_is_replaced_only_with_an_app_local_record() {
    let f = PlanFixture::new();
    let target = f.root.join(".venv");
    std::fs::create_dir_all(&target).unwrap();
    write_marker(&target, "token-1", MarkerState::InProgress, 1).unwrap();
    assert!(setup_unfinished(&target));
    // A marker alone (e.g. shipped in a repository) proves nothing.
    assert_eq!(f.plan(&f.report(vec![]), true, &f.create(&[])).unwrap_err().code(), "setup.targetOccupied");
    // With the record a setup wrote, the plan says it removes it first.
    RecordStore::new(&f.data).add(&display_path(&target), "token-1").unwrap();
    let plan = f.plan(&f.report(vec![]), true, &f.create(&[])).unwrap();
    assert!(plan.removes_unfinished && plan.creates_environment);
    assert!(plan.summary.starts_with("Removes the unfinished environment"), "{}", plan.summary);
    // A record for another token does not match.
    RecordStore::new(&f.data).add(&display_path(&target), "token-2").unwrap();
    assert_eq!(f.plan(&f.report(vec![]), true, &f.create(&[])).unwrap_err().code(), "setup.targetOccupied");
}

#[cfg(windows)]
#[test]
fn a_linked_environment_folder_is_refused() {
    let f = PlanFixture::new();
    let elsewhere = f.root.parent().unwrap().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    if std::os::windows::fs::symlink_dir(&elsewhere, f.root.join(".venv")).is_err() {
        eprintln!("SKIPPED: creating a directory symlink needs Developer Mode or elevation");
        return;
    }
    assert_eq!(f.plan(&f.report(vec![]), true, &f.create(&[])).unwrap_err().code(), "setup.targetOccupied");
    assert!(remove_owned(&f.root.join(".venv"), "any").is_err());
    assert!(elsewhere.exists());
}

#[test]
fn removal_requires_the_matching_marker() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join(".venv");
    std::fs::create_dir_all(target.join("Lib")).unwrap();
    std::fs::write(target.join("Lib").join("x.py"), b"").unwrap();
    assert!(remove_owned(&target, "t").is_err(), "no marker: kept");
    write_marker(&target, "other", MarkerState::InProgress, 1).unwrap();
    assert!(remove_owned(&target, "t").is_err(), "foreign marker: kept");
    assert!(target.join("Lib").join("x.py").exists());
    write_marker(&target, "t", MarkerState::InProgress, 1).unwrap();
    remove_owned(&target, "t").unwrap();
    assert!(!target.exists());
}

#[test]
fn log_tails_keep_pip_errors() {
    let mut tail = LineTail::default();
    tail.push("Collecting nope\r\n  Downloading\nERROR: Could not find a version that satisfies the requirement no-such-pkg\n");
    tail.push("ERROR: No matching distribution found for no-such-pkg");
    assert!(tail.error_summary().contains("No matching distribution found for no-such-pkg"));
    assert!(tail.error_summary().starts_with("ERROR: Could not find"));
    assert_eq!(tail.lines().len(), 4);
}

// ---- runner -------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Behavior {
    Succeed,
    Fail,
    /// Runs until canceled.
    Block,
    /// Rewrites the setup marker with another token, then fails.
    ReplaceMarkerThenFail,
}

#[derive(Default)]
struct FakeJob {
    output: Vec<String>,
    delivered: usize,
    end: Option<ProjectJobEnd>,
}

struct ScriptedLauncher {
    trusted: AtomicBool,
    venv: Mutex<Behavior>,
    pip: Mutex<Behavior>,
    jobs: Mutex<HashMap<String, FakeJob>>,
    specs: Mutex<Vec<JobSpec>>,
    project_probes: AtomicUsize,
    next: AtomicUsize,
    /// (working folder, owner) registrations.
    workdirs: Mutex<Vec<(PathBuf, PathBuf)>>,
    unregistered: Mutex<Vec<PathBuf>>,
    /// Steps that started in an empty working folder.
    empty_workdir: AtomicUsize,
}

impl ScriptedLauncher {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            trusted: AtomicBool::new(true),
            venv: Mutex::new(Behavior::Succeed),
            pip: Mutex::new(Behavior::Succeed),
            jobs: Mutex::new(HashMap::new()),
            specs: Mutex::new(Vec::new()),
            project_probes: AtomicUsize::new(0),
            next: AtomicUsize::new(0),
            workdirs: Mutex::new(Vec::new()),
            unregistered: Mutex::new(Vec::new()),
            empty_workdir: AtomicUsize::new(0),
        })
    }

    fn set(&self, venv: Behavior, pip: Behavior) {
        *self.venv.lock().unwrap() = venv;
        *self.pip.lock().unwrap() = pip;
    }
}

const PROBE_JSON: &str = r#"{"version":"3.12.10","implementation":"cpython","executable":"EXE","packages":{"ipykernel":"6.29.5","jupyter_client":"8.6.3"},"importable":{"ipykernel":true,"jupyter_client":true},"kernelspecs":[]}"#;

impl KernelProcessLauncher for ScriptedLauncher {
    fn launch_interactive(&self, _spec: JobSpec) -> LaunchFuture<'_> {
        Box::pin(async { Err(LaunchError::Refused("no kernels in this test".into())) })
    }
    fn run_probe(&self, _spec: &JobSpec) -> Result<ProbeOutput, LaunchError> {
        Err(LaunchError::Refused("no system probes in this test".into()))
    }
    fn run_project_probe(&self, spec: &JobSpec) -> Result<ProbeOutput, LaunchError> {
        self.project_probes.fetch_add(1, Ordering::SeqCst);
        let json = PROBE_JSON.replace("EXE", &spec.program.replace('\\', "\\\\"));
        Ok(ProbeOutput {
            exit: crate::launcher::ExitInfo { code: Some(0) },
            stdout: format!("EXPOSITION-PROBE{json}\n"),
            stderr: String::new(),
        })
    }
    fn workspace_trusted(&self, _root: &Path) -> Option<bool> {
        Some(self.trusted.load(Ordering::SeqCst))
    }
    fn register_project_workdir(&self, dir: &Path, owner: &Path) -> Result<PathBuf, String> {
        self.workdirs.lock().unwrap().push((dir.to_path_buf(), owner.to_path_buf()));
        Ok(dir.to_path_buf())
    }
    fn unregister_project_workdir(&self, dir: &Path) {
        self.unregistered.lock().unwrap().push(dir.to_path_buf());
    }
    fn start_project_job(&self, spec: &JobSpec) -> Result<String, LaunchError> {
        self.specs.lock().unwrap().push(spec.clone());
        if std::fs::read_dir(&spec.working_directory).is_ok_and(|mut entries| entries.next().is_none()) {
            self.empty_workdir.fetch_add(1, Ordering::SeqCst);
        }
        let id = format!("job-{}", self.next.fetch_add(1, Ordering::SeqCst));
        let is_venv = spec.args.iter().any(|a| a == "venv");
        let behavior = if is_venv { *self.venv.lock().unwrap() } else { *self.pip.lock().unwrap() };
        let mut job = FakeJob::default();
        if is_venv {
            let target = PathBuf::from(spec.args.last().unwrap());
            let python = venv_python(&target);
            std::fs::create_dir_all(python.parent().unwrap()).unwrap();
            std::fs::write(&python, b"").unwrap();
            std::fs::write(target.join("pyvenv.cfg"), b"home = x\n").unwrap();
            job.output.push("created\n".into());
        } else {
            job.output.push("Collecting packages\n".into());
        }
        match behavior {
            Behavior::Succeed => job.end = Some(ProjectJobEnd::Exited(0)),
            Behavior::Fail => {
                job.output.push("ERROR: No matching distribution found for no-such-pkg\n".into());
                job.end = Some(ProjectJobEnd::Exited(1));
            }
            Behavior::ReplaceMarkerThenFail => {
                let target = PathBuf::from(&spec.program).parent().unwrap().parent().unwrap().to_path_buf();
                write_marker(&target, "someone-else", MarkerState::InProgress, 1).unwrap();
                job.end = Some(ProjectJobEnd::Exited(2));
            }
            Behavior::Block => {}
        }
        self.jobs.lock().unwrap().insert(id.clone(), job);
        Ok(id)
    }
    fn poll_project_job(&self, job: &str, _cursor: Option<u64>) -> ProjectJobProgress {
        let mut jobs = self.jobs.lock().unwrap();
        let job = jobs.get_mut(job).expect("known job");
        let output = job.output[job.delivered..].to_vec();
        job.delivered = job.output.len();
        ProjectJobProgress { output, cursor: Some(job.delivered as u64), end: job.end.clone() }
    }
    fn cancel_project_job(&self, job: &str) {
        if let Some(job) = self.jobs.lock().unwrap().get_mut(job)
            && job.end.is_none()
        {
            job.end = Some(ProjectJobEnd::Canceled);
        }
    }
}

struct RunFixture {
    plan: PlanFixture,
    launcher: Arc<ScriptedLauncher>,
    service: ResearchService,
}

impl RunFixture {
    fn new() -> Self {
        let plan = PlanFixture::new();
        let launcher = ScriptedLauncher::new();
        let config = ResearchConfig::new(plan.data.clone(), launcher.clone(), Arc::new(DocumentService::new()));
        let service = ResearchService::new(config);
        service.set_workspace(&plan.root, true).unwrap();
        Self { plan, launcher, service }
    }

    fn seed(&self, extra: Vec<RuntimeEnvironment>) {
        // Discovery as the service sees it: the canonical root it registered.
        self.service.seed_environments(self.plan.report(extra));
    }

    async fn run(&self, request: &SetupRequest) -> SetupStatus {
        let root = self.service.workspace_root().unwrap();
        let plan = self.service.setup_plan(&root, request).await.unwrap();
        let started = self.service.setup_start(&plan, SessionOwner::User).await.unwrap();
        assert_eq!(started.state, SetupState::Running);
        tokio::time::timeout(Duration::from_secs(30), self.service.setup_wait(&started.id)).await.unwrap().unwrap()
    }

    fn target(&self) -> PathBuf {
        self.service.workspace_root().unwrap().join(".venv")
    }

    fn records(&self) -> Vec<UnfinishedRecord> {
        RecordStore::new(&self.plan.data).read().unfinished
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_successful_setup_creates_a_ready_environment_and_marks_it_complete() {
    let f = RunFixture::new();
    f.seed(vec![]);
    let status = f.run(&f.plan.create(&["numpy"])).await;
    assert_eq!(status.state, SetupState::Succeeded, "{status:?}");
    assert!(status.ready && !status.removed && status.message.is_none());
    assert_eq!(status.environment_id.as_deref(), Some(environment_id_for(&f.target()).as_str()));
    assert_eq!(read_marker(&f.target()).map(|m| m.state), Some(MarkerState::Complete));
    assert!(f.records().is_empty(), "no unfinished record remains");
    assert_eq!(status.jobs.len(), 2);
    assert!(status.log.iter().any(|l| l.contains("Collecting packages")));
    // Every step ran as a trusted native job in an empty application-owned
    // working folder (never the project folder), with no pip configuration.
    let specs = f.launcher.specs.lock().unwrap().clone();
    assert_eq!(specs.len(), 2);
    let workdirs = f.launcher.workdirs.lock().unwrap().clone();
    assert_eq!(workdirs.len(), 1, "one working folder per setup, bound to the project");
    let (workdir, owner) = &workdirs[0];
    assert_eq!(owner, &f.service.workspace_root().unwrap());
    assert!(workdir.starts_with(&f.plan.data), "{workdir:?} is application-owned");
    assert!(!workdir.starts_with(&f.plan.root), "{workdir:?} is not in the project");
    for spec in &specs {
        assert_eq!(spec.policy, ExecutionPolicy::TrustedNative);
        assert_eq!(spec.working_directory, display_path(workdir));
        assert_ne!(spec.working_directory, status.plan.root);
        assert!(spec.environment.inherit.is_empty());
        assert_eq!(spec.environment.set.get("PIP_CONFIG_FILE").map(String::as_str), Some(NULL_DEVICE));
    }
    assert_eq!(f.launcher.empty_workdir.load(Ordering::SeqCst), 2, "the working folder was empty for every step");
    assert!(!workdir.exists(), "the working folder is removed afterwards");
    assert_eq!(f.launcher.unregistered.lock().unwrap().as_slice(), std::slice::from_ref(workdir));
    assert_eq!(specs[1].args, status.plan.steps[1].args);
    // The service reports it as the project's last setup.
    assert_eq!(f.service.setup_status().map(|s| s.id), Some(status.id));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_install_removes_only_the_environment_the_setup_created() {
    let f = RunFixture::new();
    f.seed(vec![]);
    f.launcher.set(Behavior::Succeed, Behavior::Fail);
    let status = f.run(&f.plan.create(&["no-such-pkg"])).await;
    assert_eq!(status.state, SetupState::Failed);
    assert!(status.removed && !status.ready);
    let message = status.message.unwrap();
    assert!(message.contains("exit code 1"), "{message}");
    assert!(message.contains("No matching distribution found for no-such-pkg"), "{message}");
    assert!(message.contains("was removed"), "{message}");
    assert!(!f.target().exists(), "the new environment is gone");
    assert!(f.records().is_empty());
    // The project folder itself is untouched.
    assert!(f.service.workspace_root().unwrap().is_dir());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_canceled_setup_leaves_nothing_ready_and_removes_what_it_created() {
    let f = RunFixture::new();
    f.seed(vec![]);
    f.launcher.set(Behavior::Succeed, Behavior::Block);
    let root = f.service.workspace_root().unwrap();
    let plan = f.service.setup_plan(&root, &f.plan.create(&[])).await.unwrap();
    let started = f.service.setup_start(&plan, SessionOwner::User).await.unwrap();
    // Wait until the install step runs, then cancel.
    let mut events = f.service.subscribe_setup();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(status) = f.service.setup_status()
                && status.step == Some(1)
                && status.jobs.len() == 2
            {
                break;
            }
            let _ = tokio::time::timeout(Duration::from_millis(100), events.recv()).await;
        }
    })
    .await
    .unwrap();
    // While it runs, discovery would not offer the half-made environment.
    assert!(setup_unfinished(&f.target()));
    assert!(f.service.setup_cancel(&started.id));
    let status =
        tokio::time::timeout(Duration::from_secs(10), f.service.setup_wait(&started.id)).await.unwrap().unwrap();
    assert_eq!(status.state, SetupState::Canceled);
    assert!(status.removed && !status.ready);
    assert!(!f.target().exists());
    assert!(!f.service.setup_cancel(&started.id), "nothing left to cancel");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_install_into_an_existing_environment_deletes_nothing() {
    let f = RunFixture::new();
    let venv = f.plan.make_venv(".venv");
    std::fs::write(f.target().join("keep.txt"), b"user data").unwrap();
    f.seed(vec![venv.clone()]);
    f.launcher.set(Behavior::Succeed, Behavior::Fail);
    let status = f
        .run(&SetupRequest::InstallPackages { environment: venv.id.clone(), packages: vec!["no-such-pkg".into()] })
        .await;
    assert_eq!(status.state, SetupState::Failed);
    assert!(!status.removed);
    assert!(status.message.as_deref().unwrap().contains("Nothing was removed from .venv"), "{status:?}");
    assert_eq!(std::fs::read(f.target().join("keep.txt")).unwrap(), b"user data");
    assert!(f.target().join("pyvenv.cfg").exists());
    assert!(read_marker(&f.target()).is_none(), "an existing environment gets no setup marker");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_replaced_marker_keeps_the_folder_and_says_so() {
    let f = RunFixture::new();
    f.seed(vec![]);
    f.launcher.set(Behavior::Succeed, Behavior::ReplaceMarkerThenFail);
    let status = f.run(&f.plan.create(&[])).await;
    assert_eq!(status.state, SetupState::Failed);
    assert!(!status.removed);
    assert!(status.message.as_deref().unwrap().contains("left in place"), "{status:?}");
    assert!(f.target().exists());
    // Never reported ready, and the record stays for an explicit cleanup.
    assert!(setup_unfinished(&f.target()));
    assert_eq!(f.records().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn one_setup_at_a_time_and_a_project_switch_cancels_it() {
    let f = RunFixture::new();
    f.seed(vec![]);
    f.launcher.set(Behavior::Block, Behavior::Succeed);
    let root = f.service.workspace_root().unwrap();
    let plan = f.service.setup_plan(&root, &f.plan.create(&[])).await.unwrap();
    let started = f.service.setup_start(&plan, SessionOwner::User).await.unwrap();
    let error = f.service.setup_start(&plan, SessionOwner::Agent { task_id: "t".into() }).await.unwrap_err();
    assert_eq!(error.code(), "setup.busy", "{error:?}");
    // Wait until the first step runs: the run has made the environment folder
    // by then. A switch before the run's thread gets that far cancels a setup
    // that created nothing (nothing to remove), which CI hit under load.
    let mut events = f.service.subscribe_setup();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(status) = f.service.setup_status()
                && status.step == Some(0)
                && status.jobs.len() == 1
            {
                break;
            }
            let _ = tokio::time::timeout(Duration::from_millis(100), events.recv()).await;
        }
    })
    .await
    .unwrap();
    assert!(f.target().exists(), "the run made the environment folder");
    let other = f.plan.root.parent().unwrap().join("other");
    std::fs::create_dir_all(&other).unwrap();
    f.service.set_workspace(&other, true).unwrap();
    let status =
        tokio::time::timeout(Duration::from_secs(10), f.service.setup_wait(&started.id)).await.unwrap().unwrap();
    assert_eq!(status.state, SetupState::Canceled);
    assert!(status.removed);
    assert!(!root.join(".venv").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_changed_project_makes_the_plan_stale() {
    let f = RunFixture::new();
    f.seed(vec![]);
    let root = f.service.workspace_root().unwrap();
    let mut plan = f.service.setup_plan(&root, &f.plan.create(&["numpy"])).await.unwrap();
    // A tampered plan (other commands under the same request) is refused.
    plan.steps[1].args.push("--extra-index-url=https://evil.example/simple".into());
    assert_eq!(f.service.setup_start(&plan, SessionOwner::User).await.unwrap_err().code(), "setup.planStale");
    // Not trusted any more: refused before anything runs.
    let plan = f.service.setup_plan(&root, &f.plan.create(&[])).await.unwrap();
    f.launcher.trusted.store(false, Ordering::SeqCst);
    assert_eq!(f.service.setup_start(&plan, SessionOwner::User).await.unwrap_err().code(), "research.notTrusted");
    assert!(f.launcher.specs.lock().unwrap().is_empty(), "nothing ran");
    assert!(!f.target().exists());
}

/// R-a1 F7: planning (an agent's request is planned before its approval)
/// never runs a project interpreter; the project environment is recognized
/// by its files.
#[tokio::test(flavor = "multi_thread")]
async fn planning_never_probes_a_project_interpreter() {
    let f = RunFixture::new();
    let venv = f.plan.make_venv(".venv");
    let root = f.service.workspace_root().unwrap();
    let install = SetupRequest::InstallPackages { environment: venv.id.clone(), packages: vec![] };
    let plan = f.service.setup_plan(&root, &install).await.unwrap();
    assert_eq!(plan.target_name, ".venv");
    assert!(!plan.creates_environment);
    // Creating while .venv exists is refused from its files alone.
    assert!(f.service.setup_plan(&root, &f.plan.create(&[])).await.is_err());
    assert_eq!(f.launcher.project_probes.load(Ordering::SeqCst), 0, "no project interpreter ran");
    // Without its interpreter the folder is not an environment to install into.
    std::fs::remove_file(venv_python(&f.target())).unwrap();
    assert_eq!(f.service.setup_plan(&root, &install).await.unwrap_err().code(), "setup.unsupported");
    // A `venv` id that is not a folder of this project is unknown.
    let elsewhere =
        SetupRequest::InstallPackages { environment: "python:C:\\elsewhere\\python.exe".into(), packages: vec![] };
    assert_eq!(f.service.setup_plan(&root, &elsewhere).await.unwrap_err().code(), "research.unknownEnvironment");
    assert_eq!(f.launcher.project_probes.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn discovery_never_runs_an_unfinished_environment() {
    let f = RunFixture::new();
    let target = f.target();
    let python = venv_python(&target);
    std::fs::create_dir_all(python.parent().unwrap()).unwrap();
    std::fs::write(&python, b"").unwrap();
    write_marker(&target, "t", MarkerState::InProgress, 1).unwrap();
    let report = f.service.environments(true).await.unwrap();
    let env = report.runtimes.iter().find(|r| r.id == environment_id_for(&target)).expect("listed");
    assert!(matches!(env.availability, Availability::Unavailable { .. }));
    assert_eq!(env.setup.as_ref().and_then(|s| s.action), Some(crate::types::SetupAction::CreateEnvironment));
    assert_eq!(f.launcher.project_probes.load(Ordering::SeqCst), 0, "its interpreter never ran");
    // Once complete, discovery probes it like any project environment.
    write_marker(&target, "t", MarkerState::Complete, 1).unwrap();
    std::fs::write(target.join("pyvenv.cfg"), b"home = x\n").unwrap();
    let report = f.service.environments(true).await.unwrap();
    let env = report.runtimes.iter().find(|r| r.id == environment_id_for(&target)).expect("listed");
    assert_eq!(env.availability, Availability::Available);
    assert!(env.display_name.contains("project .venv"), "{}", env.display_name);
    assert_eq!(f.launcher.project_probes.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_environment_choice_is_remembered_per_project() {
    let f = RunFixture::new();
    let venv = f.plan.make_venv(".venv");
    f.seed(vec![venv.clone()]);
    assert_eq!(f.service.selected_environment().unwrap(), None);
    f.service.select_environment(Some(venv.id.clone())).await.unwrap();
    assert_eq!(f.service.selected_environment().unwrap(), Some(venv.id.clone()));
    let unknown = f.service.select_environment(Some("python:C:\\nowhere.exe".into())).await.unwrap_err();
    assert_eq!(unknown.code(), "research.unknownEnvironment");
    // Another project has its own (empty) choice; switching back restores it.
    let other = f.plan.root.parent().unwrap().join("other2");
    std::fs::create_dir_all(&other).unwrap();
    f.service.set_workspace(&other, true).unwrap();
    assert_eq!(f.service.selected_environment().unwrap(), None);
    f.service.set_workspace(&f.plan.root, true).unwrap();
    assert_eq!(f.service.selected_environment().unwrap(), Some(venv.id));
}
