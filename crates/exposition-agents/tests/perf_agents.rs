//! Deterministic agent benchmarks (performance assignment, AGENTS lane).
//!
//! Representative Exposition tasks run through the real task loop, document
//! service, and project graph with the development-only scripted provider
//! (no model, no network); process services are recorded, not launched. Every
//! task verifies its actual outcome: a faster wrong answer is a regression.
//!
//! Each measurement prints one `PERF {...}` line (see `scripts/perf/rust-bench.mjs`).
//! Timings are local orchestration time (the scripted provider streams with
//! no delay); counters are deterministic. Scale from `EXPOSITION_PERF_SCALE`.
//!
//!   cargo test -p exposition-agents -p exposition --test perf_agents -- --ignored --nocapture --test-threads=1
#![cfg(debug_assertions)]

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use exposition_agents::credentials::MemoryStore;
use exposition_agents::permissions::{CodeAccess, FilesAccess, PermissionChoice};
use exposition_agents::store::{Store, StoreStats};
use exposition_agents::tools::services::{
    BuildRequest, CommandRequest, LeanRequest, PythonRequest, ServiceError, ServiceKind, ToolServices,
};
use exposition_agents::*;
use exposition_agents::{attachments, prompt, tools};
use exposition_core::document::EditOrigin;
use exposition_core::artifact::{ArtifactId, ArtifactKind, ArtifactRecord};
use exposition_core::path::WorkspacePath;
use exposition_core::text::TextRange;
use exposition_graph::{
    ChangeRequest, EntityId, EntityKind, EntityRef, GraphConfig, GraphOperation, GraphService, GraphState, NewBinding,
    Origin, RelationKind, SourceSelector,
};
use exposition_workspace::DocumentService;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Scale and reporting
// ---------------------------------------------------------------------------

struct Sizes {
    name: &'static str,
    /// Bytes of the manuscript `paper.typ`.
    manuscript: usize,
    /// Extra graph entities (beyond the theorem and its lemmas).
    entities: usize,
    /// Stopped tasks with changed files, for the task list.
    tasks: usize,
    files_per_task: usize,
    changed_file: usize,
    /// Turns of the long-transcript task.
    long_turns: usize,
    stream_words: usize,
    followups: usize,
    attachments: usize,
    attachment_bytes: usize,
    samples: usize,
}

fn sizes() -> Sizes {
    match std::env::var("EXPOSITION_PERF_SCALE").as_deref() {
        Ok("large") => Sizes {
            name: "large",
            manuscript: 300 * 1024,
            entities: 150,
            tasks: 50,
            files_per_task: 5,
            changed_file: 200 * 1024,
            long_turns: 100,
            stream_words: 8000,
            followups: 5,
            attachments: 3,
            attachment_bytes: 900 * 1024,
            samples: 5,
        },
        Ok("stress") => Sizes {
            name: "stress",
            manuscript: 500 * 1024,
            entities: 300,
            tasks: 100,
            files_per_task: 5,
            changed_file: 400 * 1024,
            long_turns: 200,
            stream_words: 20000,
            followups: 8,
            attachments: 5,
            attachment_bytes: 1000 * 1024,
            samples: 5,
        },
        _ => Sizes {
            name: "small",
            manuscript: 64 * 1024,
            entities: 40,
            tasks: 10,
            files_per_task: 3,
            changed_file: 50 * 1024,
            long_turns: 24,
            stream_words: 2000,
            followups: 3,
            attachments: 2,
            attachment_bytes: 200 * 1024,
            samples: 5,
        },
    }
}

fn perf(bench: &str, scale: Value, samples_ms: &[f64], counters: Map<String, Value>) {
    let record = json!({"bench": bench, "scale": scale, "samples_ms": samples_ms, "counters": counters});
    println!("PERF {record}");
}

fn ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1000.0
}

fn counters(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect()
}

fn p(s: &str) -> WorkspacePath {
    WorkspacePath::parse(s).unwrap()
}

fn sha(text: &str) -> String {
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn write(root: &Path, path: &str, text: &str) {
    let full = root.join(path);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, text).unwrap();
}

// ---------------------------------------------------------------------------
// The project: a manuscript with a theorem and its lemmas, chapters, a Lean
// file, and a graph that knows where they are.
// ---------------------------------------------------------------------------

const THEOREM: &str = "Theorem (Spectral gap). Every connected expander in the family has a gap of at least one half.";
const LEMMA_A: &str = "Lemma (Mixing). The walk mixes in logarithmic time.";
const LEMMA_B: &str = "Lemma (Cheeger). The conductance bounds the gap from below.";
const LEAN: &str = "theorem spectral_gap : 1 = 1 := rfl\n";
const CHAPTERS: usize = 20;

fn filler(paragraph: usize) -> String {
    format!(
        "Paragraph {paragraph}: the estimate follows from the previous display by a routine computation with \
         the constants fixed in the introduction, and the remaining terms are absorbed into the error.\n\n"
    )
}

/// The manuscript: filler, then the lemma, more filler, the theorem in the
/// middle, and filler to `bytes`.
fn manuscript(bytes: usize) -> String {
    let mut text = String::from("= Expanders\n\n");
    let mut n = 0;
    while text.len() < bytes / 4 {
        text.push_str(&filler(n));
        n += 1;
    }
    text.push_str(LEMMA_A);
    text.push_str("\n\n");
    while text.len() < bytes / 2 {
        text.push_str(&filler(n));
        n += 1;
    }
    text.push_str(THEOREM);
    text.push_str("\n\n");
    while text.len() < bytes {
        text.push_str(&filler(n));
        n += 1;
    }
    text
}

fn chapter(i: usize) -> String {
    if i == 3 { format!("= Chapter {i}\n\n{LEMMA_B}\n\n{}", filler(i)) } else { format!("= Chapter {i}\n\n{}", filler(i)) }
}

struct Ids {
    theorem: EntityId,
    lemma_a: EntityId,
    lemma_b: EntityId,
    declaration: EntityId,
}

struct Project {
    _dir: TempDir,
    data: TempDir,
    root: PathBuf,
    graph: Arc<GraphService>,
    ids: Ids,
    paper: String,
}

fn project(sizes: &Sizes) -> Project {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let paper = manuscript(sizes.manuscript);
    write(&root, "paper.typ", &paper);
    for i in 0..CHAPTERS {
        write(&root, &format!("chapters/ch{i:02}.typ"), &chapter(i));
    }
    write(&root, "Main.lean", LEAN);
    write(&root, "analysis/fit.py", "print('fit')\n");
    let data = tempfile::tempdir().unwrap();
    let graph = GraphService::new(GraphConfig { data_dir: data.path().join("graph") });
    let status = graph.attach(&root);
    assert!(matches!(status.state, GraphState::Attached), "{status:?}");
    graph.ensure_resource(&p("paper.typ"), Some(&sha(&paper)), false, "bench").unwrap();
    for i in 0..CHAPTERS {
        graph.ensure_resource(&p(&format!("chapters/ch{i:02}.typ")), Some(&sha(&chapter(i))), false, "bench").unwrap();
    }
    graph.ensure_resource(&p("Main.lean"), Some(&sha(LEAN)), false, "bench").unwrap();
    let region = |needle: &str| {
        let start = paper.find(needle).unwrap() as u32;
        TextRange { start, end: start + needle.len() as u32 }
    };
    let create = |temp: String, kind, label: String, binding: Option<NewBinding>| GraphOperation::CreateEntity {
        temp_id: temp,
        kind,
        roles: vec![],
        label,
        key: None,
        attributes: BTreeMap::new(),
        binding,
    };
    let in_paper = |needle: &str| {
        Some(NewBinding { path: p("paper.typ"), selector: SourceSelector::Region, range: Some(region(needle)) })
    };
    let whole = |path: &str| Some(NewBinding { path: p(path), selector: SourceSelector::WholeFile, range: None });
    let temp = |t: &str| EntityRef::Temp { temp_id: t.into() };
    let depends = |a: &str, b: &str| GraphOperation::AddRelation {
        kind: RelationKind::DependsOn,
        from: temp(a),
        to: temp(b),
        evidence_kind: None,
        evidence: vec![],
        note: None,
    };
    let mut operations = vec![
        create("t".into(), EntityKind::Theorem, "Spectral gap theorem".into(), in_paper(THEOREM)),
        create("a".into(), EntityKind::Lemma, "Mixing lemma".into(), in_paper(LEMMA_A)),
        create("b".into(), EntityKind::Lemma, "Cheeger lemma".into(), whole("chapters/ch03.typ")),
        create("d".into(), EntityKind::Theorem, "spectral_gap declaration".into(), whole("Main.lean")),
        depends("t", "a"),
        depends("t", "b"),
    ];
    for i in 0..sizes.entities {
        let path = format!("chapters/ch{:02}.typ", i % CHAPTERS);
        operations.push(create(format!("e{i}"), EntityKind::Claim, format!("Auxiliary claim {i}"), whole(&path)));
    }
    let request = ChangeRequest { base_revision: graph.status().revision, operations, summary: None };
    let (_, outcome) = graph.propose_and_apply(Origin::User, &request).unwrap();
    let created = match outcome {
        exposition_graph::ApplyOutcome::Applied { created, .. } => created,
        other => panic!("fixture: {other:?}"),
    };
    let id = |t: &str| created.iter().find(|c| c.temp_id == t).unwrap().id.clone();
    let ids = Ids { theorem: id("t"), lemma_a: id("a"), lemma_b: id("b"), declaration: id("d") };
    Project { _dir: dir, data, root, graph, ids, paper }
}

/// Process services that record instead of launching, with results shaped
/// like the application's.
struct Recorder {
    calls: Mutex<Vec<&'static str>>,
}

#[async_trait]
impl ToolServices for Recorder {
    fn available(&self, _kind: ServiceKind) -> bool {
        true
    }
    fn restricted_backend(&self) -> Option<String> {
        Some("docker".into())
    }
    async fn build_document(&self, request: BuildRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        self.calls.lock().unwrap().push("build_document");
        Ok(json!({"status": "succeeded", "build": "build-1", "entry": request.entry.as_str(), "engine": "typst",
            "pages": 12, "durationMs": 1, "diagnostics": [], "diagnosticsOmitted": 0}))
    }
    async fn run_python(&self, _: PythonRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        self.calls.lock().unwrap().push("run_python");
        Ok(json!({"status": "succeeded", "run": "run-1", "session": "agent", "stdout": "fit done\n", "stderr": "",
            "results": [], "artifacts": [{"id": "fig", "kind": "figure", "mediaType": "image/png"}]}))
    }
    async fn check_lean(&self, request: LeanRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        self.calls.lock().unwrap().push("check_lean");
        Ok(json!({"path": request.path.as_str(), "verdict": "LEAN-VERDICT-MARK no errors", "diagnostics": []}))
    }
    async fn run_command(&self, _: CommandRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        self.calls.lock().unwrap().push("run_command");
        Ok(json!({"status": "exited", "exitCode": 0}))
    }
    fn research_artifact(&self, _run: &str, _artifact: &str) -> Result<ArtifactRecord, ServiceError> {
        Ok(ArtifactRecord {
            id: ArtifactId("fig".into()),
            kind: ArtifactKind::Figure,
            path: Some(p(".exposition/artifacts/run-1/fig.png")),
            scratch_path: None,
            media_type: "image/png".into(),
            size_bytes: 4,
            content_hash: "sha256:00".into(),
        })
    }
}

#[derive(Default, Clone)]
struct EventCounts {
    total: u64,
    text_deltas: u64,
    transcript_changed: u64,
    task_changed: u64,
    lagged: u64,
    text: String,
}

struct Bench {
    service: AgentService,
    documents: Arc<DocumentService>,
    recorder: Arc<Recorder>,
    events: broadcast::Receiver<AgentEvent>,
    counts: Arc<Mutex<EventCounts>>,
    database: PathBuf,
}

fn bench(project: &Project) -> Bench {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let database = project.data.path().join(format!("agents-{n}/agents.sqlite3"));
    let recorder = Arc::new(Recorder { calls: Mutex::new(Vec::new()) });
    let documents = Arc::new(DocumentService::new());
    let service = AgentService::new(AgentConfig {
        database: Some(database.clone()),
        documents: Arc::clone(&documents),
        workspace: None,
        credentials: Arc::new(MemoryStore::default()),
        services: recorder.clone(),
        runtime: tokio::runtime::Handle::current(),
        allow_scripted: true,
        anthropic_base_url: None,
        retry: None,
        graph: Some(Arc::clone(&project.graph)),
    })
    .unwrap();
    service
        .update_settings(ProviderSettings { provider: ProviderKind::Scripted, ..ProviderSettings::default() })
        .unwrap();
    service.set_graph_settle(Duration::from_millis(150));
    let events = service.subscribe();
    // Everything the webview would be sent, counted.
    let counts = Arc::new(Mutex::new(EventCounts::default()));
    let mut all = service.subscribe();
    let sink = Arc::clone(&counts);
    tokio::spawn(async move {
        loop {
            match all.recv().await {
                Ok(event) => {
                    let mut c = sink.lock().unwrap();
                    c.total += 1;
                    match event {
                        AgentEvent::TextDelta { text, .. } => {
                            c.text_deltas += 1;
                            c.text.push_str(&text);
                        }
                        AgentEvent::TranscriptChanged { .. } => c.transcript_changed += 1,
                        AgentEvent::TaskChanged { .. } => c.task_changed += 1,
                        _ => {}
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => sink.lock().unwrap().lagged += n,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    Bench { service, documents, recorder, events, counts, database }
}

fn request(root: &Path, permission: PermissionProfile) -> CreateTaskRequest {
    CreateTaskRequest {
        prompt: "bench task".into(),
        workspace_root: root.display().to_string(),
        scope: TaskScope::Workspace,
        permission,
        execution: ExecutionMode::ApproveEach,
        consent: Some(ContextConsent::ScopeContent),
        budgets: Some(Budgets {
            max_turns: 500,
            max_input_tokens: 200_000_000,
            max_tool_calls: 5_000,
            ..Budgets::default()
        }),
        approval_mode: None,
        provider: None,
        attachments: None,
    }
}

fn tool(id: &str, name: &str, input: Value) -> Value {
    json!({"toolUse": {"id": id, "name": name, "input": input}})
}

/// One tool call per turn, then a final text; streamed with no delay.
fn script(calls: Vec<Value>, last: &str) -> Value {
    let mut turns: Vec<Value> = calls.into_iter().map(|c| json!({"steps": [c], "chunkDelayMs": 0})).collect();
    turns.push(json!({"steps": [{"text": last}], "chunkDelayMs": 0}));
    json!({"turns": turns})
}

/// Wait for the task's run to end, approving every approval; the approvals seen.
async fn finish(h: &mut Bench, id: &TaskId) -> usize {
    let mut approvals = 0;
    tokio::time::timeout(Duration::from_secs(600), async {
        loop {
            match h.events.recv().await {
                Ok(AgentEvent::ApprovalRequested { approval }) if &approval.task_id == id => {
                    h.service.resolve_approval(id, &approval.id, true).unwrap();
                    approvals += 1;
                }
                Ok(AgentEvent::TaskChanged { task }) if &task.id == id && !task.status.is_active() => break,
                Ok(_) => {}
                // A lagged receiver may have missed the end: ask.
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    if !h.service.task(id).unwrap().status.is_active() {
                        break;
                    }
                }
                Err(e) => panic!("events closed: {e}"),
            }
        }
    })
    .await
    .expect("task finished");
    h.service.join(id).await;
    approvals
}

struct Ran {
    id: TaskId,
    summary: TaskSummary,
    ms: f64,
    approvals: usize,
    stats: StoreStats,
    events: EventCounts,
}

fn delta(before: StoreStats, after: StoreStats) -> StoreStats {
    StoreStats {
        ops: after.ops - before.ops,
        changes: after.changes - before.changes,
        review_text_bytes: after.review_text_bytes - before.review_text_bytes,
        transcript_bytes: after.transcript_bytes - before.transcript_bytes,
        tool_result_bytes: after.tool_result_bytes - before.tool_result_bytes,
    }
}

fn events_delta(before: &EventCounts, after: &EventCounts) -> EventCounts {
    EventCounts {
        total: after.total - before.total,
        text_deltas: after.text_deltas - before.text_deltas,
        transcript_changed: after.transcript_changed - before.transcript_changed,
        task_changed: after.task_changed - before.task_changed,
        lagged: after.lagged - before.lagged,
        text: after.text[before.text.len()..].to_owned(),
    }
}

async fn settle_events() {
    // The counting subscriber runs on the runtime: let it drain.
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
}

async fn run(h: &mut Bench, script: Value, req: CreateTaskRequest) -> Ran {
    settle_events().await;
    let stats = h.service.store_stats();
    let events = h.counts.lock().unwrap().clone();
    h.service.load_script(script).unwrap();
    let started = Instant::now();
    let task = h.service.create_task(req).unwrap();
    let approvals = finish(h, &task.id).await;
    let elapsed = ms(started);
    settle_events().await;
    let summary = h.service.task(&task.id).unwrap();
    Ran {
        id: task.id,
        summary,
        ms: elapsed,
        approvals,
        stats: delta(stats, h.service.store_stats()),
        events: events_delta(&events, &h.counts.lock().unwrap()),
    }
}

async fn follow_up(h: &mut Bench, id: &TaskId, prompt: &str) -> Ran {
    settle_events().await;
    let stats = h.service.store_stats();
    let events = h.counts.lock().unwrap().clone();
    let started = Instant::now();
    h.service.continue_task(id, prompt, None).unwrap();
    let approvals = finish(h, id).await;
    let elapsed = ms(started);
    settle_events().await;
    Ran {
        id: id.clone(),
        summary: h.service.task(id).unwrap(),
        ms: elapsed,
        approvals,
        stats: delta(stats, h.service.store_stats()),
        events: events_delta(&events, &h.counts.lock().unwrap()),
    }
}

/// Bytes of the system prompt and of the serialized tool definitions (what
/// every request carries before the conversation).
fn static_request_bytes() -> (usize, usize) {
    let tools: usize = tools::task_definitions(true)
        .iter()
        .map(|d| {
            json!({"name": d.name, "description": d.description, "input_schema": d.input_schema, "strict": d.strict})
                .to_string()
                .len()
        })
        .sum();
    (prompt::SYSTEM_PROMPT.len(), tools)
}

struct TaskCost {
    turns: usize,
    calls: Vec<(String, Value, usize, ToolCallStatus)>,
    /// Bytes of the conversation sent with each model request.
    history_per_turn: Vec<usize>,
}

fn cost(h: &Bench, id: &TaskId) -> TaskCost {
    let store = Store::open(&h.database).unwrap();
    let calls = store
        .tool_calls(id)
        .unwrap()
        .into_iter()
        .map(|c| (c.name, c.input, c.result.map_or(0, |r| r.len()), c.status))
        .collect();
    let mut history_per_turn = Vec::new();
    let mut sent = 0usize;
    for (role, content) in store.messages(id).unwrap() {
        if role == "assistant" {
            history_per_turn.push(sent);
        }
        sent += serde_json::to_string(&content).unwrap().len();
    }
    TaskCost { turns: history_per_turn.len(), calls, history_per_turn }
}

fn result_of(h: &Bench, id: &TaskId, tool_use_id: &str) -> Value {
    let store = Store::open(&h.database).unwrap();
    let call = store
        .tool_calls(id)
        .unwrap()
        .into_iter()
        .find(|c| c.tool_use_id == tool_use_id)
        .unwrap_or_else(|| panic!("no call {tool_use_id}"));
    assert!(!call.is_error && call.status == ToolCallStatus::Succeeded, "{tool_use_id}: {:?}", call.result);
    serde_json::from_str(&call.result.unwrap()).unwrap()
}

/// The counters every task reports.
fn task_counters(h: &Bench, ran: &Ran, extra: &[(&str, Value)]) -> Map<String, Value> {
    let c = cost(h, &ran.id);
    let (system, tools) = static_request_bytes();
    let mut read_paths = HashSet::new();
    let mut repeated_reads = 0;
    let mut discovery = 0;
    for (name, input, _, _) in &c.calls {
        match name.as_str() {
            "read_file" => {
                if !read_paths.insert(input["path"].as_str().unwrap_or_default().to_owned()) {
                    repeated_reads += 1;
                }
            }
            "list_files" | "graph_query" => discovery += 1,
            _ => {}
        }
    }
    let result_bytes: Vec<usize> = c.calls.iter().map(|call| call.2).collect();
    let fixed = system + tools;
    let mut out = counters(&[
        ("outcomeOk", json!(1)),
        ("turns", json!(c.turns)),
        ("toolCalls", json!(c.calls.len())),
        ("repeatedReads", json!(repeated_reads)),
        ("discoveryCalls", json!(discovery)),
        ("approvals", json!(ran.approvals)),
        ("resultBytesTotal", json!(result_bytes.iter().sum::<usize>())),
        ("resultBytesMax", json!(result_bytes.iter().max().copied().unwrap_or(0))),
        ("systemBytes", json!(system)),
        ("toolSchemaBytes", json!(tools)),
        ("requestBytesFirst", json!(fixed + c.history_per_turn.first().copied().unwrap_or(0))),
        ("requestBytesLast", json!(fixed + c.history_per_turn.last().copied().unwrap_or(0))),
        ("requestBytesTotal", json!(c.history_per_turn.iter().map(|b| b + fixed).sum::<usize>())),
        ("inputTokens", json!(ran.summary.usage.input_tokens)),
        ("outputTokens", json!(ran.summary.usage.output_tokens)),
        ("storeOps", json!(ran.stats.ops)),
        ("storeRowChanges", json!(ran.stats.changes)),
        ("storeOpsPerTurn", json!(ran.stats.ops / c.turns.max(1) as u64)),
        ("transcriptBytesParsed", json!(ran.stats.transcript_bytes)),
        ("toolResultBytesRead", json!(ran.stats.tool_result_bytes)),
        ("reviewTextBytesRead", json!(ran.stats.review_text_bytes)),
        ("events", json!(ran.events.total)),
        ("textDeltaEvents", json!(ran.events.text_deltas)),
        ("eventsLagged", json!(ran.events.lagged)),
    ]);
    for (k, v) in extra {
        out.insert((*k).to_owned(), v.clone());
    }
    out
}

fn scale_of(sizes: &Sizes) -> Value {
    json!({"scale": sizes.name, "manuscriptBytes": sizes.manuscript, "graphEntities": sizes.entities + 4})
}

/// Run a task `samples + 1` times, each in a fresh project (the first run
/// warms up); report the timings and the last run's counters.
async fn measure_task<F, C>(name: &str, sizes: &Sizes, make: F, check: C)
where
    F: Fn(&Project) -> (Value, CreateTaskRequest),
    C: Fn(&Project, &Bench, &Ran) -> Vec<(&'static str, Value)>,
{
    let mut samples = Vec::new();
    let mut last = Map::new();
    for i in 0..=sizes.samples {
        let project = project(sizes);
        let mut h = bench(&project);
        let (script, req) = make(&project);
        let ran = run(&mut h, script, req).await;
        assert_eq!(ran.summary.status, TaskStatus::Completed, "{name}: {:?}", ran.summary.status);
        let extra = check(&project, &h, &ran);
        if i > 0 {
            samples.push(ran.ms);
        }
        last = task_counters(&h, &ran, &extra);
    }
    perf(name, scale_of(sizes), &samples, last);
}

fn query(id: &str, q: &str, text: &str, entity: &str, path: &str, depth: u32) -> Value {
    tool(
        id,
        tools::GRAPH_QUERY,
        json!({"query": q, "text": text, "kinds": [], "entity": entity, "path": path, "depth": depth, "limit": 100}),
    )
}

// ---------------------------------------------------------------------------
// B1–B6: representative tasks
// ---------------------------------------------------------------------------

/// B1: locate a theorem and its dependencies.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "perf"]
async fn b1_locate_a_theorem_and_its_dependencies() {
    let sizes = sizes();
    measure_task(
        "agents.task.b1_locate_theorem",
        &sizes,
        |f| {
            let theorem = f.ids.theorem.as_str();
            let calls = vec![
                tool("ls", tools::LIST_FILES, json!({"path": "", "recursive": true})),
                query("search", "search", "Spectral gap", "", "", 1),
                query("near", "neighborhood", "", theorem, "", 1),
                tool("read", tools::READ_FILE, json!({"path": "paper.typ"})),
            ];
            (script(calls, "The theorem is in paper.typ and depends on two lemmas."), request(&f.root, PermissionProfile::ReadOnly))
        },
        |f, h, ran| {
            // The graph's own answer, and the theorem's text, reached the agent.
            let search = result_of(h, &ran.id, "search").to_string();
            assert!(search.contains(f.ids.theorem.as_str()), "{search}");
            let near = result_of(h, &ran.id, "near").to_string();
            assert!(near.contains(f.ids.lemma_a.as_str()) && near.contains(f.ids.lemma_b.as_str()), "{near}");
            let read = result_of(h, &ran.id, "read");
            assert!(read["text"].as_str().unwrap().contains(THEOREM));
            vec![("neededBytes", json!(THEOREM.len())), ("readBytes", json!(read["text"].as_str().unwrap().len()))]
        },
    )
    .await;
}

/// B2: edit and build a manuscript (two rounds, re-reading in between as the
/// tool description instructs).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "perf"]
async fn b2_edit_and_build_a_manuscript() {
    let sizes = sizes();
    measure_task(
        "agents.task.b2_edit_build",
        &sizes,
        |f| {
            let calls = vec![
                tool("r1", tools::READ_FILE, json!({"path": "paper.typ"})),
                tool("e1", tools::EDIT_DOCUMENT, json!({"path": "paper.typ", "base_revision": "$ref:r1/revision",
                    "edits": [{"old_text": "at least one half", "new_text": "at least one third"}]})),
                tool("b1", tools::BUILD_DOCUMENT, json!({"entry": "paper.typ", "engine": "auto"})),
                tool("r2", tools::READ_FILE, json!({"path": "paper.typ"})),
                tool("e2", tools::EDIT_DOCUMENT, json!({"path": "paper.typ", "base_revision": "$ref:r2/revision",
                    "edits": [{"old_text": "mixes in logarithmic time", "new_text": "mixes in polylogarithmic time"}]})),
                tool("b2", tools::BUILD_DOCUMENT, json!({"entry": "paper.typ", "engine": "auto"})),
            ];
            (script(calls, "Edited twice; both builds succeeded."), request(&f.root, PermissionProfile::AutoApply))
        },
        |f, h, ran| {
            let id = h.documents.find_by_path(&p("paper.typ")).expect("open");
            let text = h.documents.state(&id).unwrap().text;
            let expected = f
                .paper
                .replace("at least one half", "at least one third")
                .replace("mixes in logarithmic time", "mixes in polylogarithmic time");
            assert_eq!(text, expected);
            assert_eq!(result_of(h, &ran.id, "b2")["status"], json!("succeeded"));
            assert_eq!(h.recorder.calls.lock().unwrap().iter().filter(|c| **c == "build_document").count(), 2);
            let review = h.service.review(&ran.id).unwrap();
            assert_eq!(review.files.len(), 1);
            assert_eq!((review.files[0].state, review.files[0].change_count), (ReviewState::Pending, 2));
            assert_eq!(ran.summary.pending_review, 1);
            // The file on disk is untouched: the edits are unsaved editor text.
            assert_eq!(std::fs::read_to_string(f.root.join("paper.typ")).unwrap(), f.paper);
            vec![]
        },
    )
    .await;
}

/// B3: run an authorized experiment and insert its figure (Auto, under the
/// project's permissions: no approval card).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "perf"]
async fn b3_run_an_experiment_and_insert_its_figure() {
    let sizes = sizes();
    let mut samples = Vec::new();
    let mut last = Map::new();
    for i in 0..=sizes.samples {
        let f = project(&sizes);
        let mut h = bench(&f);
        let root = f.root.display().to_string();
        h.service
            .set_project_permissions(
                &root,
                PermissionChoice { files: FilesAccess::Edit, code: CodeAccess::Native, setup: false },
            )
            .unwrap();
        let version = h.service.project_permissions(&root).unwrap().permissions.unwrap().version;
        let calls = vec![
            tool("py", tools::RUN_PYTHON, json!({"code": "import fit\nfit.plot()\n"})),
            tool("ins", tools::INSERT_ARTIFACT, json!({"path": "chapters/ch00.typ", "run_id": "$ref:py/run",
                "artifact_id": "$ref:py/artifacts/0/id", "after_text": "", "caption": "Fit", "update_key": ""})),
            tool("build", tools::BUILD_DOCUMENT, json!({"entry": "chapters/ch00.typ", "engine": "auto"})),
        ];
        let mut req = request(&f.root, PermissionProfile::AutoApply);
        req.approval_mode = Some(ApprovalMode::Auto);
        let ran = run(&mut h, script(calls, "Ran the fit and inserted its figure."), req).await;
        assert_eq!(ran.summary.status, TaskStatus::Completed, "{:?}", ran.summary.status);
        assert_eq!(ran.approvals, 0, "Auto under the project's permissions asks nothing here");
        let inserted = result_of(&h, &ran.id, "ins");
        let key = inserted["attachmentKey"].as_str().unwrap().to_owned();
        let doc = h.documents.find_by_path(&p("chapters/ch00.typ")).expect("open");
        assert!(h.documents.state(&doc).unwrap().text.contains(&key), "the figure is in the document");
        assert_eq!(result_of(&h, &ran.id, "build")["status"], json!("succeeded"));
        let store = Store::open(&h.database).unwrap();
        for call in store.tool_calls(&ran.id).unwrap().into_iter().filter(|call| call.name != tools::BUILD_DOCUMENT) {
            assert_eq!(call.authorized_by, Some(AuthorizedBy::ProjectPermissions { version }), "{}", call.name);
        }
        if i > 0 {
            samples.push(ran.ms);
        }
        last = task_counters(&h, &ran, &[]);
    }
    perf("agents.task.b3_experiment_figure", scale_of(&sizes), &samples, last);
}

/// B4: inspect a Lean result.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "perf"]
async fn b4_inspect_a_lean_result() {
    let sizes = sizes();
    measure_task(
        "agents.task.b4_lean_result",
        &sizes,
        |f| {
            let calls = vec![
                query("inspect", "inspect", "", "", "Main.lean", 1),
                query("entity", "entity", "", f.ids.declaration.as_str(), "", 1),
                tool("check", tools::CHECK_LEAN, json!({"path": "Main.lean"})),
            ];
            (script(calls, "Main.lean checks with no errors."), request(&f.root, PermissionProfile::AutoApply))
        },
        |f, h, ran| {
            assert!(result_of(h, &ran.id, "inspect").to_string().contains(f.ids.declaration.as_str()));
            let check = result_of(h, &ran.id, "check");
            assert_eq!(check["verdict"], json!("LEAN-VERDICT-MARK no errors"), "the verdict is reported as it is");
            assert_eq!(h.recorder.calls.lock().unwrap().as_slice(), ["check_lean"]);
            vec![]
        },
    )
    .await;
}

/// The walkthrough's formal audit workflow (as in `graph_agents.rs`).
fn audit_workflow() -> Value {
    json!({
        "workflow_id": "", "expected_revision": -1,
        "title": "Check Main.lean and attach the result", "description": "", "preset": "",
        "inputs": [{"id": "lean", "label": "Lean file", "port_type": "formalSource", "path": "Main.lean", "pinned_hash": ""}],
        "steps": [
            {"id": "audit", "label": "Check the Lean file", "operation": "lean.audit", "version": 1, "parameters": [], "payload": {"kind": "none"}},
            {"id": "associate", "label": "Propose the associations", "operation": "formal.associateEvidence", "version": 1,
             "parameters": [], "payload": {"kind": "none"}}
        ],
        "connections": [
            {"kind": "data", "from_step": "", "from_port": "", "from_input": "lean", "to_step": "audit", "to_port": "source"},
            {"kind": "data", "from_step": "audit", "from_port": "evidence", "from_input": "", "to_step": "associate", "to_port": "evidence"}
        ],
        "outcomes": [
            {"id": "evidence", "kind": "auditEvidence", "label": "Audited declarations", "step": "audit", "port": "evidence", "optional": false},
            {"id": "associations", "kind": "reviewableProposal", "label": "Associations for review", "step": "associate", "port": "proposal", "optional": true}
        ],
        "ceiling": {"execution": "trustedNative", "model_requests": false, "retain_results": true, "propose_changes": true}
    })
}

/// B5: propose a valid workflow.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "perf"]
async fn b5_propose_a_valid_workflow() {
    let sizes = sizes();
    measure_task(
        "agents.task.b5_propose_workflow",
        &sizes,
        |f| {
            let calls = vec![
                tool("ls", tools::LIST_FILES, json!({"path": "", "recursive": false})),
                tool("wf", tools::WORKFLOW_PROPOSE, audit_workflow()),
            ];
            (script(calls, "Saved the audit workflow as a draft."), request(&f.root, PermissionProfile::AutoApply))
        },
        |f, h, ran| {
            let saved = result_of(h, &ran.id, "wf");
            assert_eq!(saved["saved"], json!(true), "{saved}");
            assert_eq!(saved["state"], json!("wellFormed"), "{saved}");
            assert_eq!(saved["errors"], json!(0), "{saved}");
            assert_eq!(saved["ran"], json!(false));
            assert_eq!(f.graph.workflows().unwrap().len(), 1);
            assert!(h.recorder.calls.lock().unwrap().is_empty(), "nothing ran");
            let schema = tools::definitions()
                .into_iter()
                .find(|d| d.name == tools::WORKFLOW_PROPOSE)
                .map(|d| d.input_schema.to_string().len() + d.description.len())
                .unwrap();
            vec![("workflowSchemaBytes", json!(schema))]
        },
    )
    .await;
}

/// B6: review changes. A task edits three files; a follow-up reads them and
/// reports. Measures the follow-up.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "perf"]
async fn b6_review_changes() {
    let sizes = sizes();
    let mut samples = Vec::new();
    let mut last = Map::new();
    let files = ["paper.typ", "chapters/ch01.typ", "chapters/ch02.typ"];
    for i in 0..=sizes.samples {
        let f = project(&sizes);
        let mut h = bench(&f);
        let mut calls = Vec::new();
        for (n, path) in files.iter().enumerate() {
            calls.push(tool(&format!("r{n}"), tools::READ_FILE, json!({"path": path})));
            calls.push(tool(&format!("e{n}"), tools::EDIT_DOCUMENT, json!({"path": path,
                "base_revision": format!("$ref:r{n}/revision"),
                "edits": [{"old_text": "routine computation", "new_text": "short computation"}]})));
        }
        // The same script serves the follow-up: its turns come after the first run's.
        let mut turns = script(calls, "Edited three files.")["turns"].as_array().unwrap().clone();
        for (n, path) in files.iter().enumerate() {
            turns.push(json!({"steps": [tool(&format!("v{n}"), tools::READ_FILE, json!({"path": path}))], "chunkDelayMs": 0}));
        }
        turns.push(json!({"steps": [{"text": "Three files have unsaved agent changes awaiting review."}], "chunkDelayMs": 0}));
        // Each chapter has the phrase once; the manuscript has it in every paragraph.
        let mut first_script = json!({"turns": turns});
        first_script["turns"][1]["steps"][0]["toolUse"]["input"]["edits"] =
            json!([{"old_text": "at least one half", "new_text": "at least one third"}]);
        let first = run(&mut h, first_script, request(&f.root, PermissionProfile::AutoApply)).await;
        assert_eq!(first.summary.status, TaskStatus::Completed, "{:?}", first.summary.status);
        let before = cost(&h, &first.id);
        let ran = follow_up(&mut h, &first.id, "Summarize what you changed.").await;
        assert_eq!(ran.summary.status, TaskStatus::Completed, "{:?}", ran.summary.status);
        let review = h.service.review(&ran.id).unwrap();
        let reviewed: Vec<&str> = review.files.iter().map(|file| file.path.as_str()).collect();
        let mut expected = files.to_vec();
        expected.sort_unstable();
        assert_eq!(reviewed, expected);
        assert!(review.files.iter().all(|file| file.state == ReviewState::Pending && file.unsaved == Some(true)));
        for n in 0..files.len() {
            let seen = result_of(&h, &ran.id, &format!("v{n}"));
            assert_eq!(seen["unsavedChanges"], json!(true));
            assert_eq!(seen["revision"], result_of(&h, &ran.id, &format!("e{n}"))["revision"]);
        }
        // What the follow-up itself added to the conversation before the model answered.
        let store = Store::open(&h.database).unwrap();
        let messages = store.messages(&ran.id).unwrap();
        let follow = &messages[before.turns * 2];
        assert_eq!(follow.0, "user");
        let follow_bytes = serde_json::to_string(&follow.1).unwrap().len();
        let graph_bytes: usize = follow
            .1
            .iter()
            .filter_map(|b| b["text"].as_str())
            .filter(|t| t.starts_with("<graph_context>"))
            .map(str::len)
            .sum();
        if i > 0 {
            samples.push(ran.ms);
        }
        last = task_counters(
            &h,
            &ran,
            &[("followUpMessageBytes", json!(follow_bytes)), ("followUpGraphContextBytes", json!(graph_bytes))],
        );
    }
    perf("agents.task.b6_review_changes", scale_of(&sizes), &samples, last);
}

// ---------------------------------------------------------------------------
// Scaling: transcript length, streaming, task detail, task list, follow-ups,
// attachments, tool definitions
// ---------------------------------------------------------------------------

fn long_script(turns: usize) -> Value {
    let calls = (0..turns)
        .map(|i| {
            if i % 4 == 0 {
                tool(&format!("t{i}"), tools::READ_FILE, json!({"path": "paper.typ"}))
            } else {
                tool(&format!("t{i}"), tools::LIST_FILES, json!({"path": "chapters", "recursive": false}))
            }
        })
        .collect();
    script(calls, "Done.")
}

/// Per-turn cost against transcript length: the same kind of turn, in a short
/// and in a long task. Linear per-task cost keeps ms per turn constant.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "perf"]
async fn turn_cost_against_transcript_length() {
    let sizes = sizes();
    for turns in [sizes.long_turns / 4, sizes.long_turns] {
        let mut samples = Vec::new();
        let mut last = Map::new();
        for i in 0..=sizes.samples {
            let f = project(&sizes);
            let mut h = bench(&f);
            let ran = run(&mut h, long_script(turns), request(&f.root, PermissionProfile::ReadOnly)).await;
            assert_eq!(ran.summary.status, TaskStatus::Completed, "{:?}", ran.summary.status);
            assert_eq!(ran.summary.usage.tool_calls as usize, turns);
            if i > 0 {
                samples.push(ran.ms / (turns + 1) as f64);
            }
            last = task_counters(&h, &ran, &[("taskMs", json!(ran.ms.round()))]);
        }
        let mut scale = scale_of(&sizes);
        scale["turns"] = json!(turns + 1);
        perf("agents.turn.ms_per_turn", scale, &samples, last);
    }
}

/// One long streamed answer: events sent to the webview, and the text they carry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "perf"]
async fn streaming_a_long_answer() {
    let sizes = sizes();
    let answer: String = (0..sizes.stream_words).map(|i| format!("word{i} ")).collect();
    let mut samples = Vec::new();
    let mut last = Map::new();
    for i in 0..=sizes.samples {
        let f = project(&sizes);
        let mut h = bench(&f);
        let script = json!({"turns": [{"steps": [{"thinking": "Plan."}, {"text": answer}], "chunkDelayMs": 0}]});
        let ran = run(&mut h, script, request(&f.root, PermissionProfile::ReadOnly)).await;
        assert_eq!(ran.summary.status, TaskStatus::Completed);
        assert_eq!(ran.events.lagged, 0, "the counting subscriber kept up");
        assert_eq!(ran.events.text, answer, "the streamed text is complete and in order");
        let detail = h.service.task_detail(&ran.id).unwrap();
        assert!(detail.transcript.iter().any(|item| matches!(item, TranscriptItem::AssistantText { text, .. } if *text == answer)));
        if i > 0 {
            samples.push(ran.ms);
        }
        last = task_counters(&h, &ran, &[("streamBytes", json!(answer.len()))]);
    }
    let mut scale = scale_of(&sizes);
    scale["words"] = json!(sizes.stream_words);
    perf("agents.stream.long_answer", scale, &samples, last);
}

/// Reading a task's detail (what every transcript event makes the webview do)
/// after a long task.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "perf"]
async fn task_detail_after_a_long_task() {
    let sizes = sizes();
    let f = project(&sizes);
    let mut h = bench(&f);
    let ran = run(&mut h, long_script(sizes.long_turns), request(&f.root, PermissionProfile::ReadOnly)).await;
    assert_eq!(ran.summary.status, TaskStatus::Completed);
    let mut samples = Vec::new();
    let mut bytes = 0;
    let mut items = 0;
    let before = h.service.store_stats();
    for i in 0..=sizes.samples {
        let started = Instant::now();
        let detail = h.service.task_detail(&ran.id).unwrap();
        let encoded = serde_json::to_vec(&detail).unwrap();
        if i > 0 {
            samples.push(ms(started));
        }
        bytes = encoded.len();
        items = detail.transcript.len();
    }
    let stats = delta(before, h.service.store_stats());
    let reads = (sizes.samples + 1) as u64;
    let graph_context =
        h.service.task_detail(&ran.id).unwrap().graph.context.map_or(0, |c| serde_json::to_vec(&c).unwrap().len());
    let mut scale = scale_of(&sizes);
    scale["turns"] = json!(sizes.long_turns + 1);
    perf(
        "agents.detail.full_read",
        scale,
        &samples,
        counters(&[
            ("responseBytes", json!(bytes)),
            ("graphContextBytes", json!(graph_context)),
            ("transcriptItems", json!(items)),
            ("storeOps", json!(stats.ops / reads)),
            ("transcriptBytesParsed", json!(stats.transcript_bytes / reads)),
            ("toolResultBytesRead", json!(stats.tool_result_bytes / reads)),
            ("reviewTextBytesRead", json!(stats.review_text_bytes / reads)),
            ("detailReadsDuringTask", json!(ran.events.transcript_changed)),
        ]),
    );
}

/// The task list with many stopped tasks that changed large files (what the
/// webview re-reads after the user's own edits), and one task's summary.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "perf"]
async fn task_list_with_many_changed_files() {
    let sizes = sizes();
    let f = project(&sizes);
    let body: String = {
        let mut text = String::new();
        let mut n = 0;
        while text.len() < sizes.changed_file {
            text.push_str(&filler(n));
            n += 1;
        }
        text
    };
    for t in 0..sizes.tasks {
        for k in 0..sizes.files_per_task {
            write(&f.root, &format!("work/t{t:03}/f{k}.typ"), &format!("= File {t} {k}\n\nMARKER-OLD\n\n{body}"));
        }
    }
    let mut h = bench(&f);
    let mut ids = Vec::new();
    for t in 0..sizes.tasks {
        let reads: Vec<Value> = (0..sizes.files_per_task)
            .map(|k| tool(&format!("r{k}"), tools::READ_FILE, json!({"path": format!("work/t{t:03}/f{k}.typ")})))
            .collect();
        let edits: Vec<Value> = (0..sizes.files_per_task)
            .map(|k| {
                tool(&format!("e{k}"), tools::EDIT_DOCUMENT, json!({"path": format!("work/t{t:03}/f{k}.typ"),
                    "base_revision": format!("$ref:r{k}/revision"),
                    "edits": [{"old_text": "MARKER-OLD", "new_text": "MARKER-NEW"}]}))
            })
            .collect();
        let script = json!({"turns": [
            {"steps": reads, "chunkDelayMs": 0},
            {"steps": edits, "chunkDelayMs": 0},
            {"steps": [{"text": "Edited."}], "chunkDelayMs": 0}
        ]});
        let ran = run(&mut h, script, request(&f.root, PermissionProfile::AutoApply)).await;
        assert_eq!(ran.summary.status, TaskStatus::Completed, "{:?}", ran.summary.status);
        assert_eq!(ran.summary.changed_files as usize, sizes.files_per_task);
        ids.push(ran.id);
    }
    let scale = json!({"scale": sizes.name, "tasks": sizes.tasks, "filesPerTask": sizes.files_per_task,
        "fileBytes": sizes.changed_file});
    let mut samples = Vec::new();
    let before = h.service.store_stats();
    for i in 0..=sizes.samples {
        let started = Instant::now();
        let tasks = h.service.list_tasks().unwrap();
        if i > 0 {
            samples.push(ms(started));
        }
        assert_eq!(tasks.len(), sizes.tasks);
        // The outcome: every file of every task is pending review.
        assert!(tasks.iter().all(|task| task.pending_review as usize == sizes.files_per_task));
    }
    let stats = delta(before, h.service.store_stats());
    let reads = (sizes.samples + 1) as u64;
    perf(
        "agents.summary.list_tasks",
        scale.clone(),
        &samples,
        counters(&[
            ("storeOps", json!(stats.ops / reads)),
            ("reviewTextBytesRead", json!(stats.review_text_bytes / reads)),
        ]),
    );
    // One summary (every status update of a running task computes one).
    let mut samples = Vec::new();
    let before = h.service.store_stats();
    for i in 0..=sizes.samples {
        let started = Instant::now();
        let task = h.service.task(&ids[0]).unwrap();
        if i > 0 {
            samples.push(ms(started));
        }
        assert_eq!(task.pending_review as usize, sizes.files_per_task);
    }
    let stats = delta(before, h.service.store_stats());
    perf(
        "agents.summary.one_task",
        scale,
        &samples,
        counters(&[
            ("storeOps", json!(stats.ops / reads)),
            ("reviewTextBytesRead", json!(stats.review_text_bytes / reads)),
        ]),
    );
    // The user undoes one task's change in one file: the count follows content.
    let path = p("work/t000/f0.typ");
    let doc = h.documents.find_by_path(&path).unwrap();
    h.documents.undo(&doc, EditOrigin::Source).unwrap();
    let tasks = h.service.list_tasks().unwrap();
    let undone = tasks.iter().find(|task| task.id == ids[0]).unwrap();
    assert_eq!(undone.pending_review as usize, sizes.files_per_task - 1, "an undone change is not pending");
}

/// Follow-ups on a task bound to a graph: what each adds to the conversation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "perf"]
async fn follow_ups_repeat_the_graph_context() {
    let sizes = sizes();
    let f = project(&sizes);
    let mut h = bench(&f);
    let turns: Vec<Value> =
        (0..=sizes.followups).map(|i| json!({"steps": [{"text": format!("Answer {i}.")}], "chunkDelayMs": 0})).collect();
    let first = run(&mut h, json!({"turns": turns}), request(&f.root, PermissionProfile::ReadOnly)).await;
    assert_eq!(first.summary.status, TaskStatus::Completed);
    let mut samples = Vec::new();
    for i in 0..sizes.followups {
        let ran = follow_up(&mut h, &first.id, &format!("Follow-up {i}")).await;
        assert_eq!(ran.summary.status, TaskStatus::Completed);
        samples.push(ran.ms);
    }
    let store = Store::open(&h.database).unwrap();
    let messages = store.messages(&first.id).unwrap();
    let block_bytes = |prefix: &str| -> usize {
        messages
            .iter()
            .flat_map(|(_, content)| content.iter())
            .filter_map(|b| b["text"].as_str())
            .filter(|t| t.starts_with(prefix))
            .map(str::len)
            .sum()
    };
    let total: usize = messages.iter().map(|(_, content)| serde_json::to_string(content).unwrap().len()).sum();
    let detail = serde_json::to_vec(&h.service.task_detail(&first.id).unwrap()).unwrap().len();
    let mut scale = scale_of(&sizes);
    scale["followUps"] = json!(sizes.followups);
    perf(
        "agents.followup.context",
        scale,
        &samples,
        counters(&[
            ("conversationBytes", json!(total)),
            ("graphContextBytes", json!(block_bytes("<graph_context>"))),
            ("taskContextBytes", json!(block_bytes("<task_context>"))),
            ("graphContextBlocks", json!(messages.iter().flat_map(|(_, c)| c.iter()).filter(|b| b["text"].as_str().is_some_and(|t| t.starts_with("<graph_context>"))).count())),
            ("detailResponseBytes", json!(detail)),
        ]),
    );
}

/// A task with attachments over several turns: every request carries them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "perf"]
async fn attachments_over_several_turns() {
    let sizes = sizes();
    let turns = 12;
    let mut samples = Vec::new();
    let mut last = Map::new();
    for i in 0..=sizes.samples {
        let f = project(&sizes);
        let mut h = bench(&f);
        let mut ids = Vec::new();
        for n in 0..sizes.attachments {
            let text = format!("attachment {n}\n{}", "0123456789abcdef\n".repeat(sizes.attachment_bytes / 17));
            let encoded = exposition_agents::attachments::base64_encode(text.as_bytes());
            match h.service.add_attachment_bytes(Some(&format!("notes{n}.txt")), Some("text/plain"), &encoded) {
                attachments::IngestOutcome::Added { attachment } => ids.push(attachment.id),
                attachments::IngestOutcome::Rejected { message, .. } => panic!("{message}"),
            }
        }
        let calls = (0..turns)
            .map(|t| tool(&format!("l{t}"), tools::LIST_FILES, json!({"path": "chapters", "recursive": false})))
            .collect();
        let mut req = request(&f.root, PermissionProfile::ReadOnly);
        req.attachments = Some(ids);
        let ran = run(&mut h, script(calls, "Done."), req).await;
        assert_eq!(ran.summary.status, TaskStatus::Completed, "{:?}", ran.summary.status);
        // Every request carried every attachment (the scripted provider counts what it is sent).
        let per_request = (sizes.attachments * sizes.attachment_bytes) as u64 / 4;
        assert!(ran.summary.usage.input_tokens >= per_request * (turns as u64 + 1), "{:?}", ran.summary.usage);
        if i > 0 {
            samples.push(ran.ms / (turns + 1) as f64);
        }
        last = task_counters(&h, &ran, &[]);
    }
    let mut scale = scale_of(&sizes);
    scale["attachments"] = json!(sizes.attachments);
    scale["attachmentBytes"] = json!(sizes.attachment_bytes);
    scale["turns"] = json!(turns + 1);
    perf("agents.attachments.ms_per_turn", scale, &samples, last);
}

/// The static part of every request: real serialized sizes per tool, and the
/// time to build the tool list.
#[test]
#[ignore = "perf"]
fn tool_definitions_size_and_build_time() {
    let mut per_tool = Map::new();
    let mut strict = 0;
    for d in tools::task_definitions(true) {
        let bytes = json!({"name": d.name, "description": d.description, "input_schema": d.input_schema, "strict": d.strict})
            .to_string()
            .len();
        if d.strict {
            strict += d.input_schema.to_string().len();
        }
        per_tool.insert(d.name.to_owned(), json!(bytes));
    }
    let (system, tools_bytes) = static_request_bytes();
    let discussion: usize = tools::discussion_definitions()
        .iter()
        .map(|d| d.input_schema.to_string().len() + d.description.len() + d.name.len())
        .sum();
    let mut samples = Vec::new();
    for i in 0..=10 {
        let started = Instant::now();
        for _ in 0..20 {
            std::hint::black_box(tools::task_definitions(true));
        }
        if i > 0 {
            samples.push(ms(started) / 20.0);
        }
    }
    let mut out = counters(&[
        ("systemBytes", json!(system)),
        ("toolSchemaBytes", json!(tools_bytes)),
        ("strictSchemaBytes", json!(strict)),
        ("discussionToolBytes", json!(discussion)),
        ("tools", json!(per_tool.len())),
    ]);
    out.insert("perTool".into(), Value::Object(per_tool));
    perf("agents.request.static_prefix", json!({}), &samples, out);
}
