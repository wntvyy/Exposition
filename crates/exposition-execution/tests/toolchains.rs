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
    for job in f.service.list().into_iter().filter(|j| j.kind == JobKind::Probe) {
        let ms = job.info.result.as_ref().map(|r| r.duration_ms).unwrap_or_default();
        eprintln!("probe {:<45} {ms:>6} ms", job.info.label);
    }
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

/// Discovery must never run elan's `lean`/`lake` shims (they select and may
/// DOWNLOAD toolchains) and must not change the installed toolchains. Uses a
/// fake elan layout whose executables log every invocation.
#[tokio::test(flavor = "multi_thread")]
async fn lean_discovery_never_runs_elan_shims_or_installs_toolchains() {
    let tmp = tempfile::tempdir().unwrap();
    let elan = tmp.path().join("elan");
    let exe = |name: &str| if cfg!(windows) { format!("{name}.exe") } else { name.to_owned() };
    let bin = elan.join("bin");
    let old = elan.join("toolchains").join("leanprover--lean4---v4.9.0").join("bin");
    let new = elan.join("toolchains").join("leanprover--lean4---v4.12.0").join("bin");
    for dir in [&bin, &old, &new] {
        std::fs::create_dir_all(dir).unwrap();
    }
    for name in ["elan", "lean", "lake"] {
        std::fs::copy(testproc(), bin.join(exe(name))).unwrap();
    }
    for dir in [&old, &new] {
        for name in ["lean", "lake"] {
            std::fs::copy(testproc(), dir.join(exe(name))).unwrap();
        }
    }
    let listing = |dir: &std::path::Path| -> Vec<String> {
        let mut names: Vec<String> =
            std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    };
    let toolchains_before = listing(&elan.join("toolchains"));

    // The shims are also first on PATH, and no real elan can be found.
    let sep = if cfg!(windows) { ";" } else { ":" };
    let system = std::env::var("SystemRoot")
        .map(|r| format!("{sep}{}", std::path::Path::new(&r).join("System32").display()))
        .unwrap_or_default();
    let host = exposition_execution::env::HostEnv::from_pairs(
        std::env::vars()
            .filter(|(k, _)| !["ELAN_HOME", "PATH", "Path", "USERPROFILE", "HOME"].contains(&k.as_str()))
            .chain([
                ("ELAN_HOME".to_string(), elan.display().to_string()),
                ("PATH".to_string(), format!("{}{system}", bin.display())),
                ("USERPROFILE".to_string(), tmp.path().join("profile").display().to_string()),
                ("HOME".to_string(), tmp.path().join("profile").display().to_string()),
            ]),
    );
    let service = exposition_execution::ExecutionService::with_config(exposition_execution::ExecutionConfig {
        data_dir: tmp.path().join("data"),
        host_env: host,
        settings: Some(exposition_execution::ExecutionSettings::default()),
        too_broad_to_trust: Some(vec![]),
        tool_cache_dir: Some(std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("exposition-tool-cache")),
    })
    .unwrap();
    let tools = service.refresh_toolchains().await;
    let get = |id: &str| tools.iter().find(|t| t.id == id).unwrap().clone();
    for id in ["lean", "lake"] {
        let tool = get(id);
        assert_eq!(tool.status, ToolStatus::Available, "{tool:?}");
        assert_eq!(
            tool.path.as_deref(),
            Some(&*new.join(exe(id)).display().to_string()),
            "highest installed toolchain"
        );
    }
    assert_eq!(get("elan").path.as_deref(), Some(&*bin.join(exe("elan")).display().to_string()));
    assert_eq!(get("elan").status, ToolStatus::Available);

    let shim_log = std::fs::read_to_string(bin.join("invocations.log")).unwrap_or_default();
    assert_eq!(
        shim_log.lines().collect::<Vec<_>>(),
        ["elan --version"],
        "only `elan --version` may run in ELAN_HOME/bin"
    );
    let toolchain_log = std::fs::read_to_string(new.join("invocations.log")).unwrap();
    assert!(toolchain_log.contains("lean --version") && toolchain_log.contains("lake --version"), "{toolchain_log}");
    assert_eq!(listing(&elan.join("toolchains")), toolchains_before, "toolchains changed during discovery");
    assert!(!old.join("invocations.log").exists());

    // Jobs that name `lean` get the toolchain binary, and elan's shim directory
    // is not on the constructed PATH.
    match service.locate_tool("lean") {
        exposition_execution::Located::Found { path, .. } => assert!(path.starts_with(&new)),
        other => panic!("{other:?}"),
    }
}
