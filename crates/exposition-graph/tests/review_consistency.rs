//! Independent review of the graph consistency change: targeted scenarios
//! the random generator of `review_perf_differential.rs` does not produce.
//!
//! Every scenario applies incremental passes and then compares every stored
//! record (entities, bindings, relations, coverage, awaited targets, the
//! stored index, evidence) and the overlays of unsaved buffers with a copy
//! of the same database after a forced full reindex of the same files. The
//! copy is reindexed, not the graph under test, so a difference is never
//! repaired on the way to the next step.
//!
//! Every test is enabled: the defects three of them exposed are fixed.

#![allow(unsafe_code)]

mod support;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use exposition_core::path::WorkspacePath;
use exposition_graph::reconcile::{ReconcileOptions, ReconcileReport, Scope, overlay};
use exposition_graph::*;
use serde_json::Value;
use support::{SqlWork, count_sql, sql_of};

// ----- a project under test -----

struct Project {
    _data: tempfile::TempDir,
    _dir: tempfile::TempDir,
    root: PathBuf,
    svc: Arc<GraphService>,
    /// Unsaved buffers compared on both sides: path, revision, text.
    buffers: Vec<(String, u64, String)>,
}

fn open(seed_files: Option<&Path>) -> Project {
    let data = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    // A fixed folder name: the project entity is labelled with it.
    std::fs::create_dir(dir.path().join("project")).unwrap();
    let root = std::fs::canonicalize(dir.path().join("project")).unwrap();
    if let Some(from) = seed_files {
        copy_tree(from, &root);
    }
    let svc = GraphService::new(GraphConfig { data_dir: data.path().to_path_buf() });
    let status = svc.attach(&root);
    assert!(matches!(status.state, GraphState::Attached), "{status:?}");
    Project { _data: data, _dir: dir, root, svc, buffers: Vec::new() }
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let target = to.join(&name);
        let kind = entry.file_type().unwrap();
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            copy_tree(&entry.path(), &target);
        } else if !name.to_string_lossy().ends_with("-journal") {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn wp(p: &str) -> WorkspacePath {
    WorkspacePath::parse(p).unwrap()
}

impl Project {
    fn write(&self, rel: &str, body: &str) {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    fn remove(&self, rel: &str) {
        std::fs::remove_file(self.root.join(rel)).unwrap();
    }

    fn rename(&self, from: &str, to: &str) {
        let target = self.root.join(to);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::rename(self.root.join(from), target).unwrap();
    }

    fn full(&self) -> ReconcileReport {
        self.svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: false }).unwrap()
    }

    /// Mark, then one pass over the paths (as the driver delivers events).
    fn pass(&self, paths: &[&str]) -> ReconcileReport {
        let paths: Vec<WorkspacePath> = paths.iter().map(|p| wp(p)).collect();
        self.svc.mark_pending(&paths).unwrap();
        self.svc.reconcile(&ReconcileOptions { scope: Scope::Paths(paths), force: false }).unwrap()
    }

    fn coverage(&self, p: &str) -> ResourceCoverage {
        self.svc.with_store(|s| Ok(s.coverage(&wp(p))?)).unwrap().unwrap_or_else(|| panic!("{p} has no coverage"))
    }

    fn unresolved(&self, p: &str) -> u32 {
        self.coverage(p).unresolved
    }

    fn problems(&self, p: &str) -> String {
        self.coverage(p).detail.unwrap_or_default()
    }

    fn waits_for(&self, p: &str) -> Vec<String> {
        let mut wanted: Vec<String> = self
            .svc
            .with_store(|s| Ok(s.index_wanted()?))
            .unwrap()
            .into_iter()
            .filter(|(at, _)| at.as_str() == p)
            .flat_map(|(_, wanted)| wanted)
            .collect();
        wanted.sort();
        wanted
    }

    fn live_resources(&self) -> Vec<String> {
        sql(&self.svc, "SELECT key FROM entities WHERE kind = 'resource' AND lifecycle != 'tombstoned' ORDER BY key")
            .into_iter()
            .map(|mut r| r.remove(0))
            .collect()
    }

    fn resource(&self, p: &str) -> EntityId {
        self.svc.resource(&wp(p)).unwrap().unwrap_or_else(|| panic!("{p} is not a resource")).id
    }

    /// The live entities (not files or folders) of a key.
    fn keyed(&self, key: &str) -> Vec<String> {
        sql(
            &self.svc,
            &format!(
                "SELECT id FROM entities WHERE key = '{key}' AND lifecycle != 'tombstoned' AND kind NOT IN ('resource', 'folder') ORDER BY id"
            ),
        )
        .into_iter()
        .map(|mut r| r.remove(0))
        .collect()
    }

    /// The keys of the entities `from`'s file is observed to link to.
    fn links_to(&self, from: &str, kind: RelationKind) -> Vec<String> {
        let detail = self.svc.entity_detail(&self.resource(from)).unwrap();
        let mut out: Vec<String> = detail
            .outgoing
            .iter()
            .filter(|r| r.kind == kind)
            .filter_map(|r| self.svc.with_store(|s| Ok(s.entity(&r.to)?)).unwrap())
            .filter_map(|e| e.key)
            .collect();
        out.sort();
        out
    }

    fn overlays(&self, svc: &GraphService) -> Vec<String> {
        self.buffers
            .iter()
            .map(|(path, revision, text)| match overlay::compute(svc, &wp(path), *revision, text).unwrap() {
                Some(entry) => overlay_text(&entry),
                None => format!("O {{\"overlay\":\"{path}\",\"none\":true}}"),
            })
            .collect()
    }

    /// What differs between this graph and a copy of it after a forced full
    /// reindex of the same files (records and overlays). Empty: nothing.
    fn differences(&self) -> Vec<String> {
        let incremental = records(&self.svc);
        let overlays_a = self.overlays(&self.svc);
        let b = open(Some(&self.root));
        b.svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: true }).unwrap();
        let full = records(&b.svc);
        let overlays_b = self.overlays(&b.svc);
        b.svc.detach();
        let mut out = Vec::new();
        // The session's index (a cache of the stored one) against a fresh read.
        for d in self.svc.session_index_drift().unwrap().unwrap_or_default() {
            out.push(format!("session index: {d}"));
        }
        let a: BTreeSet<&String> = incremental.iter().collect();
        let f: BTreeSet<&String> = full.iter().collect();
        for line in a.difference(&f) {
            out.push(format!("incremental only: {}", clip(line)));
        }
        for line in f.difference(&a) {
            out.push(format!("full only:        {}", clip(line)));
        }
        for (x, y) in overlays_a.iter().zip(&overlays_b) {
            if x != y {
                out.push(format!("overlay, incremental: {}", clip(x)));
                out.push(format!("overlay, full:        {}", clip(y)));
            }
        }
        out
    }

    #[track_caller]
    fn check(&self, when: &str) {
        let d = self.differences();
        assert!(d.is_empty(), "{when}: a forced full reindex leaves another graph\n{}", d.join("\n"));
    }
}

fn clip(line: &str) -> String {
    if line.len() > 600 { format!("{}...", line.chars().take(600).collect::<String>()) } else { line.to_owned() }
}

fn sql(svc: &GraphService, sql: &str) -> Vec<Vec<String>> {
    svc.with_store(|store| {
        let conn = store.connection();
        let mut stmt = conn.prepare(sql).map_err(store::StoreError::from)?;
        let n = stmt.column_count();
        let rows = stmt
            .query_map([], |r| {
                (0..n)
                    .map(|i| {
                        r.get_ref(i).map(|v| match v {
                            rusqlite::types::ValueRef::Null => String::new(),
                            rusqlite::types::ValueRef::Integer(i) => i.to_string(),
                            rusqlite::types::ValueRef::Real(f) => f.to_string(),
                            rusqlite::types::ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
                            rusqlite::types::ValueRef::Blob(_) => "<blob>".to_owned(),
                        })
                    })
                    .collect::<Result<Vec<String>, _>>()
            })
            .map_err(store::StoreError::from)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(store::StoreError::from)?);
        }
        Ok(out)
    })
    .unwrap()
}

fn exec(svc: &GraphService, statements: &str) {
    svc.with_store(|s| {
        s.connection().execute_batch(statements).map_err(store::StoreError::from)?;
        Ok(())
    })
    .unwrap();
}

/// Every stored record, one line each, ids as stored, without the times and
/// revision counters a pass advances.
fn records(svc: &GraphService) -> Vec<String> {
    let entity_ids = sql(svc, "SELECT id FROM entities ORDER BY id");
    let relation_ids = sql(svc, "SELECT id FROM relations ORDER BY id");
    let clean = |mut v: Value| {
        if let Value::Object(map) = &mut v {
            for key in ["updatedAtMs", "createdAtMs", "revision"] {
                map.remove(key);
            }
        }
        v
    };
    let mut lines = Vec::new();
    svc.with_store(|store| {
        for row in &entity_ids {
            let id = EntityId::parse(&row[0]).unwrap();
            let e = store.entity(&id)?.expect("listed entity");
            lines.push(format!("E {}", clean(serde_json::to_value(&e).unwrap())));
            for b in store.bindings(&id)? {
                lines.push(format!("B {}", serde_json::to_value(&b).unwrap()));
            }
        }
        for row in &relation_ids {
            let r = store.relation(&RelationId::parse(&row[0]).unwrap())?.expect("listed relation");
            lines.push(format!("R {}", clean(serde_json::to_value(&r).unwrap())));
        }
        for c in store.all_coverage()? {
            lines.push(format!("C {}", serde_json::to_value(&c).unwrap()));
        }
        for (path, mut wanted) in store.index_wanted()? {
            wanted.sort();
            lines.push(format!("W {path} {wanted:?}"));
        }
        for record in store.all_source_index()? {
            lines.push(format!("I {} {} {} {}", record.path, record.content_hash, record.extractor, record.body));
        }
        Ok(())
    })
    .unwrap();
    for row in sql(svc, "SELECT id, kind, subject FROM evidence ORDER BY id") {
        lines.push(format!("V {}", row.join(" ")));
    }
    lines.sort();
    lines
}

fn overlay_text(entry: &overlay::OverlayEntry) -> String {
    let mut entities: Vec<Value> = entry.entities.values().map(|e| serde_json::to_value(e).unwrap()).collect();
    entities.sort_by_key(|v| v.to_string());
    let mut bindings: Vec<Value> = entry
        .bindings
        .iter()
        .map(|(id, b)| serde_json::json!({ "entity": id, "bindings": serde_json::to_value(b).unwrap() }))
        .collect();
    bindings.sort_by_key(|v| v.to_string());
    let mut added: Vec<Value> = entry.added.iter().map(|r| serde_json::to_value(r).unwrap()).collect();
    added.sort_by_key(|v| v.to_string());
    let mut v = serde_json::json!({
        "overlay": entry.path,
        "revision": entry.revision,
        "contentHash": entry.content_hash,
        "entities": entities,
        "bindings": bindings,
        "added": added,
        "removed": entry.removed,
        "coverage": serde_json::to_value(&entry.coverage).unwrap(),
        "provisional": entry.provisional,
    });
    strip_times(&mut v);
    format!("O {v}")
}

fn strip_times(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for k in ["createdAtMs", "updatedAtMs", "computedAtMs", "checkpointMs", "indexedAt"] {
                map.remove(k);
            }
            for v in map.values_mut() {
                strip_times(v);
            }
        }
        Value::Array(items) => {
            for v in items {
                strip_times(v);
            }
        }
        _ => {}
    }
}

fn lemma(label: &str) -> String {
    format!("\\begin{{lemma}}\\label{{{label}}}\nA statement about {label}.\n\\end{{lemma}}\n")
}

fn theorem_using(label: &str, used: &str) -> String {
    format!("\\begin{{theorem}}\\label{{{label}}}\nBy Lemma~\\ref{{{used}}}.\n\\end{{theorem}}\n")
}

fn relate(svc: &GraphService, kind: RelationKind, from: &str, to: &str) -> bool {
    let out = svc.propose_and_apply(
        Origin::User,
        &ChangeRequest {
            base_revision: svc.status().revision,
            operations: vec![GraphOperation::AddRelation {
                kind,
                from: EntityRef::Id { id: EntityId::parse(from).unwrap() },
                to: EntityRef::Id { id: EntityId::parse(to).unwrap() },
                evidence_kind: None,
                evidence: vec![],
                note: None,
            }],
            summary: None,
        },
    );
    matches!(out, Ok((_, ApplyOutcome::Applied { .. })))
}

fn review_evidence(svc: &GraphService, subject: &str, about: &str) -> String {
    let by = Origin::Service { service: "review".into() };
    let path = wp(about);
    let hash = svc.with_store(|s| Ok(s.coverage(&path)?.and_then(|c| c.content_hash))).unwrap();
    let record = Evidence {
        id: EvidenceId::mint(),
        kind: EvidenceKind::Review,
        subject: Some(EntityId::parse(subject).unwrap()),
        operation: OperationRef { kind: "review".into(), id: "r1".into() },
        inputs: vec![VersionedInput { path, content_hash: hash, revision: None, unsaved: false }],
        outcome: EvidenceOutcome::Succeeded,
        environment: vec![],
        outputs: vec![],
        formal: None,
        recorded_by: by.clone(),
        recorded_at_ms: 1_000,
    };
    svc.register_evidence(&by, record).unwrap().as_str().to_owned()
}

/// The file system under `root` treats names that differ in case as one.
fn ignores_case(root: &Path) -> bool {
    std::fs::write(root.join("case-probe.tmp"), "").unwrap();
    let same = root.join("CASE-PROBE.TMP").exists();
    std::fs::remove_file(root.join("case-probe.tmp")).unwrap();
    same
}

// ----- 1. the declarations of a label -----

#[test]
fn three_declarations_of_a_label_and_back_to_one() {
    let p = open(None);
    p.write("d/a.tex", &lemma("lem:x"));
    p.write("r/r.tex", &theorem_using("thm:r", "lem:x"));
    p.write("r/plain.tex", "See Lemma~\\ref{lem:x}.\n");
    p.full();
    p.check("one declaration");
    p.write("d/b.tex", &lemma("lem:x"));
    p.pass(&["d/b.tex"]);
    p.check("two declarations");
    p.write("d/c.tex", &lemma("lem:x"));
    p.pass(&["d/c.tex"]);
    assert!(p.problems("r/r.tex").contains("matches 3 declarations"), "{}", p.problems("r/r.tex"));
    p.check("three declarations");
    p.remove("d/b.tex");
    p.pass(&["d/b.tex"]);
    assert!(p.problems("r/plain.tex").contains("matches 2 declarations"), "{}", p.problems("r/plain.tex"));
    p.check("three, then two");
    p.remove("d/c.tex");
    p.pass(&["d/c.tex"]);
    assert_eq!((p.unresolved("r/r.tex"), p.unresolved("r/plain.tex")), (0, 0));
    p.check("back to one");
    // Two more arrive in one pass and leave in one pass.
    p.write("d/b.tex", &lemma("lem:x"));
    p.write("d/c.tex", &lemma("lem:x"));
    p.pass(&["d/b.tex", "d/c.tex"]);
    p.check("three at once");
    p.remove("d/b.tex");
    p.remove("d/c.tex");
    p.pass(&["d/b.tex", "d/c.tex"]);
    assert_eq!((p.unresolved("r/r.tex"), p.unresolved("r/plain.tex")), (0, 0));
    p.check("two removed at once");
    // The original leaves while another arrives, in one pass.
    p.remove("d/a.tex");
    p.write("d/z.tex", &lemma("lem:x"));
    p.pass(&["d/a.tex", "d/z.tex"]);
    assert_eq!((p.unresolved("r/r.tex"), p.unresolved("r/plain.tex")), (0, 0));
    p.check("the declaration changed files in one pass");
    // One file declares the label twice (none is admitted), and once again.
    p.write("d/z.tex", &format!("{}{}", lemma("lem:x"), lemma("lem:x")));
    p.pass(&["d/z.tex"]);
    p.check("declared twice in one file");
    p.write("d/z.tex", &lemma("lem:x"));
    p.pass(&["d/z.tex"]);
    p.check("declared once in that file again");
    // A second file declares it twice while the first keeps its one.
    p.write("d/y.tex", &format!("{}{}", lemma("lem:x"), lemma("lem:x")));
    p.pass(&["d/y.tex"]);
    p.check("another file declares it twice");
    p.write("d/y.tex", "Nothing.\n");
    p.pass(&["d/y.tex"]);
    assert_eq!((p.unresolved("r/r.tex"), p.unresolved("r/plain.tex")), (0, 0));
    p.check("the double declaration removed");
}

/// More declarations of one label than one read of a key returns (64):
/// the count a reference reports depends on whether the declarations were
/// created in the pass (all are counted) or stored (64 are read).
#[test]
fn a_label_declared_in_more_files_than_one_read_of_a_key_returns() {
    let p = open(None);
    for i in 0..70 {
        p.write(&format!("d/f{i}.tex"), &lemma("lem:x"));
    }
    p.write("r/r.tex", &theorem_using("thm:r", "lem:x"));
    p.full();
    println!("70 declarations: {}", p.problems("r/r.tex"));
    p.check("seventy declarations");
    p.write("d/more.tex", &lemma("lem:x"));
    p.pass(&["d/more.tex"]);
    println!("71 declarations: {}", p.problems("r/r.tex"));
    p.check("one more declaration");
}

/// The same label, back to one declaration: the references resolve.
#[test]
fn a_label_declared_in_many_files_resolves_when_one_is_left() {
    let p = open(None);
    for i in 0..70 {
        p.write(&format!("d/f{i}.tex"), &lemma("lem:x"));
    }
    p.write("r/r.tex", &theorem_using("thm:r", "lem:x"));
    p.full();
    p.write("r/new.tex", "See \\ref{lem:x}.\n");
    p.remove("d/f3.tex");
    p.pass(&["r/new.tex", "d/f3.tex"]);
    p.write("d/f3.tex", &lemma("lem:x"));
    p.pass(&["d/f3.tex"]);
    // All but one are removed, in one pass over the folder.
    for i in 0..70 {
        if i != 3 {
            p.remove(&format!("d/f{i}.tex"));
        }
    }
    p.pass(&["d"]);
    assert_eq!((p.unresolved("r/r.tex"), p.unresolved("r/new.tex")), (0, 0), "{}", p.problems("r/r.tex"));
    p.check("back to one declaration");
}

#[test]
fn a_reference_and_a_declaration_that_arrive_in_one_pass() {
    let p = open(None);
    p.write("d/a.tex", &lemma("lem:x"));
    p.write("r/old.tex", "See Lemma~\\ref{lem:x} and Lemma~\\ref{lem:later}.\n");
    p.full();
    // A new reference and the second declaration of its label.
    p.write("r/new.tex", &theorem_using("thm:n", "lem:x"));
    p.write("d/b.tex", &lemma("lem:x"));
    p.pass(&["r/new.tex", "d/b.tex"]);
    assert!(p.problems("r/new.tex").contains("matches 2 declarations"), "{}", p.problems("r/new.tex"));
    p.check("a reference and the second declaration");
    // A new reference and the removal of the second declaration.
    p.write("r/newer.tex", "See Lemma~\\ref{lem:x}.\n");
    p.remove("d/b.tex");
    p.pass(&["r/newer.tex", "d/b.tex"]);
    assert_eq!((p.unresolved("r/new.tex"), p.unresolved("r/newer.tex")), (0, 0));
    p.check("a reference and the removal of the second declaration");
    // A reference and the first declaration of its label, and a file that
    // declares a label and refers to it itself while another declares it too.
    p.write("r/both.tex", &format!("{}See Lemma~\\ref{{lem:later}} and \\ref{{lem:own}}.\n", lemma("lem:own")));
    p.write("d/later.tex", &lemma("lem:later"));
    p.write("d/own.tex", &lemma("lem:own"));
    p.pass(&["r/both.tex", "d/later.tex", "d/own.tex"]);
    assert_eq!(p.unresolved("r/old.tex"), 0, "{}", p.problems("r/old.tex"));
    p.check("a reference and the first declaration");
    // The file's own declaration goes: its reference meets the other one.
    p.write("r/both.tex", "See Lemma~\\ref{lem:later} and \\ref{lem:own}.\n");
    p.pass(&["r/both.tex"]);
    p.check("the own declaration removed");
    // Two labels swap files in one pass.
    p.write("d/later.tex", &lemma("lem:own"));
    p.write("d/own.tex", &lemma("lem:later"));
    p.pass(&["d/later.tex", "d/own.tex"]);
    p.check("two labels swapped files");
}

#[test]
fn a_removed_declaration_with_dependents_returns() {
    let p = open(None);
    let body = lemma("lem:x");
    p.write("d/a.tex", &body);
    p.write("d/t.tex", &theorem_using("thm:t", "lem:x"));
    p.write("r/r.tex", "See Lemma~\\ref{lem:x}.\n");
    p.full();
    let lem = p.keyed("lem:x").remove(0);
    let thm = p.keyed("thm:t").remove(0);
    assert!(relate(&p.svc, RelationKind::DependsOn, &thm, &lem));
    review_evidence(&p.svc, &lem, "d/a.tex");
    p.check("curated records");
    // Removed by an edit: tombstoned, the references dangle.
    p.write("d/a.tex", "Nothing here.\n");
    p.pass(&["d/a.tex"]);
    assert!(p.keyed("lem:x").is_empty());
    assert_eq!(p.unresolved("r/r.tex"), 1);
    p.check("the declaration removed");
    // Exactly the same text returns: the same entity, the references resolve.
    p.write("d/a.tex", &body);
    p.pass(&["d/a.tex"]);
    assert_eq!(p.keyed("lem:x"), std::slice::from_ref(&lem), "revived, not minted again");
    assert_eq!((p.unresolved("r/r.tex"), p.unresolved("d/t.tex")), (0, 0), "{}", p.problems("r/r.tex"));
    p.check("the declaration revived");
    // The file is deleted and returns unchanged in a later pass.
    p.remove("d/a.tex");
    p.pass(&["d/a.tex"]);
    assert_eq!(p.unresolved("r/r.tex"), 1);
    p.check("the declaring file deleted");
    p.write("d/a.tex", &body);
    p.pass(&["d/a.tex"]);
    assert_eq!(p.keyed("lem:x"), std::slice::from_ref(&lem));
    assert_eq!(p.unresolved("r/r.tex"), 0, "{}", p.problems("r/r.tex"));
    p.check("the declaring file returned");
    // It returns in another file while a second declaration exists already.
    p.remove("d/a.tex");
    p.write("d/b.tex", &lemma("lem:x").replace("A statement", "Another statement"));
    p.pass(&["d/a.tex", "d/b.tex"]);
    p.check("replaced by another declaration");
    p.write("d/a.tex", &body);
    p.pass(&["d/a.tex"]);
    assert!(p.problems("r/r.tex").contains("matches 2 declarations"), "{}", p.problems("r/r.tex"));
    p.check("revived beside a second declaration");
    p.remove("d/b.tex");
    p.pass(&["d/b.tex"]);
    assert_eq!(p.unresolved("r/r.tex"), 0, "{}", p.problems("r/r.tex"));
    assert_eq!(p.keyed("lem:x"), [lem]);
    let kept = sql(&p.svc, &format!("SELECT count(*) FROM relations WHERE from_id = '{thm}' AND kind = 'dependsOn'"));
    assert_eq!(kept[0][0], "1", "the curated relation is kept");
    p.check("the second declaration removed");
}

/// A relation a person accepted between the two ends of a link the source
/// states stands in place of the observed one.
#[test]
fn an_accepted_relation_in_place_of_an_observed_link_follows_the_label() {
    let p = open(None);
    p.write("d/a.tex", &lemma("lem:x"));
    p.write("d/t.tex", "\\begin{theorem}\\label{thm:t}\nNo reference yet.\n\\end{theorem}\n");
    p.full();
    let lem = p.keyed("lem:x").remove(0);
    let thm = p.keyed("thm:t").remove(0);
    // Accepted by a person before the source states it.
    assert!(relate(&p.svc, RelationKind::References, &thm, &lem));
    p.write("d/t.tex", &theorem_using("thm:t", "lem:x"));
    p.pass(&["d/t.tex"]);
    let stored =
        sql(&p.svc, &format!("SELECT origin, status FROM relations WHERE from_id = '{thm}' AND to_id = '{lem}'"));
    println!("the relation between the ends of the stated link: {stored:?}");
    p.check("an accepted relation");
    p.write("d/b.tex", &lemma("lem:x"));
    p.pass(&["d/b.tex"]);
    p.check("a second declaration under an accepted relation");
    p.remove("d/b.tex");
    p.pass(&["d/b.tex"]);
    p.check("unique again under an accepted relation");
    p.write("d/a.tex", &lemma("lem:y"));
    p.pass(&["d/a.tex"]);
    p.check("the label renamed under an accepted relation");
    p.write("d/a.tex", &lemma("lem:x"));
    p.pass(&["d/a.tex"]);
    p.check("the label renamed back under an accepted relation");
}

/// Bibliography entries: the generator of the differential test never writes
/// a `.bib` file.
#[test]
fn citations_follow_bibliography_entries_and_labels_of_the_same_name() {
    let p = open(None);
    let entry = |k: &str| format!("@article{{{k}, title = {{Paper {k}}}, year = 2001}}\n");
    p.write("refs.bib", &entry("k1"));
    p.write("a.tex", "Cites \\cite{k1} and \\cite{k2}.\n");
    p.write("n.md", "# Note\n\nSee [@k1] and [@k2].\n");
    p.write("t.typ", "= Title <top>\nSee @k1 and @k2.\n");
    p.full();
    p.check("one bibliography");
    let all = ["a.tex", "n.md", "t.typ"];
    let state = |p: &Project| -> Vec<u32> { all.iter().map(|f| p.unresolved(f)).collect() };
    let first = state(&p);
    p.write("refs.bib", &format!("{}{}", entry("k1"), entry("k2")));
    p.pass(&["refs.bib"]);
    p.check("an entry added");
    assert_ne!(state(&p), first, "the citations of k2 resolved");
    p.write("more/other.bib", &entry("k2"));
    p.pass(&["more/other.bib"]);
    p.check("an entry of the same key in a second bibliography");
    p.write("refs.bib", &entry("k1"));
    p.pass(&["refs.bib"]);
    p.check("the first entry of the key removed");
    // A label of the name of an entry.
    p.write("lab.tex", &lemma("k2"));
    p.pass(&["lab.tex"]);
    p.check("a label named as a bibliography entry");
    p.write("lab.typ", "#lemma[A Typst lemma] <k1>\n");
    p.pass(&["lab.typ"]);
    p.check("a Typst label named as a bibliography entry");
    p.remove("more/other.bib");
    p.pass(&["more/other.bib"]);
    p.check("the bibliography deleted");
    p.remove("lab.tex");
    p.remove("lab.typ");
    p.pass(&["lab.tex", "lab.typ"]);
    p.check("the labels deleted");
    p.remove("refs.bib");
    p.pass(&["refs.bib"]);
    p.check("no bibliography");
}

// ----- 2. the Lake package roots -----

fn lean_modules(p: &Project) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for row in sql(&p.svc, "SELECT id FROM entities WHERE kind = 'formalDeclaration' AND lifecycle != 'tombstoned'") {
        let e = p.svc.with_store(|s| Ok(s.entity(&EntityId::parse(&row[0]).unwrap())?)).unwrap().unwrap();
        out.push((e.key.unwrap_or_default(), e.attributes.get("lean.module").cloned().unwrap_or_default()));
    }
    out.sort();
    out
}

fn lean(namespace: &str, name: &str, imports: &[&str]) -> String {
    let mut t = String::new();
    for i in imports {
        t.push_str(&format!("import {i}\n"));
    }
    t.push_str(&format!("namespace {namespace}\ntheorem {name} : 1 = 1 := rfl\nend {namespace}\n"));
    t
}

#[test]
fn a_lakefile_and_lean_files_change_in_one_pass() {
    let p = open(None);
    p.write("pkg/P/L.lean", &lean("P", "t", &[]));
    p.write("pkg/P/M.lean", &lean("P", "u", &["P.L"]));
    p.write("pkg/P/Sub/N.lean", &lean("P", "v", &["P.M", "P.L"]));
    p.write("notes.md", "# Notes\n");
    p.full();
    p.check("no lakefile");
    // The lakefile appears while one Lean file is edited.
    p.write("pkg/lakefile.toml", "name = \"P\"\n");
    p.write("pkg/P/M.lean", &lean("P", "u2", &["P.L"]));
    p.pass(&["pkg/lakefile.toml", "pkg/P/M.lean"]);
    println!("{:?}", lean_modules(&p));
    p.check("a lakefile appears and a Lean file is edited");
    assert_eq!(p.links_to("pkg/P/M.lean", RelationKind::Imports), ["pkg/P/L.lean"]);
    // The lakefile moves to the project folder; one Lean file is renamed,
    // one is new, one is edited.
    p.rename("pkg/lakefile.toml", "lakefile.toml");
    p.rename("pkg/P/L.lean", "pkg/P/K.lean");
    p.write("pkg/P/New.lean", &lean("P", "w", &["pkg.P.K"]));
    p.write("pkg/P/M.lean", &lean("P", "u3", &["pkg.P.K", "P.L"]));
    p.pass(&["pkg/lakefile.toml", "lakefile.toml", "pkg/P/L.lean", "pkg/P/K.lean", "pkg/P/New.lean", "pkg/P/M.lean"]);
    println!("{:?}", lean_modules(&p));
    p.check("a lakefile moves while Lean files are renamed, added and edited");
    // The lakefile's text changes without moving.
    p.write("lakefile.toml", "name = \"Q\"\n[[lean_lib]]\nname = \"Q\"\nroots = [\"Q\"]\n");
    p.pass(&["lakefile.toml"]);
    p.check("the lakefile's text changed");
    // The lakefile disappears while a Lean file is edited and one deleted.
    p.remove("lakefile.toml");
    p.write("pkg/P/M.lean", &lean("P", "u4", &["pkg.P.K"]));
    p.remove("pkg/P/New.lean");
    p.pass(&["lakefile.toml", "pkg/P/M.lean", "pkg/P/New.lean"]);
    p.check("a lakefile disappears while Lean files are edited and deleted");
    // A folder that holds the package moves (the events name the folders).
    p.write("pkg/lakefile.lean", "import Lake\n");
    p.pass(&["pkg/lakefile.lean"]);
    p.check("a lakefile.lean appears");
    p.rename("pkg", "lib/inner");
    p.pass(&["pkg", "lib/inner"]);
    println!("{:?}", lean_modules(&p));
    p.check("the package folder moved");
    // The lakefile changes its kind in place (same root).
    p.rename("lib/inner/lakefile.lean", "lib/inner/lakefile.toml");
    p.pass(&["lib/inner/lakefile.lean", "lib/inner/lakefile.toml"]);
    p.check("lakefile.lean became lakefile.toml");
    // A new Lean file and the lakefile that makes its folder a root, with
    // only the new files named.
    p.write("fresh/lakefile.toml", "name = \"F\"\n");
    p.write("fresh/F/A.lean", &lean("F", "a", &["P.K"]));
    p.pass(&["fresh/lakefile.toml", "fresh/F/A.lean"]);
    println!("{:?}", lean_modules(&p));
    p.check("a new package with its first file");
}

#[test]
fn nested_lakefiles_appear_and_disappear() {
    let p = open(None);
    p.write("lakefile.toml", "name = \"Top\"\n");
    p.write("pkg/P/L.lean", &lean("P", "t", &[]));
    p.write("pkg/P/M.lean", &lean("P", "u", &["P.L", "pkg.P.L", "L"]));
    p.write("Top/A.lean", &lean("Top", "a", &["pkg.P.M"]));
    p.full();
    p.check("a lakefile in the project folder");
    p.write("pkg/lakefile.toml", "name = \"P\"\n");
    p.pass(&["pkg/lakefile.toml"]);
    p.check("a nested lakefile");
    p.write("pkg/P/lakefile.lean", "import Lake\n");
    p.pass(&["pkg/P/lakefile.lean"]);
    p.check("a lakefile nested twice");
    assert!(lean_modules(&p).contains(&("P.t".to_owned(), "L".to_owned())), "{:?}", lean_modules(&p));
    p.remove("pkg/lakefile.toml");
    p.pass(&["pkg/lakefile.toml"]);
    p.check("the middle lakefile removed");
    p.remove("lakefile.toml");
    p.remove("pkg/P/lakefile.lean");
    p.write("pkg/lakefile.lean", "import Lake\n");
    p.pass(&["lakefile.toml", "pkg/P/lakefile.lean", "pkg/lakefile.lean"]);
    p.check("three lakefile changes in one pass");
    // Both kinds of lakefile in one folder; one goes (the root stays).
    p.write("pkg/lakefile.toml", "name = \"P\"\n");
    p.pass(&["pkg/lakefile.toml"]);
    p.check("two lakefiles in one folder");
    p.remove("pkg/lakefile.lean");
    p.pass(&["pkg/lakefile.lean"]);
    p.check("one of two lakefiles in a folder removed");
    // The whole package folder is deleted (the event names the folder).
    std::fs::remove_dir_all(p.root.join("pkg")).unwrap();
    p.pass(&["pkg"]);
    p.check("the package folder deleted");
}

// ----- 3. lookup paths -----

#[test]
fn delete_and_recreate_within_one_pass() {
    let p = open(None);
    p.write("d/a.tex", &lemma("lem:x"));
    p.write("r.tex", "See \\ref{lem:x} and \\ref{lem:y}.\n\\input{d/a}\n\\includegraphics{d/pic}\n");
    p.write("d/pic.png", "png one");
    p.full();
    p.check("start");
    // Deleted and written again with another label before the pass.
    p.remove("d/a.tex");
    p.write("d/a.tex", &lemma("lem:y"));
    p.pass(&["d/a.tex"]);
    p.check("a file deleted and written again");
    // The figure is replaced by one of another type; the pass sees both.
    p.remove("d/pic.png");
    p.write("d/pic.pdf", "pdf");
    p.pass(&["d/pic.png", "d/pic.pdf"]);
    p.check("a figure replaced by another type");
    // A file gives way to a folder of its name, and back.
    p.remove("d/a.tex");
    p.write("d/a.tex/inner.tex", &lemma("lem:x"));
    p.pass(&["d/a.tex"]);
    p.check("a file replaced by a folder of its name");
    std::fs::remove_dir_all(p.root.join("d/a.tex")).unwrap();
    p.write("d/a.tex", &lemma("lem:x"));
    p.pass(&["d/a.tex"]);
    p.check("a folder replaced by a file of its name");
    // The whole folder is deleted and recreated with other files.
    std::fs::remove_dir_all(p.root.join("d")).unwrap();
    p.write("d/pic.svg", "svg");
    p.write("d/b.tex", &lemma("lem:y"));
    p.pass(&["d"]);
    p.check("a folder deleted and recreated");
}

#[test]
fn a_folder_of_waiting_files_and_a_folder_of_targets_are_renamed() {
    let p = open(None);
    p.write("main.tex", "\\documentclass{article}\n\\begin{document}\n\\input{ch/c}\n\\end{document}\n");
    p.write(
        "ch/c.tex",
        "\\section{C}\n\\input{parts/p}\n\\includegraphics{fig/missing}\nSee \\ref{lem:nowhere}.\n% exposition:begin plot1\n\\includegraphics{out/plot}\n% exposition:end plot1\n",
    );
    p.write("ch/deep/d.tex", "\\input{../parts/p}\n\\includegraphics{pics/one}\n");
    p.write("ch/n.md", "# N\n\nSee [p](parts/p.tex) and ![one](deep/pics/one.png).\n");
    p.full();
    p.check("waiting files");
    println!("ch/c.tex waits for {:?}", p.waits_for("ch/c.tex"));
    // The folder of the waiting files is renamed.
    p.rename("ch", "chap");
    p.pass(&["ch", "chap"]);
    println!("chap/c.tex waits for {:?}", p.waits_for("chap/c.tex"));
    p.check("the folder of the waiting files renamed");
    // Their targets arrive under the new folder name, one pass each.
    for (path, body) in [
        ("chap/parts/p.tex", "\\section{P}\n"),
        ("chap/fig/missing.png", "png"),
        ("chap/deep/pics/one.png", "png"),
        ("chap/out/plot.svg", "svg"),
    ] {
        p.write(path, body);
        p.pass(&[path]);
        p.check(&format!("{path} arrived below the renamed folder"));
    }
    assert_eq!(p.links_to("chap/c.tex", RelationKind::Imports), ["chap/parts/p.tex"]);
    // A file that arrives below the old folder name is not their target.
    p.write("ch/fig/other.png", "png");
    p.pass(&["ch/fig/other.png"]);
    p.check("a file below the old folder name");
    // The folders of the targets are renamed.
    p.rename("chap/parts", "chap/pieces");
    p.pass(&["chap/parts", "chap/pieces"]);
    assert!(p.links_to("chap/c.tex", RelationKind::Imports).is_empty());
    p.check("the folder of a target renamed");
    p.rename("chap/pieces", "chap/parts");
    p.rename("chap/fig", "fig");
    p.pass(&["chap/parts", "chap/pieces", "chap/fig", "fig"]);
    p.check("one target folder renamed back, one moved to the project folder");
    // The waiting files' folder moves below a folder that is new.
    p.rename("chap", "book/chap");
    p.pass(&["chap", "book"]);
    p.check("the folder moved below a new folder");
    // Only the new name is reported.
    p.rename("book", "volume");
    p.pass(&["volume"]);
    p.pass(&["book"]);
    p.check("a folder rename reported in two passes");
}

#[test]
fn latex_main_files_move_with_the_files_looked_up_below_them() {
    let p = open(None);
    let main = "\\documentclass{article}\n\\begin{document}\n\\input{../ch/c}\n\\end{document}\n";
    p.write("paper/main.tex", main);
    p.write("paper/fig/q.png", "png");
    p.write("paper/sec/s.tex", "\\section{S}\n");
    p.write("ch/c.tex", "\\section{C}\n\\includegraphics{fig/q}\n\\input{sec/s}\n\\includegraphics{fig/never}\n");
    p.write("ch/own.tex", "\\includegraphics{fig/q}\n");
    p.full();
    assert_eq!(p.links_to("ch/c.tex", RelationKind::UsesAsset), ["paper/fig/q.png"]);
    p.check("one main file");
    // The main file moves to another folder (its folder leaves the lookups).
    p.rename("paper/main.tex", "draft/main.tex");
    p.pass(&["paper/main.tex", "draft/main.tex"]);
    assert!(p.links_to("ch/c.tex", RelationKind::UsesAsset).is_empty());
    p.check("the main file moved");
    // A second main file in the same folder; the first is deleted.
    p.write("draft/alt.tex", main);
    p.pass(&["draft/alt.tex"]);
    p.check("a second main file in the folder");
    p.remove("draft/main.tex");
    p.pass(&["draft/main.tex"]);
    p.check("one of two main files of a folder deleted");
    // A main file and a figure below it arrive in one pass.
    p.write("other/main2.tex", main);
    p.write("other/fig/q.pdf", "pdf");
    p.write("other/fig/never.svg", "svg");
    p.pass(&["other/main2.tex", "other/fig/q.pdf", "other/fig/never.svg"]);
    assert_eq!(p.links_to("ch/c.tex", RelationKind::UsesAsset), ["other/fig/never.svg", "other/fig/q.pdf"]);
    p.check("a main file and its figures arrive together");
    // The folder of the main file is renamed.
    p.rename("other", "another");
    p.pass(&["other", "another"]);
    assert_eq!(p.links_to("ch/c.tex", RelationKind::UsesAsset), ["another/fig/never.svg", "another/fig/q.pdf"]);
    p.check("the folder of the main file renamed");
    // One main file stops being one while a file elsewhere becomes one.
    p.write("another/main2.tex", "\\section{Plain}\n");
    p.write("paper/top.tex", main);
    p.pass(&["another/main2.tex", "paper/top.tex"]);
    assert_eq!(p.links_to("ch/c.tex", RelationKind::UsesAsset), ["paper/fig/q.png"]);
    assert_eq!(p.links_to("ch/c.tex", RelationKind::Imports), ["paper/sec/s.tex"]);
    p.check("the main file swapped folders in one pass");
    // An earlier lookup folder takes the figure, with the main set unchanged.
    p.write("fig/q.svg", "svg");
    p.pass(&["fig/q.svg"]);
    assert_eq!(p.links_to("ch/c.tex", RelationKind::UsesAsset), ["fig/q.svg"]);
    p.check("the project folder takes the figure");
    p.remove("fig/q.svg");
    p.remove("paper/top.tex");
    p.pass(&["fig/q.svg", "paper/top.tex"]);
    p.check("the figure and the main file removed in one pass");
    // The last main file (draft/alt.tex) is deleted with its folder.
    std::fs::remove_dir_all(p.root.join("draft")).unwrap();
    p.pass(&["draft"]);
    p.check("no main file");
}

#[test]
fn python_packages_and_modules_appear_on_the_import_path() {
    let p = open(None);
    p.write(
        "tools/run.py",
        "import helper\nfrom pkg import mod\nimport pkg\nfrom pkg.sub import deep\nimport numpy\nX = 1\n",
    );
    p.write("top.py", "import helper\nimport pkg.mod\n");
    p.full();
    println!("tools/run.py waits for {:?}", p.waits_for("tools/run.py"));
    p.check("nothing to import");
    let steps: &[&str] = &[
        "helper/__init__.py",
        "tools/helper.py",
        "pkg/__init__.py",
        "pkg/mod.py",
        "pkg/sub/__init__.py",
        "pkg/sub/deep.py",
        "tools/pkg/__init__.py",
        "tools/pkg/mod/__init__.py",
        "helper.py",
        "pkg.py",
    ];
    for path in steps {
        p.write(path, "Y = 2\n");
        p.pass(&[path]);
        p.check(&format!("{path} appeared"));
    }
    println!("tools/run.py imports {:?}", p.links_to("tools/run.py", RelationKind::Imports));
    assert!(!p.links_to("tools/run.py", RelationKind::Imports).is_empty());
    for path in steps.iter().rev() {
        p.remove(path);
        p.pass(&[path]);
        p.check(&format!("{path} removed"));
    }
    // A package folder appears and is renamed (the events name the folder).
    p.write("pkg/__init__.py", "Y = 2\n");
    p.write("pkg/mod.py", "Y = 3\n");
    p.pass(&["pkg"]);
    p.check("a package folder appeared");
    p.rename("pkg", "pkg2");
    p.pass(&["pkg", "pkg2"]);
    p.check("the package folder renamed");
    let _ = std::fs::remove_dir_all(p.root.join("tools/pkg"));
    p.rename("pkg2", "tools/pkg");
    p.pass(&["pkg2", "tools/pkg"]);
    p.check("the package folder moved beside the script");
}

// ----- 4. unsaved buffers and curated records -----

#[test]
fn overlays_of_unsaved_buffers_follow_other_files() {
    let mut p = open(None);
    p.write("d/a.tex", &lemma("lem:x"));
    p.write("r/r.tex", &theorem_using("thm:r", "lem:x"));
    p.write("fig/p.png", "png");
    p.write("lakefile.toml", "name = \"P\"\n");
    p.write("P/L.lean", &lean("P", "t", &[]));
    p.write("P/M.lean", &lean("P", "u", &[]));
    p.full();
    p.buffers = vec![
        (
            "r/r.tex".into(),
            7,
            format!(
                "{}{}See \\ref{{lem:z}}.\n\\includegraphics{{fig/p}}\n\\includegraphics{{fig/late}}\n",
                theorem_using("thm:r", "lem:x"),
                theorem_using("thm:s", "lem:x")
            ),
        ),
        ("P/M.lean".into(), 3, lean("P", "u", &["P.L", "P.Late"])),
        ("d/a.tex".into(), 4, format!("{}{}", lemma("lem:x"), lemma("lem:z"))),
    ];
    p.check("typed");
    p.write("d/b.tex", &lemma("lem:x"));
    p.pass(&["d/b.tex"]);
    p.check("a second declaration of a label an unsaved buffer refers to");
    p.write("d/z.tex", &lemma("lem:z"));
    p.write("fig/late.pdf", "pdf");
    p.write("P/Late.lean", &lean("P", "late", &[]));
    p.pass(&["d/z.tex", "fig/late.pdf", "P/Late.lean"]);
    p.check("the targets an unsaved buffer waits for");
    p.rename("lakefile.toml", "P/lakefile.toml");
    p.pass(&["lakefile.toml", "P/lakefile.toml"]);
    p.check("the lakefile moved under unsaved Lean text");
    // The saved file of a buffer changes below it; another is deleted.
    p.write("r/r.tex", "See \\ref{lem:x}.\n");
    p.remove("d/a.tex");
    p.pass(&["r/r.tex", "d/a.tex"]);
    p.check("the files of unsaved buffers changed and were deleted");
}

/// An overlay resolves a buffer's links against the session's index of the
/// project's labels and LaTeX main files, as a pass does. A new session (the
/// project attached again) has not read that index until its first pass;
/// an overlay computed before then must still see every label and main file
/// a fresh index has: a section label (not an entity), a label declared
/// twice in one file (ambiguous, so no entity either), and the folder of a
/// main file that an `\input` is looked up in.
#[test]
fn an_overlay_in_a_new_session_sees_the_labels_a_fresh_index_has() {
    let mut p = open(None);
    p.write("notes/a.tex", "\\section{Intro}\\label{sec:intro}\nText.\n");
    p.write("notes/twice.tex", &format!("{}{}", lemma("lem:twice"), lemma("lem:twice")));
    p.write("paper/main.tex", "\\documentclass{article}\n\\begin{document}\n\\input{ch/one}\n\\end{document}\n");
    p.write("paper/ch/one.tex", "One.\n");
    p.write("paper/ch/two.tex", "Two.\n");
    p.write("notes/r.tex", "See \\ref{sec:intro}.\n");
    p.full();
    p.buffers = vec![
        ("notes/r.tex".into(), 3, "See \\ref{sec:intro} and \\ref{lem:twice}.\n".into()),
        ("paper/ch/one.tex".into(), 2, "One.\n\\input{ch/two}\n".into()),
    ];
    p.check("in the session that indexed the project");
    p.svc.detach();
    assert!(matches!(p.svc.attach(&p.root).state, GraphState::Attached));
    p.check("in a new session, before its first pass");
    // The overlay is the same once a pass has read the index.
    p.pass(&["notes/a.tex"]);
    p.check("after the new session's first pass");
}

// ----- 5. more awaited targets than a list holds -----

fn many_references(n: usize) -> String {
    let mut t = String::from("\\section{Many}\n");
    for i in 0..n {
        t.push_str(&format!("See \\ref{{miss:{i}}}.\n"));
    }
    t.push_str("\\includegraphics{fig/missing}\n\\input{parts/absent}\n");
    t
}

#[test]
fn a_file_that_waits_for_more_targets_than_a_list_holds() {
    let p = open(None);
    p.write("many.tex", &many_references(260));
    p.write("few.tex", "See \\ref{miss:3}.\n");
    p.write("P/L.lean", &lean("P", "t", &[]));
    p.full();
    assert_eq!(p.waits_for("many.tex"), ["*"]);
    assert_eq!(p.unresolved("many.tex"), 262, "{}", p.problems("many.tex"));
    p.check("more awaited targets than a list holds");
    p.write("d/a.tex", &lemma("miss:7"));
    p.pass(&["d/a.tex"]);
    assert_eq!(p.unresolved("many.tex"), 261);
    p.check("one of them declared");
    p.write("d/b.tex", &lemma("miss:7"));
    p.pass(&["d/b.tex"]);
    assert_eq!(p.unresolved("many.tex"), 262);
    p.check("declared twice");
    p.remove("d/b.tex");
    p.write("fig/missing.png", "png");
    p.pass(&["d/b.tex", "fig/missing.png"]);
    assert_eq!(p.unresolved("many.tex"), 260);
    p.check("unique again, the figure arrived");
    p.write("paper/main.tex", "\\documentclass{article}\n\\begin{document}\nText.\n\\end{document}\n");
    p.write("paper/parts/absent.tex", "\\section{Present}\n");
    p.pass(&["paper/parts/absent.tex"]);
    p.pass(&["paper/main.tex"]);
    assert_eq!(p.unresolved("many.tex"), 259, "{}", p.problems("many.tex"));
    p.check("a main file whose folder holds the input");
    p.remove("paper/main.tex");
    p.pass(&["paper/main.tex"]);
    assert_eq!(p.unresolved("many.tex"), 260);
    p.check("the main file removed");
    // Back below the limit: the list names its targets again.
    p.write("many.tex", &many_references(20));
    p.pass(&["many.tex"]);
    assert!(!p.waits_for("many.tex").contains(&"*".to_owned()), "{:?}", p.waits_for("many.tex"));
    p.check("fewer awaited targets");
    // Exactly at and one past the limit (the list holds 200).
    for n in [196, 197, 198, 199, 200, 201] {
        p.write("many.tex", &many_references(n));
        p.pass(&["many.tex"]);
        p.write("d/edge.tex", &lemma(&format!("miss:{}", n - 1)));
        p.pass(&["d/edge.tex"]);
        p.check(&format!("{n} dangling references"));
    }
}

// ----- 6. cost (statement counters, no clocks) -----

/// A project of `noise` unrelated files, `referrers` files that refer to
/// `lem:hot`, and `waiting` files that wait for the same missing figure.
fn cost_project(noise: usize, referrers: usize, waiting: usize, capped: usize) -> Project {
    let p = open(None);
    for i in 0..noise {
        p.write(
            &format!("noise/n{}/f{i}.tex", i % 10),
            &format!("{}See \\ref{{noise:{}}}.\n", lemma(&format!("noise:{i}")), i.saturating_sub(1)),
        );
    }
    for i in 0..referrers {
        p.write(&format!("refs/r{i}.tex"), "See Lemma~\\ref{lem:hot}.\n");
    }
    for i in 0..waiting {
        p.write(&format!("wait/w{i}.tex"), "\\includegraphics{fig/hot-missing}\n\\input{parts/hot-missing}\n");
    }
    for i in 0..capped {
        p.write(&format!("capped/c{i}.tex"), &many_references(260));
    }
    p.write("main.tex", "\\documentclass{article}\n\\begin{document}\nText.\n\\end{document}\n");
    p.write("d/hot.tex", &lemma("lem:hot"));
    p.write("d/edited.tex", &lemma("lem:edited"));
    p.write("d/second.tex", "Nothing yet.\n");
    p.full();
    count_sql(&p.svc);
    p
}

/// The SQL work of one pass over `path` after it is written.
fn pass_cost(p: &Project, path: &str, body: &str) -> SqlWork {
    p.write(path, body);
    // The mark is a write: the first status after it counts the tables
    // (whoever asks first), so it is taken before the measurement.
    p.svc.mark_pending(&[wp(path)]).unwrap();
    p.svc.status();
    let options = ReconcileOptions { scope: Scope::Paths(vec![wp(path)]), force: false };
    let (report, work) = sql_of(|| p.svc.reconcile(&options).unwrap());
    assert_eq!(report.read, 1, "{report:?}");
    work
}

#[test]
fn the_cost_of_a_label_change_follows_its_references_not_the_project() {
    let measure = |noise: usize, referrers: usize| -> (SqlWork, SqlWork, SqlWork) {
        let p = cost_project(noise, referrers, 0, 0);
        // Warm-up, then: an edit that keeps the hot label; a second
        // declaration of the hot label; an unrelated new label.
        pass_cost(&p, "d/edited.tex", &format!("{}Warm.\n", lemma("lem:edited")));
        let keep = pass_cost(&p, "d/hot.tex", &format!("{}More text.\n", lemma("lem:hot")));
        let second = pass_cost(&p, "d/second.tex", &lemma("lem:hot"));
        assert!(p.problems("refs/r0.tex").contains("matches 2 declarations"), "{}", p.problems("refs/r0.tex"));
        let unrelated = pass_cost(&p, "d/edited.tex", &format!("{}{}", lemma("lem:edited"), lemma("lem:fresh")));
        (keep, second, unrelated)
    };
    let small = measure(60, 10);
    let large = measure(360, 10);
    let many = measure(60, 60);
    println!(
        "COST label: noise 60/refs 10 {small:?}\nCOST label: noise 360/refs 10 {large:?}\nCOST label: noise 60/refs 60 {many:?}"
    );
    let same = |what: &str, a: SqlWork, b: SqlWork| {
        assert!(
            b.statements <= a.statements + 6 && b.rows <= a.rows + 40 && b.fullscan_steps <= a.fullscan_steps + 40,
            "{what} costs more in the larger project:\n small {a:?}\n large {b:?}"
        );
    };
    same("an edit that keeps a label", small.0, large.0);
    same("a second declaration of a label", small.1, large.1);
    same("an unrelated new label", small.2, large.2);
    // An edit that keeps the label does not resolve the references to it.
    same("an edit that keeps a label with many references", small.0, many.0);
    // A second declaration costs the references: more of them, more work,
    // within a bound per reference.
    assert!(many.1.statements > small.1.statements, "{:?} / {:?}", small.1, many.1);
    assert!(many.1.statements <= small.1.statements + 50 * 40, "{:?} / {:?}", small.1, many.1);
}

#[test]
fn many_files_waiting_for_one_target_do_not_make_other_passes_expensive() {
    let measure = |waiting: usize, capped: usize| -> (SqlWork, SqlWork, SqlWork) {
        let p = cost_project(40, 5, waiting, capped);
        pass_cost(&p, "d/edited.tex", &format!("{}Warm.\n", lemma("lem:edited")));
        // An edit without a new target; an edit that declares a new label;
        // an edit that makes a second file a non-main LaTeX file again.
        let plain = pass_cost(&p, "d/edited.tex", &format!("{}Warm again.\n", lemma("lem:edited")));
        let label = pass_cost(&p, "d/edited.tex", &format!("{}{}", lemma("lem:edited"), lemma("lem:fresh")));
        let main = pass_cost(&p, "main.tex", "\\documentclass{article}\n\\begin{document}\nEdited.\n\\end{document}\n");
        (plain, label, main)
    };
    let few = measure(5, 0);
    let many = measure(205, 0);
    println!("COST waiting 5: {few:?}\nCOST waiting 205: {many:?}");
    for (what, a, b) in [
        ("an edit without a new target", few.0, many.0),
        ("an edit that declares an unrelated label", few.1, many.1),
        ("an edit of a main file that stays one", few.2, many.2),
    ] {
        assert!(
            b.statements <= a.statements + 6 && b.rows <= a.rows + 40,
            "{what} costs more with many files waiting for another target:\n few {a:?}\n many {b:?}"
        );
    }
    // Files past the list limit are resolved again by every pass that
    // introduces any target: reported, not asserted (the documented fallback).
    let capped = measure(5, 3);
    println!("COST capped 3 files (260 dangling references each): {capped:?}");
    assert!(
        capped.0.statements <= few.0.statements + 6,
        "an edit without a target wakes nothing: {capped:?} / {few:?}"
    );
}

// Work per statement text (SQLite's own counters, installed from the test).
thread_local! {
    static PROFILE: std::cell::RefCell<std::collections::BTreeMap<String, (u64, u64)>> =
        const { std::cell::RefCell::new(std::collections::BTreeMap::new()) };
}

unsafe extern "C" fn profile(
    event: std::ffi::c_uint,
    _ctx: *mut std::ffi::c_void,
    p: *mut std::ffi::c_void,
    _x: *mut std::ffi::c_void,
) -> std::ffi::c_int {
    if event == rusqlite::ffi::SQLITE_TRACE_PROFILE {
        let stmt = p.cast::<rusqlite::ffi::sqlite3_stmt>();
        // SAFETY: for a PROFILE event SQLite passes the prepared statement
        // that just finished; its text and counters are valid while the
        // callback runs.
        let (text, steps) = unsafe {
            let sql = rusqlite::ffi::sqlite3_sql(stmt);
            let text = if sql.is_null() {
                String::new()
            } else {
                std::ffi::CStr::from_ptr(sql).to_string_lossy().split_whitespace().collect::<Vec<_>>().join(" ")
            };
            (text, rusqlite::ffi::sqlite3_stmt_status(stmt, rusqlite::ffi::SQLITE_STMTSTATUS_VM_STEP, 1))
        };
        PROFILE.with(|m| {
            let mut m = m.borrow_mut();
            let entry = m.entry(text).or_default();
            entry.0 += 1;
            entry.1 += steps.max(0) as u64;
        });
    }
    0
}

/// (runs, virtual-machine steps) per statement text of `f`.
fn statements_of(svc: &GraphService, f: impl FnOnce()) -> std::collections::BTreeMap<String, (u64, u64)> {
    svc.with_store(|store| {
        // SAFETY: the connection is alive for the call; the callback is a
        // plain function that touches thread-local state only.
        unsafe {
            rusqlite::ffi::sqlite3_trace_v2(
                store.connection().handle(),
                rusqlite::ffi::SQLITE_TRACE_PROFILE,
                Some(profile),
                std::ptr::null_mut(),
            );
        }
        Ok(())
    })
    .unwrap();
    PROFILE.with(|m| m.borrow_mut().clear());
    f();
    PROFILE.with(|m| std::mem::take(&mut *m.borrow_mut()))
}

/// A second declaration of a label resolves the files that refer to it
/// again. The rows each of its statements visits must not follow the size
/// of the project (`docs/performance.md`: a change that concerns other files
/// costs those files and no more).
#[test]
fn resolving_the_references_to_a_label_visits_rows_of_those_files_only() {
    let measure = |noise: usize| {
        let p = cost_project(noise, 10, 0, 0);
        // Warm-up of the same shape (a second declaration of another label,
        // and its removal): a cached statement's counters accumulate from
        // its first use, so every statement of the measured pass must have
        // run, and been counted, once before.
        pass_cost(&p, "d/second.tex", &lemma("noise:0"));
        pass_cost(&p, "d/second.tex", "Nothing yet.\n");
        pass_cost(&p, "d/second.tex", &lemma("noise:1"));
        pass_cost(&p, "d/second.tex", "Nothing yet.\n");
        p.write("d/second.tex", &lemma("lem:hot"));
        p.svc.mark_pending(&[wp("d/second.tex")]).unwrap();
        p.svc.status();
        let options = ReconcileOptions { scope: Scope::Paths(vec![wp("d/second.tex")]), force: false };
        statements_of(&p.svc, || {
            p.svc.reconcile(&options).unwrap();
        })
    };
    let (small, large) = (measure(60), measure(360));
    let total = |m: &std::collections::BTreeMap<String, (u64, u64)>| m.values().map(|v| v.1).sum::<u64>();
    let mut grown: Vec<(u64, String)> = large
        .iter()
        .map(|(sql, (runs, steps))| {
            let before = small.get(sql).map(|v| v.1).unwrap_or(0);
            (steps.saturating_sub(before), format!("{runs} runs, {before} -> {steps} steps: {sql}"))
        })
        .filter(|(more, _)| *more > 200)
        .collect();
    grown.sort();
    grown.reverse();
    for (_, line) in &grown {
        println!("GROWS {}", clip(line));
    }
    println!("TOTAL steps: 60 noise files {}, 360 noise files {}", total(&small), total(&large));
    assert!(
        total(&large) <= total(&small) + 5_000,
        "the pass visits more rows in the larger project ({} -> {} steps):\n{}",
        total(&small),
        total(&large),
        grown.iter().map(|(_, l)| clip(l)).collect::<Vec<_>>().join("\n")
    );
}

// ----- 7. one key per file -----

#[test]
fn a_case_only_rename_keeps_declarations_dependents_and_curated_records() {
    let p = open(None);
    if !ignores_case(&p.root) {
        println!("SKIPPED: this file system distinguishes case");
        return;
    }
    p.write("Defs/a.tex", &format!("{}{}", lemma("lem:x"), "Long enough to be told apart. ".repeat(6)));
    p.write("Defs/fig/p.png", "png");
    p.write(
        "r/r.tex",
        &format!("{}\\input{{../Defs/a}}\n\\includegraphics{{../Defs/fig/p}}\n", theorem_using("thm:r", "lem:x")),
    );
    p.full();
    assert_eq!(p.unresolved("r/r.tex"), 0, "{}", p.problems("r/r.tex"));
    let file = p.resource("Defs/a.tex");
    let lem = p.keyed("lem:x").remove(0);
    let thm = p.keyed("thm:r").remove(0);
    assert!(relate(&p.svc, RelationKind::DependsOn, &thm, &lem));
    let between_files = relate(&p.svc, RelationKind::DependsOn, p.resource("r/r.tex").as_str(), file.as_str());
    println!("curated relation between the two files applied: {between_files}");
    let evidence = review_evidence(&p.svc, &lem, "Defs/a.tex");
    let curated = |p: &Project| -> (String, String) {
        (
            sql(&p.svc, "SELECT count(*) FROM relations WHERE origin LIKE '%user%'")[0][0].clone(),
            sql(&p.svc, &format!("SELECT subject FROM evidence WHERE id = '{evidence}'"))[0][0].clone(),
        )
    };
    let before = curated(&p);
    // The file, reported as its new name only.
    p.rename("Defs/a.tex", "Defs/A.tex");
    p.pass(&["Defs/A.tex"]);
    assert_eq!(p.resource("Defs/A.tex"), file);
    assert_eq!(p.keyed("lem:x"), std::slice::from_ref(&lem));
    p.check("the declaring file renamed in case, new name reported");
    // Back, reported as its old name only.
    p.rename("Defs/A.tex", "Defs/a.tex");
    p.pass(&["Defs/A.tex"]);
    assert_eq!(p.resource("Defs/a.tex"), file);
    p.check("renamed in case, old name reported");
    // The folder, reported in another case than either name.
    p.rename("Defs", "defs");
    p.pass(&["DEFS"]);
    assert_eq!(p.live_resources(), ["defs/a.tex", "defs/fig/p.png", "r/r.tex"]);
    assert_eq!(p.resource("defs/a.tex"), file);
    assert_eq!(p.keyed("lem:x"), std::slice::from_ref(&lem));
    p.check("the folder renamed in case, a third spelling reported");
    // The file and its folder renamed in case at once while it is edited.
    p.rename("defs/a.tex", "defs/A.tex");
    p.rename("defs", "DEFS");
    p.write("DEFS/A.tex", &format!("{}Edited. {}", lemma("lem:x"), "Long enough to be told apart. ".repeat(6)));
    p.pass(&["defs/a.tex", "DEFS/A.tex"]);
    // Only the file's events were delivered: the figure beside it stays
    // under the folder's old spelling until a pass covers it (documented).
    assert_eq!(p.live_resources(), ["DEFS/A.tex", "defs/fig/p.png", "r/r.tex"]);
    assert_eq!(p.resource("DEFS/A.tex"), file);
    let figure = p.resource("defs/fig/p.png");
    p.pass(&["defs"]);
    assert_eq!(p.live_resources(), ["DEFS/A.tex", "DEFS/fig/p.png", "r/r.tex"]);
    assert_eq!(p.resource("DEFS/fig/p.png"), figure, "the figure keeps its identity");
    p.check("file and folder renamed in case with an edit");
    assert_eq!(p.keyed("lem:x"), std::slice::from_ref(&lem), "the declaration keeps its identity");
    assert_eq!(curated(&p), before, "curated records are kept");
    let folders =
        sql(&p.svc, "SELECT key FROM entities WHERE kind = 'folder' AND lifecycle != 'tombstoned' ORDER BY key");
    assert_eq!(folders, [["DEFS"], ["DEFS/fig"], ["r"]], "one folder entity per folder");
}

/// Naming an existing file in another spelling never retires its resource
/// and never gives it a second one.
#[test]
fn another_spelling_of_an_existing_path_never_retires_or_doubles_it() {
    let p = open(None);
    if !ignores_case(&p.root) {
        println!("SKIPPED: this file system distinguishes case");
        return;
    }
    p.write("Notes/Sub/a.md", "# A\n");
    p.write("Notes/b.md", "# B\n\nSee [a](Sub/a.md).\n");
    p.full();
    let stored = p.live_resources();
    let before = records(&p.svc);
    let spellings: &[&[&str]] = &[
        &["notes/sub/A.MD"],
        &["NOTES"],
        &["Notes/SUB"],
        &["notes/sub/a.md", "Notes/Sub/a.md", "NOTES/SUB/A.MD"],
        &["notes/./sub//a.md"],
        &["Notes\\Sub\\A.md"],
        &["notes/b.MD", "notes/missing.md", "NOTES/gone/x.md"],
    ];
    for scope in spellings {
        let report = p.pass(scope);
        assert_eq!((report.resources_added, report.resources_removed, report.read), (0, 0, 0), "{scope:?}: {report:?}");
        assert_eq!(p.live_resources(), stored, "after a pass over {scope:?}");
    }
    assert_eq!(records(&p.svc), before, "nothing was written");
    for spelling in ["NOTES/SUB/A.MD", "notes/sub/a.md", "Notes/Sub/A.md"] {
        let e = p.svc.ensure_resource(&wp(spelling), None, false, "review").unwrap();
        assert_eq!(e.key.as_deref(), Some("Notes/Sub/a.md"), "{spelling}");
    }
    assert_eq!(p.live_resources(), stored);
    // Paths that leave the project or name it absolutely are not paths.
    for bad in [
        "../x.md",
        "Notes/../../x.md",
        "C:/x.md",
        "\\\\?\\C:\\x.md",
        "\\\\server\\share\\x.md",
        "Notes/a.md.",
        "Notes /a.md",
    ] {
        assert!(WorkspacePath::parse(bad).is_err(), "{bad} parses");
    }
    p.check("other spellings");
}

/// Letters whose case mapping in Rust differs from the file system's: a
/// spelling the file system folds but `to_lowercase` does not must not give
/// the file a second resource.
#[test]
fn spellings_the_file_system_folds_but_unicode_lowercase_does_not() {
    let p = open(None);
    if !ignores_case(&p.root) {
        println!("SKIPPED: this file system distinguishes case");
        return;
    }
    // (stored name, another spelling)
    let pairs = [
        ("\u{3b1}\u{3c2}.md", "\u{3b1}\u{3c3}.md"), // final sigma / sigma
        ("\u{3b1}\u{3c2}.md", "\u{391}\u{3a3}.md"), // upper case
        ("stra\u{df}e.md", "STRASSE.md"),
        ("stra\u{df}e.md", "STRA\u{1e9e}E.md"),
        ("\u{131}x.md", "Ix.md"), // dotless i
        ("\u{130}y.md", "iy.md"), // dotted capital I
        ("\u{130}y.md", "i\u{307}y.md"),
        ("\u{1c5}z.md", "\u{1c6}z.md"),  // titlecase digraph
        ("\u{17f}w.md", "Sw.md"),        // long s
        ("\u{212a}v.md", "kv.md"),       // Kelvin sign
        ("\u{3a9}u.md", "\u{2126}u.md"), // Omega / Ohm sign
        ("\u{e9}t.md", "\u{c9}t.md"),    // é / É (ordinary)
    ];
    let mut folded = Vec::new();
    for (stored, other) in pairs {
        let dir = format!("u{}", folded.len());
        p.write(&format!("{dir}/{stored}"), "# U\n");
        let same = p.root.join(&dir).join(other).exists();
        println!("file system folds {stored:?} and {other:?}: {same}");
        folded.push((format!("{dir}/{stored}"), format!("{dir}/{other}"), same));
    }
    p.full();
    let stored = p.live_resources();
    for (kept, other, same) in &folded {
        if !*same {
            continue;
        }
        let report = p.pass(&[other]);
        assert_eq!(
            (report.resources_added, report.resources_removed),
            (0, 0),
            "{other:?} names the file stored as {kept:?}: {report:?}"
        );
        assert_eq!(p.live_resources(), stored, "after a pass over {other:?}");
    }
    p.check("spellings the file system folds");
}

/// The short (8.3) name of a file reaches the file but is not its name.
#[test]
fn a_short_name_is_not_a_second_resource() {
    let p = open(None);
    p.write("docs/a long file name.md", "# Long\n");
    p.full();
    let listing = std::process::Command::new("cmd").args(["/C", "dir", "/X"]).arg(p.root.join("docs")).output();
    let short = listing.ok().and_then(|o| {
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .find(|l| l.contains("a long file name.md"))
            .and_then(|l| l.split_whitespace().find(|w| w.contains('~')).map(str::to_owned))
    });
    let Some(short) = short else {
        println!("SKIPPED: this volume keeps no short names");
        return;
    };
    assert!(p.root.join("docs").join(&short).exists());
    let stored = p.live_resources();
    let report = p.pass(&[&format!("docs/{short}")]);
    assert_eq!(p.live_resources(), stored, "a pass over docs/{short}: {report:?}");
    p.check("a short name");
}

/// A folder reached through a junction is not scanned, and a path through
/// it is never respelled to a key inside the project.
#[test]
fn a_path_through_a_junction_is_neither_scanned_nor_respelled() {
    let p = open(None);
    if !cfg!(windows) {
        println!("SKIPPED: junctions are a Windows feature");
        return;
    }
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(outside.path().join("DATA")).unwrap();
    std::fs::write(outside.path().join("DATA/a.md"), "# Outside\n").unwrap();
    p.write("real/b.md", "# B\n");
    let made = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(p.root.join("data"))
        .arg(outside.path().join("DATA"))
        .output()
        .is_ok_and(|o| o.status.success());
    if !made {
        println!("SKIPPED: no junction could be made");
        return;
    }
    p.full();
    assert_eq!(p.live_resources(), ["real/b.md"]);
    for scope in [vec!["data"], vec!["data/a.md"], vec!["DATA/a.md"], vec!["Data", "real"]] {
        p.pass(&scope);
        assert_eq!(p.live_resources(), ["real/b.md"], "after a pass over {scope:?}");
    }
    let spelled = reconcile::scan::on_disk_spelling(&p.root, &wp("data/a.md"));
    println!("on_disk_spelling(data/a.md) through a junction to .../DATA: {spelled}");
    std::fs::remove_dir(p.root.join("data")).unwrap();
}

/// A path through a junction whose target folder has the junction's name in
/// another case is respelled to the target's name: a key no path of the
/// project has.
#[test]
fn a_path_through_a_junction_keeps_its_own_spelling() {
    let p = open(None);
    if !cfg!(windows) {
        println!("SKIPPED: junctions are a Windows feature");
        return;
    }
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(outside.path().join("DATA")).unwrap();
    std::fs::write(outside.path().join("DATA/a.md"), "# Outside\n").unwrap();
    let made = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(p.root.join("data"))
        .arg(outside.path().join("DATA"))
        .output()
        .is_ok_and(|o| o.status.success());
    if !made {
        println!("SKIPPED: no junction could be made");
        return;
    }
    let spelled = reconcile::scan::on_disk_spelling(&p.root, &wp("data/a.md"));
    let admitted = p.svc.ensure_resource(&wp("data/a.md"), None, false, "review").map(|e| e.key);
    std::fs::remove_dir(p.root.join("data")).unwrap();
    assert_eq!(spelled.as_str(), "data/a.md");
    assert_eq!(admitted.ok().flatten().as_deref(), Some("data/a.md"));
}

/// Two real files on a folder that distinguishes case are never merged.
#[test]
fn two_files_that_differ_in_case_keep_their_declarations_apart() {
    let p = open(None);
    if ignores_case(&p.root) {
        let enabled = cfg!(windows)
            && std::process::Command::new("fsutil")
                .args(["file", "setCaseSensitiveInfo"])
                .arg(&p.root)
                .arg("enable")
                .output()
                .is_ok_and(|o| o.status.success());
        if !enabled || ignores_case(&p.root) {
            println!("SKIPPED: this host has no case-sensitive folder to test in");
            return;
        }
    }
    p.write("a.tex", &lemma("lem:lower"));
    p.write("A.tex", &lemma("lem:upper"));
    p.write("r.tex", "See \\ref{lem:lower} and \\ref{lem:upper}.\n\\input{a}\n\\input{A}\n");
    p.full();
    assert_eq!(p.live_resources(), ["A.tex", "a.tex", "r.tex"]);
    let (lower, upper) = (p.resource("a.tex"), p.resource("A.tex"));
    p.check("two files that differ in case");
    // One is deleted while only the other's name is reported, and the reverse.
    p.remove("a.tex");
    p.pass(&["A.tex"]);
    assert_eq!(p.resource("A.tex"), upper);
    p.pass(&["a.tex"]);
    assert_eq!(p.live_resources(), ["A.tex", "r.tex"]);
    p.check("one of the two deleted");
    // A file arrives beside one that differs in case only: it is new.
    p.write("a.tex", &lemma("lem:lower"));
    p.pass(&["a.tex"]);
    assert_eq!(p.live_resources(), ["A.tex", "a.tex", "r.tex"]);
    assert_eq!(p.resource("A.tex"), upper);
    let _ = lower;
    p.check("a file beside one that differs in case");
    // A real rename in case: the old name is gone, the new one exists.
    p.rename("a.tex", "b.tex");
    p.rename("A.tex", "B.tex");
    p.pass(&["a.tex", "A.tex", "b.tex", "B.tex"]);
    assert_eq!(p.resource("B.tex"), upper, "identity follows the file, not the letters");
    p.check("both renamed in one pass");
}

// ----- 8. a graph derived under earlier rules -----

fn rules_project() -> Project {
    let p = open(None);
    p.write("paper/main.tex", "\\documentclass{article}\n\\begin{document}\n\\input{../ch/c}\n\\end{document}\n");
    p.write(
        "ch/c.tex",
        &format!(
            "{}\\includegraphics{{fig/q}}\n\\includegraphics{{fig/p}}\nSee \\ref{{lem:far}} and \\ref{{lem:none}}.\n",
            lemma("lem:c")
        ),
    );
    p.write("fig/p.png", "png");
    p.write("d/far.tex", &lemma("lem:far"));
    p.write("d/far2.tex", &lemma("lem:far"));
    p.write("pkg/lakefile.toml", "name = \"P\"\n");
    p.write("pkg/P/L.lean", &lean("P", "t", &[]));
    p.write("pkg/P/M.lean", &lean("P", "u", &["P.L", "P.Absent"]));
    p.write("tools/run.py", "import helper\nfrom pkg import mod\n");
    p.write("refs.bib", "@article{k1, title = {Paper}, year = 2001}\n");
    p.write("n.md", "# N\n\nSee [@k1] and [@k9] and [c](ch/c.tex) and ![q](fig/q.png).\n");
    p.full();
    p
}

/// Make the stored graph look as an earlier build left it: no rules marker,
/// awaited lists in the earlier form (full candidate paths, no names for
/// ambiguous references, no token), stale module names.
fn age(p: &Project) {
    assert_eq!(sql(&p.svc, "SELECT value FROM meta WHERE key = 'link_rules'"), [["2"]]);
    exec(
        &p.svc,
        "DELETE FROM meta WHERE key = 'link_rules';
         UPDATE source_index SET wanted = '[]';
         UPDATE source_index SET wanted = '[\"path:ch/fig/q.pdf\",\"path:ch/fig/q.png\",\"path:ch/fig/q.jpg\",\"path:fig/q.pdf\",\"name:lem:none\"]' WHERE path = 'ch/c.tex';
         UPDATE source_index SET wanted = '[\"path:pkg/P/Absent.lean\"]' WHERE path = 'pkg/P/M.lean';
         UPDATE entities SET attributes = replace(attributes, '\"lean.module\":\"P.', '\"lean.module\":\"pkg.P.') WHERE kind = 'formalDeclaration';",
    );
    // A new session, as after opening the project with the newer build.
    p.svc.detach();
    let status = p.svc.attach(&p.root);
    assert!(matches!(status.state, GraphState::Attached), "{status:?}");
}

#[test]
fn a_graph_derived_under_earlier_rules_converges_in_one_full_pass() {
    let p = rules_project();
    let current = records(&p.svc);
    age(&p);
    assert_ne!(records(&p.svc), current);
    let report = p.full();
    assert_eq!(report.read, 0, "no file is read: {report:?}");
    assert_eq!(sql(&p.svc, "SELECT value FROM meta WHERE key = 'link_rules'"), [["2"]]);
    let after = records(&p.svc);
    let only: Vec<&String> = after.iter().filter(|l| !current.contains(l)).collect();
    assert!(after == current, "the full pass did not restore the records:\n{only:#?}");
    p.check("derived again");
    // Not repeated: the next full pass writes nothing.
    count_sql(&p.svc);
    let revision = p.svc.status().revision;
    let (report, work) = sql_of(|| p.full());
    println!("COST second full pass after the rules were recorded: {work:?}");
    assert_eq!((report.read, p.svc.status().revision), (0, revision), "{report:?}");
    assert_eq!(records(&p.svc), current);
    let changes = sql(&p.svc, "SELECT count(*) FROM changes")[0][0].clone();
    p.full();
    assert_eq!(sql(&p.svc, "SELECT count(*) FROM changes")[0][0], changes);
}

#[test]
fn scoped_passes_before_the_full_pass_do_not_record_the_rules() {
    let p = rules_project();
    age(&p);
    // Scoped passes over an aged graph: an edit, and the arrival of a file
    // an earlier list names by its full candidate path.
    p.write("d/far2.tex", "Nothing.\n");
    p.pass(&["d/far2.tex"]);
    p.write("ch/fig/q.png", "png");
    p.pass(&["ch/fig/q.png"]);
    assert!(
        sql(&p.svc, "SELECT value FROM meta WHERE key = 'link_rules'").is_empty(),
        "a scoped pass recorded the rules"
    );
    assert_eq!(
        p.links_to("ch/c.tex", RelationKind::UsesAsset),
        ["ch/fig/q.png", "fig/p.png"],
        "an earlier list is honoured"
    );
    // A reattach in between (the pass was never run: the app was closed).
    p.svc.detach();
    p.svc.attach(&p.root);
    let report = p.full();
    assert_eq!(report.read, 0, "{report:?}");
    assert_eq!(sql(&p.svc, "SELECT value FROM meta WHERE key = 'link_rules'"), [["2"]]);
    p.check("an aged graph after scoped passes and one full pass");
}

/// Another value of the marker (a later or an unknown build wrote it) is
/// treated as "other rules": derived again once, then recorded.
#[test]
fn an_unknown_rules_marker_is_derived_again_once() {
    let p = rules_project();
    for value in ["1", "3", ""] {
        exec(&p.svc, &format!("INSERT OR REPLACE INTO meta (key, value) VALUES ('link_rules', '{value}')"));
        exec(&p.svc, "UPDATE source_index SET wanted = '[\"path:nothing/at/all.tex\"]' WHERE path = 'ch/c.tex'");
        p.svc.detach();
        p.svc.attach(&p.root);
        let report = p.full();
        assert_eq!(report.read, 0, "{report:?}");
        assert_eq!(sql(&p.svc, "SELECT value FROM meta WHERE key = 'link_rules'"), [["2"]]);
        p.check(&format!("marker {value:?}"));
    }
}

// ----- 9. limits the change states it leaves -----

/// A deleted file named in another case than stored: nothing on disk gives
/// the spelling. Stated as not fixed; it must not create a second resource
/// and must converge at the next full pass.
#[test]
fn a_deleted_path_named_in_another_case_converges_at_the_next_full_pass() {
    let p = open(None);
    if !ignores_case(&p.root) {
        println!("SKIPPED: this file system distinguishes case");
        return;
    }
    p.write("Notes/a.md", "# A\n");
    p.write("Notes/keep.md", "# Keep\n");
    p.write("Gone/b.md", "# B\n");
    p.write("r.md", "# R\n\nSee [a](Notes/a.md) and [b](Gone/b.md).\n");
    p.full();
    // The folder still exists: the missing file is found by its folder.
    p.remove("Notes/a.md");
    let report = p.pass(&["notes/A.MD"]);
    println!("deleted file, existing folder, other case: removed {}", report.resources_removed);
    let after_first = p.live_resources();
    // The folder is gone too: no component has a spelling on disk.
    std::fs::remove_dir_all(p.root.join("Gone")).unwrap();
    let report = p.pass(&["gone/b.md"]);
    println!("deleted folder, other case: removed {}", report.resources_removed);
    let after_second = p.live_resources();
    println!("live after the two passes: {after_first:?} / {after_second:?}");
    for path in &after_second {
        assert_eq!(
            after_second.iter().filter(|o| o.to_lowercase() == path.to_lowercase()).count(),
            1,
            "two resources for {path}"
        );
    }
    // No mark is left on a file that exists.
    let pending = sql(&p.svc, "SELECT path FROM coverage WHERE pending > 0 ORDER BY path");
    println!("pending after the passes: {pending:?}");
    p.full();
    assert_eq!(p.live_resources(), ["Notes/keep.md", "r.md"]);
    assert!(sql(&p.svc, "SELECT path FROM coverage WHERE pending > 0").is_empty());
    p.check("after the full pass");
}

/// `mark_pending` matches stored paths bytewise: a mark made under another
/// spelling marks nothing, and the pass that follows still reconciles the
/// file and leaves no mark behind.
#[test]
fn a_mark_under_another_spelling_is_harmless() {
    let p = open(None);
    if !ignores_case(&p.root) {
        println!("SKIPPED: this file system distinguishes case");
        return;
    }
    p.write("Notes/a.md", "# A\n");
    p.write("Notes/b.md", "# B\n");
    p.full();
    let marked = p.svc.mark_pending(&[wp("notes/a.md"), wp("NOTES")]).unwrap();
    println!("rows marked under another spelling: {marked}");
    p.write("Notes/a.md", "# A, edited\n\nSee [b](b.md).\n");
    let report = p.pass(&["notes/A.md"]);
    assert_eq!(report.read, 1, "{report:?}");
    assert_eq!(p.links_to("Notes/a.md", RelationKind::References), ["Notes/b.md"]);
    assert!(sql(&p.svc, "SELECT path FROM coverage WHERE pending > 0").is_empty());
    // A mark under the stored spelling is cleared by a pass that names
    // another spelling (the pass is responsible for the spelling on disk).
    assert_eq!(p.svc.mark_pending(&[wp("Notes/a.md")]).unwrap(), 1);
    p.svc.reconcile(&ReconcileOptions { scope: Scope::Paths(vec![wp("NOTES/A.MD")]), force: false }).unwrap();
    assert!(sql(&p.svc, "SELECT path FROM coverage WHERE pending > 0").is_empty(), "the mark stays");
    // Impact of a path named in another case (project operations).
    let stored = p.svc.path_impacts(&[wp("Notes/a.md")]).map(|v| v.len()).unwrap_or(usize::MAX);
    let other = p.svc.path_impacts(&[wp("notes/a.md")]).map(|v| v.len()).unwrap_or(usize::MAX);
    println!("path_impacts: stored spelling {stored}, another spelling {other}");
    p.check("marks under another spelling");
}
