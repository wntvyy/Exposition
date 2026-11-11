//! Toolchain discovery and real tools under the clean environment. Tools that
//! are not installed on the host are reported (and their guidance checked)
//! rather than failing the test.

mod common;

use common::*;
use exposition_core::job::{JobOutcome, JobSpec};
use exposition_execution::{JobKind, Located, ToolStatus};

#[tokio::test(flavor = "multi_thread")]
async fn discovery_reports_every_catalog_tool_with_versions_or_guidance() {
    let f = fixture();
    let tools = f.service.toolchains().await;
    let ids: Vec<&str> = tools.iter().map(|t| t.id.as_str()).collect();
    for id in ["pdflatex", "lualatex", "latexmk", "biber", "bibtex", "synctex", "python", "python-jupyter"] {
        assert!(ids.contains(&id), "{id} missing from {ids:?}");
    }
    for id in ["rscript", "r", "irkernel", "julia", "ijulia", "lean", "lake", "elan", "docker", "git"] {
        assert!(ids.contains(&id), "{id} missing from {ids:?}");
    }
    for tool in &tools {
        eprintln!(
            "{:<15} {:<9} {:<60} {}",
            tool.id,
            format!("{:?}", tool.status),
            tool.version.clone().unwrap_or_default(),
            tool.path.clone().unwrap_or_default()
        );
        match tool.status {
            ToolStatus::Available => {
                assert!(tool.path.is_some() && tool.version.is_some(), "{tool:?}");
                assert!(tool.guidance.is_none());
            }
            ToolStatus::Missing | ToolStatus::Broken => {
                assert!(tool.guidance.as_deref().is_some_and(|g| g.len() > 20), "{tool:?}");
            }
        }
    }
    // Probes are jobs of kind Probe that needed no workspace trust.
    let probes = f.service.list().into_iter().filter(|j| j.kind == JobKind::Probe).count();
    assert!(probes > 0 || tools.iter().all(|t| t.path.is_none()));
    // Cached until refreshed.
    let again = f.service.toolchains().await;
    assert_eq!(again, tools);
    let after_cache = f.service.list().into_iter().filter(|j| j.kind == JobKind::Probe).count();
    assert_eq!(after_cache, probes, "cached call must not re-probe");

    // Tools that exist on this host must be reported available.
    for id in ["pdflatex", "lualatex", "python"] {
        if matches!(f.service.locate_tool(id), Located::Found { .. }) {
            let tool = tools.iter().find(|t| t.id == id).unwrap();
            assert_eq!(tool.status, ToolStatus::Available, "{tool:?}");
        }
    }
}

fn tex_spec(f: &Fixture, program: &str, file: &str) -> JobSpec {
    let mut spec = f.spec(&[]);
    spec.program = program.into();
    spec.args =
        vec!["-interaction=nonstopmode".into(), "-halt-on-error".into(), "-no-shell-escape".into(), file.into()];
    spec.timeout_ms = Some(300_000);
    spec
}

#[tokio::test(flavor = "multi_thread")]
async fn tex_engines_build_under_the_clean_environment() {
    let f = fixture();
    std::fs::write(
        f.workspace.join("doc.tex"),
        "\\documentclass{article}\\begin{document}Hello $e^{i\\pi}+1=0$.\\end{document}\n",
    )
    .unwrap();
    for engine in ["pdflatex", "lualatex"] {
        if !matches!(f.service.locate_tool(engine), Located::Found { .. }) {
            eprintln!("{engine} not installed: skipped");
            continue;
        }
        let _ = std::fs::remove_file(f.workspace.join("doc.pdf"));
        let id = f.service.launch(tex_spec(&f, engine, "doc.tex"));
        let result = f.service.wait(&id).await.unwrap();
        assert_eq!(result.outcome, JobOutcome::Exited { code: 0 }, "{engine}: {}", f.stdout(&id));
        assert!(f.workspace.join("doc.pdf").exists(), "{engine} produced no PDF");
        eprintln!("{engine}: built doc.pdf in {} ms", result.duration_ms);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn python_with_jupyter_runs_under_the_clean_environment() {
    let f = fixture();
    if !matches!(f.service.locate_tool("python"), Located::Found { .. }) {
        eprintln!("python not installed: skipped");
        return;
    }
    let mut spec = f.spec(&[]);
    spec.program = "python".into();
    spec.args = vec![
        "-c".into(),
        "import os, sys, tempfile; print(sys.version.split()[0]); print(tempfile.gettempdir()); print('ANTHROPIC_API_KEY' in os.environ)"
            .into(),
    ];
    let id = f.service.launch(spec);
    assert_eq!(f.service.wait(&id).await.unwrap().outcome, JobOutcome::Exited { code: 0 }, "{}", f.stdout(&id));
    let out = f.stdout(&id);
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[1].contains(&id.0), "python temp dir is the per-job dir: {out}");
    assert_eq!(lines[2], "False");
}
