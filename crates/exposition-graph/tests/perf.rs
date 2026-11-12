//! Reconciliation timings on a generated project (run explicitly):
//! `cargo test -p exposition-graph --test perf -- --ignored --nocapture`
//! `EXPOSITION_PERF_FILES` sets the number of files (default 5000).

use std::time::Instant;

use exposition_core::path::WorkspacePath;
use exposition_graph::reconcile::{ReconcileOptions, Scope};
use exposition_graph::*;

/// Write a synthetic project of about `n` files: LaTeX chapters with
/// theorem environments, labels, references and citations; Typst and
/// Markdown notes; Lean files; Python scripts with local imports; datasets
/// and images; a bibliography; plus caches that must be skipped.
pub fn generate(root: &std::path::Path, n: usize) {
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
    let mut main = String::from("\\documentclass{article}\n\\begin{document}\n");
    let tex = n * 12 / 100;
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
    let py = n.saturating_sub(tex + typ + md + lean + 300);
    for i in 0..py {
        let import = if i > 0 { format!("from pkg{} import m{}\n", (i - 1) % 40, i - 1) } else { String::new() };
        w(&format!("pkg{}/m{i}.py", i % 40), &format!("{import}import numpy\nX = {i}\n"));
    }
    for i in 0..200 {
        w(&format!("data/d{i}.csv"), &format!("x,y\n{i},{}\n", i * 2));
    }
    // Excluded: must not cost entities.
    for i in 0..300 {
        w(&format!("node_modules/p/i{i}.js"), "x");
        w(&format!(".lake/build/L{i}.olean"), "o");
    }
}

#[test]
#[ignore = "timing run; invoke explicitly"]
fn synthetic_project_timings() {
    let n: usize = std::env::var("EXPOSITION_PERF_FILES").ok().and_then(|v| v.parse().ok()).unwrap_or(5000);
    let data = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let t = Instant::now();
    generate(&root, n);
    println!("generated {n} files in {:?}", t.elapsed());
    let svc = GraphService::new(GraphConfig { data_dir: data.path().to_path_buf() });
    let t = Instant::now();
    svc.attach(&root);
    let attach = t.elapsed();
    let t = Instant::now();
    let cold = svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: false }).unwrap();
    let cold_time = t.elapsed();
    let status = svc.status();
    println!(
        "attach {attach:?}; cold index {cold_time:?}: files {}, read {}, extracted {}, resources {}, declarations {}, relations {}; graph: {} entities, {} relations, coverage {:?}",
        cold.files,
        cold.read,
        cold.extracted,
        cold.resources_added,
        cold.declarations_added,
        cold.relations_added,
        status.entities,
        status.relations,
        status.coverage
    );
    let t = Instant::now();
    let idle = svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: false }).unwrap();
    println!("unchanged full pass {:?} (read {})", t.elapsed(), idle.read);
    // One saved edit (a new label referenced elsewhere).
    let edited = root.join("chapters/c5/ch5.tex");
    let mut text = std::fs::read_to_string(&edited).unwrap();
    text.push_str("\\begin{corollary}\\label{cor:new}\nFrom Theorem~\\ref{thm:5}.\n\\end{corollary}\n");
    std::fs::write(&edited, text).unwrap();
    let t = Instant::now();
    let one = svc
        .reconcile(&ReconcileOptions {
            scope: Scope::Paths(vec![WorkspacePath::parse("chapters/c5/ch5.tex").unwrap()]),
            force: false,
        })
        .unwrap();
    println!(
        "incremental (one saved file) {:?}: read {}, declarations +{}, relations +{} -{}",
        t.elapsed(),
        one.read,
        one.declarations_added,
        one.relations_added,
        one.relations_removed
    );
    let t = Instant::now();
    svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: false }).unwrap();
    println!("full pass after the edit {:?}", t.elapsed());
    let t = Instant::now();
    let forced = svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: true }).unwrap();
    println!("forced reindex {:?} (read {})", t.elapsed(), forced.read);
    let t = Instant::now();
    svc.detach();
    svc.attach(&root);
    println!("reattach (open + verify contents) {:?}", t.elapsed());
    let db = root.join(".exposition").join("graph.sqlite3");
    println!("database size {} KiB", std::fs::metadata(db).unwrap().len() / 1024);
    assert!(cold.resources_added as usize >= n * 9 / 10);
}
