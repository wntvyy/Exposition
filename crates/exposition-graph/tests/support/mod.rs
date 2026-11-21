//! Shared helpers of the graph cost tests (`perf_graph.rs`, `cost_bounds.rs`,
//! `completeness.rs`): deterministic SQL work counters, a generated project,
//! and bulk curated records written straight into the store.
//!
//! The counters come from SQLite's own statement tracing, installed on the
//! service's connection from the test only (the product never traces):
//! statements started, rows returned, virtual-machine steps and full-scan
//! steps. They do not depend on the machine or the build profile, so a cost
//! per edit can be asserted without a wall-clock threshold.
#![allow(dead_code, unsafe_code)]

use std::cell::Cell;
use std::collections::BTreeMap;
use std::ffi::{c_int, c_uint, c_void};
use std::path::Path;
use std::time::Instant;

use exposition_graph::store::{Writer, now_ms};
use exposition_graph::*;
use rusqlite::ffi;

/// SQL work done on this thread since the last [`take`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SqlWork {
    /// Statements started.
    pub statements: u64,
    /// Rows returned to the caller.
    pub rows: u64,
    /// Virtual-machine steps (proportional to rows visited and compared).
    pub vm_steps: u64,
    /// Rows stepped over by full table scans.
    pub fullscan_steps: u64,
}

impl SqlWork {
    pub fn json(&self) -> String {
        format!(
            "{{\"statements\":{},\"rows\":{},\"vm_steps\":{},\"fullscan_steps\":{}}}",
            self.statements, self.rows, self.vm_steps, self.fullscan_steps
        )
    }
}

thread_local! {
    static WORK: Cell<SqlWork> = const { Cell::new(SqlWork { statements: 0, rows: 0, vm_steps: 0, fullscan_steps: 0 }) };
}

unsafe extern "C" fn trace(event: c_uint, _ctx: *mut c_void, p: *mut c_void, _x: *mut c_void) -> c_int {
    WORK.with(|w| {
        let mut work = w.get();
        match event {
            ffi::SQLITE_TRACE_STMT => work.statements += 1,
            ffi::SQLITE_TRACE_ROW => work.rows += 1,
            ffi::SQLITE_TRACE_PROFILE => {
                let stmt = p.cast::<ffi::sqlite3_stmt>();
                // SAFETY: for a PROFILE event SQLite passes the prepared
                // statement that just finished; reading (and resetting) its
                // counters is valid while the callback runs.
                let (vm, full) = unsafe {
                    (
                        ffi::sqlite3_stmt_status(stmt, ffi::SQLITE_STMTSTATUS_VM_STEP, 1),
                        ffi::sqlite3_stmt_status(stmt, ffi::SQLITE_STMTSTATUS_FULLSCAN_STEP, 1),
                    )
                };
                work.vm_steps += vm.max(0) as u64;
                work.fullscan_steps += full.max(0) as u64;
            }
            _ => {}
        }
        w.set(work);
    });
    0
}

/// Count the SQL work of the attached graph's connection from now on (on the
/// calling thread: the service runs statements on its caller's thread).
pub fn count_sql(svc: &GraphService) {
    svc.with_store(|store| {
        // SAFETY: the connection is alive for the duration of the call, the
        // callback is a plain function that touches thread-local state only,
        // and no context pointer is registered.
        unsafe {
            ffi::sqlite3_trace_v2(
                store.connection().handle(),
                ffi::SQLITE_TRACE_STMT | ffi::SQLITE_TRACE_ROW | ffi::SQLITE_TRACE_PROFILE,
                Some(trace),
                std::ptr::null_mut(),
            );
        }
        Ok(())
    })
    .unwrap();
    take();
}

/// The work counted since the previous call (and reset).
pub fn take() -> SqlWork {
    WORK.with(|w| w.replace(SqlWork::default()))
}

/// The SQL work of `f`.
pub fn sql_of<T>(f: impl FnOnce() -> T) -> (T, SqlWork) {
    take();
    let out = f();
    (out, take())
}

// ----- scale -----

#[derive(Clone, Copy, Debug)]
pub struct Scale {
    pub name: &'static str,
    /// Project files generated on disk.
    pub files: usize,
    /// Declarations in the one large manuscript file.
    pub big_file_declarations: usize,
    /// Curated lemmas written into the store.
    pub lemmas: usize,
    /// Curated `dependsOn` relations among them.
    pub relations: usize,
    /// Degree of each of the two hub lemmas (one incoming, one outgoing).
    pub hub_degree: usize,
}

pub const SMALL: Scale =
    Scale { name: "small", files: 1_000, big_file_declarations: 300, lemmas: 5_000, relations: 20_000, hub_degree: 2_000 };
pub const LARGE: Scale = Scale {
    name: "large",
    files: 18_000,
    big_file_declarations: 2_000,
    lemmas: 50_000,
    relations: 300_000,
    hub_degree: 25_000,
};
pub const STRESS: Scale = Scale {
    name: "stress",
    files: 19_000,
    big_file_declarations: 4_000,
    lemmas: 100_000,
    relations: 1_000_000,
    hub_degree: 50_000,
};

impl Scale {
    pub fn from_env() -> Scale {
        match std::env::var("EXPOSITION_PERF_SCALE").as_deref() {
            Ok("large") => LARGE,
            Ok("stress") => STRESS,
            _ => SMALL,
        }
    }

    pub fn json(&self) -> String {
        format!(
            "{{\"name\":\"{}\",\"files\":{},\"bigFileDeclarations\":{},\"lemmas\":{},\"relations\":{},\"hubDegree\":{}}}",
            self.name, self.files, self.big_file_declarations, self.lemmas, self.relations, self.hub_degree
        )
    }
}

// ----- measurement -----

/// Run `f` once to warm up, then `samples` times; returns the timings in
/// milliseconds and the SQL work of the last run.
pub fn measure(samples: usize, mut f: impl FnMut(usize)) -> (Vec<f64>, SqlWork) {
    f(0);
    let mut out = Vec::with_capacity(samples);
    let mut work = SqlWork::default();
    for i in 1..=samples {
        take();
        let t = Instant::now();
        f(i);
        out.push(t.elapsed().as_secs_f64() * 1000.0);
        work = take();
    }
    (out, work)
}

pub fn report(bench: &str, scale: &Scale, samples: &[f64], counters: &str) {
    let samples: Vec<String> = samples.iter().map(|s| format!("{s:.3}")).collect();
    println!(
        "PERF {{\"bench\":\"{bench}\",\"scale\":{},\"samples_ms\":[{}],\"counters\":{counters}}}",
        scale.json(),
        samples.join(",")
    );
}

// ----- generated project -----

pub const BIG_FILE: &str = "big/big.tex";
pub const EDITED_FILE: &str = "chapters/c5/ch5.tex";

/// The text of the large manuscript file: `declarations` lemmas, each with a
/// label and a reference to the previous one.
pub fn big_file_text(declarations: usize, marker: &str) -> String {
    let mut text = String::from("\\section{Big}\n");
    for i in 0..declarations {
        let prev = if i > 0 { format!("Lemma~\\ref{{big:{}}}", i - 1) } else { "nothing".into() };
        text.push_str(&format!(
            "\\begin{{lemma}}\\label{{big:{i}}}\nBig statement {i} uses {prev}. {marker}\n\\end{{lemma}}\n"
        ));
    }
    text
}

/// A synthetic project of about `n` files: LaTeX chapters with theorem
/// environments, labels, references and citations; Typst and Markdown notes;
/// Lean files; Python scripts with local imports; datasets and images; a
/// bibliography; one large manuscript file.
pub fn generate(root: &Path, n: usize, big_file_declarations: usize) {
    let w = |rel: &str, body: &str| {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    };
    let mut bib = String::new();
    for i in 0..200 {
        bib.push_str(&format!("@article{{ref{i}, title = {{Paper {i}}}, year = {}}}\n", 1990 + i % 30));
    }
    w("refs.bib", &bib);
    let tex = n * 12 / 100;
    let mut main = String::from("\\documentclass{article}\n\\begin{document}\n");
    for i in 0..tex {
        main.push_str(&format!("\\input{{chapters/c{}/ch{i}}}\n", i % 20));
    }
    main.push_str("\\bibliography{refs}\n\\end{document}\n");
    w("main.tex", &main);
    for i in 0..tex {
        let prev = if i > 0 { format!("Lemma~\\ref{{lem:{}}}", i - 1) } else { "nothing".into() };
        w(
            &format!("chapters/c{}/ch{i}.tex", i % 20),
            &format!(
                "\\section{{Chapter {i}}}\nText citing \\cite{{ref{}}}.\n\\begin{{lemma}}\\label{{lem:{i}}}\nStatement {i} uses {prev}.\n\\end{{lemma}}\n\\begin{{proof}}\nBy \\eqref{{eq:{i}}}.\n\\end{{proof}}\n\\begin{{theorem}}[Result {i}]\\label{{thm:{i}}}\nSee Lemma~\\ref{{lem:{i}}}.\n\\begin{{equation}}\\label{{eq:{i}}} x = {i} \\end{{equation}}\n\\end{{theorem}}\n\\includegraphics{{figures/f{}}}\n",
                i % 200,
                i % 100
            ),
        );
    }
    w(BIG_FILE, &big_file_text(big_file_declarations, ""));
    for i in 0..100 {
        w(&format!("figures/f{i}.png"), "png");
    }
    let typ = n * 6 / 100;
    for i in 0..typ {
        w(
            &format!("notes/typst/n{i}.typ"),
            &format!(
                "= Note {i} <note{i}>\n#lemma[Typst lemma {i}] <tl{i}>\nSee @tl{} and @note{i}.\n",
                i.saturating_sub(1)
            ),
        );
    }
    let md = n * 20 / 100;
    for i in 0..md {
        w(
            &format!("notes/md/d{}/m{i}.md", i % 30),
            &format!(
                "# Note {i}\n\nSee [the next](m{}.md) and ![plot](../../../figures/f{}.png) [@ref{}].\n",
                i + 1,
                i % 100,
                i % 200
            ),
        );
    }
    let lean = n * 2 / 100;
    w("lakefile.toml", "name = \"P\"\n");
    for i in 0..lean {
        let import = if i > 0 { format!("import P.L{}\n", i - 1) } else { String::new() };
        w(
            &format!("P/L{i}.lean"),
            &format!("{import}namespace P\ntheorem t{i} : {i} = {i} := rfl\ndef d{i} : Nat := {i}\nend P\n"),
        );
    }
    let py = n.saturating_sub(tex + typ + md + lean + 304);
    for i in 0..py {
        let import = if i > 0 { format!("from pkg{} import m{}\n", (i - 1) % 40, i - 1) } else { String::new() };
        w(&format!("pkg{}/m{i}.py", i % 40), &format!("{import}import numpy\nX = {i}\n"));
    }
    for i in 0..200 {
        w(&format!("data/d{i}.csv"), &format!("x,y\n{i},{}\n", i * 2));
    }
}

// ----- bulk curated records -----

pub fn entity(kind: EntityKind, label: &str) -> Entity {
    let now = now_ms();
    Entity {
        id: EntityId::mint(),
        kind,
        roles: vec![],
        label: label.to_owned(),
        key: None,
        lifecycle: Lifecycle::Active,
        tombstone_reason: None,
        revision: 1,
        origin: Origin::User,
        attributes: BTreeMap::new(),
        created_at_ms: now,
        updated_at_ms: now,
    }
}

pub fn relation(kind: RelationKind, from: &EntityId, to: &EntityId) -> Relation {
    let now = now_ms();
    Relation {
        id: RelationId::mint(),
        kind,
        from: from.clone(),
        to: to.clone(),
        origin: Origin::User,
        status: AssertionStatus::Accepted,
        evidence_kind: None,
        evidence: Vec::new(),
        from_version: None,
        to_version: None,
        note: None,
        reviewed_statement: None,
        review: None,
        revision: 1,
        created_at_ms: now,
        updated_at_ms: now,
    }
}

/// Curated records written in bulk.
pub struct Bulk {
    pub lemmas: Vec<EntityId>,
    /// A lemma `hub_degree` other lemmas depend on (incoming `dependsOn`).
    pub hub_in: EntityId,
    /// A lemma that depends on `hub_degree` other lemmas (outgoing `dependsOn`).
    pub hub_out: EntityId,
}

/// Write `lemmas` curated lemmas, `relations` `dependsOn` relations spread
/// among them (each lemma depends on a few later ones: no cycles), and two
/// hubs of `hub_degree`, in one transaction (a recorded change).
pub fn bulk(svc: &GraphService, lemmas: usize, relations: usize, hub_degree: usize) -> Bulk {
    assert!(hub_degree < lemmas, "hubs relate to distinct lemmas");
    svc.with_store(|store| {
        let w = Writer::new(store.transaction()?);
        let mut ids = Vec::with_capacity(lemmas);
        for i in 0..lemmas {
            let e = entity(EntityKind::Lemma, &format!("Bulk lemma {i:07}"));
            w.put_entity(&e)?;
            ids.push(e.id);
        }
        // Labels sort the hubs first among lemmas, so default snapshots and
        // the Mathematics projection include them.
        let hub_in = entity(EntityKind::Lemma, "A hub lemma (depended on)");
        let hub_out = entity(EntityKind::Lemma, "A hub lemma (depends on many)");
        w.put_entity(&hub_in)?;
        w.put_entity(&hub_out)?;
        for other in ids.iter().take(hub_degree) {
            w.put_relation(&relation(RelationKind::DependsOn, other, &hub_in.id))?;
            w.put_relation(&relation(RelationKind::DependsOn, &hub_out.id, other))?;
        }
        let per = (relations / lemmas.max(1)).max(1);
        let mut written = 0usize;
        'all: for i in 0..lemmas {
            for k in 1..=per {
                let j = i + k * 7 + (i % 13);
                if j >= lemmas {
                    continue;
                }
                w.put_relation(&relation(RelationKind::DependsOn, &ids[i], &ids[j]))?;
                written += 1;
                if written >= relations {
                    break 'all;
                }
            }
        }
        let mut delta = GraphDelta::default();
        w.record_change(&Origin::User, "bulk curated records", &mut delta)?;
        w.commit()?;
        Ok(Bulk { lemmas: ids, hub_in: hub_in.id, hub_out: hub_out.id })
    })
    .unwrap()
}

/// A service attached to a fresh empty project folder.
pub fn attached() -> (tempfile::TempDir, tempfile::TempDir, std::path::PathBuf, std::sync::Arc<GraphService>) {
    let data = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(project.path()).unwrap();
    let svc = GraphService::new(GraphConfig { data_dir: data.path().to_path_buf() });
    let status = svc.attach(&root);
    assert!(matches!(status.state, GraphState::Attached), "{status:?}");
    (data, project, root, svc)
}
