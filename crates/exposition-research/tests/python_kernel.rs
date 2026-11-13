//! Supervisor tests against a real Python interpreter with ipykernel.
//!
//! These use whatever interpreter discovery finds. When none is available the
//! tests print a skip notice and pass, unless EXPOSITION_REQUIRE_RESEARCH_PYTHON
//! is set (then they fail), so local evidence runs can insist on the real thing.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use exposition_core::artifact::{ArtifactKind, Producer, RunStatus};
use exposition_core::document::TextEncoding;
use exposition_core::path::WorkspacePath;
use exposition_core::text::TextRange;
use exposition_execution::{ExecutionConfig, ExecutionService};
use exposition_research::helper::sha256_hex;
use exposition_research::launcher::ExecutionLauncher;
use exposition_research::types::{
    Availability, ExecuteRequest, OutputItem, ResearchEvent, RunSource, RunSummary, RuntimeLanguage, SessionInfo,
    SessionOwner, SessionState,
};
use exposition_research::{ResearchConfig, ResearchError, ResearchService, StartSessionRequest};
use exposition_workspace::{DocumentService, OpenDocument};
use tokio::sync::broadcast::Receiver;

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    /// A second, never-trusted project folder.
    other: PathBuf,
    data: PathBuf,
    execution: ExecutionService,
    service: ResearchService,
    documents: Arc<DocumentService>,
    events: Receiver<ResearchEvent>,
}

fn required() -> bool {
    std::env::var_os("EXPOSITION_REQUIRE_RESEARCH_PYTHON").is_some_and(|v| !v.is_empty())
}

async fn fixture(trusted: bool) -> Fixture {
    fixture_with(trusted, |_| {}).await
}

async fn fixture_with(trusted: bool, configure: impl FnOnce(&mut ResearchConfig)) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap().join("project");
    std::fs::create_dir_all(&root).unwrap();
    let other = std::fs::canonicalize(dir.path()).unwrap().join("other");
    std::fs::create_dir_all(&other).unwrap();
    let other = std::fs::canonicalize(&other).unwrap();
    let data = std::fs::canonicalize(dir.path()).unwrap().join("appdata");
    let documents = Arc::new(DocumentService::new());
    // The real shared execution service: containment, clean environment, trust.
    let execution = ExecutionService::with_config(ExecutionConfig::new(&data)).unwrap();
    let mut config =
        ResearchConfig::new(data.clone(), Arc::new(ExecutionLauncher::new(execution.clone())), documents.clone());
    config.cancel_grace = Duration::from_secs(3);
    configure(&mut config);
    let service = ResearchService::new(config);
    let root = std::fs::canonicalize(&root).unwrap();
    if trusted {
        execution.grant_trust(&root.display().to_string()).unwrap();
    }
    // Trust is the execution service's decision; the flag passed here is ignored.
    let status = service.set_workspace(&root, !trusted).unwrap();
    assert_eq!(status.trusted, trusted);
    let events = service.subscribe();
    Fixture { _dir: dir, root, other, data, execution, service, documents, events }
}

/// The best available Python environment (with matplotlib/pandas when asked).
async fn python(fixture: &Fixture, science: bool) -> Option<String> {
    let report = fixture.service.environments(true).await.unwrap();
    let found = report.runtimes.iter().find(|r| {
        r.language == RuntimeLanguage::Python
            && r.availability == Availability::Available
            && (!science || ["matplotlib", "pandas"].iter().all(|p| r.packages.iter().any(|t| t.name == *p)))
    });
    match found {
        Some(env) => Some(env.id.clone()),
        None => {
            assert!(!required(), "EXPOSITION_REQUIRE_RESEARCH_PYTHON is set but no usable Python was found");
            eprintln!("SKIPPED: no Python interpreter with ipykernel/jupyter_client was discovered");
            None
        }
    }
}

async fn start(fixture: &Fixture, env: &str, owner: SessionOwner) -> SessionInfo {
    fixture.service.start_session(StartSessionRequest { environment_id: env.to_owned(), owner }).await.unwrap()
}

fn run(fixture: &Fixture, session: &SessionInfo, source: RunSource) -> RunSummary {
    fixture
        .service
        .execute(ExecuteRequest { session_id: session.id.clone(), source, requester: session.owner.clone() })
        .unwrap()
}

fn input(code: &str) -> RunSource {
    RunSource::Input { code: code.to_owned() }
}

async fn finished(events: &mut Receiver<ResearchEvent>, run_id: &str, timeout: Duration) -> RunSummary {
    tokio::time::timeout(timeout, async {
        loop {
            if let Ok(ResearchEvent::RunFinished { run }) = events.recv().await
                && run.id.0 == run_id
            {
                return run;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("run {run_id} did not finish within {timeout:?}"))
}

async fn started(events: &mut Receiver<ResearchEvent>, run_id: &str) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(ResearchEvent::RunStarted { run_id: id, .. }) = events.recv().await
                && id.0 == run_id
            {
                return;
            }
        }
    })
    .await
    .expect("run did not start");
}

async fn session_state(events: &mut Receiver<ResearchEvent>, predicate: impl Fn(&SessionState) -> bool) -> SessionInfo {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if let Ok(ResearchEvent::SessionChanged { session }) = events.recv().await
                && predicate(&session.state)
            {
                return session;
            }
        }
    })
    .await
    .expect("session did not reach the expected state")
}

fn texts(fixture: &Fixture, run_id: &str) -> (String, Vec<String>) {
    let page = fixture.service.run_output(run_id, None, 500).unwrap();
    let mut stream = String::new();
    let mut displays = Vec::new();
    for entry in page.entries {
        match entry.item {
            OutputItem::Stream { text, .. } => stream.push_str(&text),
            OutputItem::Display { text: Some(text), .. } => displays.push(text),
            _ => {}
        }
    }
    (stream, displays)
}

/// Processes (helpers, kernels, and kernel launcher children) whose command
/// line mentions this fixture's application data directory. After shutdown
/// there must be none: no orphaned kernels.
async fn assert_no_leftover_processes(fx: &Fixture) {
    if !cfg!(windows) {
        return;
    }
    let root = std::env::var_os("SYSTEMROOT").expect("SYSTEMROOT");
    let powershell = Path::new(&root).join("System32").join("WindowsPowerShell").join("v1.0").join("powershell.exe");
    let mut count = String::new();
    for _ in 0..20 {
        let output = std::process::Command::new(&powershell)
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "@(Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -and $_.CommandLine.Contains($env:EXPOSITION_PROBE_DIR) }).Count",
            ])
            .env("EXPOSITION_PROBE_DIR", fx.data.display().to_string())
            .output()
            .unwrap();
        count = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if count == "0" {
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("{count} research processes outlived their session");
}

fn kill_pid(pid: u32) {
    let status = if cfg!(windows) {
        let root = std::env::var_os("SYSTEMROOT").expect("SYSTEMROOT");
        std::process::Command::new(Path::new(&root).join("System32").join("taskkill.exe"))
            .args(["/PID", &pid.to_string(), "/F"])
            .output()
            .unwrap()
            .status
    } else {
        std::process::Command::new("kill").args(["-9", &pid.to_string()]).output().unwrap().status
    };
    assert!(status.success(), "failed to kill {pid}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn runs_record_portable_provenance() {
    let mut fx = fixture(true).await;
    let Some(env) = python(&fx, false).await else { return };
    let session = start(&fx, &env, SessionOwner::User).await;
    assert_eq!(session.state, SessionState::Idle);
    assert_eq!(session.kernel.as_ref().unwrap().language, "python");
    assert_eq!(session.interrupt_mode, if cfg!(windows) { "event" } else { "signal" });
    eprintln!("python session transport encryption: {}", session.transport_encryption);

    // Panel input.
    let code = "print('hello')\n6*7";
    let summary = run(&fx, &session, input(code));
    let done = finished(&mut fx.events, &summary.id.0, Duration::from_secs(60)).await;
    assert_eq!(done.status, RunStatus::Succeeded);
    assert_eq!(texts(&fx, &summary.id.0), ("hello\n".to_owned(), vec!["42".to_owned()]));
    let manifest_path = fx.root.join(".exposition").join("runs").join(format!("{}.json", summary.id.0));
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    assert_eq!(manifest["format"], "exposition.run/1");
    assert_eq!(manifest["code"]["sha256"], sha256_hex(code.as_bytes()));
    assert_eq!(manifest["record"]["status"]["kind"], "succeeded");
    let environment = manifest["record"]["environment"].as_array().unwrap();
    let version_of = |name: &str| environment.iter().find(|t| t["name"] == name).map(|t| t["version"].clone());
    assert!(version_of("python").is_some_and(|v| v.as_str().unwrap().starts_with('3')));
    assert!(version_of("jupyter_client").is_some());
    assert!(version_of("ipykernel").is_some());
    assert_eq!(version_of("exposition-kernel-host").unwrap(), "0.1.0");
    let stored = fx.service.run_manifest(&summary.id.0).unwrap();
    assert!(matches!(stored.record.producer, Producer::KernelSession { ref runtime, .. } if runtime == "python"));
    assert!(!stored.session.interpreter.location.is_empty());

    // Secret-looking output is redacted before it is kept or shown.
    let leak = run(&fx, &session, input("print('key: sk-ant-' + 'a1b2c3d4' * 4)"));
    finished(&mut fx.events, &leak.id.0, Duration::from_secs(60)).await;
    let (stream, _) = texts(&fx, &leak.id.0);
    assert!(stream.contains("[REDACTED]") && !stream.contains("a1b2c3d4a1b2c3d4"), "{stream}");

    // A file on disk: pinned by content hash.
    let source = "squares = [n * n for n in range(5)]\nprint(sum(squares))\n";
    std::fs::write(fx.root.join("analysis.py"), source).unwrap();
    let path = WorkspacePath::parse("analysis.py").unwrap();
    let file_run = run(&fx, &session, RunSource::File { path: path.clone() });
    let done = finished(&mut fx.events, &file_run.id.0, Duration::from_secs(60)).await;
    assert_eq!(done.status, RunStatus::Succeeded);
    assert_eq!(texts(&fx, &file_run.id.0).0, "30\n");
    let manifest = fx.service.run_manifest(&file_run.id.0).unwrap();
    assert_eq!(manifest.record.inputs.len(), 1);
    assert_eq!(manifest.record.inputs[0].path, path);
    assert_eq!(manifest.record.inputs[0].revision, None);
    assert_eq!(
        manifest.record.inputs[0].content_hash.as_deref(),
        Some(format!("sha256:{}", sha256_hex(source.as_bytes()))).as_deref()
    );

    // A selection of an open document: pinned by revision.
    let doc = fx.documents.open(OpenDocument {
        path: Some(path.clone()),
        text: source.to_owned(),
        language: "python".into(),
        encoding: TextEncoding::Utf8,
        read_only: None,
    });
    let selection =
        RunSource::Selection { document: doc.id.clone(), revision: doc.revision, range: TextRange::new(0, 35) };
    let sel_run = run(&fx, &session, selection);
    let done = finished(&mut fx.events, &sel_run.id.0, Duration::from_secs(60)).await;
    assert_eq!(done.status, RunStatus::Succeeded);
    let manifest = fx.service.run_manifest(&sel_run.id.0).unwrap();
    assert_eq!(manifest.record.inputs[0].revision, Some(doc.revision));
    assert_eq!(manifest.code.text.as_deref(), Some("squares = [n * n for n in range(5)]"));
    let stale = fx.service.execute(ExecuteRequest {
        session_id: session.id.clone(),
        source: RunSource::Selection { document: doc.id, revision: doc.revision + 5, range: TextRange::new(0, 3) },
        requester: SessionOwner::User,
    });
    assert!(matches!(stale, Err(ResearchError::StaleSource(_))));

    // Wrong language and wrong owner are refused.
    std::fs::write(fx.root.join("paper.typ"), "= Title").unwrap();
    let wrong = fx.service.execute(ExecuteRequest {
        session_id: session.id.clone(),
        source: RunSource::File { path: WorkspacePath::parse("paper.typ").unwrap() },
        requester: SessionOwner::User,
    });
    assert!(matches!(wrong, Err(ResearchError::InvalidRequest(_))));
    let agent = fx.service.execute(ExecuteRequest {
        session_id: session.id.clone(),
        source: input("1"),
        requester: SessionOwner::Agent { task_id: "t1".into() },
    });
    assert!(matches!(agent, Err(ResearchError::OwnerMismatch { .. })));

    // History survives in the project.
    let history = fx.service.runs().unwrap();
    assert!(history.len() >= 3);
    fx.service.shutdown(&session.id).await.unwrap();
    assert!(fx.service.sessions().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn figures_and_tables_become_artifacts() {
    let mut fx = fixture(true).await;
    let Some(env) = python(&fx, true).await else { return };
    let session = start(&fx, &env, SessionOwner::User).await;
    let code = "import matplotlib.pyplot as plt\nimport pandas as pd\n\
                plt.plot([0, 1, 2], [0, 1, 4])\nplt.title('squares')\nplt.show()\n\
                pd.DataFrame({'n': [1, 2, 3], 'square': [1, 4, 9]})";
    let summary = run(&fx, &session, input(code));
    let done = finished(&mut fx.events, &summary.id.0, Duration::from_secs(120)).await;
    assert_eq!(done.status, RunStatus::Succeeded, "{:?}", done.error);
    let figure = done.outputs.iter().find(|a| a.kind == ArtifactKind::Figure).expect("figure artifact");
    assert_eq!(figure.media_type, "image/png");
    let relative = figure.path.as_ref().unwrap();
    assert!(relative.as_str().starts_with(&format!(".exposition/artifacts/{}/", summary.id.0)));
    let bytes = std::fs::read(fx.root.join(relative.as_str())).unwrap();
    assert!(bytes.starts_with(b"\x89PNG"));
    assert_eq!(figure.content_hash, format!("sha256:{}", sha256_hex(&bytes)));
    assert_eq!(figure.size_bytes, bytes.len() as u64);
    let (_, served) = fx.service.artifact_bytes(&summary.id.0, &figure.id.0).unwrap();
    assert_eq!(served, bytes);

    let table = done.outputs.iter().find(|a| a.kind == ArtifactKind::Table).expect("table artifact");
    assert_eq!(table.media_type, "text/csv");
    let csv = std::fs::read_to_string(fx.root.join(table.path.as_ref().unwrap().as_str())).unwrap();
    assert_eq!(csv, ",n,square\n0,1,1\n1,2,4\n2,3,9\n");
    assert!(done.outputs.iter().any(|a| a.media_type == "text/html"));
    assert!(fx.service.artifact(&summary.id.0, "run-other/output-1.png").is_err());

    // Output entries reference artifacts rather than inlining them.
    let page = fx.service.run_output(&summary.id.0, None, 100).unwrap();
    let referenced: Vec<_> = page
        .entries
        .iter()
        .filter_map(|e| match &e.item {
            OutputItem::Display { artifacts, .. } => Some(artifacts.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    assert!(referenced.contains(&figure.id) && referenced.contains(&table.id));
    fx.service.shutdown(&session.id).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupt_and_cancel_long_runs() {
    let mut fx = fixture(true).await;
    let Some(env) = python(&fx, false).await else { return };
    let session = start(&fx, &env, SessionOwner::User).await;

    // Interrupt a blocking sleep.
    let sleeper = run(&fx, &session, input("import time\ntime.sleep(60)"));
    started(&mut fx.events, &sleeper.id.0).await;
    // A second run waits behind it; cancel it while queued.
    let queued = run(&fx, &session, input("print('should not run')"));
    assert!(queued.queued);
    fx.service.cancel_run(&queued.id.0).await.unwrap();
    let canceled = finished(&mut fx.events, &queued.id.0, Duration::from_secs(10)).await;
    assert_eq!(canceled.status, RunStatus::Canceled);
    let begin = std::time::Instant::now();
    fx.service.interrupt(&session.id).await.unwrap();
    let interrupted = finished(&mut fx.events, &sleeper.id.0, Duration::from_secs(30)).await;
    assert_eq!(interrupted.status, RunStatus::Interrupted);
    assert!(begin.elapsed() < Duration::from_secs(10), "interrupt took {:?}", begin.elapsed());

    // Cancel a running loop.
    let looping = run(&fx, &session, input("import time\nwhile True:\n    time.sleep(0.5)"));
    started(&mut fx.events, &looping.id.0).await;
    fx.service.cancel_run(&looping.id.0).await.unwrap();
    let canceled = finished(&mut fx.events, &looping.id.0, Duration::from_secs(30)).await;
    assert_eq!(canceled.status, RunStatus::Canceled);

    // Cancel a run that swallows interrupts: escalates to a kernel restart.
    let stubborn = run(
        &fx,
        &session,
        input(
            "import time\nwhile True:\n    try:\n        time.sleep(0.2)\n    except KeyboardInterrupt:\n        pass",
        ),
    );
    started(&mut fx.events, &stubborn.id.0).await;
    fx.service.cancel_run(&stubborn.id.0).await.unwrap();
    let canceled = finished(&mut fx.events, &stubborn.id.0, Duration::from_secs(60)).await;
    assert_eq!(canceled.status, RunStatus::Canceled);
    let idle = session_state(&mut fx.events, |s| *s == SessionState::Idle).await;
    assert!(idle.generation >= 2, "escalation must restart the kernel");
    let after = run(&fx, &session, input("'still usable'"));
    let done = finished(&mut fx.events, &after.id.0, Duration::from_secs(60)).await;
    assert_eq!(done.status, RunStatus::Succeeded);
    fx.service.shutdown(&session.id).await.unwrap();
    assert_no_leftover_processes(&fx).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crashed_kernel_and_helper_recover_by_restart() {
    let mut fx = fixture(true).await;
    let Some(env) = python(&fx, false).await else { return };
    let session = start(&fx, &env, SessionOwner::User).await;
    let setup = run(&fx, &session, input("state = 'kept'"));
    finished(&mut fx.events, &setup.id.0, Duration::from_secs(60)).await;

    // Kill the kernel mid-run.
    let victim = run(&fx, &session, input("import time\ntime.sleep(30)"));
    started(&mut fx.events, &victim.id.0).await;
    kill_pid(fx.service.sessions()[0].kernel_pid.expect("kernel pid"));
    let failed = finished(&mut fx.events, &victim.id.0, Duration::from_secs(30)).await;
    assert!(matches!(failed.status, RunStatus::Failed { ref message } if message.contains("kernel died")));
    let crashed = session_state(&mut fx.events, |s| matches!(s, SessionState::Crashed { .. })).await;
    assert!(matches!(crashed.state, SessionState::Crashed { ref reason } if reason.contains("kernel")));
    let refused = fx.service.execute(ExecuteRequest {
        session_id: session.id.clone(),
        source: input("1"),
        requester: SessionOwner::User,
    });
    assert!(matches!(refused, Err(ResearchError::SessionNotReady { .. })));

    let restarted = fx.service.restart(&session.id).await.unwrap();
    assert_eq!(restarted.state, SessionState::Idle);
    assert_eq!(restarted.generation, 2);
    let probe = run(&fx, &session, input("'state' in globals()"));
    finished(&mut fx.events, &probe.id.0, Duration::from_secs(60)).await;
    assert_eq!(texts(&fx, &probe.id.0).1, vec!["False".to_owned()], "restart must give a clean kernel");

    // Kill the helper itself.
    let helper_pid = fx.service.sessions()[0].helper_pid.expect("helper pid");
    kill_pid(helper_pid);
    let crashed = session_state(&mut fx.events, |s| matches!(s, SessionState::Crashed { .. })).await;
    assert!(matches!(crashed.state, SessionState::Crashed { ref reason } if reason.contains("helper")));
    let relaunched = fx.service.restart(&session.id).await.unwrap();
    assert_eq!(relaunched.state, SessionState::Idle);
    assert_ne!(relaunched.helper_pid, Some(helper_pid));
    let after = run(&fx, &session, input("21 * 2"));
    let done = finished(&mut fx.events, &after.id.0, Duration::from_secs(60)).await;
    assert_eq!(done.status, RunStatus::Succeeded);
    assert_eq!(texts(&fx, &after.id.0).1, vec!["42".to_owned()]);
    fx.service.shutdown(&session.id).await.unwrap();
    assert_no_leftover_processes(&fx).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn untrusted_workspaces_cannot_start_kernels() {
    let fx = fixture(false).await;
    let refused = fx
        .service
        .start_session(StartSessionRequest { environment_id: "python:any".into(), owner: SessionOwner::User });
    let error = refused.await.unwrap_err();
    assert_eq!(error, ResearchError::NotTrusted);
    assert_eq!(error.code(), "research.notTrusted");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_runtimes_report_setup_guidance() {
    let fx = fixture(true).await;
    let report = fx.service.environments(true).await.unwrap();
    for language in [RuntimeLanguage::R, RuntimeLanguage::Julia] {
        let entry = report.runtimes.iter().find(|r| r.language == language).expect("entry per language");
        match &entry.availability {
            Availability::Available => assert!(entry.kernel.is_some()),
            Availability::Unavailable { reason, guidance } => {
                assert!(!reason.is_empty());
                // Whichever case this host is in: the kernel package or its
                // registration is missing, or (the runtime is present but) no
                // Python with jupyter_client can host the kernel.
                assert!(
                    ["IRkernel", "IJulia", "installkernel", "jupyter_client"].iter().any(|fix| guidance.contains(fix)),
                    "{guidance}"
                );
                assert!(guidance.contains("never installs") || guidance.contains("never registers"), "{guidance}");
                eprintln!("{language:?}: {reason} {guidance}");
            }
        }
    }
    let unavailable = report.runtimes.iter().find(|r| matches!(r.availability, Availability::Unavailable { .. }));
    if let Some(env) = unavailable {
        let error = fx
            .service
            .start_session(StartSessionRequest { environment_id: env.id.clone(), owner: SessionOwner::User })
            .await
            .unwrap_err();
        assert!(matches!(error, ResearchError::EnvironmentUnavailable { .. }));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn helper_unit_tests_pass_with_real_interpreter() {
    let fx = fixture(true).await;
    let report = fx.service.environments(true).await.unwrap();
    let Some(python) = report
        .runtimes
        .iter()
        .find(|r| r.language == RuntimeLanguage::Python && r.availability == Availability::Available)
        .and_then(|r| r.executable.clone())
    else {
        assert!(!required(), "EXPOSITION_REQUIRE_RESEARCH_PYTHON is set but no usable Python was found");
        eprintln!("SKIPPED: no Python interpreter with ipykernel/jupyter_client was discovered");
        return;
    };
    let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../workers/kernel-host/tests");
    let output = tokio::task::spawn_blocking(move || {
        std::process::Command::new(python)
            .args(["-m", "unittest", "discover", "-s", &tests.display().to_string(), "-v"])
            .env("PYTHONUTF8", "1")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    let report = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "helper tests failed:\n{report}");
    assert!(report.contains("OK"), "{report}");
}

/// R (IRkernel) and Julia (IJulia) sessions run through the same helper when
/// their kernelspecs are registered; otherwise discovery reports guidance (see
/// `missing_runtimes_report_setup_guidance`). EXPOSITION_REQUIRE_RESEARCH_KERNELS
/// makes their absence a failure.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r_and_julia_kernels_execute_when_registered() {
    let mut fx = fixture(true).await;
    let report = fx.service.environments(true).await.unwrap();
    let require = std::env::var_os("EXPOSITION_REQUIRE_RESEARCH_KERNELS").is_some_and(|v| !v.is_empty());
    for (language, code, expected_stream, figure_code) in [
        (
            RuntimeLanguage::R,
            "x <- c(1, 2, 3)\ncat(sum(x), '\n')\ndata.frame(n = 1:2, sq = c(1, 4))",
            "6 \n",
            "plot(1:10)",
        ),
        (RuntimeLanguage::Julia, "println(1 + 2)\nsum([1, 2, 3])", "3\n", ""),
    ] {
        let Some(env) = report
            .runtimes
            .iter()
            .find(|r| r.language == language && r.availability == Availability::Available)
            .cloned()
        else {
            assert!(!require, "{language:?} kernel required but not available");
            eprintln!("SKIPPED: no registered {language:?} kernel");
            continue;
        };
        let variable = if language == RuntimeLanguage::R { "R_LIBS_USER" } else { "JULIA_DEPOT_PATH" };
        assert!(env.kernel_environment.contains_key(variable), "{language:?} discovery must derive {variable}");
        eprintln!("{language:?} kernel environment: {:?}", env.kernel_environment.keys().collect::<Vec<_>>());
        let session = start(&fx, &env.id, SessionOwner::User).await;
        let kernel = session.kernel.clone().unwrap();
        eprintln!("{language:?} session: {} kernel {} {}", env.display_name, kernel.language, kernel.language_version);
        let summary = run(&fx, &session, input(code));
        let done = finished(&mut fx.events, &summary.id.0, Duration::from_secs(180)).await;
        assert_eq!(done.status, RunStatus::Succeeded, "{language:?}: {:?}", done.error);
        let (stream, displays) = texts(&fx, &summary.id.0);
        assert_eq!(stream, expected_stream, "{language:?}");
        assert!(!displays.is_empty(), "{language:?} produced no result");
        if language == RuntimeLanguage::R {
            assert!(done.outputs.iter().any(|a| a.kind == ArtifactKind::Table), "R data.frame table");
            let plot = run(&fx, &session, input(figure_code));
            let done = finished(&mut fx.events, &plot.id.0, Duration::from_secs(120)).await;
            assert!(done.outputs.iter().any(|a| a.kind == ArtifactKind::Figure), "R plot figure");
        }
        let manifest = fx.service.run_manifest(&summary.id.0).unwrap();
        assert!(manifest.record.environment.iter().any(|t| t.name.eq_ignore_ascii_case(&kernel.language)));
        // Stop a long sleep. Kernels that honor interrupts are interrupted;
        // otherwise (IRkernel on Windows) cancel escalates to a kernel restart.
        let sleep = if language == RuntimeLanguage::R { "Sys.sleep(60)" } else { "sleep(60)" };
        let sleeper = run(&fx, &session, input(sleep));
        started(&mut fx.events, &sleeper.id.0).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let begin = std::time::Instant::now();
        let mode = fx.service.sessions()[0].interrupt_mode.clone();
        let stopped = if mode == "unsupported" {
            fx.service.cancel_run(&sleeper.id.0).await.unwrap();
            let stopped = finished(&mut fx.events, &sleeper.id.0, Duration::from_secs(90)).await;
            assert_eq!(stopped.status, RunStatus::Canceled);
            stopped
        } else {
            fx.service.interrupt(&session.id).await.unwrap();
            let stopped = finished(&mut fx.events, &sleeper.id.0, Duration::from_secs(90)).await;
            assert_eq!(stopped.status, RunStatus::Interrupted, "{language:?}");
            stopped
        };
        eprintln!("{language:?} stop via {mode}: {:?} after {:?}", stopped.status, begin.elapsed());
        assert!(begin.elapsed() < Duration::from_secs(45), "{language:?} stop too slow");
        fx.service.shutdown(&session.id).await.unwrap();
        assert_no_leftover_processes(&fx).await;
    }
}

fn norm(path: &str) -> String {
    path.trim().trim_matches('\'').replace("\\\\", "\\").replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

/// R-m4 #1: a session belongs to exactly one workspace and its trust.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sessions_are_bound_to_their_workspace_and_trust() {
    let mut fx = fixture(true).await;
    let Some(env) = python(&fx, false).await else { return };
    std::fs::write(fx.other.join("plot.py"), "open('ran-in-wrong-project.txt', 'w').write('x')\n").unwrap();
    let a = start(&fx, &env, SessionOwner::User).await;

    // Switching to another (untrusted) project detaches A's session at once.
    fx.service.set_workspace(&fx.other, false).unwrap();
    let detached = session_state(&mut fx.events, |s| matches!(s, SessionState::Detached { .. })).await;
    assert_eq!(detached.id, a.id);
    let mismatch = fx.service.execute(ExecuteRequest {
        session_id: a.id.clone(),
        source: RunSource::File { path: WorkspacePath::parse("plot.py").unwrap() },
        requester: SessionOwner::User,
    });
    assert!(matches!(mismatch, Err(ResearchError::WorkspaceMismatch { .. })), "{mismatch:?}");
    assert!(!fx.other.join("ran-in-wrong-project.txt").exists() && !fx.root.join("ran-in-wrong-project.txt").exists());
    // Coming back does not revive it: a detached session never runs code again.
    fx.service.set_workspace(&fx.root, false).unwrap();
    let again = fx.service.execute(ExecuteRequest {
        session_id: a.id.clone(),
        source: input("1"),
        requester: SessionOwner::User,
    });
    assert!(matches!(again, Err(ResearchError::SessionNotReady { .. })), "{again:?}");
    assert!(fx.service.restart(&a.id).await.is_err(), "a detached session cannot be restarted");

    // Revoking trust stops the kernel (watchdog) and refuses code.
    let b = start(&fx, &env, SessionOwner::User).await;
    fx.execution.revoke_trust(&fx.root.display().to_string()).unwrap();
    let revoked = session_state(&mut fx.events, |s| matches!(s, SessionState::Detached { .. })).await;
    assert_eq!(revoked.id, b.id);
    assert!(matches!(&revoked.state, SessionState::Detached { reason } if reason.contains("trust")));
    let refused = fx.service.execute(ExecuteRequest {
        session_id: b.id.clone(),
        source: input("1"),
        requester: SessionOwner::User,
    });
    assert!(refused.is_err());
    assert!(matches!(
        fx.service.start_session(StartSessionRequest { environment_id: env.clone(), owner: SessionOwner::User }).await,
        Err(ResearchError::NotTrusted)
    ));

    // Closing the project detaches every session.
    fx.execution.grant_trust(&fx.root.display().to_string()).unwrap();
    let c = start(&fx, &env, SessionOwner::User).await;
    fx.service.clear_workspace();
    let closed = session_state(&mut fx.events, |s| matches!(s, SessionState::Detached { .. })).await;
    assert_eq!(closed.id, c.id);
    assert!(matches!(
        fx.service.execute(ExecuteRequest {
            session_id: c.id.clone(),
            source: input("1"),
            requester: SessionOwner::User
        }),
        Err(ResearchError::WorkspaceMismatch { .. })
    ));
    assert_no_leftover_processes(&fx).await;
}

/// R-m4 #7: every kernel's working directory is the workspace root, as a plain path.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kernels_start_in_the_workspace_root() {
    let mut fx = fixture(true).await;
    let report = fx.service.environments(true).await.unwrap();
    let expected = norm(&fx.root.display().to_string().replace(r"\\?\", ""));
    for (language, code) in [
        (RuntimeLanguage::Python, "import os\nprint(os.getcwd())"),
        (RuntimeLanguage::R, "cat(getwd(), '\\n')"),
        (RuntimeLanguage::Julia, "println(pwd())"),
    ] {
        let Some(env) =
            report.runtimes.iter().find(|r| r.language == language && r.availability == Availability::Available)
        else {
            assert!(!(language == RuntimeLanguage::Python && required()), "Python required");
            eprintln!("SKIPPED: no {language:?} kernel");
            continue;
        };
        let session = start(&fx, &env.id, SessionOwner::User).await;
        let summary = run(&fx, &session, input(code));
        let done = finished(&mut fx.events, &summary.id.0, Duration::from_secs(120)).await;
        assert_eq!(done.status, RunStatus::Succeeded, "{language:?}");
        let cwd = texts(&fx, &summary.id.0).0;
        assert!(!cwd.contains(r"\\?\"), "{language:?} cwd is a verbatim path: {cwd}");
        assert_eq!(norm(&cwd), expected, "{language:?} kernel cwd");
        fx.service.shutdown(&session.id).await.unwrap();
    }
}

/// R-m4 #12: research leaves interactive-job headroom for other services.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_caps_are_per_owner() {
    let fx = fixture_with(true, |config| {
        config.max_user_sessions = 1;
        config.max_agent_sessions = 1;
    })
    .await;
    let Some(env) = python(&fx, false).await else { return };
    let user = start(&fx, &env, SessionOwner::User).await;
    let refused = fx
        .service
        .start_session(StartSessionRequest { environment_id: env.clone(), owner: SessionOwner::User })
        .await
        .unwrap_err();
    assert!(matches!(refused, ResearchError::TooManySessions { limit: 1, .. }), "{refused:?}");
    assert_eq!(refused.code(), "research.tooManySessions");
    let agent = start(&fx, &env, SessionOwner::Agent { task_id: "t".into() }).await;
    fx.service.shutdown(&user.id).await.unwrap();
    let again = start(&fx, &env, SessionOwner::User).await;
    fx.service.shutdown(&again.id).await.unwrap();
    fx.service.shutdown(&agent.id).await.unwrap();
}

/// R-m4 #4: interpreters inside a project run only when it is trusted, as
/// trusted project jobs; discovery never takes executables from callers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_interpreters_are_probed_only_when_trusted() {
    let program_dir = if cfg!(windows) { ".venv/Scripts" } else { ".venv/bin" };
    let program_name = if cfg!(windows) { "python.exe" } else { "python" };
    for trusted in [false, true] {
        let fx = fixture(trusted).await;
        let planted = fx.root.join(program_dir).join(program_name);
        std::fs::create_dir_all(planted.parent().unwrap()).unwrap();
        std::fs::write(&planted, b"not a real interpreter").unwrap();
        fx.service.environments(true).await.unwrap();
        let jobs: Vec<_> = fx
            .execution
            .list()
            .into_iter()
            .filter(|job| {
                let program = job.info.program.to_lowercase();
                program.contains(".venv") && program.ends_with(program_name)
            })
            .collect();
        if trusted {
            assert!(!jobs.is_empty(), "a trusted project's .venv interpreter is probed");
            assert!(jobs.iter().all(|j| j.kind == exposition_execution::JobKind::Batch), "as a trusted project job");
        } else {
            assert!(jobs.is_empty(), "an untrusted project's interpreter must not run: {jobs:?}");
        }
    }
}

/// The cached discovery report follows trust: environments discovered while
/// the project was untrusted (no project interpreters) are rediscovered once
/// it is trusted, without an explicit refresh, and dropped again on revoke.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cached_environments_follow_trust_changes() {
    let program_dir = if cfg!(windows) { ".venv/Scripts" } else { ".venv/bin" };
    let program_name = if cfg!(windows) { "python.exe" } else { "python" };
    let fx = fixture(false).await;
    let planted = fx.root.join(program_dir).join(program_name);
    std::fs::create_dir_all(planted.parent().unwrap()).unwrap();
    std::fs::write(&planted, b"not a real interpreter").unwrap();
    let venv_probes = || {
        fx.execution
            .list()
            .into_iter()
            .filter(|job| {
                let program = job.info.program.to_lowercase();
                program.contains(".venv") && program.ends_with(program_name)
            })
            .count()
    };
    fx.service.environments(false).await.unwrap();
    assert_eq!(venv_probes(), 0, "an untrusted project's interpreter must not run");
    fx.execution.grant_trust(&fx.root.display().to_string()).unwrap();
    fx.service.environments(false).await.unwrap();
    let after_grant = venv_probes();
    assert!(after_grant > 0, "trusting the project rediscovers its interpreters");
    // Cached while trust is unchanged.
    fx.service.environments(false).await.unwrap();
    assert_eq!(venv_probes(), after_grant);
    fx.execution.revoke_trust(&fx.root.display().to_string()).unwrap();
    fx.service.environments(false).await.unwrap();
    assert_eq!(venv_probes(), after_grant, "nothing from the project runs after the revoke");
}

/// M8: a workflow step's session is its own: users, agents, and other
/// workflow steps cannot run code in it, and it cannot use theirs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workflow_sessions_are_isolated_from_users_agents_and_other_steps() {
    let mut fx = fixture(true).await;
    let Some(env) = python(&fx, false).await else { return };
    let owner = SessionOwner::Workflow { run: "wf-1".into(), step: "fit".into() };
    let workflow = start(&fx, &env, owner.clone()).await;
    let user = start(&fx, &env, SessionOwner::User).await;
    for intruder in [
        SessionOwner::User,
        SessionOwner::Agent { task_id: "t1".into() },
        SessionOwner::Workflow { run: "wf-1".into(), step: "other".into() },
        SessionOwner::Workflow { run: "wf-2".into(), step: "fit".into() },
    ] {
        let refused = fx.service.execute(ExecuteRequest {
            session_id: workflow.id.clone(),
            source: input("1"),
            requester: intruder.clone(),
        });
        assert!(matches!(refused, Err(ResearchError::OwnerMismatch { .. })), "{intruder:?}: {refused:?}");
    }
    let refused = fx.service.execute(ExecuteRequest {
        session_id: user.id.clone(),
        source: input("1"),
        requester: owner.clone(),
    });
    assert!(matches!(refused, Err(ResearchError::OwnerMismatch { .. })), "{refused:?}");
    let own = run(&fx, &workflow, input("print(6 * 7)"));
    let done = finished(&mut fx.events, &own.id.0, Duration::from_secs(60)).await;
    assert_eq!(done.status, RunStatus::Succeeded);
    assert_eq!(done.owner, owner);
    fx.service.shutdown(&workflow.id).await.unwrap();
    fx.service.shutdown(&user.id).await.unwrap();
}
