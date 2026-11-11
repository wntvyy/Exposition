//! Execution service behavior against real child processes.

mod common;

use std::time::Duration;

use common::*;
use exposition_core::job::{EnvironmentSpec, ExecutionPolicy, JobEvent, JobOutcome, OutputStream};
use exposition_execution::{ExecutionSettings, InteractiveOptions, JobState, ProtocolLog, RefusalCode};

fn refusal(f: &Fixture, id: &exposition_core::job::JobId) -> RefusalCode {
    f.service.summary(id).unwrap().refusal.expect("job should have been refused").code
}

#[tokio::test(flavor = "multi_thread")]
async fn environment_is_built_from_empty_without_credentials() {
    let f = fixture();
    let id = f.service.launch(f.spec(&["env"]));
    let result = f.service.wait(&id).await.unwrap();
    assert_eq!(result.outcome, JobOutcome::Exited { code: 0 });
    let env = f.stdout(&id);
    let names: Vec<&str> = env.lines().filter_map(|l| l.split('=').next()).collect();
    for forbidden in ["ANTHROPIC_API_KEY", "GITHUB_TOKEN", "EXPOSITION_TEST_HOST_ONLY"] {
        assert!(!names.iter().any(|n| n.eq_ignore_ascii_case(forbidden)), "{forbidden} leaked:\n{env}");
    }
    assert!(!env.contains(FAKE_GITHUB_TOKEN) && !env.contains("FAKEFAKE"));
    let temp = env.lines().find_map(|l| l.strip_prefix("TEMP=").or_else(|| l.strip_prefix("TMPDIR="))).unwrap();
    assert!(temp.contains(&id.0) && temp.ends_with("tmp"), "per-job temp dir: {temp}");
    if cfg!(windows) {
        assert!(names.iter().any(|n| n.eq_ignore_ascii_case("SystemRoot")));
        let path = env.lines().find_map(|l| l.strip_prefix("Path=").or_else(|| l.strip_prefix("PATH="))).unwrap();
        assert!(path.to_ascii_lowercase().contains(r"\system32"), "{path}");
    }
    // The per-job temp dir is removed afterwards.
    assert!(!std::path::Path::new(temp).exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn inherit_and_set_allowlists_apply_but_credentials_are_refused() {
    let f = fixture();
    let mut spec = f.spec(&["env"]);
    spec.environment = EnvironmentSpec {
        inherit: vec!["EXPOSITION_TEST_HOST_ONLY".into()],
        set: [("MPLBACKEND".to_string(), "Agg".to_string())].into(),
    };
    let id = f.service.launch(spec);
    f.service.wait(&id).await.unwrap();
    let env = f.stdout(&id);
    assert!(env.contains("EXPOSITION_TEST_HOST_ONLY=host-only-value"), "{env}");
    assert!(env.contains("MPLBACKEND=Agg"));

    for name in ["ANTHROPIC_API_KEY", "GITHUB_TOKEN", "OPENAI_API_KEY", "MY_PASSWORD"] {
        let mut inherit = f.spec(&["env"]);
        inherit.environment.inherit = vec![name.into()];
        let id = f.service.launch(inherit);
        let result = f.service.wait(&id).await.unwrap();
        assert!(matches!(result.outcome, JobOutcome::FailedToStart { .. }), "{name}: {result:?}");
        assert_eq!(refusal(&f, &id), RefusalCode::EnvironmentRefused);
        let mut set = f.spec(&["env"]);
        set.environment.set = [(name.to_string(), "value".to_string())].into();
        let id = f.service.launch(set);
        f.service.wait(&id).await.unwrap();
        assert_eq!(refusal(&f, &id), RefusalCode::EnvironmentRefused);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn working_directory_must_be_inside_registered_roots() {
    let f = fixture();
    let sep = std::path::MAIN_SEPARATOR;
    let cases = [
        ("relative", RefusalCode::InvalidWorkingDirectory),
        (&*format!("{}{sep}..{sep}data", f.workspace.display()), RefusalCode::InvalidWorkingDirectory),
        (&*f.workspace.join("missing").display().to_string(), RefusalCode::InvalidWorkingDirectory),
        (&*f.tmp.path().display().to_string(), RefusalCode::OutsideAllowedRoots),
    ];
    for (dir, code) in cases {
        let mut spec = f.spec(&["cwd"]);
        spec.working_directory = dir.to_string();
        let id = f.service.launch(spec);
        let result = f.service.wait(&id).await.unwrap();
        assert!(matches!(result.outcome, JobOutcome::FailedToStart { .. }), "{dir}");
        assert_eq!(refusal(&f, &id), code, "{dir}");
    }
    // A subdirectory works and the child really runs there.
    let sub = f.workspace.join("chapters");
    std::fs::create_dir_all(&sub).unwrap();
    let mut spec = f.spec(&["cwd"]);
    spec.working_directory = sub.display().to_string();
    let id = f.service.launch(spec);
    assert_eq!(f.service.wait(&id).await.unwrap().outcome, JobOutcome::Exited { code: 0 });
    assert!(f.stdout(&id).trim().to_lowercase().ends_with("chapters"));
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread")]
async fn junction_escaping_the_workspace_is_rejected() {
    let f = fixture();
    let outside = f.tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let link = f.workspace.join("escape");
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&link)
        .arg(&outside)
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "mklink /J failed");
    let mut spec = f.spec(&["cwd"]);
    spec.working_directory = link.display().to_string();
    let id = f.service.launch(spec);
    f.service.wait(&id).await.unwrap();
    assert_eq!(refusal(&f, &id), RefusalCode::OutsideAllowedRoots);
}

#[tokio::test(flavor = "multi_thread")]
async fn programs_must_be_absolute_existing_executables_or_tool_ids() {
    let f = fixture();
    let missing = f.workspace.join(if cfg!(windows) { "nope.exe" } else { "nope" });
    let mut cases = vec![
        (missing.display().to_string(), RefusalCode::ProgramNotFound),
        ("bin/tool.exe".to_string(), RefusalCode::ProgramNotFound),
        ("definitely-not-a-tool".to_string(), RefusalCode::ProgramNotFound),
        ("python-jupyter".to_string(), RefusalCode::ProgramRefused),
    ];
    if cfg!(windows) {
        let bat = f.workspace.join("build.bat");
        std::fs::write(&bat, "@echo pwned").unwrap();
        let cmd_file = f.workspace.join("build.cmd");
        std::fs::write(&cmd_file, "@echo pwned").unwrap();
        let script = f.workspace.join("tool.ps1");
        std::fs::write(&script, "x").unwrap();
        cases.push((bat.display().to_string(), RefusalCode::ProgramRefused));
        cases.push((cmd_file.display().to_string(), RefusalCode::ProgramRefused));
        cases.push((script.display().to_string(), RefusalCode::ProgramRefused));
        cases.push((r"C:\Windows\System32\cmd.exe".to_string(), RefusalCode::ProgramRefused));
    }
    for (program, code) in cases {
        let mut spec = f.spec(&[]);
        spec.program = program.clone();
        let id = f.service.launch(spec);
        let result = f.service.wait(&id).await.unwrap();
        assert!(matches!(result.outcome, JobOutcome::FailedToStart { .. }), "{program}");
        assert_eq!(refusal(&f, &id), code, "{program}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn exit_codes_and_stdin_are_reported() {
    let f = fixture();
    for code in [0, 3, 42] {
        let id = f.service.launch(f.spec(&["exit", &code.to_string()]));
        assert_eq!(f.service.wait(&id).await.unwrap().outcome, JobOutcome::Exited { code });
    }
    let mut spec = f.spec(&["stdin-count"]);
    spec.stdin = Some("hello".into());
    let id = f.service.launch(spec);
    f.service.wait(&id).await.unwrap();
    assert_eq!(f.stdout(&id).trim(), "5");
    // Arguments are passed verbatim: no shell interpolation of metacharacters.
    let tricky = r#"a "quoted" & echo pwned | x > y %PATH% $(whoami)"#;
    let id = f.service.launch(f.spec(&["print", tricky]));
    f.service.wait(&id).await.unwrap();
    assert_eq!(f.stdout(&id).trim_end(), tricky);
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_kills_the_whole_process_tree() {
    let f = fixture();
    let mut events = f.service.subscribe();
    let id = f.service.launch(f.spec(&["tree", "2", "120"]));
    let text = f.wait_stdout(&id, Duration::from_secs(20), |t| pids(t).len() == 3).await;
    let tree = pids(&text);
    assert!(tree.iter().all(|p| alive(*p)), "tree should be running: {tree:?}");
    assert_eq!(f.service.summary(&id).unwrap().state, JobState::Running);
    assert!(f.service.cancel(&id));
    let result = f.service.wait(&id).await.unwrap();
    assert_eq!(result.outcome, JobOutcome::Canceled);
    assert_all_dead(&tree).await;
    assert!(!f.service.cancel(&id), "finished jobs cannot be canceled again");
    // Lifecycle events arrive in order.
    let mut kinds = Vec::new();
    while let Ok(event) = events.try_recv() {
        let kind = match event {
            JobEvent::Queued { .. } => "queued",
            JobEvent::Started { .. } => "started",
            JobEvent::Output { .. } => "output",
            JobEvent::CancelRequested { .. } => "cancel",
            JobEvent::Finished { .. } => "finished",
        };
        if kinds.last() != Some(&kind) {
            kinds.push(kind);
        }
    }
    assert_eq!(kinds, ["queued", "started", "output", "cancel", "finished"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn timeout_kills_the_whole_process_tree() {
    let f = fixture();
    let mut spec = f.spec(&["tree", "2", "120"]);
    spec.timeout_ms = Some(2_500);
    let id = f.service.launch(spec);
    let text = f.wait_stdout(&id, Duration::from_secs(20), |t| pids(t).len() == 3).await;
    let result = f.service.wait(&id).await.unwrap();
    assert_eq!(result.outcome, JobOutcome::TimedOut);
    assert!(result.duration_ms >= 2_400, "{result:?}");
    assert_all_dead(&pids(&text)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn descendants_never_outlive_the_primary_process() {
    let f = fixture();
    let id = f.service.launch(f.spec(&["orphan", "120"]));
    let result = f.service.wait(&id).await.unwrap();
    assert_eq!(result.outcome, JobOutcome::Exited { code: 0 });
    let orphan = pids(&f.stdout(&id));
    assert_eq!(orphan.len(), 1);
    assert_all_dead(&orphan).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_kills_running_jobs() {
    let f = fixture();
    let id = f.service.launch(f.spec(&["tree", "1", "120"]));
    let text = f.wait_stdout(&id, Duration::from_secs(20), |t| pids(t).len() == 2).await;
    f.service.shutdown();
    assert_all_dead(&pids(&text)).await;
    assert_eq!(f.service.result(&id).unwrap().outcome, JobOutcome::Canceled);
    let late = f.service.launch(f.spec(&["exit", "0"]));
    assert_eq!(refusal(&f, &late), RefusalCode::ShuttingDown);
}

#[tokio::test(flavor = "multi_thread")]
async fn logs_are_bounded_and_paged() {
    let f = fixture();
    let mut spec = f.spec(&["flood", "2000000"]);
    spec.log_limit_bytes = Some(64 * 1024);
    let id = f.service.launch(spec);
    let result = f.service.wait(&id).await.unwrap();
    assert_eq!(result.outcome, JobOutcome::Exited { code: 0 });
    assert!(result.output_truncated);
    let retained = f.stdout(&id);
    assert!(retained.len() <= 64 * 1024, "retained {} bytes", retained.len());
    assert!(retained.len() > 32 * 1024, "keeps the newest output: {}", retained.len());
    // Paging walks the retained chunks in order without gaps or duplicates.
    let mut after = None;
    let mut seen = 0usize;
    let mut last_seq = 0;
    loop {
        let page = f.service.output(&id, after, 8 * 1024).unwrap();
        assert!(page.complete && page.truncated);
        for chunk in &page.chunks {
            assert!(chunk.seq > last_seq);
            last_seq = chunk.seq;
            seen += chunk.text.len();
        }
        after = page.chunks.last().map(|c| c.seq);
        if !page.has_more {
            break;
        }
    }
    assert_eq!(seen, retained.len());
}

#[tokio::test(flavor = "multi_thread")]
async fn output_events_are_rate_limited() {
    let f = fixture();
    let mut events = f.service.subscribe();
    let id = f.service.launch(f.spec(&["flood", "5000000"]));
    let result = f.service.wait(&id).await.unwrap();
    let mut outputs = 0u64;
    while let Ok(event) = events.try_recv() {
        if matches!(event, JobEvent::Output { .. }) {
            outputs += 1;
        }
    }
    let seconds = (result.duration_ms as f64 / 1000.0).max(0.1);
    // Two streams flushed at most every 100 ms, plus the final flush.
    assert!((outputs as f64) <= 20.0 * seconds + 4.0, "{outputs} output events in {seconds}s");
}

#[tokio::test(flavor = "multi_thread")]
async fn secrets_are_redacted_from_logs_and_events() {
    let f = fixture();
    let mut events = f.service.subscribe();
    let lines = [
        format!("anthropic {FAKE_ANTHROPIC_KEY}"),
        format!("github {FAKE_GITHUB_TOKEN}"),
        "pat ghp_abcdefghijklmnopqrstuvwxyz0123456789".to_string(),
        "fine github_pat_11ABCDEFG0123456789_abcdefghijklmnop end".to_string(),
    ];
    let mut ids = Vec::new();
    for line in &lines {
        ids.push(f.service.launch(f.spec(&["print", line])));
        ids.push(f.service.launch(f.spec(&["eprint", line])));
    }
    // A secret in the arguments is also redacted from the job listing.
    for id in &ids {
        f.service.wait(id).await.unwrap();
        let summary = f.service.summary(id).unwrap();
        assert!(!summary.info.args.join(" ").contains("FAKEFAKE"));
        let all = format!("{}{}", f.stdout(id), f.service.output_text(id, OutputStream::Stderr).unwrap_or_default());
        assert!(all.contains("[REDACTED]"), "{all}");
        for secret in ["FAKEFAKE", FAKE_GITHUB_TOKEN, "ghp_abc", "github_pat_11"] {
            assert!(!all.contains(secret), "{secret} leaked: {all}");
        }
    }
    while let Ok(event) = events.try_recv() {
        if let JobEvent::Output { text, .. } = event {
            for secret in ["FAKEFAKE", FAKE_GITHUB_TOKEN, "ghp_abc", "github_pat_11"] {
                assert!(!text.contains(secret), "{secret} leaked in event: {text}");
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn trusted_native_requires_workspace_trust() {
    let f = fixture_with(ExecutionSettings::default(), false);
    let id = f.service.launch(f.spec(&["exit", "0"]));
    let result = f.service.wait(&id).await.unwrap();
    let JobOutcome::FailedToStart { reason } = result.outcome else { panic!("{result:?}") };
    assert!(reason.contains("not trusted"), "{reason}");
    let summary = f.service.summary(&id).unwrap();
    let refusal = summary.refusal.unwrap();
    assert_eq!(refusal.code, RefusalCode::UntrustedWorkspace);
    assert_eq!(refusal.workspace_root.as_deref(), Some(&*f.workspace.display().to_string()));
    assert!(summary.info.started_at_ms.is_none(), "nothing was started");

    // Application commands run without trust, but only in unbound app dirs.
    let probe = f.service.launch_application(spec_in(f.service.probe_dir(), &["exit", "0"]));
    assert_eq!(f.service.wait(&probe).await.unwrap().outcome, JobOutcome::Exited { code: 0 });
    let in_workspace = f.service.launch_application(f.spec(&["exit", "0"]));
    f.service.wait(&in_workspace).await.unwrap();
    assert_eq!(f.service.summary(&in_workspace).unwrap().refusal.unwrap().code, RefusalCode::ApplicationDirectoryOnly);
    // A batch job may not use an unbound app dir to dodge trust.
    let batch_in_probe = f.service.launch(spec_in(f.service.probe_dir(), &["exit", "0"]));
    f.service.wait(&batch_in_probe).await.unwrap();
    assert_eq!(
        f.service.summary(&batch_in_probe).unwrap().refusal.unwrap().code,
        RefusalCode::ApplicationDirectoryOnly
    );
    // A build dir bound to the workspace follows the workspace's trust.
    let build = f.tmp.path().join("build");
    std::fs::create_dir_all(&build).unwrap();
    f.service.register_app_dir(&build, Some(&f.workspace)).unwrap();
    let untrusted_build = f.service.launch(spec_in(&build, &["exit", "0"]));
    f.service.wait(&untrusted_build).await.unwrap();
    assert_eq!(f.service.summary(&untrusted_build).unwrap().refusal.unwrap().code, RefusalCode::UntrustedWorkspace);
    f.service.grant_trust(&f.workspace.display().to_string()).unwrap();
    let trusted_build = f.service.launch(spec_in(&build, &["exit", "0"]));
    assert_eq!(f.service.wait(&trusted_build).await.unwrap().outcome, JobOutcome::Exited { code: 0 });
    let trusted = f.service.launch(f.spec(&["exit", "0"]));
    assert_eq!(f.service.wait(&trusted).await.unwrap().outcome, JobOutcome::Exited { code: 0 });
    f.service.revoke_trust(&f.workspace.display().to_string()).unwrap();
    let revoked = f.service.launch(f.spec(&["exit", "0"]));
    f.service.wait(&revoked).await.unwrap();
    assert_eq!(f.service.summary(&revoked).unwrap().refusal.unwrap().code, RefusalCode::UntrustedWorkspace);
}

#[tokio::test(flavor = "multi_thread")]
async fn restricted_backend_unavailable_is_refused_without_native_fallback() {
    // (a) Docker missing (configured path does not exist).
    let mut settings = ExecutionSettings::default();
    settings.tool_paths.insert("docker".into(), r"C:\definitely\missing\docker.exe".into());
    settings.docker.image = Some("node:26-trixie-slim".into());
    let f = fixture_with(settings, false);
    let marker = f.workspace.join("marker.txt");
    let restricted = |f: &Fixture| {
        let mut spec = f.spec(&["print", "ran"]);
        // If this ever ran natively, testproc would print; a container would
        // resolve the program inside the image instead.
        spec.program = testproc();
        spec.policy = ExecutionPolicy::Restricted { backend: "docker".into() };
        spec
    };
    let id = f.service.launch(restricted(&f));
    let result = f.service.wait(&id).await.unwrap();
    let JobOutcome::FailedToStart { reason } = &result.outcome else { panic!("{result:?}") };
    assert!(reason.contains("not run natively"), "{reason}");
    assert_eq!(refusal(&f, &id), RefusalCode::RestrictedBackendUnavailable);
    assert!(f.stdout(&id).is_empty() && !marker.exists());

    // (b) Unknown backend.
    let mut spec = restricted(&f);
    spec.policy = ExecutionPolicy::Restricted { backend: "firejail".into() };
    let id = f.service.launch(spec);
    f.service.wait(&id).await.unwrap();
    assert_eq!(refusal(&f, &id), RefusalCode::UnsupportedBackend);

    // (c) Docker present but no image configured.
    let g = fixture_with(ExecutionSettings::default(), false);
    if matches!(g.service.locate_tool("docker"), exposition_execution::Located::Found { .. }) {
        let id = g.service.launch(restricted(&g));
        let result = g.service.wait(&id).await.unwrap();
        assert_eq!(refusal(&g, &id), RefusalCode::RestrictedBackendUnavailable, "{result:?}");
        // (d) Image not present locally (never pulled).
        g.service.update_settings(|s| s.docker.image = Some("exposition-test/never-present:0".into())).unwrap();
        let id = g.service.launch(restricted(&g));
        let result = g.service.wait(&id).await.unwrap();
        let JobOutcome::FailedToStart { reason } = &result.outcome else { panic!("{result:?}") };
        assert!(reason.contains("not run natively"), "{reason}");
        assert!(g.stdout(&id).is_empty());
        eprintln!("docker present: verified missing-image refusal: {reason}");
    } else {
        eprintln!("docker not found: skipped the docker-present refusal cases");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrency_limit_queues_jobs_and_queued_jobs_cancel_cleanly() {
    let settings = ExecutionSettings { max_concurrent_jobs: Some(1), ..Default::default() };
    let f = fixture_with(settings, true);
    let first = f.service.launch(f.spec(&["sleep", "30"]));
    let second = f.service.launch(f.spec(&["exit", "0"]));
    f.wait_stdout(&first, Duration::from_secs(10), |_| f.service.summary(&first).unwrap().state == JobState::Running)
        .await;
    assert_eq!(f.service.summary(&second).unwrap().state, JobState::Queued);
    assert!(f.service.cancel(&second));
    let result = f.service.wait(&second).await.unwrap();
    assert_eq!(result.outcome, JobOutcome::Canceled);
    assert!(f.service.summary(&second).unwrap().info.started_at_ms.is_none());
    let third = f.service.launch(f.spec(&["exit", "0"]));
    assert!(f.service.cancel(&first));
    assert_eq!(f.service.wait(&third).await.unwrap().outcome, JobOutcome::Exited { code: 0 });
    let listed: Vec<_> = f.service.list().into_iter().map(|s| s.info.id).collect();
    assert_eq!(listed, [first, second, third]);
}

#[tokio::test(flavor = "multi_thread")]
async fn interactive_jobs_round_trip_lines_and_cancel_kills() {
    let f = fixture();
    let mut job = f
        .service
        .launch_interactive(f.spec(&["echo-lines"]), InteractiveOptions::default())
        .await
        .expect("interactive job starts");
    let pid = job.pid.unwrap();
    for i in 0..50 {
        let message = format!("{{\"id\":{i},\"secret\":\"{FAKE_GITHUB_TOKEN}\"}}");
        job.input.write_line(&message).await.unwrap();
        let line = tokio::time::timeout(Duration::from_secs(10), job.output.next_line()).await.unwrap().unwrap();
        // The owner receives the exact protocol bytes (not redacted in place).
        assert_eq!(line, format!("echo: {message}"));
    }
    // The job log records only a summary of the protocol stream.
    let log = f.stdout(&job.id);
    assert!(log.contains("protocol stream") && !log.contains(FAKE_GITHUB_TOKEN), "{log}");
    assert_eq!(f.service.summary(&job.id).unwrap().state, JobState::Running);
    assert!(f.service.cancel(&job.id));
    assert_eq!(f.service.wait(&job.id).await.unwrap().outcome, JobOutcome::Canceled);
    assert_all_dead(&[pid]).await;
    assert!(job.output.next_line().await.is_none(), "stdout closes after cancel");
}

#[tokio::test(flavor = "multi_thread")]
async fn interactive_eof_and_redacted_copy() {
    let f = fixture();
    let options = InteractiveOptions { stdout_log: ProtocolLog::RedactedCopy };
    let mut job = f.service.launch_interactive(f.spec(&["echo-lines"]), options).await.unwrap();
    job.input.write_line(&format!("token {FAKE_GITHUB_TOKEN}")).await.unwrap();
    let line = job.output.next_line().await.unwrap();
    assert!(line.contains(FAKE_GITHUB_TOKEN));
    drop(job.input);
    assert_eq!(f.service.wait(&job.id).await.unwrap().outcome, JobOutcome::Exited { code: 0 });
    let log = f.stdout(&job.id);
    assert!(log.contains("echo: token [REDACTED]"), "{log}");

    // Refusals are synchronous for interactive jobs.
    let mut bad = f.spec(&["echo-lines"]);
    bad.working_directory = f.tmp.path().display().to_string();
    let refused = f.service.launch_interactive(bad, InteractiveOptions::default()).await;
    assert!(matches!(refused, Err(r) if matches!(r.outcome, JobOutcome::FailedToStart { .. })));
}
