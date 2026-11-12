//! The reconciler driver with the real workspace and document services: the
//! graph follows files and buffers through Rust subscriptions alone (no
//! frontend), edits are overlaid as unsaved without being written, saves and
//! undo follow the document service's actual state, and applying an edit is
//! never blocked by reconciliation.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use exposition_core::document::{
    ConflictPolicy, DocumentEdit, EditOrigin, TextPatch, TransactionId, TransactionRequest, UndoBehavior,
};
use exposition_core::path::WorkspacePath;
use exposition_graph::*;
use exposition_workspace::{DocumentService, WorkspaceService};

fn wp(s: &str) -> WorkspacePath {
    WorkspacePath::parse(s).unwrap()
}

async fn wait_for<T>(what: &str, timeout: Duration, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

struct Setup {
    _data: tempfile::TempDir,
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    workspace: Arc<WorkspaceService>,
    graph: Arc<GraphService>,
    reconciler: reconcile::ReconcilerHandle,
}

async fn setup(files: &[(&str, &str)]) -> Setup {
    let data = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    for (rel, body) in files {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    let documents = Arc::new(DocumentService::new());
    let workspace = WorkspaceService::new(documents, data.path().join("app"));
    tokio::spawn(Arc::clone(&workspace).run());
    let graph = GraphService::new(GraphConfig { data_dir: data.path().join("app") });
    let reconciler = reconcile::spawn(Arc::clone(&graph), Arc::clone(&workspace));
    workspace.open_workspace(&root).unwrap();
    graph.attach(&root);
    Setup { _data: data, _dir: dir, root, workspace, graph, reconciler }
}

fn synchronized(g: &GraphService) -> bool {
    g.freshness().state == SyncState::Synchronized
}

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

const MAIN: &str = "\\documentclass{article}\n\\begin{document}\n\\begin{lemma}\\label{lem:key}\nKey.\n\\end{lemma}\n\\begin{theorem}\\label{thm:main}\nBy Lemma~\\ref{lem:key}.\n\\end{theorem}\n\\begin{proof}\nObvious.\n\\end{proof}\n\\end{document}\n";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_graph_follows_files_and_buffers_without_any_frontend() {
    let s = setup(&[("main.tex", MAIN), ("notes.md", "# Notes\n")]).await;
    let g = &s.graph;
    wait_for("initial reconciliation", Duration::from_secs(20), || {
        (synchronized(g) && g.resource(&wp("main.tex")).ok().flatten().is_some()).then_some(())
    })
    .await;
    let theorem = |g: &GraphService| {
        g.snapshot(&GraphQuery { kinds: Some(vec![EntityKind::Theorem]), ..Default::default() }).unwrap().entities
    };
    let thm = wait_for("theorem extracted", Duration::from_secs(10), || theorem(g).into_iter().next()).await;

    // A file created outside the app is admitted through the watcher.
    write(&s.root, "chapters/extra.md", "# Extra\n");
    wait_for("external file admitted", Duration::from_secs(20), || {
        g.resource(&wp("chapters/extra.md")).ok().flatten()
    })
    .await;

    // Edit main.tex in a buffer: an unsaved overlay, never written.
    let opened = s.workspace.open_document(&wp("main.tex")).unwrap();
    let doc = opened.state.info.id.clone();
    let insert_at = MAIN.find("\\end{document}").unwrap() as u32;
    let started = Instant::now();
    let outcome = s
        .workspace
        .documents()
        .apply(TransactionRequest {
            id: TransactionId("t-1".into()),
            origin: EditOrigin::Source,
            edits: vec![DocumentEdit {
                document: doc.clone(),
                base_revision: opened.state.info.revision,
                patches: vec![TextPatch::insert(
                    insert_at,
                    "\\begin{corollary}\\label{cor:new}\nFollows from Theorem~\\ref{thm:main}.\n\\end{corollary}\n",
                )],
            }],
            undo: UndoBehavior::NewGroup,
            conflict: ConflictPolicy::Reject,
            label: None,
            selection: None,
        })
        .unwrap();
    let apply_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert!(apply_ms < 50.0, "applying an edit is not blocked by the reconciler ({apply_ms:.1} ms)");
    let revision = outcome.applied[0].revision;
    let inspection = wait_for("overlay", Duration::from_secs(10), || {
        let i = g
            .inspect_at_source(&InspectAtSourceRequest {
                path: wp("main.tex"),
                offset: Some(insert_at + 20),
                revision: Some(revision),
            })
            .unwrap();
        i.unsaved.then_some(i)
    })
    .await;
    let corollary = inspection.entities.iter().find(|e| e.entity.kind == EntityKind::Corollary).expect("unsaved corollary");
    assert_eq!(corollary.entity.attributes.get("overlay.provisional").map(String::as_str), Some("true"));
    assert_eq!(corollary.binding.version.as_ref().map(|v| v.saved), Some(false), "labelled unsaved");
    assert!(
        corollary.relations.iter().any(|r| r.kind == RelationKind::References && r.to == thm.id),
        "the unsaved reference is visible in dependency inspection: {:?}",
        corollary.relations
    );
    assert!(g.freshness().overlay.iter().any(|o| o.path == wp("main.tex") && o.revision == revision));
    // The saved projection does not contain it.
    assert!(
        g.snapshot(&GraphQuery { kinds: Some(vec![EntityKind::Corollary]), ..Default::default() }).unwrap().entities.is_empty(),
        "unsaved text is never written as saved state"
    );
    assert_eq!(std::fs::read_to_string(s.root.join("main.tex")).unwrap(), MAIN);

    // Undo back to the saved text: the overlay is dropped.
    s.workspace.documents().undo(&doc, EditOrigin::Source).unwrap();
    wait_for("overlay dropped after undo", Duration::from_secs(10), || {
        g.freshness().overlay.is_empty().then_some(())
    })
    .await;

    // Redo and save: the declaration becomes a saved entity.
    s.workspace.documents().redo(&doc, EditOrigin::Source).unwrap();
    s.workspace.save_document(&doc, false).unwrap();
    let saved = wait_for("saved corollary reconciled", Duration::from_secs(20), || {
        g.snapshot(&GraphQuery { kinds: Some(vec![EntityKind::Corollary]), ..Default::default() })
            .unwrap()
            .entities
            .into_iter()
            .next()
    })
    .await;
    assert!(g.freshness().overlay.is_empty());
    assert_eq!(theorem(g)[0].id, thm.id, "the theorem kept its identity through the edit");
    let detail = g.entity_detail(&saved.id).unwrap();
    assert!(detail.outgoing.iter().any(|r| r.kind == RelationKind::References && r.to == thm.id));
    assert!(
        !detail.outgoing.iter().any(|r| r.kind == RelationKind::DependsOn),
        "a reference is never a dependency"
    );

    // Forced reindex through the handle.
    let report = s.reconciler.reindex_blocking(true, Duration::from_secs(30)).unwrap();
    assert!(report.read >= 3, "{report:?}");
    assert_eq!(theorem(g)[0].id, thm.id);
    s.reconciler.shutdown();
}
