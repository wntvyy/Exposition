//! Differential test of the incremental reconciliation paths (independent
//! review of the graph performance change).
//!
//! A seeded random sequence of file operations (create, edit, delete, rename,
//! folder moves, labels and references that dangle and are satisfied later,
//! unsaved overlays saved or discarded, bursts on both sides of the
//! scoped-pass threshold, curated relations and evidence) is applied to a
//! small generated project. After every step the events are reconciled the
//! way the driver does it (marks, then a pass over the changed paths), and
//! the graph is compared, record by record and id by id, with a copy of the
//! same database after a forced full reindex of the same files: every
//! entity, binding, relation, coverage row, awaited target and stored index
//! record, and the overlay of every unsaved buffer. The status counts are
//! compared with the tables at every step.
//!
//! Environment (all optional; the defaults are the fast fixed run):
//! - `EXPOSITION_DIFF_SEEDS`: `a..b` or a comma list of seeds.
//! - `EXPOSITION_DIFF_STEPS`: steps per seed.
//! - `EXPOSITION_DIFF_OUT`: a folder; one file per seed receives a hash per
//!   step of the id-independent form of the graph and its read models, so
//!   two builds of the crate can be compared step by step.
//! - `EXPOSITION_DIFF_FULL`: `seed:step` whose id-independent form is written
//!   in full next to the hashes.
//! - `EXPOSITION_DIFF_SOFT`: collect differences and report them at the end
//!   of each seed instead of stopping at the first.
//! - `EXPOSITION_DIFF_NO_FULL`: skip the full reindex (hashes only).
//! - `EXPOSITION_DIFF_TRACE`: print the events of every step.
//! - `EXPOSITION_DIFF_KEEP`: a folder that receives a copy of the project
//!   (files and graph) as it is at the `seed:step` of `EXPOSITION_DIFF_FULL`.
//! - `EXPOSITION_DIFF_NO_OVERLAP`: never name a folder and a path beneath
//!   it in one pass.
//! - `EXPOSITION_DIFF_CASE`: also deliver events whose folder case differs
//!   from the stored paths.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use exposition_core::path::WorkspacePath;
use exposition_graph::reconcile::{ReconcileOptions, Scope, overlay};
use exposition_graph::*;
use serde_json::Value;

// ----- deterministic randomness -----

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03)
    }
    fn next(&mut self) -> u64 {
        // splitmix64
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

fn fnv(text: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

// ----- the generated project -----

/// Folders: sibling prefixes (`a/b`, `a/bc`, `a/b.d`), LIKE pattern
/// characters (`_`, `%`) next to names they would match as patterns (`aXb`,
/// `aZ`), spaces and mixed case.
const DIRS: &[&str] = &[
    "",
    "a",
    "a/b",
    "a/bc",
    "a/b.d",
    "a_b",
    "aXb",
    "a%b",
    "a%",
    "aZ",
    "Sub Dir",
    "Sub Dir/In ner",
    "P",
    "MiXed/Case_1",
    "x_y/z%w",
];

/// File names: `b.tex` and `b0.md` in `a` sit right at the bounds of the key
/// range of folder `a/b`.
const NAMES: &[&str] = &[
    "f0.tex",
    "f1.tex",
    "f2.tex",
    "main.tex",
    "b.tex",
    "b0.md",
    "m0.md",
    "m 1.md",
    "t0.typ",
    "img0.png",
    "img_1.png",
    "L0.lean",
    "L1.lean",
    "s0.py",
    "lakefile.toml",
    "d%1.csv",
];

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() { name.to_owned() } else { format!("{dir}/{name}") }
}

fn wp(p: &str) -> WorkspacePath {
    WorkspacePath::parse(p).unwrap()
}

fn ext(p: &str) -> &str {
    p.rsplit('.').next().unwrap_or("")
}

fn candidate_path(rng: &mut Rng) -> String {
    join(rng.pick(DIRS), rng.pick(NAMES))
}

/// The text of a new version of `path`. Labels come from a small pool, so
/// references dangle, become satisfied by a later file, become ambiguous
/// (two files define one label) and dangle again.
fn content(rng: &mut Rng, path: &str, counter: u64) -> Vec<u8> {
    let label = |rng: &mut Rng| format!("lab:{}", rng.below(10));
    let other = |rng: &mut Rng| candidate_path(rng);
    let strip = |p: &str| p.rsplit_once('.').map(|(s, _)| s.to_owned()).unwrap_or_else(|| p.to_owned());
    let depth = path.matches('/').count();
    let relative = |target: &str| format!("{}{}", "../".repeat(depth), target);
    let mut t = String::new();
    match ext(path) {
        "tex" => {
            if path.ends_with("main.tex") {
                t.push_str("\\documentclass{article}\n\\begin{document}\n");
            }
            t.push_str(&format!("\\section{{Section {counter}}}\n"));
            for _ in 0..rng.below(4) {
                let env = *rng.pick(&["lemma", "theorem", "definition", "proposition"]);
                t.push_str(&format!("\\begin{{{env}}}\\label{{{}}}\nStatement {counter}", label(rng)));
                for _ in 0..rng.below(3) {
                    t.push_str(&format!(" uses Lemma~\\ref{{{}}}", label(rng)));
                }
                t.push_str(".\n");
                t.push_str(&format!("\\end{{{env}}}\n"));
                if rng.chance(30) {
                    t.push_str(&format!("\\begin{{proof}}\nBy \\eqref{{{}}}.\n\\end{{proof}}\n", label(rng)));
                }
            }
            for _ in 0..rng.below(3) {
                match rng.below(4) {
                    0 => t.push_str(&format!("\\input{{{}}}\n", strip(&other(rng)))),
                    1 => t.push_str(&format!("\\includegraphics{{{}}}\n", strip(&other(rng)))),
                    2 => t.push_str(&format!("See \\ref{{{}}}.\n", label(rng))),
                    _ => t.push_str(&format!("Cites \\cite{{ref{}}}.\n", rng.below(3))),
                }
            }
            if path.ends_with("main.tex") {
                t.push_str("\\end{document}\n");
            }
            t.push_str(&format!("% v{counter}\n"));
        }
        "md" => {
            t.push_str(&format!("# Note {counter}\n\n"));
            for _ in 0..rng.below(4) {
                let target = other(rng);
                if rng.chance(50) {
                    t.push_str(&format!("See [the other]({}).\n", relative(&target).replace(' ', "%20")));
                } else {
                    t.push_str(&format!("![plot]({})\n", relative(&target).replace(' ', "%20")));
                }
            }
            t.push_str(&format!("\n<!-- v{counter} -->\n"));
        }
        "typ" => {
            t.push_str(&format!("= Note {counter} <ty{}>\n", rng.below(4)));
            for _ in 0..rng.below(3) {
                t.push_str(&format!("#lemma[Typst lemma {counter}] <tl{}>\nSee @tl{}.\n", rng.below(5), rng.below(5)));
            }
            t.push_str(&format!("// v{counter}\n"));
        }
        "lean" => {
            for _ in 0..rng.below(3) {
                t.push_str(&format!("import P.L{}\n", rng.below(3)));
            }
            t.push_str(&format!(
                "namespace P\ntheorem t{} : {counter} = {counter} := rfl\ndef d{} : Nat := {counter}\nend P\n",
                rng.below(4),
                rng.below(4)
            ));
        }
        "py" => {
            t.push_str(&format!("from P import s{}\nimport numpy\nX = {counter}\n", rng.below(2)));
        }
        "toml" => t.push_str(&format!("name = \"P\"\n# v{counter}\n")),
        "csv" => t.push_str(&format!("x,y\n{counter},{}\n", counter * 2)),
        _ => t.push_str(&format!("png{counter}")),
    }
    t.into_bytes()
}

// ----- one project under test -----

struct Project {
    _data: tempfile::TempDir,
    _dir: tempfile::TempDir,
    root: PathBuf,
    svc: Arc<GraphService>,
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
    Project { _data: data, _dir: dir, root, svc }
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let target = to.join(&name);
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else if !name.to_string_lossy().ends_with("-journal") {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn sql_strings(svc: &GraphService, sql: &str) -> Vec<Vec<String>> {
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

fn sql_count(svc: &GraphService, sql: &str) -> u32 {
    sql_strings(svc, sql)[0][0].parse().unwrap_or(0)
}

/// Remove keys anywhere in a JSON value.
fn strip(value: &mut Value, keys: &[&str]) {
    match value {
        Value::Object(map) => {
            for k in keys {
                map.remove(*k);
            }
            for v in map.values_mut() {
                strip(v, keys);
            }
        }
        Value::Array(items) => {
            for v in items {
                strip(v, keys);
            }
        }
        _ => {}
    }
}

/// Order every array by its elements' text (ids were random; order by id
/// carries no meaning across runs).
fn sort_arrays(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for v in map.values_mut() {
                sort_arrays(v);
            }
        }
        Value::Array(items) => {
            for v in items.iter_mut() {
                sort_arrays(v);
            }
            items.sort_by_key(|v| v.to_string());
        }
        _ => {}
    }
}

/// Times, and the minted project id.
const TIMES: &[&str] = &["createdAtMs", "updatedAtMs", "computedAtMs", "checkpointMs", "indexedAt", "project"];
/// Fields the change under review added to read models.
const NEW_FLAGS: &[&str] = &["relationsTruncated", "neighborsTruncated", "evidenceTruncated", "entitiesTruncated"];

/// Every stored record of the graph, one line each, ids as stored.
/// `revisions`: keep the per-record revision counters.
fn records(svc: &GraphService, revisions: bool) -> Vec<String> {
    let entity_ids = sql_strings(svc, "SELECT id FROM entities ORDER BY id");
    let relation_ids = sql_strings(svc, "SELECT id FROM relations ORDER BY id");
    let mut lines = Vec::new();
    svc.with_store(|store| {
        for row in &entity_ids {
            let id = EntityId::parse(&row[0]).unwrap();
            let e = store.entity(&id)?.expect("listed entity");
            let mut v = serde_json::to_value(&e).unwrap();
            if let Value::Object(map) = &mut v {
                map.remove("updatedAtMs");
                if !revisions {
                    map.remove("revision");
                }
            }
            lines.push(format!("E {v}"));
            for b in store.bindings(&id)? {
                lines.push(format!("B {}", serde_json::to_value(&b).unwrap()));
            }
        }
        for row in &relation_ids {
            let id = RelationId::parse(&row[0]).unwrap();
            let r = store.relation(&id)?.expect("listed relation");
            let mut v = serde_json::to_value(&r).unwrap();
            if let Value::Object(map) = &mut v {
                map.remove("updatedAtMs");
                map.remove("createdAtMs");
                if !revisions {
                    map.remove("revision");
                }
            }
            lines.push(format!("R {v}"));
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
    for row in sql_strings(svc, "SELECT id, kind, subject FROM evidence ORDER BY id") {
        lines.push(format!("V {}", row.join(" ")));
    }
    lines.sort();
    lines
}

/// The live resource of each path.
fn resources(svc: &GraphService) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for row in sql_strings(
        svc,
        "SELECT key, id FROM entities WHERE kind = 'resource' AND lifecycle != 'tombstoned' ORDER BY created_at DESC, id DESC",
    ) {
        out.insert(row[0].clone(), row[1].clone());
    }
    out
}

/// Entities bound in each file.
fn bound(svc: &GraphService) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for row in sql_strings(svc, "SELECT path, entity_id FROM bindings") {
        out.entry(row[0].clone()).or_default().insert(row[1].clone());
    }
    out
}

/// Replace every minted id by a name that does not depend on the run.
fn rename_ids(text: &str, names: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    'scan: while i < bytes.len() {
        // An overlay's relation ids are derived from the ids of its ends.
        let unsaved = "rel-unsaved-";
        if text[i..].starts_with(unsaved)
            && i + unsaved.len() + 24 <= bytes.len()
            && bytes[i + unsaved.len()..i + unsaved.len() + 24].iter().all(u8::is_ascii_hexdigit)
        {
            out.push_str("rel-unsaved");
            i += unsaved.len() + 24;
            continue 'scan;
        }
        for prefix in ["ent-", "rel-", "evd-"] {
            let end = i + prefix.len() + 32;
            if text[i..].starts_with(prefix)
                && end <= bytes.len()
                && bytes[i + prefix.len()..end].iter().all(u8::is_ascii_hexdigit)
            {
                match names.get(&text[i..end]) {
                    Some(name) => out.push_str(name),
                    None => out.push_str(&format!("{prefix}?")),
                }
                i = end;
                continue 'scan;
            }
        }
        let ch = text[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// The id-independent form of the stored graph and of its read models.
fn canonical(svc: &GraphService, evidence_order: &BTreeMap<String, usize>, overlays: &[String]) -> Vec<String> {
    // Names: kind, key, label and where it is bound.
    let mut names: BTreeMap<String, String> = BTreeMap::new();
    for row in sql_strings(
        svc,
        "SELECT e.id, e.kind, coalesce(e.key, ''), e.label,
                coalesce((SELECT b.path || ' ' || b.selector FROM bindings b WHERE b.entity_id = e.id ORDER BY b.ordinal LIMIT 1), '')
         FROM entities e",
    ) {
        names.insert(row[0].clone(), format!("<{}|{}|{}|{}>", row[1], row[2], row[3], row[4]).replace('"', "'"));
    }
    for row in sql_strings(svc, "SELECT id, kind, from_id, to_id FROM relations") {
        let from = names.get(&row[2]).cloned().unwrap_or_default();
        let to = names.get(&row[3]).cloned().unwrap_or_default();
        names.insert(row[0].clone(), format!("<rel {} {from} {to}>", row[1]));
    }
    for (id, n) in evidence_order {
        names.insert(id.clone(), format!("<evd {n}>"));
    }
    let canon = |line: &str| -> String {
        let renamed = rename_ids(line, &names);
        match renamed.split_once(' ') {
            Some((tag, body)) if matches!(tag, "E" | "B" | "R" | "C" | "J" | "O") => {
                match serde_json::from_str::<Value>(body) {
                    Ok(mut v) => {
                        strip(&mut v, TIMES);
                        strip(&mut v, NEW_FLAGS);
                        sort_arrays(&mut v);
                        format!("{tag} {v}")
                    }
                    Err(_) => renamed,
                }
            }
            _ => renamed,
        }
    };
    let mut lines: Vec<String> = records(svc, true).iter().map(|l| canon(l)).collect();
    // Read models.
    let json = |tag: &str, v: Value| canon(&format!("J {}", serde_json::json!({ "view": tag, "value": v })));
    let status = svc.status();
    lines.push(format!(
        "STATUS rev={} entities={} relations={} coverage={:?}",
        status.revision, status.entities, status.relations, status.coverage
    ));
    let ids: Vec<EntityId> = sql_strings(svc, "SELECT id FROM entities ORDER BY id")
        .into_iter()
        .map(|r| EntityId::parse(&r[0]).unwrap())
        .collect();
    for id in &ids {
        let detail = svc.entity_detail(id).unwrap();
        lines.push(json("detail", serde_json::to_value(&detail).unwrap()));
    }
    let snapshot = svc
        .snapshot(&GraphQuery { kinds: None, text: None, around: None, include_inactive: true, limit: Some(2000) })
        .unwrap();
    lines.push(json("snapshot", serde_json::to_value(&snapshot).unwrap()));
    lines.push(json("math", serde_json::to_value(svc.math_overview().unwrap()).unwrap()));
    // Impact of every entity.
    for id in &ids {
        {
            let report = svc.impact(&ImpactRequest { entities: vec![id.clone()], max_depth: None, max_items: None });
            lines.push(match report {
                Ok(r) => {
                    // Which cycle and which path are reported first follows
                    // the order of the (random) ids: only what is reached.
                    let mut v = serde_json::to_value(&r).unwrap();
                    strip(&mut v, &["cycles"]);
                    if let Some(items) = v.get_mut("items").and_then(Value::as_array_mut) {
                        for item in items {
                            strip(item, &["path", "reason", "action", "locatedIn"]);
                        }
                    }
                    json("impact", v)
                }
                Err(e) => canon(&format!("IMPACT-ERR {id} {}", e.code)),
            });
        }
    }
    for path in resources(svc).keys() {
        let req = InspectAtSourceRequest { path: wp(path), offset: None, revision: None };
        lines.push(match svc.inspect_at_source(&req) {
            Ok(r) => json("inspect", serde_json::to_value(&r).unwrap()),
            Err(e) => format!("INSPECT-ERR {path} {}", e.code),
        });
    }
    for o in overlays {
        lines.push(canon(o));
    }
    lines.sort();
    lines
}

/// An overlay as text (ids as stored; provisional ids are derived from the
/// content, not minted).
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
    strip(&mut v, TIMES);
    format!("O {v}")
}

// ----- the run -----

struct Run {
    seed: u64,
    rng: Rng,
    a: Project,
    /// Files on disk (workspace-relative).
    files: BTreeSet<String>,
    counter: u64,
    /// Unsaved buffers: path -> (revision, text).
    overlays: BTreeMap<String, (u64, String)>,
    evidence: BTreeMap<String, usize>,
    findings: Vec<String>,
    soft: bool,
    case_events: bool,
    stats: BTreeMap<&'static str, u32>,
    /// The overlays as last computed (for the id-independent form).
    overlay_lines: Vec<String>,
}

impl Run {
    fn count(&mut self, what: &'static str) {
        *self.stats.entry(what).or_default() += 1;
    }

    fn finding(&mut self, step: usize, what: String) {
        let text = format!("seed {} step {step}: {what}", self.seed);
        if self.soft {
            if self.findings.len() < 40 {
                eprintln!("FINDING {text}");
            }
            self.findings.push(text);
        } else {
            panic!("{text}");
        }
    }

    fn write(&mut self, path: &str, bytes: &[u8]) {
        let p = self.a.root.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
        self.files.insert(path.to_owned());
    }

    fn prune_empty(&self, dir: Option<&Path>) {
        let mut dir = dir.map(Path::to_path_buf);
        while let Some(d) = dir {
            if d == self.a.root || std::fs::remove_dir(&d).is_err() {
                break;
            }
            dir = d.parent().map(Path::to_path_buf);
        }
    }

    fn remove(&mut self, path: &str) {
        let p = self.a.root.join(path);
        std::fs::remove_file(&p).unwrap();
        self.files.remove(path);
        self.prune_empty(p.parent());
    }

    fn existing(&mut self) -> Option<String> {
        if self.files.is_empty() {
            return None;
        }
        let i = self.rng.below(self.files.len());
        self.files.iter().nth(i).cloned()
    }

    fn fresh_text(&mut self, path: &str) -> Vec<u8> {
        self.counter += 1;
        content(&mut self.rng, path, self.counter)
    }

    /// One random file operation; returns the event paths it raises.
    fn operate(&mut self, touched: &mut BTreeSet<String>) -> Vec<String> {
        let roll = self.rng.below(100);
        let few = self.files.len() < 12;
        if roll < 22 || few {
            // Create (or overwrite) a file.
            let path = candidate_path(&mut self.rng);
            let text = self.fresh_text(&path);
            self.write(&path, &text);
            self.count("create");
            touched.insert(path.clone());
            return vec![path];
        }
        if roll < 50 {
            let Some(path) = self.existing() else { return vec![] };
            let text = self.fresh_text(&path);
            self.write(&path, &text);
            self.count("edit");
            touched.insert(path.clone());
            return vec![path];
        }
        if roll < 55 {
            // Touch: the same bytes written again.
            let Some(path) = self.existing() else { return vec![] };
            let bytes = std::fs::read(self.a.root.join(&path)).unwrap();
            self.write(&path, &bytes);
            self.count("touch");
            touched.insert(path.clone());
            return vec![path];
        }
        if roll < 66 {
            let Some(path) = self.existing() else { return vec![] };
            self.remove(&path);
            self.overlays.remove(&path);
            self.count("delete");
            touched.insert(path.clone());
            return vec![path];
        }
        if roll < 78 {
            // Rename or move one file (same kind of file).
            let Some(old) = self.existing() else { return vec![] };
            let same: Vec<&str> = NAMES.iter().copied().filter(|n| ext(n) == ext(&old)).collect();
            let new = join(self.rng.pick(DIRS), self.rng.pick(&same));
            if self.files.contains(&new) || new == old {
                return vec![];
            }
            let to = self.a.root.join(&new);
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            let from = self.a.root.join(&old);
            std::fs::rename(&from, &to).unwrap();
            self.prune_empty(from.parent());
            self.files.remove(&old);
            self.files.insert(new.clone());
            self.overlays.remove(&old);
            self.count("rename");
            touched.insert(old.clone());
            touched.insert(new.clone());
            return vec![old, new];
        }
        if roll < 88 {
            // Move a folder (events name the folders, as a watcher does).
            let dirs: Vec<&str> = DIRS
                .iter()
                .copied()
                .filter(|d| !d.is_empty() && self.files.iter().any(|f| f.starts_with(&format!("{d}/"))))
                .collect();
            if dirs.is_empty() {
                return vec![];
            }
            let old = (*self.rng.pick(&dirs)).to_owned();
            let new = (*self.rng.pick(DIRS)).to_owned();
            let inside = |p: &str, d: &str| p == d || p.starts_with(&format!("{d}/"));
            if new.is_empty() || inside(&new, &old) || inside(&old, &new) || self.a.root.join(&new).exists() {
                return vec![];
            }
            let to = self.a.root.join(&new);
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            let from = self.a.root.join(&old);
            std::fs::rename(&from, &to).unwrap();
            self.prune_empty(from.parent());
            let moved: Vec<String> = self.files.iter().filter(|f| inside(f, &old)).cloned().collect();
            for f in moved {
                self.files.remove(&f);
                let target = format!("{new}{}", &f[old.len()..]);
                touched.insert(f.clone());
                touched.insert(target.clone());
                self.overlays.remove(&f);
                self.files.insert(target);
            }
            self.count("move-folder");
            return vec![old, new];
        }
        if roll < 92 {
            // Delete a folder.
            let dirs: Vec<&str> = DIRS
                .iter()
                .copied()
                .filter(|d| !d.is_empty() && self.files.iter().any(|f| f.starts_with(&format!("{d}/"))))
                .collect();
            if dirs.is_empty() {
                return vec![];
            }
            let dir = (*self.rng.pick(&dirs)).to_owned();
            let at = self.a.root.join(&dir);
            std::fs::remove_dir_all(&at).unwrap();
            self.prune_empty(at.parent());
            let gone: Vec<String> = self.files.iter().filter(|f| f.starts_with(&format!("{dir}/"))).cloned().collect();
            for f in gone {
                self.files.remove(&f);
                self.overlays.remove(&f);
                touched.insert(f);
            }
            self.count("delete-folder");
            return vec![dir];
        }
        // An unsaved buffer: typed now; saved or discarded in a later step.
        let Some(path) = self.existing() else { return vec![] };
        if matches!(ext(&path), "png" | "csv") {
            return vec![];
        }
        let text = String::from_utf8(self.fresh_text(&path)).unwrap();
        let revision = self.counter;
        self.overlays.insert(path, (revision, text));
        self.count("type");
        vec![]
    }

    /// Save or discard an unsaved buffer.
    fn settle_overlay(&mut self, touched: &mut BTreeSet<String>) -> Vec<String> {
        let Some(path) = self.overlays.keys().next().cloned() else { return vec![] };
        let (_, text) = self.overlays.remove(&path).unwrap();
        self.a.svc.clear_overlay(&wp(&path));
        if self.rng.chance(60) && self.files.contains(&path) {
            self.write(&path, text.as_bytes());
            self.count("save");
            touched.insert(path.clone());
            return vec![path];
        }
        self.count("discard");
        vec![]
    }

    /// Curated records: a relation between two extracted declarations and a
    /// record about one of them (with the file version it was made from).
    fn curate(&mut self) {
        let rows = sql_strings(
            &self.a.svc,
            "SELECT e.id, e.kind || '|' || coalesce(e.key, '') || '|' || e.label || '|' ||
                    coalesce((SELECT b.path FROM bindings b WHERE b.entity_id = e.id ORDER BY b.ordinal LIMIT 1), ''),
                    coalesce((SELECT b.path FROM bindings b WHERE b.entity_id = e.id ORDER BY b.ordinal LIMIT 1), '')
             FROM entities e
             WHERE e.lifecycle = 'active' AND e.kind IN ('lemma', 'theorem', 'definition', 'proposition')",
        );
        // Only declarations a run-independent name identifies.
        let mut by_name: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for r in rows {
            by_name.entry(r[1].clone()).or_default().push((r[0].clone(), r[2].clone()));
        }
        let unique: Vec<(String, String)> =
            by_name.into_values().filter(|v| v.len() == 1).map(|mut v| v.remove(0)).collect();
        if unique.len() < 2 {
            return;
        }
        let (from, _) = unique[self.rng.below(unique.len())].clone();
        let (to, to_path) = unique[self.rng.below(unique.len())].clone();
        let svc = &self.a.svc;
        if from != to {
            let kind = *self.rng.pick(&[RelationKind::DependsOn, RelationKind::Assumes, RelationKind::References]);
            let base = svc.status().revision;
            let out = svc.propose_and_apply(
                Origin::User,
                &ChangeRequest {
                    base_revision: base,
                    operations: vec![GraphOperation::AddRelation {
                        kind,
                        from: EntityRef::Id { id: EntityId::parse(&from).unwrap() },
                        to: EntityRef::Id { id: EntityId::parse(&to).unwrap() },
                        evidence_kind: None,
                        evidence: vec![],
                        note: None,
                    }],
                    summary: None,
                },
            );
            let applied = matches!(out, Ok((_, ApplyOutcome::Applied { .. })));
            self.count(if applied { "curated-relation" } else { "curated-relation-refused" });
        }
        let svc = &self.a.svc;
        let inputs = match WorkspacePath::parse(&to_path) {
            Ok(path) => {
                let hash = svc.with_store(|s| Ok(s.coverage(&path)?.and_then(|c| c.content_hash))).unwrap();
                vec![VersionedInput { path, content_hash: hash, revision: None, unsaved: false }]
            }
            Err(_) => vec![],
        };
        let by = Origin::Service { service: "review".into() };
        let record = Evidence {
            id: EvidenceId::mint(),
            kind: EvidenceKind::Review,
            subject: Some(EntityId::parse(&to).unwrap()),
            operation: OperationRef { kind: "review".into(), id: format!("r{}", self.counter) },
            inputs,
            outcome: EvidenceOutcome::Succeeded,
            environment: vec![],
            outputs: vec![],
            formal: None,
            recorded_by: by.clone(),
            recorded_at_ms: 1_000 + self.evidence.len() as u64,
        };
        match svc.register_evidence(&by, record) {
            Ok(id) => {
                let n = self.evidence.len();
                self.evidence.insert(id.as_str().to_owned(), n);
                self.count("evidence");
            }
            Err(_) => self.count("evidence-refused"),
        }
    }

    /// Reconcile `events` the way the driver does: mark, then one pass over
    /// the paths (the whole project beyond the driver's threshold).
    fn deliver(&mut self, step: usize, events: Vec<String>) {
        let mut paths: Vec<String> = events;
        paths.sort();
        paths.dedup();
        if paths.is_empty() {
            return;
        }
        let mut batches: Vec<Vec<String>> = Vec::new();
        if paths.len() >= 2 && self.rng.chance(15) {
            // The events arrive in two settle windows.
            let cut = 1 + self.rng.below(paths.len() - 1);
            let rest = paths.split_off(cut);
            batches.push(paths);
            batches.push(rest);
            self.count("split-delivery");
        } else {
            batches.push(paths);
        }
        for mut batch in batches {
            if self.rng.chance(12) {
                // Other (unchanged) paths in the same window: past the
                // scoped-pass threshold, sometimes past the full-scan one.
                let extra = if self.rng.chance(20) { 210 } else { 17 + self.rng.below(10) };
                let known: Vec<String> = self.files.iter().cloned().collect();
                for i in 0..extra {
                    batch.push(match known.get(i) {
                        Some(p) => p.clone(),
                        None => format!("noise/n{i}.md"),
                    });
                }
                batch.sort();
                batch.dedup();
                self.count("padded-burst");
            }
            if self.case_events && self.rng.chance(25) {
                // The same folders, named with another case.
                let flipped: Vec<String> =
                    batch
                        .iter()
                        .filter_map(|p| p.split_once('/'))
                        .map(|(dir, _)| {
                            dir.chars()
                                .map(|c| {
                                    if c.is_ascii_lowercase() { c.to_ascii_uppercase() } else { c.to_ascii_lowercase() }
                                })
                                .collect::<String>()
                        })
                        .collect();
                batch.extend(flipped);
                batch.sort();
                batch.dedup();
                self.count("case-events");
            }
            if std::env::var_os("EXPOSITION_DIFF_NO_OVERLAP").is_some() {
                // A folder covers what is beneath it: name each file once.
                let all = batch.clone();
                batch.retain(|p| !all.iter().any(|d| d != p && p.starts_with(&format!("{d}/"))));
            }
            let wps: Vec<WorkspacePath> = batch.iter().map(|p| wp(p)).collect();
            let svc = self.a.svc.clone();
            svc.mark_pending(&wps).unwrap();
            self.check_status(step, "after marking");
            let scope = if wps.len() > reconcile::driver::FULL_SCAN_THRESHOLD {
                self.count("pass-full");
                Scope::Full
            } else {
                self.count(if wps.len() > 16 { "pass-many-paths" } else { "pass-scoped" });
                Scope::Paths(wps)
            };
            svc.reconcile(&ReconcileOptions { scope, force: false }).unwrap();
            self.check_status(step, "after the pass");
        }
    }

    /// `status()` against the tables.
    fn check_status(&mut self, step: usize, when: &str) {
        let svc = self.a.svc.clone();
        let status = svc.status();
        let entities = sql_count(&svc, "SELECT count(*) FROM entities WHERE lifecycle != 'tombstoned'");
        let relations = sql_count(&svc, "SELECT count(*) FROM relations");
        let files = sql_count(&svc, "SELECT count(*) FROM coverage");
        let pending = sql_count(&svc, "SELECT count(*) FROM coverage WHERE pending > 0");
        let unresolved = sql_count(&svc, "SELECT coalesce(sum(unresolved), 0) FROM coverage");
        let revision = sql_count(&svc, "SELECT value FROM meta WHERE key = 'revision'");
        let got = (
            status.entities,
            status.relations,
            status.coverage.resources,
            status.coverage.pending,
            status.coverage.unresolved_bindings,
            status.revision as u32,
        );
        let want = (entities, relations, files, pending, unresolved, revision);
        if got != want {
            self.finding(step, format!("status {when} is stale: status {got:?}, tables {want:?}"));
        }
    }

    /// Compare with a copy of the database after a forced full reindex.
    fn check_against_full(&mut self, step: usize) {
        let svc = self.a.svc.clone();
        // Unsaved buffers, recomputed as the driver does after a pass.
        let mut overlays_a: Vec<String> = Vec::new();
        for (path, (revision, text)) in &self.overlays {
            match overlay::compute(&svc, &wp(path), *revision, text).unwrap() {
                Some(entry) => {
                    overlays_a.push(overlay_text(&entry));
                    svc.set_overlay(svc.generation(), entry);
                }
                None => {
                    overlays_a.push(format!("O {{\"overlay\":\"{path}\",\"none\":true}}"));
                    svc.clear_overlay(&wp(path));
                }
            }
        }
        let incremental = records(&svc, false);
        let pending: Vec<&String> =
            incremental.iter().filter(|l| l.starts_with("C ") && l.contains("\"state\":\"pending\"")).collect();
        if !pending.is_empty() {
            let text = format!("coverage still pending after every event was reconciled: {pending:?}");
            self.finding(step, text);
        }
        if std::env::var_os("EXPOSITION_DIFF_NO_FULL").is_some() {
            self.overlay_lines = overlays_a;
            return;
        }
        if let Some(keep) = std::env::var_os("EXPOSITION_DIFF_KEEP")
            && std::env::var("EXPOSITION_DIFF_FULL").ok().as_deref() == Some(&format!("{}:{step}", self.seed))
        {
            copy_tree(&self.a.root, &PathBuf::from(keep));
        }
        let b = open(Some(&self.a.root));
        b.svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: true }).unwrap();
        let full = records(&b.svc, false);
        let mut overlays_b: Vec<String> = Vec::new();
        for (path, (revision, text)) in &self.overlays {
            overlays_b.push(match overlay::compute(&b.svc, &wp(path), *revision, text).unwrap() {
                Some(entry) => overlay_text(&entry),
                None => format!("O {{\"overlay\":\"{path}\",\"none\":true}}"),
            });
        }
        b.svc.detach();
        if incremental != full {
            let a: BTreeSet<&String> = incremental.iter().collect();
            let f: BTreeSet<&String> = full.iter().collect();
            let mut text = String::from("the incremental graph differs from a forced full reindex of the same files\n");
            for line in a.difference(&f).take(12) {
                text.push_str(&format!("  incremental only: {}\n", clip(line)));
            }
            for line in f.difference(&a).take(12) {
                text.push_str(&format!("  full only:        {}\n", clip(line)));
            }
            self.finding(step, text);
        }
        if overlays_a != overlays_b {
            let mut text = String::from("an overlay differs after a forced full reindex\n");
            for (x, y) in overlays_a.iter().zip(&overlays_b) {
                if x != y {
                    text.push_str(&format!("  incremental: {}\n  full:        {}\n", clip(x), clip(y)));
                }
            }
            self.finding(step, text);
        }
        self.overlay_lines = overlays_a;
    }
}

fn clip(line: &str) -> String {
    if line.len() > 700 { format!("{}...", line.chars().take(700).collect::<String>()) } else { line.to_owned() }
}

impl Run {
    fn run(seed: u64, steps: usize) -> Vec<String> {
        let soft = std::env::var_os("EXPOSITION_DIFF_SOFT").is_some();
        let out_dir = std::env::var_os("EXPOSITION_DIFF_OUT").map(PathBuf::from);
        let full_at: Option<(u64, usize)> = std::env::var("EXPOSITION_DIFF_FULL").ok().and_then(|v| {
            let (s, t) = v.split_once(':')?;
            Some((s.parse().ok()?, t.parse().ok()?))
        });
        let mut run = Run {
            seed,
            rng: Rng::new(seed),
            a: open(None),
            files: BTreeSet::new(),
            counter: 0,
            overlays: BTreeMap::new(),
            evidence: BTreeMap::new(),
            findings: Vec::new(),
            soft,
            case_events: std::env::var_os("EXPOSITION_DIFF_CASE").is_some(),
            stats: BTreeMap::new(),
            overlay_lines: Vec::new(),
        };
        let mut hashes = out_dir.as_ref().map(|dir| {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::File::create(dir.join(format!("seed-{seed}.txt"))).unwrap()
        });
        // The initial project, indexed by a full pass.
        for _ in 0..14 {
            let path = candidate_path(&mut run.rng);
            let text = run.fresh_text(&path);
            run.write(&path, &text);
        }
        run.a.svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: false }).unwrap();
        for step in 0..steps {
            let before_resources = resources(&run.a.svc);
            let before_bound = bound(&run.a.svc);
            let mut touched: BTreeSet<String> = BTreeSet::new();
            let mut events: Vec<String> = Vec::new();
            let roll = run.rng.below(100);
            let operations = if roll < 60 {
                1
            } else if roll < 85 {
                2 + run.rng.below(4)
            } else if roll < 93 {
                17 + run.rng.below(10)
            } else {
                0
            };
            for _ in 0..operations {
                events.extend(run.operate(&mut touched));
            }
            if run.rng.chance(25) {
                events.extend(run.settle_overlay(&mut touched));
            }
            match roll {
                93..=94 => {
                    // Close and reopen the project (a new connection and session index).
                    run.a.svc.detach();
                    let status = run.a.svc.attach(&run.a.root);
                    assert!(matches!(status.state, GraphState::Attached), "{status:?}");
                    run.count("reattach");
                }
                95..=96 => {
                    run.a.svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: false }).unwrap();
                    run.count("full-pass");
                }
                97..=99 => run.curate(),
                _ => {}
            }
            if std::env::var_os("EXPOSITION_DIFF_TRACE").is_some() {
                eprintln!("TRACE seed {seed} step {step}: events {events:?}; touched {touched:?}");
            }
            run.deliver(step, events);
            if run.rng.chance(20) {
                run.curate();
            }
            run.check_status(step, "at the end of the step");
            run.check_against_full(step);

            // Identities that must be stable: a file no operation touched
            // keeps its resource and the entities bound in it.
            let after_resources = resources(&run.a.svc);
            let after_bound = bound(&run.a.svc);
            let mut kept = 0u32;
            let mut changed_untouched_declarations = 0u32;
            for (path, id) in &before_resources {
                if touched.contains(path) {
                    if after_resources.get(path) == Some(id) {
                        kept += 1;
                    }
                    continue;
                }
                if after_resources.get(path) != Some(id) {
                    let text = format!(
                        "the untouched file {path} changed its resource: {id} -> {:?}",
                        after_resources.get(path)
                    );
                    run.finding(step, text);
                }
                if before_bound.get(path) != after_bound.get(path) {
                    changed_untouched_declarations += 1;
                }
            }
            // Renamed and moved files keep their identity when the pass saw both paths.
            let moved_kept = after_resources
                .iter()
                .filter(|(path, id)| {
                    touched.contains(*path)
                        && before_resources.get(*path) != Some(*id)
                        && before_resources.values().any(|old| old == *id)
                })
                .count();

            if hashes.is_some() || full_at == Some((seed, step)) {
                let mut lines = canonical(&run.a.svc, &run.evidence, &run.overlay_lines);
                lines.push(format!(
                    "IDENTITY touched-kept={kept} moved-kept={moved_kept} untouched-files-with-other-bound-entities={changed_untouched_declarations}"
                ));
                let text = lines.join("\n");
                if let Some(file) = hashes.as_mut() {
                    // One hash per kind of line, so a difference names its part.
                    let mut parts: BTreeMap<String, String> = BTreeMap::new();
                    for line in &lines {
                        let tag = line.split(' ').next().unwrap_or("");
                        let key = match tag {
                            "J" => {
                                let at = line.rfind("\"view\":\"").map(|i| i + 8).unwrap_or(0);
                                line[at..].trim_end_matches(['"', '}']).to_owned()
                            }
                            other => other.to_owned(),
                        };
                        let part = parts.entry(key).or_default();
                        part.push_str(line);
                        part.push('\n');
                    }
                    let parts: Vec<String> = parts.iter().map(|(k, v)| format!("{k}={:08x}", fnv(v) as u32)).collect();
                    writeln!(file, "{step} files={} {}", run.files.len(), parts.join(" ")).unwrap();
                }
                if full_at == Some((seed, step))
                    && let Some(dir) = &out_dir
                {
                    std::fs::write(dir.join(format!("seed-{seed}-step-{step}.full.txt")), &text).unwrap();
                }
            }
        }
        eprintln!(
            "seed {seed}: {steps} steps, {} files at the end, {} findings; {:?}",
            run.files.len(),
            run.findings.len(),
            run.stats
        );
        run.a.svc.detach();
        std::mem::take(&mut run.findings)
    }
}

fn seeds() -> Vec<u64> {
    match std::env::var("EXPOSITION_DIFF_SEEDS") {
        Ok(v) => match v.split_once("..") {
            Some((a, b)) => (a.parse().unwrap()..b.parse().unwrap()).collect(),
            None => v.split(',').map(|s| s.trim().parse().unwrap()).collect(),
        },
        Err(_) => vec![1, 2, 3],
    }
}

fn steps() -> usize {
    std::env::var("EXPOSITION_DIFF_STEPS").ok().and_then(|v| v.parse().ok()).unwrap_or(40)
}

#[test]
fn incremental_passes_leave_the_graph_a_forced_full_reindex_leaves() {
    let mut findings = Vec::new();
    for seed in seeds() {
        findings.extend(Run::run(seed, steps()));
    }
    assert!(findings.is_empty(), "{} findings:\n{}", findings.len(), findings.join("\n"));
}
