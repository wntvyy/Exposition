//! R and Julia workflow batches (M9) against the real execution service.
//!
//! - Refusals need neither Docker nor R/Julia.
//! - Trusted native tests run when research discovery finds `Rscript` /
//!   `julia` on this computer (otherwise they print `SKIPPED`).
//! - Restricted (Docker) tests are `#[ignore]`d: they need Docker Desktop with
//!   the local images `r-base:4.6.2` and `julia:1.13` (never pulled; override
//!   with `EXPOSITION_TEST_R_IMAGE` / `EXPOSITION_TEST_JULIA_IMAGE`) and
//!   `python:3.13-slim`. Run them with
//!   `EXPOSITION_DOCKER_TESTS=1 cargo test -p exposition-research --test batch_languages -- --ignored`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use exposition_core::artifact::RunStatus;
use exposition_core::path::WorkspacePath;
use exposition_execution::env::HostEnv;
use exposition_execution::{DockerSettings, ExecutionConfig, ExecutionService, ExecutionSettings};
use exposition_research::batch::{
    BatchCode, BatchControl, BatchInput, BatchLanguage, BatchMode, BatchOwner, BatchPort, BatchRequest, BatchResult,
    BatchRunner,
};
use exposition_research::helper::sha256_hex;
use exposition_research::launcher::ExecutionLauncher;
use exposition_research::sandbox::batch_runner;
use exposition_research::types::{RunExecution, SessionOwner};
use exposition_research::{ResearchConfig, ResearchError, ResearchService};
use exposition_workspace::DocumentService;

const PYTHON_IMAGE: &str = "python:3.13-slim";
const FAKE_KEY: &str = "sk-ant-api03-FAKEFAKEFAKE-rj-batch-test-0123456789";
const R: BatchLanguage = BatchLanguage::R;
const JULIA: BatchLanguage = BatchLanguage::Julia;

fn r_image() -> String {
    std::env::var("EXPOSITION_TEST_R_IMAGE").unwrap_or_else(|_| "r-base:4.6.2".into())
}

fn julia_image() -> String {
    std::env::var("EXPOSITION_TEST_JULIA_IMAGE").unwrap_or_else(|_| "julia:1.13".into())
}

fn image_of(language: BatchLanguage) -> String {
    match language {
        BatchLanguage::R => r_image(),
        BatchLanguage::Julia => julia_image(),
        BatchLanguage::Python => PYTHON_IMAGE.into(),
    }
}

fn docker_enabled() -> bool {
    let on = std::env::var("EXPOSITION_DOCKER_TESTS").is_ok_and(|v| v == "1");
    if !on {
        eprintln!(
            "SKIPPED: set EXPOSITION_DOCKER_TESTS=1 (Docker Desktop with local {} and {})",
            r_image(),
            julia_image()
        );
    }
    on
}

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    data: PathBuf,
    execution: ExecutionService,
    research: Arc<ResearchService>,
    runner: Arc<dyn BatchRunner>,
}

struct Images<'a> {
    python: Option<&'a str>,
    r: Option<&'a str>,
    julia: Option<&'a str>,
}

const NO_IMAGES: Images<'static> = Images { python: None, r: None, julia: None };

fn fixture_with(images: Images<'_>, trusted: bool) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let root = base.join("project");
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let data = base.join("appdata");
    let host = HostEnv::from_pairs(std::env::vars().filter(|(k, _)| k != "ANTHROPIC_API_KEY").chain([
        ("ANTHROPIC_API_KEY".to_string(), FAKE_KEY.to_string()),
        ("EXPOSITION_TEST_HOST_ONLY".to_string(), "host-only-value".to_string()),
    ]));
    let settings = ExecutionSettings {
        docker: DockerSettings {
            image: images.python.map(str::to_owned),
            r_image: images.r.map(str::to_owned),
            julia_image: images.julia.map(str::to_owned),
            ..Default::default()
        },
        ..Default::default()
    };
    let execution = ExecutionService::with_config(ExecutionConfig {
        data_dir: data.clone(),
        host_env: host,
        settings: Some(settings),
        too_broad_to_trust: Some(vec![]),
        tool_cache_dir: None,
    })
    .unwrap();
    if trusted {
        execution.grant_trust(&root.display().to_string()).unwrap();
    }
    let documents = Arc::new(DocumentService::new());
    let research = Arc::new(ResearchService::new(ResearchConfig::new(
        data.clone(),
        Arc::new(ExecutionLauncher::new(execution.clone())),
        documents,
    )));
    research.set_workspace(&root, trusted).unwrap();
    let runner = batch_runner(research.clone(), execution.clone());
    Fixture { _tmp: tmp, root, data, execution, research, runner }
}

fn restricted_fixture(language: BatchLanguage) -> Fixture {
    let image = image_of(language);
    let images = match language {
        BatchLanguage::R => Images { python: Some(PYTHON_IMAGE), r: Some(&image), julia: None },
        _ => Images { python: Some(PYTHON_IMAGE), r: None, julia: Some(&image) },
    };
    fixture_with(images, false)
}

/// A trusted fixture, or `None` (SKIPPED) when `language` is not installed.
fn native_fixture(language: BatchLanguage) -> Option<Fixture> {
    let f = fixture_with(NO_IMAGES, true);
    match f.runner.language_availability(&f.root, language, BatchMode::TrustedNative) {
        Ok(()) => Some(f),
        Err(reason) => {
            eprintln!("SKIPPED: no {} for trusted native batches ({reason})", language.title());
            None
        }
    }
}

fn write(root: &Path, rel: &str, bytes: &[u8]) -> BatchInput {
    let path = WorkspacePath::parse(rel).unwrap();
    let host = path.components().fold(root.to_path_buf(), |p, c| p.join(c));
    std::fs::create_dir_all(host.parent().unwrap()).unwrap();
    std::fs::write(&host, bytes).unwrap();
    BatchInput { path, sha256: format!("sha256:{}", sha256_hex(bytes)) }
}

fn request(mode: BatchMode, code: &str, inputs: Vec<BatchInput>, outputs: Vec<BatchPort>) -> BatchRequest {
    BatchRequest {
        mode,
        owner: BatchOwner { run: "wfrun-rj".into(), step: "analyze".into() },
        code: BatchCode::Inline { source: code.into() },
        inputs,
        outputs,
        time_limit: Duration::from_secs(180),
        max_output_bytes: 8 * 1024 * 1024,
        restricted_image_id: None,
    }
}

fn port(name: &str, exts: &[&str]) -> BatchPort {
    BatchPort { port: name.into(), extensions: exts.iter().map(|e| e.to_string()).collect() }
}

fn standard_ports() -> Vec<BatchPort> {
    vec![port("dataset", &["csv"]), port("figure", &["png", "svg"]), port("table", &["csv"])]
}

/// Run a `language` batch; `cancel_after` cancels that long after dispatch.
fn run(
    f: &Fixture,
    language: BatchLanguage,
    request: &BatchRequest,
    cancel_after: Option<Duration>,
) -> (Result<BatchResult, ResearchError>, Option<String>) {
    let dispatched: Mutex<Option<(String, Instant)>> = Mutex::new(None);
    let canceled = || match (cancel_after, dispatched.lock().unwrap().as_ref()) {
        (Some(after), Some((_, at))) => at.elapsed() >= after,
        _ => false,
    };
    let on_dispatched = |id: &str| {
        let mut slot = dispatched.lock().unwrap();
        assert!(slot.is_none(), "on_dispatched is called once");
        *slot = Some((id.to_owned(), Instant::now()));
    };
    let control = BatchControl { canceled: &canceled, on_dispatched: &on_dispatched };
    let result = f.runner.run_language(&f.root, language, request, &control);
    let id = dispatched.lock().unwrap().as_ref().map(|(id, _)| id.clone());
    (result, id)
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    names.sort();
    names
}

fn artifact_dir(f: &Fixture, run: &str) -> PathBuf {
    f.root.join(".exposition").join("artifacts").join(run)
}

fn containers_of(f: &Fixture) -> String {
    let filter = format!("label=org.exposition.instance={}", f.execution.docker_instance());
    let out = std::process::Command::new("docker").args(["ps", "-aq", "--filter", &filter]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn assert_staging_removed(f: &Fixture) {
    let batches = f.data.join("research").join("batches");
    assert!(names_in(&batches).is_empty(), "staging folders are removed: {:?}", names_in(&batches));
}

fn assert_clean(f: &Fixture) {
    assert_eq!(containers_of(f), "", "no container is left behind");
    assert_staging_removed(f);
    let jobs = f.data.join("execution").join("jobs");
    assert!(names_in(&jobs).is_empty(), "job output areas are removed: {:?}", names_in(&jobs));
}

fn assert_nothing_ran(f: &Fixture, dispatched: Option<String>) {
    assert!(dispatched.is_none(), "nothing was dispatched");
    assert!(names_in(&f.root.join(".exposition").join("runs")).is_empty(), "no run was recorded");
    assert_staging_removed(f);
}

fn image_id(image: &str) -> String {
    let out = std::process::Command::new("docker")
        .args(["image", "inspect", "--format", "{{.Id}}", image])
        .output()
        .expect("docker image inspect");
    let id = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    assert!(id.starts_with("sha256:"), "{image} must be present locally: {id}");
    id
}

// ------------------------------------------------------------------ code ----

const R_ANALYSIS: &str = r#"
out <- Sys.getenv("EXPOSITION_OUTPUT_DIR")
d <- read.csv("data/obs.csv")
total <- sum(d$y)
write.csv(data.frame(n = nrow(d), total = total), file.path(out, "dataset.csv"), row.names = FALSE)
write.csv(data.frame(x = d$x, y2 = d$y * 2), file.path(out, "table.csv"), row.names = FALSE)
png(file.path(out, "figure.png"), width = 320, height = 240)
plot(d$x, d$y, type = "b")
invisible(dev.off())
writeLines("extra output", file.path(out, "notes.txt"))
cat("rows", nrow(d), "total", total, "\n")
cat("undeclared-visible", file.exists("helpers.R"), "\n")
cat("key-visible", nzchar(Sys.getenv("ANTHROPIC_API_KEY")), "\n")
"#;

const JULIA_ANALYSIS: &str = r#"
out = ENV["EXPOSITION_OUTPUT_DIR"]
rows = [split(l, ",") for l in readlines("data/obs.csv")[2:end]]
ys = [parse(Float64, r[2]) for r in rows]
total = sum(ys)
open(joinpath(out, "dataset.csv"), "w") do f
    println(f, "n,total")
    println(f, length(rows), ",", total)
end
open(joinpath(out, "table.csv"), "w") do f
    println(f, "x,y2")
    for r in rows
        println(f, r[1], ",", 2 * parse(Float64, r[2]))
    end
end
bars = join(["<rect x=\"$(20 * (i - 1))\" y=\"0\" width=\"15\" height=\"$(10 * y)\"/>" for (i, y) in enumerate(ys)])
open(joinpath(out, "figure.svg"), "w") do f
    print(f, "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"100\" height=\"100\">$bars</svg>")
end
write(joinpath(out, "notes.txt"), "extra output")
println("rows ", length(rows), " total ", total)
println("undeclared-visible ", isfile("helpers.jl"))
println("key-visible ", haskey(ENV, "ANTHROPIC_API_KEY"))
"#;

fn analysis(language: BatchLanguage) -> &'static str {
    match language {
        BatchLanguage::R => R_ANALYSIS,
        _ => JULIA_ANALYSIS,
    }
}

/// Checks shared by native and restricted analysis runs; returns the result.
fn run_analysis(f: &Fixture, language: BatchLanguage, mode: BatchMode, image_id: Option<String>) -> BatchResult {
    let data = write(&f.root, "data/obs.csv", b"x,y\n1,2.5\n2,3.5\n");
    std::fs::write(f.root.join("helpers.R"), "VALUE <- 1\n").unwrap();
    std::fs::write(f.root.join("helpers.jl"), "VALUE = 1\n").unwrap();
    let mut req = request(mode, analysis(language), vec![data.clone()], standard_ports());
    req.restricted_image_id = image_id;
    let (result, dispatched) = run(f, language, &req, None);
    let result = result.unwrap();
    assert_eq!(result.status, RunStatus::Succeeded, "{}", result.log);
    assert_eq!(dispatched.as_deref(), Some(result.research_run.as_str()));
    assert_eq!(result.exit_code, Some(0));
    let total = if language == R { "rows 2 total 6" } else { "rows 2 total 6.0" };
    assert!(result.log.contains(total), "{}", result.log);
    let flag = |name: &str, value: bool| {
        let v = match (language, value) {
            (BatchLanguage::R, true) => "TRUE",
            (BatchLanguage::R, false) => "FALSE",
            (_, true) => "true",
            (_, false) => "false",
        };
        format!("{name} {v}")
    };
    assert!(result.log.contains(&flag("undeclared-visible", false)), "only declared files: {}", result.log);
    assert!(result.log.contains(&flag("key-visible", false)), "no host key: {}", result.log);
    assert!(!result.log.contains(FAKE_KEY));
    assert!(result.rejected.is_empty(), "{:?}", result.rejected);
    let figure = if language == R { "figure.png" } else { "figure.svg" };
    let ports: Vec<(&str, &str)> =
        result.ports.iter().map(|p| (p.port.as_str(), p.artifact.id.0.rsplit('/').next().unwrap())).collect();
    assert_eq!(ports, [("dataset", "dataset.csv"), ("figure", figure), ("table", "table.csv")]);
    assert_eq!(result.extra.len(), 1, "notes.txt is kept as an extra artifact");
    let dir = artifact_dir(f, &result.research_run);
    let mut expected = vec!["dataset.csv", figure, "notes.txt", "table.csv", "transcript.json"];
    expected.sort();
    assert_eq!(names_in(&dir), expected);
    for p in &result.ports {
        let path = p.artifact.path.as_ref().unwrap();
        let host = path.components().fold(f.root.clone(), |acc, c| acc.join(c));
        let bytes = std::fs::read(&host).unwrap();
        assert_eq!(p.artifact.content_hash, format!("sha256:{}", sha256_hex(&bytes)));
        assert_eq!(p.artifact.size_bytes, bytes.len() as u64);
    }
    let dataset = std::fs::read_to_string(dir.join("dataset.csv")).unwrap();
    assert!(dataset.contains("2,6"), "{dataset}");
    if language == R {
        assert!(std::fs::read(dir.join("figure.png")).unwrap().starts_with(b"\x89PNG"), "a real PNG");
    }
    let manifest = f.research.run_manifest(&result.research_run).unwrap();
    assert_eq!(manifest.session.owner, SessionOwner::Workflow { run: "wfrun-rj".into(), step: "analyze".into() });
    assert_eq!(manifest.record.inputs.len(), 1);
    assert_eq!(manifest.record.inputs[0].content_hash.as_deref(), Some(data.sha256.as_str()));
    assert_eq!(manifest.record.outputs.len(), 4);
    assert_eq!(manifest.code.text.as_deref(), Some(analysis(language)), "the code is recorded verbatim");
    // Reachable through the research service like any run artifact.
    let (_, bytes) = f.research.artifact_bytes(&result.research_run, &result.ports[0].artifact.id.0).unwrap();
    assert_eq!(bytes, dataset.as_bytes());
    result
}

// -------------------------------------------------------------- refusals ----

/// A language without its own image is refused even when the Python image
/// is configured: never the Python image, never native.
#[test]
fn restricted_r_and_julia_without_their_image_are_refused() {
    let f = fixture_with(Images { python: Some(PYTHON_IMAGE), r: None, julia: None }, true);
    for (language, setting) in [(R, "execution.docker.rImage"), (JULIA, "execution.docker.juliaImage")] {
        let reason = f.runner.language_availability(&f.root, language, BatchMode::Restricted).unwrap_err();
        assert!(reason.contains(setting), "{reason}");
        let err = f.runner.restricted_image(language).unwrap_err();
        assert!(err.contains(setting), "{err}");
        let (result, dispatched) =
            run(&f, language, &request(BatchMode::Restricted, "1", Vec::new(), Vec::new()), None);
        let err = result.unwrap_err();
        assert_eq!(err.code(), "research.environmentUnavailable", "{err}");
        assert!(err.to_string().contains("nothing runs natively"), "{err}");
        assert_nothing_ran(&f, dispatched);
    }
}

#[test]
fn native_r_and_julia_need_a_trusted_folder() {
    let f = fixture_with(NO_IMAGES, false);
    for language in [R, JULIA] {
        let reason = f.runner.language_availability(&f.root, language, BatchMode::TrustedNative).unwrap_err();
        assert!(reason.contains("trusted"), "{reason}");
        let (result, dispatched) =
            run(&f, language, &request(BatchMode::TrustedNative, "1", Vec::new(), Vec::new()), None);
        assert_eq!(result.unwrap_err().code(), "research.environmentUnavailable");
        assert_nothing_ran(&f, dispatched);
    }
}

/// A runner that implements only Python refuses other languages (the trait's
/// defaults): an R step never runs as Python.
#[test]
fn a_runner_without_language_support_refuses_other_languages() {
    struct PythonOnly;
    impl BatchRunner for PythonOnly {
        fn availability(&self, _root: &Path, _mode: BatchMode) -> Result<(), String> {
            Ok(())
        }
        fn run(&self, _: &Path, _: &BatchRequest, _: &BatchControl<'_>) -> Result<BatchResult, ResearchError> {
            panic!("a Python-only runner must not be asked to run R or Julia code");
        }
    }
    let runner = PythonOnly;
    let canceled = || false;
    let on_dispatched = |_: &str| panic!("not dispatched");
    let control = BatchControl { canceled: &canceled, on_dispatched: &on_dispatched };
    for language in [R, JULIA] {
        assert!(runner.language_availability(Path::new("."), language, BatchMode::TrustedNative).is_err());
        assert!(runner.restricted_image(language).is_err());
        let req = request(BatchMode::TrustedNative, "1", Vec::new(), Vec::new());
        let err = runner.run_language(Path::new("."), language, &req, &control).unwrap_err();
        assert_eq!(err.code(), "research.languageUnsupported");
    }
    assert!(runner.language_availability(Path::new("."), BatchLanguage::Python, BatchMode::Restricted).is_ok());
}

/// A batch's script must be of the batch's language (checked before
/// anything is staged or run).
#[test]
fn scripts_of_another_language_are_refused() {
    let Some(f) = native_fixture(R) else { return };
    for (language, rel, body) in
        [(R, "src/analysis.py", &b"print(1)\n"[..]), (R, "src/model.jl", b"1\n"), (JULIA, "src/analysis.R", b"1\n")]
    {
        let script = write(&f.root, rel, body);
        let mut req = request(BatchMode::TrustedNative, "", Vec::new(), Vec::new());
        req.code = BatchCode::Script { path: script.path.clone(), sha256: script.sha256.clone() };
        let (result, dispatched) = run(&f, language, &req, None);
        let err = result.unwrap_err();
        // (Julia may be missing on this host: then the batch is refused earlier.)
        if language == JULIA && err.code() == "research.environmentUnavailable" {
            continue;
        }
        assert_eq!(err.code(), "research.invalidRequest", "{rel}: {err}");
        assert!(err.to_string().contains(language.title()), "{err}");
        assert_nothing_ran(&f, dispatched);
    }
}

// ---------------------------------------------------------------- native ----

#[test]
fn native_r_batch_reads_declared_input_and_registers_outputs() {
    let Some(f) = native_fixture(R) else { return };
    let result = run_analysis(&f, R, BatchMode::TrustedNative, None);
    let manifest = f.research.run_manifest(&result.research_run).unwrap();
    match manifest.execution.as_ref().unwrap() {
        RunExecution::TrustedNative { interpreter } => assert!(interpreter.contains("Rscript"), "{interpreter}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(manifest.session.interpreter.implementation, "r");
    assert!(result.environment.iter().any(|(k, v)| k == "mode" && v.contains("trusted native")));
    assert!(result.environment.iter().any(|(k, v)| k == "rVersion" && v.starts_with('4')), "{:?}", result.environment);
    assert!(f.research.sessions().is_empty(), "never the user's kernel");
    assert_staging_removed(&f);
}

#[test]
fn native_julia_batch_reads_declared_input_and_registers_outputs() {
    let Some(f) = native_fixture(JULIA) else { return };
    let result = run_analysis(&f, JULIA, BatchMode::TrustedNative, None);
    let manifest = f.research.run_manifest(&result.research_run).unwrap();
    match manifest.execution.as_ref().unwrap() {
        RunExecution::TrustedNative { interpreter } => assert!(interpreter.contains("julia"), "{interpreter}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(manifest.session.interpreter.implementation, "julia");
    assert!(
        result.environment.iter().any(|(k, v)| k == "juliaVersion" && v.starts_with('1')),
        "{:?}",
        result.environment
    );
    assert!(f.research.sessions().is_empty(), "never the user's kernel");
    assert_staging_removed(&f);
}

/// Errors are reported in the language's terms; an undeclared file is simply
/// absent (an undeclared `source`/`include` fails visibly).
#[test]
fn native_errors_and_undeclared_sources_fail_visibly() {
    for (language, code, expect) in [
        (R, "source('helpers.R')\n", "cannot open"),
        (R, "stop('boom')\n", "boom"),
        (JULIA, "include(\"helpers.jl\")\n", "SystemError"),
        (JULIA, "error(\"boom\")\n", "boom"),
    ] {
        let Some(f) = native_fixture(language) else { continue };
        std::fs::write(f.root.join("helpers.R"), "VALUE <- 1\n").unwrap();
        std::fs::write(f.root.join("helpers.jl"), "VALUE = 1\n").unwrap();
        let (result, _) = run(&f, language, &request(BatchMode::TrustedNative, code, Vec::new(), Vec::new()), None);
        let result = result.unwrap();
        match &result.status {
            RunStatus::Failed { message } => {
                assert!(message.contains("exited with code"), "{message}");
                assert!(message.contains(expect), "{language:?} {code}: {message}\n{}", result.log);
            }
            other => panic!("{other:?}"),
        }
        let manifest = f.research.run_manifest(&result.research_run).unwrap();
        let error = manifest.error.expect("the error is recorded");
        assert!(format!("{}: {}", error.ename, error.evalue).contains(expect), "{error:?}");
    }
}

#[test]
fn native_time_limit_and_cancel_stop_the_process() {
    for (language, sleep) in [(R, "Sys.sleep(120)\n"), (JULIA, "sleep(120)\n")] {
        let Some(f) = native_fixture(language) else { continue };
        let mut req = request(BatchMode::TrustedNative, sleep, Vec::new(), Vec::new());
        req.time_limit = Duration::from_secs(4);
        let started = Instant::now();
        let (result, _) = run(&f, language, &req, None);
        match result.unwrap().status {
            RunStatus::Failed { message } => assert!(message.contains("time limit"), "{message}"),
            other => panic!("{other:?}"),
        }
        req.time_limit = Duration::from_secs(120);
        let (result, _) = run(&f, language, &req, Some(Duration::from_secs(2)));
        assert_eq!(result.unwrap().status, RunStatus::Canceled);
        assert!(started.elapsed() < Duration::from_secs(60), "{:?}", started.elapsed());
        assert_staging_removed(&f);
    }
}

/// A declared script runs at its project path; a script of the language is
/// staged with the declared inputs only.
#[test]
fn native_declared_scripts_run_with_their_declared_inputs() {
    for (language, rel, body, expect) in [
        (R, "analysis/fit.R", "d <- read.csv('data/obs.csv')\ncat('script rows', nrow(d), '\\n')\n", "script rows 2"),
        (
            JULIA,
            "analysis/fit.jl",
            "println(\"script rows \", length(readlines(\"data/obs.csv\")) - 1)\n",
            "script rows 2",
        ),
    ] {
        let Some(f) = native_fixture(language) else { continue };
        let data = write(&f.root, "data/obs.csv", b"x,y\n1,2.5\n2,3.5\n");
        let script = write(&f.root, rel, body.as_bytes());
        let mut req = request(BatchMode::TrustedNative, "", vec![data], Vec::new());
        req.code = BatchCode::Script { path: script.path.clone(), sha256: script.sha256.clone() };
        let (result, _) = run(&f, language, &req, None);
        let result = result.unwrap();
        assert_eq!(result.status, RunStatus::Succeeded, "{}", result.log);
        assert!(result.log.contains(expect), "{}", result.log);
        // A script changed after it was declared is refused.
        std::fs::write(f.root.join(rel), "changed\n").unwrap();
        let (result, dispatched) = run(&f, language, &req, None);
        assert_eq!(result.unwrap_err().code(), "research.inputChanged");
        assert!(dispatched.is_none());
    }
}

// ------------------------------------------------------------ restricted ----

#[test]
#[ignore = "needs Docker Desktop and the local r-base/julia images; run with EXPOSITION_DOCKER_TESTS=1 -- --ignored"]
fn restricted_r_and_julia_batches_register_outputs_in_their_own_image() {
    if !docker_enabled() {
        return;
    }
    for language in [R, JULIA] {
        let f = restricted_fixture(language);
        let image = image_of(language);
        assert!(f.runner.language_availability(&f.root, language, BatchMode::Restricted).is_ok());
        let (reported, id) = f.runner.restricted_image(language).unwrap();
        assert_eq!((reported.as_str(), id.clone()), (image.as_str(), Some(image_id(&image))));
        let result = run_analysis(&f, language, BatchMode::Restricted, id.clone());
        let manifest = f.research.run_manifest(&result.research_run).unwrap();
        match manifest.execution.as_ref().unwrap() {
            RunExecution::RestrictedContainer { backend, image: recorded, image_id, network, user, .. } => {
                assert_eq!((backend.as_str(), recorded.as_str(), network.as_str()), ("docker", image.as_str(), "none"));
                assert_eq!(image_id, &id, "the identity it ran in is recorded");
                assert_ne!(user, "0");
            }
            other => panic!("{other:?}"),
        }
        let program = if language == R { "Rscript" } else { "julia" };
        assert!(result.environment.iter().any(|(k, v)| k == "interpreter" && v == program), "{:?}", result.environment);
        assert!(result.environment.iter().any(|(k, v)| k == "image" && *v == image));
        assert!(manifest.session.interpreter.location.contains(&image), "{:?}", manifest.session.interpreter);
        assert_clean(&f);
    }
}

const R_CONTAINMENT: &str = r#"
cat("visible:", paste(sort(list.files(".")), collapse = ","), "|", paste(list.files("data"), collapse = ","), "\n")
cat("uid", system("id -u", intern = TRUE), "\n")
net <- tryCatch({ con <- socketConnection("1.1.1.1", 53, timeout = 3); close(con); "CONNECTED" },
                error = function(e) "blocked", warning = function(w) "blocked")
cat("network", net, "\n")
env <- Sys.getenv()
for (name in names(env)) cat("ENV ", name, "=", env[[name]], "\n", sep = "")
w <- tryCatch({ writeLines("x", "new.txt"); "WROTE" }, error = function(e) "refused", warning = function(w) "refused")
cat("new file", w, "\n")
writeLines("tampered", "data/input.csv")
"#;

const JULIA_CONTAINMENT: &str = r#"
using Sockets
println("visible: ", join(sort(readdir(".")), ","), " | ", join(readdir("data"), ","))
println("uid ", ccall(:getuid, Cuint, ()))
net = try
    close(connect(ip"1.1.1.1", 53)); "CONNECTED"
catch
    "blocked"
end
println("network ", net)
for (k, v) in ENV
    println("ENV ", k, "=", v)
end
w = try
    write("new.txt", "x"); "WROTE"
catch
    "refused"
end
println("new file ", w)
write("data/input.csv", "tampered")
"#;

#[test]
#[ignore = "needs Docker Desktop and the local r-base/julia images; run with EXPOSITION_DOCKER_TESTS=1 -- --ignored"]
fn restricted_r_and_julia_have_no_network_no_host_environment_read_only_inputs_and_run_as_non_root() {
    if !docker_enabled() {
        return;
    }
    for (language, code, entry) in
        [(R, R_CONTAINMENT, "__exposition_main__.R"), (JULIA, JULIA_CONTAINMENT, "__exposition_main__.jl")]
    {
        let f = restricted_fixture(language);
        let data = write(&f.root, "data/input.csv", b"a\n1\n");
        std::fs::write(f.root.join("secret.txt"), "undeclared").unwrap();
        std::fs::write(f.root.join("paper.tex"), "manuscript").unwrap();
        let (result, _) = run(&f, language, &request(BatchMode::Restricted, code, vec![data], Vec::new()), None);
        let result = result.unwrap();
        let log = &result.log;
        assert!(
            matches!(result.status, RunStatus::Failed { .. }),
            "writing a declared input fails: {:?}\n{log}",
            result.status
        );
        assert!(log.contains(&format!("visible: {entry},data | input.csv")), "{log}");
        assert!(log.contains("uid 65534"), "{log}");
        assert!(log.contains("network blocked") && !log.contains("CONNECTED"), "{log}");
        assert!(log.contains("new file refused") && !log.contains("WROTE"), "{log}");
        assert!(log.contains("Read-only file system") || log.contains("read-only"), "{log}");
        let env: Vec<&str> = log.lines().filter_map(|l| l.strip_prefix("ENV ")).collect();
        assert!(env.contains(&"EXPOSITION_OUTPUT_DIR=/out"), "{env:?}");
        for name in
            ["ANTHROPIC_API_KEY", "EXPOSITION_TEST_HOST_ONLY", "USERPROFILE", "USERNAME", "APPDATA", "GITHUB_TOKEN"]
        {
            assert!(!env.iter().any(|l| l.starts_with(&format!("{name}="))), "{name} reached the container: {env:?}");
        }
        assert!(!log.contains(FAKE_KEY));
        assert!(!env.iter().any(|l| l.contains(r"C:\") || l.contains("Users")), "host paths leaked: {env:?}");
        assert_eq!(std::fs::read(f.root.join("data/input.csv")).unwrap(), b"a\n1\n", "the project file is untouched");
        assert!(!f.root.join("new.txt").exists());
        assert_clean(&f);
    }
}

const R_ESCAPES: &str = r#"
out <- Sys.getenv("EXPOSITION_OUTPUT_DIR")
attempt <- function(label, f) {
  ok <- tryCatch({ f(); TRUE }, error = function(e) FALSE, warning = function(w) FALSE)
  cat(if (ok) "made" else "failed", label, "\n")
}
attempt("abs-symlink", function() stopifnot(file.symlink("/etc/passwd", file.path(out, "table.csv"))))
attempt("rel-symlink", function() stopifnot(file.symlink("../workspace/data/input.csv", file.path(out, "rel.csv"))))
attempt("dir-symlink", function() stopifnot(file.symlink("/etc", file.path(out, "etc.csv"))))
attempt("directory", function() { dir.create(file.path(out, "nested")); writeLines("x", file.path(out, "nested", "inner.csv")) })
attempt("parent", function() writeLines("x", file.path(out, "..", "x.csv")))
attempt("hardlink", function() { writeLines("1", file.path(out, "a.csv")); stopifnot(file.link(file.path(out, "a.csv"), file.path(out, "b.csv"))) })
attempt("input-hardlink", function() stopifnot(file.link("/workspace/data/input.csv", file.path(out, "input.csv"))))
attempt("oversize", function() writeLines(strrep("1", 200000), file.path(out, "big.csv")))
attempt("mislabelled", function() writeLines("<html><script>alert(1)</script>", file.path(out, "fake.png")))
attempt("hidden", function() writeLines("x", file.path(out, ".hidden.csv")))
attempt("exe", function() writeLines("MZ", file.path(out, "tool.exe")))
attempt("ok", function() writeLines(c("a", "1"), file.path(out, "ok.csv")))
cat("listing", paste(sort(list.files(out, all.files = TRUE, no.. = TRUE)), collapse = ","), "\n")
"#;

const JULIA_ESCAPES: &str = r#"
out = ENV["EXPOSITION_OUTPUT_DIR"]
function attempt(label, f)
    try
        f(); println("made ", label)
    catch e
        println("failed ", label, " ", typeof(e))
    end
end
attempt("abs-symlink", () -> symlink("/etc/passwd", joinpath(out, "table.csv")))
attempt("rel-symlink", () -> symlink("../workspace/data/input.csv", joinpath(out, "rel.csv")))
attempt("dir-symlink", () -> symlink("/etc", joinpath(out, "etc.csv")))
attempt("directory", () -> (mkdir(joinpath(out, "nested")); write(joinpath(out, "nested", "inner.csv"), "x")))
attempt("parent", () -> write(joinpath(out, "..", "x.csv"), "x"))
attempt("hardlink", () -> (write(joinpath(out, "a.csv"), "1\n"); hardlink(joinpath(out, "a.csv"), joinpath(out, "b.csv"))))
attempt("input-hardlink", () -> hardlink("/workspace/data/input.csv", joinpath(out, "input.csv")))
attempt("oversize", () -> write(joinpath(out, "big.csv"), "1"^200000))
attempt("mislabelled", () -> write(joinpath(out, "fake.png"), "<html><script>alert(1)</script>"))
attempt("hidden", () -> write(joinpath(out, ".hidden.csv"), "x"))
attempt("exe", () -> write(joinpath(out, "tool.exe"), "MZ"))
attempt("ok", () -> write(joinpath(out, "ok.csv"), "a\n1\n"))
println("listing ", join(sort(readdir(out)), ","))
"#;

#[test]
#[ignore = "needs Docker Desktop and the local r-base/julia images; run with EXPOSITION_DOCKER_TESTS=1 -- --ignored"]
fn restricted_r_and_julia_escaping_outputs_are_rejected_and_never_copied() {
    if !docker_enabled() {
        return;
    }
    for (language, code) in [(R, R_ESCAPES), (JULIA, JULIA_ESCAPES)] {
        let f = restricted_fixture(language);
        let data = write(&f.root, "data/input.csv", b"a\n1\n");
        let mut req = request(BatchMode::Restricted, code, vec![data], vec![port("table", &["csv"])]);
        req.max_output_bytes = 64 * 1024;
        let (result, _) = run(&f, language, &req, None);
        let result = result.unwrap();
        eprintln!("{language:?} container log:\n{}\nrejected: {:#?}", result.log, result.rejected);
        assert_eq!(result.status, RunStatus::Succeeded, "{}", result.log);
        assert!(result.log.contains("failed parent"), "writing outside /out is impossible: {}", result.log);
        let accepted: Vec<&str> = result.extra.iter().map(|a| a.id.0.rsplit('/').next().unwrap()).collect();
        assert_eq!(accepted, ["ok.csv"], "{:?}", result.rejected);
        assert!(result.ports.is_empty(), "`table` is not realized by a link: {:?}", result.ports);
        let dir = artifact_dir(&f, &result.research_run);
        assert_eq!(names_in(&dir), ["ok.csv", "transcript.json"]);
        let rejected = |name: &str| result.rejected.iter().find(|r| r.name == name).map(|r| r.reason.clone());
        for name in ["big.csv", "fake.png", ".hidden.csv", "tool.exe", "nested"] {
            assert!(rejected(name).is_some(), "{name} must be rejected: {:?}", result.rejected);
        }
        for name in ["table.csv", "rel.csv", "etc.csv", "a.csv", "b.csv", "input.csv"] {
            assert!(!dir.join(name).exists(), "{name} is not in the project");
        }
        for entry in names_in(&dir) {
            let text = std::fs::read_to_string(dir.join(&entry)).unwrap_or_default();
            assert!(!text.contains("root:x:0:0"), "/etc/passwd content leaked into {entry}");
        }
        assert_clean(&f);
    }
}

/// A batch approved for one image identity runs only in that identity of
/// its own language's image (the Python image's identity does not count).
#[test]
#[ignore = "needs Docker Desktop and the local r-base/julia images; run with EXPOSITION_DOCKER_TESTS=1 -- --ignored"]
fn restricted_r_and_julia_run_only_in_the_approved_image_identity() {
    if !docker_enabled() {
        return;
    }
    for (language, code) in [
        (R, "writeLines('ran', file.path(Sys.getenv('EXPOSITION_OUTPUT_DIR'), 'dataset.txt'))\n"),
        (JULIA, "write(joinpath(ENV[\"EXPOSITION_OUTPUT_DIR\"], \"dataset.txt\"), \"ran\")\n"),
    ] {
        let f = restricted_fixture(language);
        let actual = image_id(&image_of(language));
        let mut req = request(BatchMode::Restricted, code, Vec::new(), vec![port("dataset", &["txt"])]);
        for wrong in [image_id(PYTHON_IMAGE), format!("sha256:{}", "0".repeat(64))] {
            req.restricted_image_id = Some(wrong);
            let (result, dispatched) = run(&f, language, &req, None);
            let err = result.unwrap_err();
            assert_eq!(err.code(), "research.imageChanged", "{err}");
            assert_nothing_ran(&f, dispatched);
        }
        req.restricted_image_id = Some(actual.clone());
        let (result, _) = run(&f, language, &req, None);
        let result = result.unwrap();
        assert_eq!(result.status, RunStatus::Succeeded, "{}", result.log);
        assert_eq!(result.ports.len(), 1);
        match f.research.run_manifest(&result.research_run).unwrap().execution.unwrap() {
            RunExecution::RestrictedContainer { image_id, .. } => {
                assert_eq!(image_id.as_deref(), Some(actual.as_str()))
            }
            other => panic!("{other:?}"),
        }
        assert_clean(&f);
    }
}

#[test]
#[ignore = "needs Docker Desktop and the local r-base/julia images; run with EXPOSITION_DOCKER_TESTS=1 -- --ignored"]
fn restricted_r_and_julia_cancel_and_time_limit_remove_the_container() {
    if !docker_enabled() {
        return;
    }
    for (language, code) in [
        (R, "cat('started\\n'); flush(stdout()); Sys.sleep(120); cat('NOT CANCELED\\n')\n"),
        (JULIA, "println(\"started\"); flush(stdout); sleep(120); println(\"NOT CANCELED\")\n"),
    ] {
        let f = restricted_fixture(language);
        let started = Instant::now();
        let req = request(BatchMode::Restricted, code, Vec::new(), Vec::new());
        let (result, dispatched) = run(&f, language, &req, Some(Duration::from_secs(5)));
        let result = result.unwrap();
        assert!(dispatched.is_some());
        assert_eq!(result.status, RunStatus::Canceled, "{}", result.log);
        assert!(!result.log.contains("NOT CANCELED"));
        assert_eq!(f.research.run_manifest(&result.research_run).unwrap().record.status, RunStatus::Canceled);
        assert_clean(&f);
        let mut req = req;
        req.time_limit = Duration::from_secs(6);
        let (result, _) = run(&f, language, &req, None);
        match result.unwrap().status {
            RunStatus::Failed { message } => assert!(message.contains("time limit"), "{message}"),
            other => panic!("{other:?}"),
        }
        assert!(started.elapsed() < Duration::from_secs(120), "{:?}", started.elapsed());
        assert_clean(&f);
    }
}

#[test]
#[ignore = "needs Docker Desktop; run with EXPOSITION_DOCKER_TESTS=1 -- --ignored"]
fn restricted_with_a_missing_language_image_is_refused_without_fallback() {
    if !docker_enabled() {
        return;
    }
    let missing = "exposition-test-missing-image:0.0";
    let f = fixture_with(Images { python: Some(PYTHON_IMAGE), r: Some(missing), julia: Some(missing) }, true);
    for language in [R, JULIA] {
        let reason = f.runner.language_availability(&f.root, language, BatchMode::Restricted).unwrap_err();
        assert!(reason.contains("not present locally"), "{reason}");
        let (result, dispatched) =
            run(&f, language, &request(BatchMode::Restricted, "1\n", Vec::new(), Vec::new()), None);
        assert_eq!(result.unwrap_err().code(), "research.environmentUnavailable");
        assert_nothing_ran(&f, dispatched);
    }
}
