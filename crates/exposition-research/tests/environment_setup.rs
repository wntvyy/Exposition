//! Environment setup against real tools: a host Python creates the project's
//! `.venv`, pip installs the kernel packages from PyPI, discovery reports the
//! environment ready, and a session runs code in it; a failing install and a
//! canceled setup leave honest state.
//!
//! Needs a discovered Python outside the project and network access to
//! pypi.org. Without either the tests print a skip notice and pass, unless
//! EXPOSITION_REQUIRE_SETUP_TOOLS is set (then they fail). Temporary folders
//! come from the system temporary directory (set TMP/TEMP to keep them
//! elsewhere); every folder is removed at the end.

use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use exposition_core::artifact::RunStatus;
use exposition_execution::{ExecutionConfig, ExecutionService};
use exposition_research::launcher::ExecutionLauncher;
use exposition_research::setup::{SetupPhase, SetupRequest, SetupState};
use exposition_research::types::{
    Availability, ExecuteRequest, OutputItem, ResearchEvent, RunSource, RuntimeEnvironment, RuntimeLanguage,
    SessionOwner, SetupAction,
};
use exposition_research::{ResearchConfig, ResearchService, StartSessionRequest};
use exposition_workspace::DocumentService;

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    execution: ExecutionService,
    service: ResearchService,
}

fn required() -> bool {
    std::env::var_os("EXPOSITION_REQUIRE_SETUP_TOOLS").is_some_and(|v| !v.is_empty())
}

fn skip(reason: &str) {
    assert!(!required(), "EXPOSITION_REQUIRE_SETUP_TOOLS is set but {reason}");
    eprintln!("SKIPPED: {reason}");
}

fn pypi_reachable() -> bool {
    let Ok(mut addrs) = ("pypi.org", 443).to_socket_addrs() else { return false };
    addrs.next().is_some_and(|addr| TcpStream::connect_timeout(&addr, Duration::from_secs(5)).is_ok())
}

fn fixture() -> Fixture {
    let dir = tempfile::Builder::new().prefix("exp-setup-").tempdir().unwrap();
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
    Fixture { _dir: dir, root, execution, service }
}

fn inside(path: &str, root: &Path) -> bool {
    let p = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
    p.starts_with(root)
}

/// A discovered Python outside the project, preferring one that lacks the
/// kernel packages (the real "setup needed" case).
async fn base_python(f: &Fixture) -> Option<RuntimeEnvironment> {
    let report = f.service.environments(true).await.unwrap();
    let mut candidates: Vec<RuntimeEnvironment> = report
        .runtimes
        .into_iter()
        .filter(|r| {
            r.language == RuntimeLanguage::Python && r.executable.as_deref().is_some_and(|e| !inside(e, &f.root))
        })
        .collect();
    candidates.sort_by_key(|r| r.availability == Availability::Available);
    let base = candidates.into_iter().next();
    match &base {
        Some(env) => eprintln!("base interpreter: {} ({:?})", env.display_name, env.executable),
        None => skip("no Python interpreter was discovered"),
    }
    base
}

async fn ready_base(f: &Fixture) -> Option<RuntimeEnvironment> {
    if !pypi_reachable() {
        skip("pypi.org is not reachable");
        return None;
    }
    base_python(f).await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_python_sets_up_a_ready_project_environment_that_runs_code() {
    let f = fixture();
    let Some(base) = ready_base(&f).await else { return };
    if let Availability::Unavailable { reason, .. } = &base.availability {
        eprintln!("the base lacks the kernel packages: {reason}");
        assert_eq!(base.setup.as_ref().and_then(|s| s.action), Some(SetupAction::CreateEnvironment));
    }
    let request = SetupRequest::CreateEnvironment { base_environment: base.id.clone(), packages: vec![] };
    let plan = f.service.setup_plan(&f.root, &request).await.unwrap();
    eprintln!("plan: {}", plan.summary);
    for step in &plan.steps {
        eprintln!("  {} {}", step.program, step.args.join(" "));
    }
    assert!(plan.creates_environment);
    let status = tokio::time::timeout(Duration::from_secs(900), f.service.setup_run(&plan, SessionOwner::User))
        .await
        .expect("setup finished in time")
        .unwrap();
    eprintln!("setup: {:?} {:?} jobs {:?}", status.state, status.message, status.jobs);
    assert_eq!(status.state, SetupState::Succeeded, "{:?}\n{}", status.message, status.log.join("\n"));
    assert!(status.ready && status.phase == SetupPhase::Finished);
    let env_id = status.environment_id.clone().expect("the new environment is discovered");
    let report = f.service.environments(false).await.unwrap();
    let env = report.runtimes.iter().find(|r| r.id == env_id).expect("listed");
    assert_eq!(env.availability, Availability::Available);
    assert!(env.display_name.contains("project .venv"), "{}", env.display_name);
    eprintln!("discovered: {} with {:?}", env.display_name, env.packages);

    // A session in the new environment runs code from the project environment.
    let mut events = f.service.subscribe();
    let session = f
        .service
        .start_session(StartSessionRequest { environment_id: env_id.clone(), owner: SessionOwner::User })
        .await
        .unwrap();
    let run = f
        .service
        .execute(ExecuteRequest {
            session_id: session.id.clone(),
            source: RunSource::Input { code: "import sys; print(sys.prefix)".into() },
            requester: SessionOwner::User,
        })
        .unwrap();
    let mut printed = String::new();
    let finished = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            match events.recv().await {
                Ok(ResearchEvent::Output { run_id, entry }) if run_id == run.id => {
                    if let OutputItem::Stream { text, .. } = entry.item {
                        printed.push_str(&text);
                    }
                }
                Ok(ResearchEvent::RunFinished { run: done }) if done.id == run.id => return done,
                _ => {}
            }
        }
    })
    .await
    .expect("the run finished");
    assert_eq!(finished.status, RunStatus::Succeeded, "{finished:?}");
    eprintln!("sys.prefix in the session: {}", printed.trim());
    assert!(printed.to_lowercase().contains(".venv"), "{printed}");
    f.service.shutdown(&session.id).await.unwrap();

    // A failing install (no such package) leaves the environment intact and ready.
    let failing = SetupRequest::InstallPackages {
        environment: env_id.clone(),
        packages: vec!["exposition-no-such-package-a1z".into()],
    };
    let plan = f.service.setup_plan(&f.root, &failing).await.unwrap();
    assert!(!plan.creates_environment);
    let status = f.service.setup_run(&plan, SessionOwner::User).await.unwrap();
    eprintln!("failing install: {:?} {:?}", status.state, status.message);
    assert_eq!(status.state, SetupState::Failed);
    assert!(!status.removed);
    let message = status.message.unwrap();
    assert!(message.contains("exposition-no-such-package-a1z"), "{message}");
    assert!(message.contains("Nothing was removed"), "{message}");
    assert!(f.root.join(".venv").join("pyvenv.cfg").is_file());
    let report = f.service.environments(true).await.unwrap();
    let env = report.runtimes.iter().find(|r| r.id == env_id).expect("still listed");
    assert_eq!(env.availability, Availability::Available, "still ready after the failed install");
    drop(f.execution);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_canceled_setup_removes_only_what_it_created_and_nothing_reads_ready() {
    let f = fixture();
    let Some(base) = ready_base(&f).await else { return };
    // Something of the person's in the project stays.
    std::fs::write(f.root.join("analysis.py"), "print(1)\n").unwrap();
    let request = SetupRequest::CreateEnvironment { base_environment: base.id.clone(), packages: vec!["numpy".into()] };
    let plan = f.service.setup_plan(&f.root, &request).await.unwrap();
    let started = f.service.setup_start(&plan, SessionOwner::User).await.unwrap();
    // Cancel once the install from PyPI is under way (or the environment exists).
    let mut events = f.service.subscribe_setup();
    let reached = tokio::time::timeout(Duration::from_secs(300), async {
        loop {
            if let Some(status) = f.service.setup_status()
                && (status.step == Some(1) || status.state != SetupState::Running)
            {
                return status;
            }
            let _ = tokio::time::timeout(Duration::from_millis(200), events.recv()).await;
        }
    })
    .await
    .expect("setup reached the install step");
    assert_eq!(reached.state, SetupState::Running, "{:?}", reached.message);
    // While it runs, discovery does not offer the half-made environment.
    let report = f.service.environments(true).await.unwrap();
    let venv_ready = report.runtimes.iter().any(|r| {
        r.executable.as_deref().is_some_and(|e| inside(e, &f.root)) && r.availability == Availability::Available
    });
    assert!(!venv_ready, "a half-made environment never reads ready");
    assert!(f.service.setup_cancel(&started.id));
    let status =
        tokio::time::timeout(Duration::from_secs(60), f.service.setup_wait(&started.id)).await.unwrap().unwrap();
    eprintln!("canceled: {:?} {:?}", status.state, status.message);
    assert_eq!(status.state, SetupState::Canceled);
    assert!(status.removed && !status.ready);
    assert!(!f.root.join(".venv").exists(), "the environment it created is gone");
    assert_eq!(std::fs::read_to_string(f.root.join("analysis.py")).unwrap(), "print(1)\n");
    let report = f.service.environments(true).await.unwrap();
    assert!(
        !report.runtimes.iter().any(|r| r.executable.as_deref().is_some_and(|e| inside(e, &f.root))),
        "no project environment remains"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn revoking_trust_stops_a_running_setup() {
    let f = fixture();
    let Some(base) = ready_base(&f).await else { return };
    let request = SetupRequest::CreateEnvironment { base_environment: base.id.clone(), packages: vec![] };
    let plan = f.service.setup_plan(&f.root, &request).await.unwrap();
    let started = f.service.setup_start(&plan, SessionOwner::User).await.unwrap();
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            if f.service.setup_status().is_some_and(|s| !s.jobs.is_empty()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    f.execution.revoke_trust(&f.root.display().to_string()).unwrap();
    let status =
        tokio::time::timeout(Duration::from_secs(60), f.service.setup_wait(&started.id)).await.unwrap().unwrap();
    eprintln!("after revoke: {:?} {:?}", status.state, status.message);
    assert_ne!(status.state, SetupState::Succeeded);
    assert!(!status.ready);
    assert!(!f.root.join(".venv").exists() || status.message.is_some());
}
