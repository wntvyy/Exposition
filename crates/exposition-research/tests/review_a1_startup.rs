//! R-a1 (independent review of A1): interpreter start-up files in the
//! project folder and the user's Python research session (kernel host +
//! ipykernel, working directory = the project). Does anything in the project
//! root run when the session starts, before any code the user chose?
//!
//! Method (deterministic): session 1 reports every third-party top-level
//! module loaded once the kernel is ready; the project then gets a shadow
//! file for each of those names (a module imported only AFTER IPython puts
//! the working directory on `sys.path` would load the shadow), plus
//! `sitecustomize.py`, `usercustomize.py`, and a `.pth` file. Session 2 starts
//! and runs `x = 1`; every shadow writes a marker if it is imported.
//!
//! Run: `cargo test -p exposition-research --test review_a1_startup -- --nocapture`.
//! Needs a discovered Python with ipykernel/jupyter_client; prints SKIPPED otherwise.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use exposition_execution::{ExecutionConfig, ExecutionService};
use exposition_research::launcher::ExecutionLauncher;
use exposition_research::types::{
    Availability, ExecuteRequest, OutputItem, ResearchEvent, RunSource, RuntimeLanguage, SessionOwner, StreamName,
};
use exposition_research::{ResearchConfig, ResearchService, StartSessionRequest};
use exposition_workspace::DocumentService;

async fn run_code(service: &ResearchService, session: &str, code: &str) -> (String, String) {
    let mut events = service.subscribe();
    let run = service
        .execute(ExecuteRequest {
            session_id: session.to_owned(),
            source: RunSource::Input { code: code.to_owned() },
            requester: SessionOwner::User,
        })
        .unwrap();
    let id = run.id.0.clone();
    let finished = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            if let Ok(ResearchEvent::RunFinished { run }) = events.recv().await
                && run.id.0 == id
            {
                return run;
            }
        }
    })
    .await
    .expect("run finished");
    let mut stdout = String::new();
    if let Ok(page) = service.run_output(&id, None, 500) {
        for entry in page.entries {
            if let OutputItem::Stream { name: StreamName::Stdout, text } = entry.item {
                stdout.push_str(&text);
            }
        }
    }
    (format!("{:?}", finished.status), stdout)
}

const LIST_MODULES: &str = r#"
import sys, json
names = sorted({m.split('.')[0] for m, mod in list(sys.modules.items())
                if getattr(mod, '__file__', None) and 'site-packages' in (mod.__file__ or '').replace('\\', '/')})
cwd_index = sys.path.index('') if '' in sys.path else -1
print('MODS' + json.dumps({'names': names, 'cwd_index': cwd_index, 'path_head': sys.path[:4]}))
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn review_a1_held_project_files_do_not_run_at_python_session_start() {
    let dir = tempfile::tempdir().unwrap();
    let outer = std::fs::canonicalize(dir.path()).unwrap();
    let root = outer.join("project");
    let markers = outer.join("markers");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&markers).unwrap();
    let data = outer.join("appdata");
    let execution = ExecutionService::with_config(ExecutionConfig::new(&data)).unwrap();
    let service = ResearchService::new(ResearchConfig::new(
        data,
        Arc::new(ExecutionLauncher::new(execution.clone())),
        Arc::new(DocumentService::new()),
    ));
    execution.grant_trust(&root.display().to_string()).unwrap();
    service.set_workspace(&root, false).unwrap();
    let report = service.environments(true).await.unwrap();
    let Some(env) = report
        .runtimes
        .iter()
        .find(|r| {
            r.language == RuntimeLanguage::Python
                && r.availability == Availability::Available
                && r.project_folder.is_none()
        })
        .cloned()
    else {
        eprintln!("SKIPPED: no Python with ipykernel/jupyter_client was discovered");
        return;
    };
    eprintln!("environment: {}", env.display_name);
    let start =
        || service.start_session(StartSessionRequest { environment_id: env.id.clone(), owner: SessionOwner::User });

    // Session 1: which third-party modules does a ready kernel have loaded?
    let first = start().await.unwrap();
    let (status, out) = run_code(&service, &first.id, LIST_MODULES).await;
    service.shutdown(&first.id).await.unwrap();
    let line = out.lines().find_map(|l| l.strip_prefix("MODS")).unwrap_or_else(|| panic!("{status}: {out}"));
    let info: serde_json::Value = serde_json::from_str(line).unwrap();
    let names: Vec<String> =
        info["names"].as_array().unwrap().iter().filter_map(|v| v.as_str().map(str::to_owned)).collect();
    eprintln!(
        "cwd on sys.path at index {} (head {}); {} third-party modules",
        info["cwd_index"],
        info["path_head"],
        names.len()
    );

    // Shadows for each, plus start-up files.
    let write_marker = |name: &str| {
        let marker = markers.join(format!("{name}.txt")).display().to_string().replace('\\', "/");
        format!("open({marker:?}, 'w').write('ran')\n")
    };
    let mut planted = Vec::new();
    for name in names.iter().chain(["sitecustomize".to_owned(), "usercustomize".to_owned()].iter()) {
        if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            std::fs::write(root.join(format!("{name}.py")), write_marker(name)).unwrap();
            planted.push(name.clone());
        }
    }
    std::fs::write(root.join("review_a1.pth"), format!("import os; {}", write_marker("pth"))).unwrap();

    // Session 2: start and run trivial code.
    let second = start().await.unwrap();
    let (status, _) = run_code(&service, &second.id, "x = 1").await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    service.shutdown(&second.id).await.unwrap();
    let ran: Vec<String> = std::fs::read_dir(&markers)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().trim_end_matches(".txt").to_owned())
        .collect();
    eprintln!("planted {} shadows + .pth; session 2 run: {status}; ran: {ran:?}", planted.len());
    drop(execution);
    assert!(ran.is_empty(), "project files ran at session start: {ran:?}");
    let _ = Path::new("");
}
