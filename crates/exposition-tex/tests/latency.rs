//! Edit-to-new-page latency benchmark (run explicitly):
//! `cargo test -p exposition-tex --release --test latency -- --ignored --nocapture`

mod common;

use std::time::Instant;

use exposition_core::build::{BuildStatus, Engine};
use exposition_core::document::{
    ConflictPolicy, DocumentEdit, EditOrigin, TextPatch, TransactionId, TransactionRequest, UndoBehavior,
};
use exposition_core::text::utf16_len;
use exposition_tex::{BuildMode, TexPageSession};

const RUNS: usize = 5;

fn stats(mut v: Vec<f64>) -> String {
    v.sort_by(f64::total_cmp);
    format!("min {:.0} / median {:.0} ms", v[0], v[v.len() / 2])
}

#[test]
#[ignore = "benchmark; run explicitly"]
fn edit_to_new_page_latency() {
    for engine in [Engine::PdfLatex, Engine::LuaLatex] {
        let mut cold = Vec::new();
        let mut warm_full = Vec::new();
        let mut draft = Vec::new();
        let mut full_after_edit = Vec::new();
        let mut map = Vec::new();
        let mut render = Vec::new();
        let mut total = Vec::new();
        for run in 0..RUNS {
            let Some(h) = common::Harness::new("latex-basic", engine) else { return };
            let main = h.open("main.tex");
            let t = Instant::now();
            let build = h.builder.build(&h.request(BuildMode::Full));
            cold.push(t.elapsed().as_secs_f64() * 1e3);
            assert_eq!(build.result.status, BuildStatus::Succeeded);
            let t = Instant::now();
            h.builder.build(&h.request(BuildMode::Full));
            warm_full.push(t.elapsed().as_secs_f64() * 1e3);

            // Page edit: insert a word in the first paragraph, then draft rebuild.
            let text = h.text(&main);
            let at = utf16_len(&text[..text.find("baseline").unwrap()]) as u32;
            let t_total = Instant::now();
            h.documents
                .apply(TransactionRequest {
                    id: TransactionId(format!("e{run}")),
                    origin: EditOrigin::Page,
                    edits: vec![DocumentEdit {
                        document: main.clone(),
                        base_revision: 1,
                        patches: vec![TextPatch::insert(at, "new ")],
                    }],
                    undo: UndoBehavior::NewGroup,
                    conflict: ConflictPolicy::RebaseDisjoint,
                    label: None,
                    selection: None,
                })
                .unwrap();
            let t = Instant::now();
            let rebuilt = h.builder.build(&h.request(BuildMode::Draft));
            draft.push(t.elapsed().as_secs_f64() * 1e3);
            let t = Instant::now();
            let session = TexPageSession::open(&rebuilt).unwrap();
            let mapping = session.map_page(0).unwrap();
            map.push(t.elapsed().as_secs_f64() * 1e3);
            let t = Instant::now();
            let _ = session.render_page(0, 2.0).unwrap();
            render.push(t.elapsed().as_secs_f64() * 1e3);
            total.push(t_total.elapsed().as_secs_f64() * 1e3);
            assert!(mapping.stats.mapped > 600);
            // A full build after the same kind of edit (aux unchanged → one pass).
            let t = Instant::now();
            h.builder.build(&h.request(BuildMode::Full));
            full_after_edit.push(t.elapsed().as_secs_f64() * 1e3);
        }
        eprintln!("{engine:?} cold full build (fresh build dir, engine+bibtex+reruns): {}", stats(cold));
        eprintln!("{engine:?} warm full build (unchanged): {}", stats(warm_full));
        eprintln!("{engine:?} draft build after edit (1 pass): {}", stats(draft));
        eprintln!("{engine:?} full build after edit: {}", stats(full_after_edit));
        eprintln!("{engine:?} open + extract + map page 1: {}", stats(map));
        eprintln!("{engine:?} render page 1 at 2x: {}", stats(render));
        eprintln!("{engine:?} edit-to-new-page (apply + draft + map + render): {}", stats(total));
    }
}
