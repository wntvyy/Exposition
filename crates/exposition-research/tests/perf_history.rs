//! Scale measurements of research history, the explorer listing, environment
//! discovery, and run retention (run on purpose, never in a validation plan):
//!
//! `cargo test -p exposition-research -p exposition --test perf_history -- --ignored --nocapture --test-threads=1`
//!
//! History and listing use recorded manifests on disk (no kernel). Discovery
//! and retention use the real execution service; retention needs a Python
//! with ipykernel and prints `SKIPPED` without one. Each test prints
//! `PERF {...}` lines. Scale: `EXPOSITION_PERF_SCALE` = small | large | stress.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use exposition_core::artifact::{
    ArtifactId, ArtifactKind, ArtifactRecord, Producer, RunId, RunRecord, RunStatus, ToolVersion,
};
use exposition_core::job::JobSpec;
use exposition_core::path::WorkspacePath;
use exposition_execution::{ExecutionConfig, ExecutionService};
use exposition_graph::{GraphConfig, GraphService};
use exposition_research::helper::sha256_hex;
use exposition_research::launcher::{ExecutionLauncher, KernelProcessLauncher, LaunchError, LaunchFuture, ProbeOutput};
use exposition_research::project::types::{RetainRequest, RetainState};
use exposition_research::store::{write_manifest, write_transcript};
use exposition_research::types::{
    Availability, CodeOrigin, CodeProvenance, ExecuteRequest, InterpreterProvenance, KernelDescriptor, OutputEntry,
    OutputItem, OutputSummary, ResearchEvent, RunManifest, RunSource, RuntimeLanguage, SessionOwner,
    SessionProvenance, StreamName,
};
use exposition_research::{ResearchConfig, ResearchProject, ResearchService, StartSessionRequest};
use exposition_workspace::DocumentService;

fn scale() -> String {
    std::env::var("EXPOSITION_PERF_SCALE").unwrap_or_else(|_| "small".into())
}

fn pick<T: Copy>(small: T, large: T, stress: T) -> T {
    match scale().as_str() {
        "large" => large,
        "stress" => stress,
        _ => small,
    }
}

fn perf(bench: &str, scale: serde_json::Value, samples_ms: &[f64], counters: serde_json::Value) {
    println!(
        "PERF {}",
        serde_json::json!({"bench": bench, "scale": scale, "samples_ms": samples_ms, "counters": counters})
    );
}

fn ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1000.0
}

fn sample<T>(n: usize, mut f: impl FnMut() -> T) -> Vec<f64> {
    f();
    (0..n)
        .map(|_| {
            let t = Instant::now();
            f();
            ms(t)
        })
        .collect()
}

struct NoLauncher;

impl KernelProcessLauncher for NoLauncher {
    fn launch_interactive(&self, _spec: JobSpec) -> LaunchFuture<'_> {
        Box::pin(async { Err(LaunchError::Refused("no kernels here".into())) })
    }
    fn run_probe(&self, _spec: &JobSpec) -> Result<ProbeOutput, LaunchError> {
        Err(LaunchError::Refused("no probes here".into()))
    }
    fn run_project_probe(&self, _spec: &JobSpec) -> Result<ProbeOutput, LaunchError> {
        Err(LaunchError::Refused("no probes here".into()))
    }
}

fn path(p: &str) -> WorkspacePath {
    WorkspacePath::parse(p).unwrap()
}

const CSV: &str = "x,y\n1,2\n3,4\n";

/// Record a finished run: a manifest with `code_bytes` of code, one CSV
/// artifact, and a transcript of `transcript_bytes` of stream output.
fn record_run(root: &Path, run: &str, index: u64, code_bytes: usize, transcript_bytes: usize) {
    let dir = root.join(".exposition/artifacts").join(run);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("output-1.csv"), CSV).unwrap();
    let code = "value = compute(previous)  # a line of experiment code\n".repeat(code_bytes / 54 + 1);
    let chunk = "epoch 0001 loss=0.123456 accuracy=0.9876 elapsed=12.5s\n".repeat(64);
    let entries: Vec<OutputEntry> = (0..(transcript_bytes / chunk.len()).max(1))
        .map(|i| OutputEntry {
            seq: i as u32 + 1,
            item: OutputItem::Stream { name: StreamName::Stdout, text: chunk.clone() },
        })
        .collect();
    let manifest = RunManifest {
        format: "exposition.run/1".into(),
        record: RunRecord {
            id: RunId(run.into()),
            producer: Producer::KernelSession { session_id: "s".into(), runtime: "python".into() },
            inputs: Vec::new(),
            environment: vec![ToolVersion { name: "python".into(), version: "3.14.0".into() }],
            outputs: vec![ArtifactRecord {
                id: ArtifactId(format!("{run}/output-1.csv")),
                kind: ArtifactKind::Table,
                path: Some(path(&format!(".exposition/artifacts/{run}/output-1.csv"))),
                scratch_path: None,
                media_type: "text/csv".into(),
                size_bytes: CSV.len() as u64,
                content_hash: format!("sha256:{}", sha256_hex(CSV.as_bytes())),
            }],
            status: RunStatus::Succeeded,
            started_at_ms: 1_000 + index,
            finished_at_ms: Some(2_000 + index),
        },
        session: SessionProvenance {
            session_id: "s".into(),
            owner: SessionOwner::User,
            environment_id: "python:x".into(),
            generation: 1,
            interpreter: InterpreterProvenance {
                implementation: "CPython".into(),
                version: "3.14.0".into(),
                location: "~".into(),
            },
            kernel: KernelDescriptor::default(),
            helper_version: "1".into(),
            protocol: 1,
        },
        code: CodeProvenance {
            origin: CodeOrigin::Input,
            sha256: sha256_hex(code.as_bytes()),
            bytes: code.len() as u64,
            text: Some(code),
        },
        execution_count: Some(1),
        error: None,
        output: OutputSummary { entries: entries.len() as u32, ..Default::default() },
        unsaved_inputs: Vec::new(),
        execution: None,
        authorization: None,
    };
    write_manifest(root, &manifest).unwrap();
    write_transcript(root, run, &entries).unwrap();
}

fn run_id(i: usize) -> String {
    format!("run-{i:016x}")
}

fn dir_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|d| d.flatten().filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum())
        .unwrap_or(0)
}

/// Run history with 50 / 500 / 2,000 recorded runs: the cost of one listing,
/// the bytes it reads to build summaries, how many runs it can reach, and
/// the cost of reading one run's whole transcript by pages.
#[test]
#[ignore = "perf"]
fn run_history() {
    let code_bytes = pick(20_000usize, 200_000, 250_000);
    let transcript_bytes = pick(200_000usize, 2_000_000, 2_000_000);
    let sizes: &[usize] = match scale().as_str() {
        "small" => &[50, 500],
        _ => &[50, 500, 2000],
    };
    for &runs in sizes {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap().join("project");
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        for i in 0..runs {
            // Only the newest runs carry a full transcript (they are the ones read).
            record_run(&root, &run_id(i), i as u64, code_bytes, if i + 3 >= runs { transcript_bytes } else { 2_000 });
        }
        let service = ResearchService::new(ResearchConfig::new(
            dir.path().join("appdata"),
            Arc::new(NoLauncher),
            Arc::new(DocumentService::new()),
        ));
        service.set_workspace(&root, true).unwrap();
        let manifest_bytes = dir_bytes(&root.join(".exposition/runs"));
        let mut listed = 0;
        let samples = sample(5, || listed = service.runs().unwrap().len());
        let scale = serde_json::json!({"runs_on_disk": runs, "code_bytes": code_bytes, "transcript_bytes": transcript_bytes});
        perf(
            "research.history.list",
            scale.clone(),
            &samples,
            serde_json::json!({
                "listed": listed,
                "unreachable_runs": runs - listed,
                "manifest_bytes_on_disk": manifest_bytes,
                "mean_manifest_bytes": manifest_bytes / runs as u64,
            }),
        );
        // Selecting the newest run: every page of its stored transcript.
        let newest = run_id(runs - 1);
        let (mut pages, mut entries) = (0u32, 0usize);
        let select = sample(5, || {
            pages = 0;
            entries = 0;
            let mut after = None;
            loop {
                let page = service.run_output(&newest, after, 500).unwrap();
                pages += 1;
                entries += page.entries.len();
                let Some(last) = page.entries.last().map(|e| e.seq) else { break };
                if last >= page.last_seq {
                    break;
                }
                after = Some(last);
            }
        });
        let transcript = std::fs::metadata(root.join(".exposition/artifacts").join(&newest).join("transcript.json"))
            .map(|m| m.len())
            .unwrap_or(0);
        perf(
            "research.history.select_run",
            scale.clone(),
            &select,
            serde_json::json!({
                "pages": pages,
                "entries": entries,
                "transcript_file_bytes": transcript,
                "transcript_bytes_parsed": u64::from(pages) * transcript,
            }),
        );
        let manifest = sample(5, || service.run_manifest(&newest).unwrap());
        perf("research.history.manifest", scale, &manifest, serde_json::json!({}));
    }
}

/// The Research explorer listing with many files in the space and many
/// retained results (recomputed on graph changes).
#[test]
#[ignore = "perf"]
fn explorer_listing() {
    let files = pick(100usize, 500, 900);
    let results = pick(40usize, 200, 400);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("project");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("paper.typ"), "= Paper\n").unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let graph = GraphService::new(GraphConfig { data_dir: dir.path().join("data") });
    assert!(graph.attach(&root).is_usable());
    let documents = Arc::new(DocumentService::new());
    let research =
        Arc::new(ResearchService::new(ResearchConfig::new(dir.path().join("data"), Arc::new(NoLauncher), documents.clone())));
    research.set_workspace(&root, true).unwrap();
    let project = ResearchProject::new(graph, research, documents);
    let space = project.enable().unwrap();
    for (group, share) in [("sources", 2), ("data", 1), ("notes", 1)] {
        let folder = space.root.as_str().split('/').fold(root.clone(), |p, part| p.join(part)).join(group);
        std::fs::create_dir_all(&folder).unwrap();
        for i in 0..files * share / 4 {
            std::fs::write(folder.join(format!("file-{i:04}.py")), "x = 1\n").unwrap();
        }
    }
    for i in 0..results {
        let run = run_id(i);
        record_run(&root, &run, i as u64, 500, 500);
        project
            .retain(&RetainRequest {
                run_id: run.clone(),
                artifact_id: format!("{run}/output-1.csv"),
                state: RetainState::Active,
                experiment: None,
            })
            .unwrap();
    }
    // The first listings admit the files the graph does not know yet.
    let t = Instant::now();
    let mut admitting = 0;
    loop {
        let listing = project.listing().unwrap();
        admitting += 1;
        let known = listing.sources.iter().chain(&listing.data).chain(&listing.notes).all(|f| f.entity.is_some());
        if known || admitting > 20 {
            break;
        }
    }
    let admit_ms = ms(t);
    let mut counts = (0, 0, false);
    let samples = sample(10, || {
        let listing = project.listing().unwrap();
        counts = (
            listing.sources.len() + listing.data.len() + listing.notes.len(),
            listing.results.len(),
            listing.truncated,
        );
    });
    perf(
        "research.explorer.listing",
        serde_json::json!({"space_files": files, "retained_results": results}),
        &samples,
        serde_json::json!({
            "files_listed": counts.0,
            "results_listed": counts.1,
            "truncated": counts.2,
            "listings_to_admit_all": admitting,
            "admit_ms": admit_ms,
        }),
    );
}

struct Real {
    _dir: tempfile::TempDir,
    data: PathBuf,
    roots: Vec<PathBuf>,
    execution: ExecutionService,
    service: Arc<ResearchService>,
}

fn real(projects: usize) -> Real {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let data = base.join("appdata");
    let execution = ExecutionService::with_config(ExecutionConfig::new(&data)).unwrap();
    let mut config = ResearchConfig::new(
        data.clone(),
        Arc::new(ExecutionLauncher::new(execution.clone())),
        Arc::new(DocumentService::new()),
    );
    config.cancel_grace = Duration::from_secs(3);
    let service = Arc::new(ResearchService::new(config));
    let roots = (0..projects)
        .map(|i| {
            let root = base.join(format!("project-{i}"));
            std::fs::create_dir_all(&root).unwrap();
            let root = std::fs::canonicalize(&root).unwrap();
            execution.grant_trust(&root.display().to_string()).unwrap();
            root
        })
        .collect();
    Real { _dir: dir, data, roots, execution, service }
}

/// Environment discovery: the first one, one after each project switch,
/// a repeat for the same project, and two concurrent requests. Counts the
/// probe jobs each starts.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "perf"]
async fn discovery_after_project_switch() {
    let f = real(3);
    let jobs = |f: &Real| f.execution.list().len();
    f.service.set_workspace(&f.roots[0], true).unwrap();
    let before = jobs(&f);
    let t = Instant::now();
    let report = f.service.environments(false).await.unwrap();
    let first_ms = ms(t);
    let first_probes = jobs(&f) - before;
    let mut switch_ms = Vec::new();
    let mut switch_probes = Vec::new();
    for i in [1usize, 2, 0, 1, 2] {
        f.service.set_workspace(&f.roots[i], true).unwrap();
        let before = jobs(&f);
        let t = Instant::now();
        f.service.environments(false).await.unwrap();
        switch_ms.push(ms(t));
        switch_probes.push(jobs(&f) - before);
    }
    let before = jobs(&f);
    let t = Instant::now();
    f.service.environments(false).await.unwrap();
    let cached_ms = ms(t);
    let cached_probes = jobs(&f) - before;
    // Two surfaces asking at once after a switch (panel mount + start session).
    f.service.set_workspace(&f.roots[0], true).unwrap();
    let before = jobs(&f);
    let t = Instant::now();
    let (a, b) = tokio::join!(f.service.environments(false), f.service.environments(false));
    a.unwrap();
    b.unwrap();
    let concurrent_ms = ms(t);
    let concurrent_probes = jobs(&f) - before;
    perf(
        "research.discovery.project_switch",
        serde_json::json!({"runtimes_found": report.runtimes.len()}),
        &switch_ms,
        serde_json::json!({
            "first_discovery_ms": first_ms,
            "probe_jobs_first": first_probes,
            "probe_jobs_per_switch": switch_probes,
            "same_project_again_ms": cached_ms,
            "probe_jobs_same_project_again": cached_probes,
            "two_concurrent_requests_ms": concurrent_ms,
            "probe_jobs_two_concurrent_requests": concurrent_probes,
        }),
    );
}

fn entries(dir: &Path) -> usize {
    std::fs::read_dir(dir).map(|d| d.count()).unwrap_or(0)
}

/// Many runs in one session: what stays in memory after they finish and
/// after the project is switched, and what is left on disk and running
/// after the session is shut down.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "perf"]
async fn retained_run_states() {
    let runs = pick(40usize, 300, 1000);
    let chars_per_run = pick(100_000usize, 1_000_000, 2_000_000);
    let f = real(2);
    f.service.set_workspace(&f.roots[0], true).unwrap();
    let report = f.service.environments(true).await.unwrap();
    let Some(env) = report
        .runtimes
        .iter()
        .find(|r| r.language == RuntimeLanguage::Python && r.availability == Availability::Available)
        .map(|r| r.id.clone())
    else {
        println!("SKIPPED: no Python interpreter with ipykernel/jupyter_client was discovered");
        return;
    };
    let mut events = f.service.subscribe();
    let session = f
        .service
        .start_session(StartSessionRequest { environment_id: env, owner: SessionOwner::User })
        .await
        .unwrap();
    let code = format!("print('x' * 99 + '\\n', end='', flush=False)\nfor _ in range({}):\n    print('y' * 99)\n", chars_per_run / 100);
    let mut samples = Vec::new();
    for _ in 0..runs {
        let t = Instant::now();
        let run = f
            .service
            .execute(ExecuteRequest {
                session_id: session.id.clone(),
                source: RunSource::Input { code: code.clone() },
                requester: SessionOwner::User,
            })
            .unwrap();
        tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                match events.recv().await {
                    Ok(ResearchEvent::RunFinished { run: done }) if done.id == run.id => break,
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(e) => panic!("{e}"),
                }
            }
        })
        .await
        .expect("the run did not finish");
        samples.push(ms(t));
    }
    let after_runs = f.service.retained_runs();
    let t = Instant::now();
    let listed = f.service.runs().unwrap().len();
    let list_ms = ms(t);
    f.service.set_workspace(&f.roots[1], true).unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let after_switch = f.service.retained_runs();
    for s in f.service.sessions() {
        let _ = f.service.shutdown(&s.id).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    let active_jobs = f.execution.list().iter().filter(|j| j.info.result.is_none()).count();
    perf(
        "research.runs.retained",
        serde_json::json!({"runs": runs, "chars_per_run": chars_per_run}),
        &samples,
        serde_json::json!({
            "run_states_after_runs": after_runs.runs,
            "retained_output_chars_after_runs": after_runs.output_chars,
            "retained_code_bytes_after_runs": after_runs.code_bytes,
            "run_states_after_project_switch": after_switch.runs,
            "retained_output_chars_after_project_switch": after_switch.output_chars,
            "history_listed": listed,
            "history_list_ms": list_ms,
            "jobs_still_active_after_shutdown": active_jobs,
            "runtime_files_left": entries(&f.data.join("research").join("runtime")),
            "job_dirs_left": entries(&f.data.join("execution").join("jobs")),
        }),
    );
}
