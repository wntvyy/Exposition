//! R-a1 (independent review of A1): an agent's write into the project's
//! Python environment runs natively through environment discovery.
//!
//! Discovery probes a trusted project's `.venv` interpreter as
//! `<.venv python> -I -c <probe>`; `-I` does not skip `site`, so every
//! `site-packages/*.pth` line that starts with `import` runs. An agent that
//! may only change files (Auto: Files edit, Code off) can create such a file;
//! the next discovery runs it with the user's privileges — including the one
//! the agent's own `install_packages` call triggers to PLAN the setup, before
//! its approval card is shown.
//!
//! Tests marked `#[ignore = "R-a1 F<n>: ..."]` demonstrate a CURRENT defect.
//! Run: `cargo test -p exposition-agents --test review_a1_venv -- --include-ignored --nocapture`.
//! Needs a host Python (`py -3.12`, `py -3`, or `python`); no network.
#![cfg(debug_assertions)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use exposition_agents::credentials::MemoryStore;
use exposition_agents::permissions::{CodeAccess, FilesAccess, PermissionChoice};
use exposition_agents::tools::execution::AppToolServices;
use exposition_agents::*;
use exposition_execution::{ExecutionConfig, ExecutionService, ExecutionSettings};
use exposition_research::launcher::ExecutionLauncher;
use exposition_research::{ResearchConfig, ResearchService};
use exposition_workspace::{DocumentService, WorkspaceService};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::broadcast;

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

struct Harness {
    service: AgentService,
    execution: ExecutionService,
    research: Arc<ResearchService>,
    project: TempDir,
    events: broadcast::Receiver<AgentEvent>,
    _data: TempDir,
}

fn harness(project: TempDir) -> Harness {
    let data = tempfile::tempdir().unwrap();
    let documents = Arc::new(DocumentService::new());
    let workspace = WorkspaceService::new(Arc::clone(&documents), data.path().join("workspace"));
    workspace.open_workspace(project.path()).unwrap();
    let graph =
        exposition_graph::GraphService::new(exposition_graph::GraphConfig { data_dir: data.path().join("graph") });
    graph.attach(&std::fs::canonicalize(project.path()).unwrap());
    let mut config = ExecutionConfig::new(data.path());
    config.settings = Some(ExecutionSettings::default());
    config.too_broad_to_trust = Some(Vec::new());
    let execution = ExecutionService::with_config(config).unwrap();
    execution.register_workspace_root(project.path()).unwrap();
    let research = Arc::new(ResearchService::new(ResearchConfig::new(
        data.path().join("research"),
        Arc::new(ExecutionLauncher::new(execution.clone())),
        Arc::clone(&documents),
    )));
    let services = AppToolServices::new(execution.clone())
        .with_workspace(Arc::clone(&workspace))
        .with_research(Arc::clone(&research));
    let service = AgentService::new(AgentConfig {
        database: None,
        documents: Arc::clone(&documents),
        workspace: Some(workspace),
        credentials: Arc::new(MemoryStore::default()),
        services: Arc::new(services),
        runtime: tokio::runtime::Handle::current(),
        allow_scripted: true,
        anthropic_base_url: None,
        retry: None,
        graph: Some(graph),
    })
    .unwrap();
    service
        .update_settings(ProviderSettings { provider: ProviderKind::Scripted, ..ProviderSettings::default() })
        .unwrap();
    let events = service.subscribe();
    Harness { service, execution, research, project, events, _data: data }
}

/// Run a task; every approval is declined. Returns (tool results, cards).
async fn run(h: &mut Harness, script: Value, mode: ApprovalMode) -> (Vec<(bool, Value)>, Vec<ApprovalRequest>) {
    h.service.load_script(script).unwrap();
    let task = h
        .service
        .create_task(CreateTaskRequest {
            prompt: "Prepare the analysis environment".into(),
            workspace_root: h.project.path().display().to_string(),
            scope: TaskScope::Workspace,
            permission: PermissionProfile::AutoApply,
            execution: ExecutionMode::ApproveEach,
            consent: Some(ContextConsent::ScopeContent),
            budgets: None,
            approval_mode: Some(mode),
        })
        .unwrap();
    let mut cards = Vec::new();
    tokio::time::timeout(Duration::from_secs(300), async {
        loop {
            match h.events.recv().await {
                Ok(AgentEvent::ApprovalRequested { approval }) if approval.task_id == task.id => {
                    h.service.resolve_approval(&task.id, &approval.id, false).unwrap();
                    cards.push(approval);
                }
                Ok(AgentEvent::TaskChanged { task: t }) if t.id == task.id && !t.status.is_active() => break,
                _ => {}
            }
        }
    })
    .await
    .expect("task finished");
    h.service.join(&task.id).await;
    let results = h
        .service
        .task_detail(&task.id)
        .unwrap()
        .transcript
        .into_iter()
        .filter_map(|item| match item {
            TranscriptItem::ToolCall { is_error, result, .. } => {
                Some((is_error, result.and_then(|r| serde_json::from_str(&r).ok()).unwrap_or(Value::Null)))
            }
            _ => None,
        })
        .collect();
    (results, cards)
}

/// R-a1 F4. Trusted folder with an existing `.venv`; Auto with Files: edit,
/// Code: off (no native code, no setup). The agent creates
/// `.venv/Lib/site-packages/review_a1.pth` and then asks to install a
/// package (the card is declined). Expected: no agent-written code runs —
/// neither during the planning discovery before the card nor at the user's
/// next discovery (Research page open / Check again).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "R-a1 F4: an agent-written .pth in the project .venv runs natively on the next environment discovery"]
async fn review_a1_f4_agent_pth_runs_on_environment_discovery() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("analysis.py"), "print(1)\n").unwrap();
    if !make_venv(project.path()) {
        eprintln!("SKIPPED: no host Python could create a virtual environment");
        return;
    }
    let mut h = harness(project);
    let root = h.project.path().display().to_string();
    h.execution.grant_trust(&root).unwrap();
    h.service
        .set_project_permissions(
            &root,
            PermissionChoice { files: FilesAccess::Edit, code: CodeAccess::Off, setup: false },
        )
        .unwrap();
    let site = site_packages(h.project.path()).expect("the venv has site-packages");
    let relative = site.strip_prefix(h.project.path()).unwrap().display().to_string().replace('\\', "/");
    let outside = tempfile::tempdir().unwrap();
    let marker = outside.path().join("pth-ran.txt");
    let pth = format!(
        "import os; open({:?}, 'a').write('ran pid %d\\n' % os.getpid())\n",
        marker.display().to_string().replace('\\', "/")
    );
    let script = json!({"turns": [
        {"steps": [{"toolUse": {"id": "c", "name": "create_file", "input": {"path": format!("{relative}/review_a1.pth"), "content": pth}}}]},
        {"steps": [{"toolUse": {"id": "i", "name": "install_packages", "input": {"packages": ["tomli"]}}}]},
        {"steps": [{"text": "done"}]}
    ]});
    let (out, cards) = run(&mut h, script, ApprovalMode::Auto).await;
    eprintln!("create_file: error={} {}", out[0].0, out[0].1);
    eprintln!(
        "install_packages: error={} {}",
        out.get(1).map(|o| o.0).unwrap_or_default(),
        out.get(1).map(|o| o.1.to_string()).unwrap_or_default()
    );
    eprintln!("cards: {:?}", cards.iter().map(|c| c.title.clone()).collect::<Vec<_>>());
    let during_planning = marker.exists();
    eprintln!("agent .pth ran during the setup planning (before/while the card was shown): {during_planning}");
    if during_planning {
        eprintln!("marker: {}", std::fs::read_to_string(&marker).unwrap_or_default().trim());
    }
    let _ = std::fs::remove_file(&marker);
    // The user opens the Research page (discovery with refresh).
    let report = h.research.environments(true).await;
    let after_refresh = marker.exists();
    eprintln!("agent .pth ran at the user's next discovery: {after_refresh}");
    if let Ok(report) = &report {
        let venv: Vec<_> = report
            .runtimes
            .iter()
            .filter(|r| r.project_folder.is_some())
            .map(|r| (r.display_name.clone(), r.availability.clone()))
            .collect();
        eprintln!("project environments after refresh: {venv:?}");
    }
    assert!(
        !during_planning && !after_refresh,
        "DEFECT: agent-written .pth ran natively (planning: {during_planning}, refresh: {after_refresh})"
    );
}

/// The 8.3 short name Windows gave `<dir>/<name>`, if any (`dir /x`).
fn short_name(dir: &Path, name: &str) -> Option<String> {
    let out = std::process::Command::new("cmd").args(["/c", "dir", "/x", "/a"]).arg(dir).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.lines().find_map(|line| {
        let parts: Vec<&str> = line.split_whitespace().collect();
        (parts.last() == Some(&name) && parts.len() >= 2)
            .then(|| parts[parts.len() - 2].to_owned())
            .filter(|s| s.contains('~'))
    })
}

fn tool(id: &str, name: &str, input: Value) -> Value {
    json!({"steps": [{"toolUse": {"id": id, "name": name, "input": input}}]})
}

/// Routes around the protected project environments (R-a1 F4 fix, main
/// 18dd709): a case variant, a junction inside the project that resolves into
/// `.venv`, a trailing-dot spelling, the 8.3 short name, an edit and a delete
/// of an existing file in `.venv`, and copies of a prepared tree into `.venv`
/// (by name and through the junction). Every change is refused and nothing an
/// agent wrote runs at the next discovery. Auto with Files: edit (the default
/// profile), Code: off.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn review_a1_held_routes_around_protected_environments_are_refused() {
    let project = tempfile::tempdir().unwrap();
    if !make_venv(project.path()) {
        eprintln!("SKIPPED: no host Python could create a virtual environment");
        return;
    }
    let site = site_packages(project.path()).expect("the venv has site-packages");
    let relative = site.strip_prefix(project.path()).unwrap().display().to_string().replace('\\', "/");
    // A junction inside the project that resolves into the environment.
    let junction = project.path().join("libs");
    let made = std::process::Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(&junction)
        .arg(&site)
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(made, "control: junction created");
    let short = short_name(project.path(), ".venv");
    eprintln!("8.3 name of .venv: {short:?}");
    let mut h = harness(project);
    let root = h.project.path().display().to_string();
    h.execution.grant_trust(&root).unwrap();
    h.service
        .set_project_permissions(
            &root,
            PermissionChoice { files: FilesAccess::Edit, code: CodeAccess::Off, setup: false },
        )
        .unwrap();
    let outside = tempfile::tempdir().unwrap();
    let marker = outside.path().join("pth-ran.txt");
    let pth =
        format!("import os; open({:?}, \"a\").write(\"ran\\n\")\n", marker.display().to_string().replace('\\', "/"));
    let upper = relative.replacen(".venv", ".VENV", 1);
    let trailing = relative.replacen(".venv", ".venv.", 1);
    let mut turns = vec![
        tool("case", "create_file", json!({"path": format!("{upper}/a.pth"), "content": pth})),
        tool("junction", "create_file", json!({"path": "libs/b.pth", "content": pth})),
        tool("dot", "create_file", json!({"path": format!("{trailing}/c.pth"), "content": pth})),
        tool("read-cfg", "read_file", json!({"path": ".venv/pyvenv.cfg"})),
        tool(
            "edit-cfg",
            "edit_document",
            json!({"path": ".venv/pyvenv.cfg", "base_revision": "$ref:read-cfg/revision",
                   "edits": [{"old_text": "home", "new_text": "home "}]}),
        ),
        tool("stage", "create_file", json!({"path": "stage/Lib/site-packages/e.pth", "content": pth})),
        tool("copy", "copy_paths", json!({"paths": ["stage/Lib"], "destination": ".venv"})),
        tool("copy-junction", "copy_paths", json!({"paths": ["stage/Lib/site-packages/e.pth"], "destination": "libs"})),
        tool("delete", "delete_paths", json!({"paths": [".venv/pyvenv.cfg"]})),
    ];
    if let Some(short) = &short {
        let alias = relative.replacen(".venv", short, 1);
        turns.push(tool("short", "create_file", json!({"path": format!("{alias}/d.pth"), "content": pth})));
    }
    turns.push(json!({"steps": [{"text": "done"}]}));
    let ids: Vec<String> =
        turns.iter().filter_map(|t| t["steps"][0]["toolUse"]["id"].as_str().map(str::to_owned)).collect();
    let (out, cards) = run(&mut h, json!({ "turns": turns }), ApprovalMode::Auto).await;
    let mut not_refused = Vec::new();
    for (id, (is_error, result)) in ids.iter().zip(&out) {
        let shown: String = result.to_string().chars().take(160).collect();
        eprintln!("{id}: error={is_error} code={} {shown}", result["code"]);
        let control = id == "stage" || id == "read-cfg";
        if !control && !is_error {
            not_refused.push(id.clone());
        }
    }
    eprintln!("cards: {:?}", cards.iter().map(|c| c.title.clone()).collect::<Vec<_>>());
    let names = ["a.pth", "b.pth", "c.pth", "d.pth", "e.pth"];
    let pth_files: Vec<String> = std::fs::read_dir(&site)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| names.contains(&n.as_str()))
        .collect();
    let cfg_intact = h.project.path().join(".venv").join("pyvenv.cfg").is_file();
    let _ = h.research.environments(true).await;
    let ran = marker.exists();
    eprintln!("agent .pth files in site-packages: {pth_files:?}; pyvenv.cfg present: {cfg_intact}; ran: {ran}");
    assert!(not_refused.is_empty(), "changes not refused: {not_refused:?}");
    assert!(pth_files.is_empty() && cfg_intact && !ran, "an agent change reached the environment");
}

/// R-a1 F7 (planning order; LOW once agents cannot write into environments).
/// `install_packages` is planned before the gate decides; planning runs
/// environment discovery, which runs the project's interpreter natively
/// (here a `.pth` the user's own environment contains stands for that
/// environment's startup code). Under Auto with Code: off and Setup: off the
/// call needs an approval, yet the interpreter has run before the card is
/// shown. Expected: nothing runs natively for a call the gate has not allowed
/// (plan from cached discovery, or plan after the gate).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "R-a1 F7: install_packages planning runs the project interpreter (discovery) before the approval"]
async fn review_a1_f7_setup_planning_runs_the_project_interpreter_before_approval() {
    let project = tempfile::tempdir().unwrap();
    if !make_venv(project.path()) {
        eprintln!("SKIPPED: no host Python could create a virtual environment");
        return;
    }
    let outside = tempfile::tempdir().unwrap();
    let marker = outside.path().join("interpreter-ran.txt");
    let site = site_packages(project.path()).expect("the venv has site-packages");
    std::fs::write(
        site.join("users_own_startup.pth"),
        format!("import os; open({:?}, \"a\").write(\"ran\\n\")\n", marker.display().to_string().replace('\\', "/")),
    )
    .unwrap();
    let mut h = harness(project);
    let root = h.project.path().display().to_string();
    h.execution.grant_trust(&root).unwrap();
    h.service
        .set_project_permissions(
            &root,
            PermissionChoice { files: FilesAccess::Edit, code: CodeAccess::Off, setup: false },
        )
        .unwrap();
    let script = json!({"turns": [
        tool("i", "install_packages", json!({"packages": ["tomli"]})),
        {"steps": [{"text": "done"}]}
    ]});
    let (out, cards) = run(&mut h, script, ApprovalMode::Auto).await;
    let titles: Vec<String> = cards.iter().map(|c| c.title.clone()).collect();
    eprintln!("install_packages: error={} cards={titles:?}", out[0].0);
    let ran = marker.exists();
    eprintln!("project interpreter ran before/while the card was shown (declined): {ran}");
    assert!(!ran, "DEFECT: the project interpreter ran natively for a call that was never approved");
}

/// R-a1 F8 (the same class as F4, outside the protected folders). R reads
/// `.Rprofile` from its working directory at startup; the IRkernel kernelspec
/// (`R --slave -e IRkernel::main()`) has no `--vanilla`/`--no-init-file`, and
/// the user's R research session starts in the project folder. An agent that
/// may only change files creates `.Rprofile`; the user's next R session runs
/// it natively before any code the user chose. Expected: agents cannot plant
/// interpreter startup files the app's sessions load (or sessions start
/// without them).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "R-a1 F8: an agent-written .Rprofile runs natively when the user's R session starts"]
async fn review_a1_f8_agent_rprofile_runs_when_the_user_starts_an_r_session() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("analysis.R"), "x <- 1\n").unwrap();
    let mut h = harness(project);
    let root = h.project.path().display().to_string();
    h.execution.grant_trust(&root).unwrap();
    h.service
        .set_project_permissions(
            &root,
            PermissionChoice { files: FilesAccess::Edit, code: CodeAccess::Off, setup: false },
        )
        .unwrap();
    let outside = tempfile::tempdir().unwrap();
    let marker = outside.path().join("rprofile-ran.txt");
    let rprofile = format!("writeLines(\"ran\", {:?})\n", marker.display().to_string().replace('\\', "/"));
    let script = json!({"turns": [
        tool("c", "create_file", json!({"path": ".Rprofile", "content": rprofile})),
        {"steps": [{"text": "done"}]}
    ]});
    let (out, cards) = run(&mut h, script, ApprovalMode::Auto).await;
    eprintln!("create_file .Rprofile: error={} cards={}", out[0].0, cards.len());
    if out[0].0 {
        eprintln!("refused: {}", out[0].1);
        return;
    }
    // The Research page follows the open project (as `research::sync_workspace` does in the app).
    h.research.set_workspace(&std::fs::canonicalize(h.project.path()).unwrap(), false).unwrap();
    let report = h.research.environments(true).await.unwrap();
    let Some(r) = report
        .runtimes
        .iter()
        .find(|r| {
            r.language == exposition_research::types::RuntimeLanguage::R
                && r.availability == exposition_research::types::Availability::Available
        })
        .cloned()
    else {
        eprintln!("SKIPPED: no R environment with a registered IRkernel was discovered");
        return;
    };
    assert!(!marker.exists(), "control: discovery (R --vanilla) did not read .Rprofile");
    // The user starts an R session (Research page / Run on an R file).
    let session = h
        .research
        .start_session(exposition_research::StartSessionRequest {
            environment_id: r.id.clone(),
            owner: exposition_research::types::SessionOwner::User,
        })
        .await;
    let deadline = std::time::Instant::now() + Duration::from_secs(90);
    while !marker.exists() && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let ran = marker.exists();
    eprintln!(
        "session: {:?}; agent .Rprofile ran at the user's R session start: {ran}",
        session.as_ref().map(|s| &s.state)
    );
    if let Ok(session) = session {
        let _ = h.research.shutdown(&session.id).await;
    }
    assert!(!ran, "DEFECT: the agent's .Rprofile ran natively when the user's R session started");
}
