//! Axiom audit: what the declarations of a checked revision really rest on.
//!
//! Lean's diagnostics alone cannot establish a checked proof: a `sorry` can be
//! imported, its warning can be silenced (`#guard_msgs`, `warn.sorry false`),
//! a proof can use a user `axiom` or `native_decide` (trusting compiled code),
//! `#exit` can stop elaboration early, and declarations can be added without
//! the kernel (`debug.skipKernelTC`, set by name or computed at run time, or
//! any elaborator calling `addDeclWithoutChecking`). A Lean file is also a
//! program: its elaborators, macros, attributes, and `initialize` blocks run
//! while it (or a module importing it) is elaborated, and they can print
//! anything. So after Lean reports a revision free of errors, the service
//! audits exactly that text in two separate processes, and nothing the file
//! controls decides the verdict:
//!
//! 1. **Compile** (`Compile.lean`, runs the file's code): elaborate the exact
//!    text as a temporary module and write its `.olean`. Lean's frontend stops
//!    at a terminal command (`#exit`); whether the last command was the end of
//!    the input is read from the frontend's own command snapshots. The one
//!    result line is tagged with a per-run secret written to the process's
//!    stdin (then closed) and read before any of the file's code runs, so text
//!    printed by the file or by an imported `initialize` block cannot
//!    impersonate it.
//! 2. **Check** (`Check.lean`, runs none of the file's code): a program that
//!    imports only toolchain modules reads the compiled module and the
//!    non-toolchain modules it imports as `.olean` data, without initializing
//!    any of their environment extensions or running any of their
//!    initializers, elaborators, or macros. It re-checks every non-toolchain
//!    declaration the module depends on with Lean's kernel on top of the
//!    toolchain's environment (`Lean.Kernel.Environment.replay`, as
//!    lean4checker does), then collects the axioms of each declaration from
//!    the replayed terms. An axiom counts as standard only when it is the
//!    toolchain's own constant.
//!
//! Both programs are written by the application into its own directory and
//! compiled once per toolchain and session, each in its own directory (only
//! the compile program's is on step 1's `LEAN_PATH`). Their files are pinned
//! by SHA-256 when compiled and verified before each step, after step 1's
//! process tree has ended (job objects), so a file that rewrites them while it
//! is compiled gets `Unverified` and the next audit compiles a fresh copy.
//! Every job runs the toolchain's `lean` directly with the toolchain's library
//! first on `LEAN_PATH` (a project cannot shadow `Init`/`Std`/`Lean`); the
//! project's own module path comes from `lake env`. All jobs are supervised
//! `TrustedNative` jobs (step 1 runs project code, like the language server).
//! Nothing is written to the project. A speculative audit (started before
//! Lean finishes a version) runs only for imports Lean has already finished
//! with, and one that fails is run again once Lean is done.
//!
//! Limits (see `docs/capabilities.md`): a `Checked` result certifies the
//! environment Lean produced from the text, not how the text reads (a file can
//! redefine its own syntax); the file's code runs with the user's privileges
//! during step 1, so code that deliberately tampers with the toolchain or
//! other processes outside Lean is not contained; and the audit programs use
//! the Lean 4.35 APIs, so another toolchain whose APIs differ reports
//! `Unverified` with the compiler's message.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use exposition_core::job::{EnvironmentSpec, ExecutionPolicy, JobId, JobOutcome, JobSpec, OutputStream};
use exposition_execution::ExecutionService;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::{OnceCell, watch};

use crate::project::{LeanProject, ProjectKind};
use crate::toolchain::ResolvedToolchain;

/// The axioms of Lean's standard foundations. Anything else is reported.
pub const STANDARD_AXIOMS: &[&str] = &["propext", "Classical.choice", "Quot.sound"];
pub const SORRY_AXIOM: &str = "sorryAx";
const AUDIT_TIMEOUT_MS: u64 = 15 * 60 * 1000;
const AUDIT_LOG_LIMIT: u64 = 32 * 1024 * 1024;
const TOOLS_TIMEOUT_MS: u64 = 5 * 60 * 1000;
const LAKE_ENV_TIMEOUT_MS: u64 = 5 * 60 * 1000;

/// Step 1: elaborate the checked text (runs the file's code).
const COMPILE_SOURCE: &str = include_str!("Compile.lean");
/// Step 2: kernel replay and axioms from data (runs none of the file's code).
const CHECK_SOURCE: &str = include_str!("Check.lean");
const TOOL_PACKAGE: &str = "ExpositionAuditTool";
const AUDIT_PACKAGE: &str = "ExpositionAudit";

/// The toolchain axioms through which a proof trusts compiled code (older
/// toolchains: `Lean.ofReduceBool` and friends).
const TOOLCHAIN_COMPILED_CODE: &[&str] = &["Lean.ofReduceBool", "Lean.ofReduceNat", "Lean.trustCompiler"];

/// An axiom that native evaluation adds for one result (`native_decide`,
/// `bv_decide`, or any tactic using `Lean.Meta.nativeEqTrue`):
/// `<decl>._native.<tactic>.ax_…`.
pub fn is_native_evaluation_axiom(name: &str) -> bool {
    let parts: Vec<&str> = name.split('.').collect();
    parts
        .iter()
        .position(|p| *p == "_native")
        .is_some_and(|i| i + 2 < parts.len() && parts.last().is_some_and(|l| l.starts_with("ax")))
}

/// True for axioms through which a proof trusts compiled code instead of the
/// kernel (by name; [`AxiomRef::kind`] also takes the axiom's origin into account).
pub fn is_compiled_code_axiom(name: &str) -> bool {
    TOOLCHAIN_COMPILED_CODE.contains(&name) || is_native_evaluation_axiom(name)
}

/// An axiom a declaration depends on.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AxiomRef {
    pub name: String,
    /// The toolchain's own constant (from the toolchain's library), as opposed
    /// to one declared by the project, its dependencies, or the file.
    pub toolchain: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AxiomKind {
    /// `propext`, `Classical.choice`, `Quot.sound` of the toolchain.
    Standard,
    /// The toolchain's `sorryAx`.
    Sorry,
    /// Trusts compiled code (`native_decide` and the like).
    CompiledCode,
    /// Any other axiom, including a non-toolchain one named like the above.
    Other,
}

impl AxiomRef {
    pub fn kind(&self) -> AxiomKind {
        let name = self.name.as_str();
        if self.toolchain && STANDARD_AXIOMS.contains(&name) {
            AxiomKind::Standard
        } else if self.toolchain && name == SORRY_AXIOM {
            AxiomKind::Sorry
        } else if is_native_evaluation_axiom(name) || (self.toolchain && TOOLCHAIN_COMPILED_CODE.contains(&name)) {
            AxiomKind::CompiledCode
        } else {
            AxiomKind::Other
        }
    }

    /// The name shown to users; a non-toolchain axiom that reuses the name of
    /// a toolchain axiom is marked so it cannot pass for it.
    pub fn display(&self) -> String {
        let name = self.name.as_str();
        let reused = STANDARD_AXIOMS.contains(&name) || name == SORRY_AXIOM || TOOLCHAIN_COMPILED_CODE.contains(&name);
        if !self.toolchain && reused {
            format!("{} (declared outside the toolchain)", self.name)
        } else {
            self.name.clone()
        }
    }
}

/// One constant of the audited module and the axioms it depends on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclAxioms {
    /// Fully qualified name (private names shown without their `_private` prefix).
    pub name: String,
    pub internal: bool,
    pub axioms: Vec<AxiomRef>,
    /// Zero-based (line, UTF-16 column) of the declaration name's start and end.
    pub range: Option<((u32, u32), (u32, u32))>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuditOutcome {
    /// Every declaration of the file was elaborated, re-checked by the
    /// kernel, and audited.
    Complete(Vec<DeclAxioms>),
    /// Elaboration stopped before the end of the file (`#exit`); the
    /// declarations before that point were re-checked and audited.
    Stopped {
        decls: Vec<DeclAxioms>,
        /// Zero-based line of the command that ended elaboration.
        line: Option<u32>,
    },
    /// Lean's kernel rejected a declaration the file depends on when the audit
    /// re-checked it: something was added without (or around) the kernel.
    KernelRejected(String),
    /// The audit could not run or its output was not trustworthy.
    Failed(String),
}

pub struct AuditRequest<'a> {
    pub project: &'a LeanProject,
    pub toolchain: &'a ResolvedToolchain,
    pub text: Arc<str>,
    /// Display name of the audited file (for job labels).
    pub file_name: String,
}

/// Runs audits; owns the compiled audit programs (one set per toolchain, kept
/// for the lifetime of the service in a fresh application-owned directory).
pub struct Auditor {
    /// `<scratch>/audit`: runs go to `runs/<id>`, programs to `tools-<session>`.
    dir: PathBuf,
    session: String,
    tools: Mutex<HashMap<PathBuf, Arc<OnceCell<Tools>>>>,
}

/// Compiled audit programs and the SHA-256 of every file a run reads. The
/// two programs live in separate directories: only `compile` is on the
/// `LEAN_PATH` of the step that runs the checked file's code.
#[derive(Clone)]
struct Tools {
    compile: PathBuf,
    check: PathBuf,
    digests: Vec<(PathBuf, Vec<u8>)>,
}

impl Tools {
    /// The files are unchanged since they were compiled (a checked file's code
    /// runs with the user's privileges and could otherwise replace them for
    /// every later audit).
    fn intact(&self) -> bool {
        self.digests
            .iter()
            .all(|(path, digest)| std::fs::read(path).is_ok_and(|b| Sha256::digest(&b).as_slice() == digest))
    }
}

impl Auditor {
    /// `scratch` is application-owned; created on first use.
    pub fn new(scratch: PathBuf) -> Self {
        Auditor { dir: scratch, session: nonce(), tools: Mutex::new(HashMap::new()) }
    }

    /// Audit `request.text`. `cancel` aborts the jobs.
    pub async fn run(
        &self,
        exec: &ExecutionService,
        request: AuditRequest<'_>,
        mut cancel: watch::Receiver<bool>,
    ) -> AuditOutcome {
        let id = nonce();
        let dir = self.dir.join("runs").join(&id);
        let outcome = self.run_in(exec, &request, &dir, &id, &mut cancel).await;
        let _ = std::fs::remove_dir_all(&dir);
        outcome
    }

    async fn run_in(
        &self,
        exec: &ExecutionService,
        request: &AuditRequest<'_>,
        dir: &Path,
        id: &str,
        cancel: &mut watch::Receiver<bool>,
    ) -> AuditOutcome {
        let module_leaf = format!("T{id}");
        let module = format!("{AUDIT_PACKAGE}.{module_leaf}");
        let package = dir.join(AUDIT_PACKAGE);
        let source = package.join(format!("{module_leaf}.lean"));
        let olean = package.join(format!("{module_leaf}.olean"));
        if let Err(e) = std::fs::create_dir_all(&package).and_then(|_| std::fs::write(&source, &*request.text)) {
            return AuditOutcome::Failed(format!("could not prepare the axiom audit: {e}"));
        }
        let mut path_cancel = cancel.clone();
        let (tools, project_path) = tokio::join!(
            self.tools(exec, request.project, request.toolchain),
            project_module_path(exec, request.project, request.toolchain, &mut path_cancel),
        );
        let tools = match tools {
            Ok(t) => t,
            Err(e) => {
                return AuditOutcome::Failed(format!(
                    "the audit programs could not be compiled for {} ({e})",
                    request.toolchain.name
                ));
            }
        };
        let project_path = match project_path {
            Ok(p) => p,
            Err(e) => return AuditOutcome::Failed(format!("the project's module path could not be read ({e})")),
        };
        if *cancel.borrow() {
            return AuditOutcome::Failed("canceled".into());
        }
        let lib = toolchain_lib(request.toolchain);
        let paths = lean_path(&lib, &[&tools.compile, dir], &project_path)
            .and_then(|compile| Ok((compile, lean_path(&lib, &[&tools.check, dir], &project_path)?)));
        let (compile_path, check_path) = match paths {
            Ok(p) => p,
            Err(e) => return AuditOutcome::Failed(e),
        };
        // Step 1: compile (the file's code runs here).
        if !self.verify_tools(request.toolchain) {
            return AuditOutcome::Failed(
                "the audit programs were modified; they are compiled again for the next check".into(),
            );
        }
        let secret = nonce();
        let mut compile = job_spec(
            format!("Lean axiom audit: compile {}", request.file_name),
            request.project,
            request.toolchain.lean.display().to_string(),
            vec![
                "--run".into(),
                tools.compile.join("RunCompile.lean").display().to_string(),
                source.display().to_string(),
                olean.display().to_string(),
                module.clone(),
            ],
            Some(&compile_path),
        );
        compile.stdin = Some(format!("{secret}\n"));
        let compiled = match run(exec, compile, cancel).await {
            Ran::Exited { id, .. } => {
                parse_compile_output(&exec.output_text(&id, OutputStream::Stdout).unwrap_or_default(), &secret)
            }
            Ran::Failed(e) => Err(e),
        };
        let compiled = match compiled {
            Ok(c) => c,
            Err(e) => return AuditOutcome::Failed(format!("compiling the checked text failed ({e})")),
        };
        // Step 2: check (none of the file's code runs here).
        if !self.verify_tools(request.toolchain) {
            return AuditOutcome::Failed(
                "the audit programs were modified; they are compiled again for the next check".into(),
            );
        }
        let tag = nonce();
        let check = job_spec(
            format!("Lean axiom audit: {}", request.file_name),
            request.project,
            request.toolchain.lean.display().to_string(),
            vec![
                "--run".into(),
                tools.check.join("RunCheck.lean").display().to_string(),
                tag.clone(),
                lib.display().to_string(),
                module.clone(),
            ],
            Some(&check_path),
        );
        let checked = match run(exec, check, cancel).await {
            Ran::Exited { id, .. } => {
                parse_check_output(&exec.output_text(&id, OutputStream::Stdout).unwrap_or_default(), &tag)
            }
            Ran::Failed(e) => Err(e),
        };
        match checked {
            Ok(CheckResult::KernelRejected(message)) => AuditOutcome::KernelRejected(message),
            Ok(CheckResult::Decls(decls)) => {
                let decls = combine(decls, &compiled.ranges, &module);
                if compiled.complete {
                    AuditOutcome::Complete(decls)
                } else {
                    AuditOutcome::Stopped { decls, line: compiled.stop.map(|(line, _)| line) }
                }
            }
            Err(e) => AuditOutcome::Failed(format!("the axiom audit failed ({e})")),
        }
    }

    /// The compiled audit programs for `toolchain` (compiled on first use).
    async fn tools(
        &self,
        exec: &ExecutionService,
        project: &LeanProject,
        toolchain: &ResolvedToolchain,
    ) -> Result<Tools, String> {
        let cell = {
            let mut tools = self.tools.lock().unwrap_or_else(|e| e.into_inner());
            Arc::clone(tools.entry(toolchain.lean.clone()).or_default())
        };
        let key = hex(&Sha256::digest(toolchain.lean.display().to_string().as_bytes()))[..16].to_owned();
        let dir = self.dir.join(format!("tools-{}", self.session)).join(format!("{key}-{}", &nonce()[..8]));
        cell.get_or_try_init(|| compile_tools(exec, project, toolchain, dir)).await.cloned()
    }

    /// Check the programs' digests; on a mismatch forget them, so the next
    /// audit compiles a fresh copy.
    fn verify_tools(&self, toolchain: &ResolvedToolchain) -> bool {
        let mut tools = self.tools.lock().unwrap_or_else(|e| e.into_inner());
        let intact = tools.get(&toolchain.lean).and_then(|cell| cell.get()).is_some_and(Tools::intact);
        if !intact {
            tools.remove(&toolchain.lean);
        }
        intact
    }
}

impl Drop for Auditor {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.dir.join(format!("tools-{}", self.session)));
    }
}

/// Write and compile `Compile.lean` and `Check.lean` into `dir/compile` and
/// `dir/check` (in parallel).
async fn compile_tools(
    exec: &ExecutionService,
    project: &LeanProject,
    toolchain: &ResolvedToolchain,
    dir: PathBuf,
) -> Result<Tools, String> {
    let lib = toolchain_lib(toolchain);
    let lean_path = lean_path(&lib, &[], &[])?;
    let mut specs = Vec::new();
    let mut digested = Vec::new();
    for (name, source) in [("Compile", COMPILE_SOURCE), ("Check", CHECK_SOURCE)] {
        let root = dir.join(name.to_lowercase());
        let package = root.join(TOOL_PACKAGE);
        let stub = root.join(format!("Run{name}.lean"));
        let olean = package.join(format!("{name}.olean"));
        let lean = package.join(format!("{name}.lean"));
        std::fs::create_dir_all(&package)
            .and_then(|_| std::fs::write(&lean, source))
            .and_then(|_| std::fs::write(&stub, format!("import {TOOL_PACKAGE}.{name}\n")))
            .map_err(|e| format!("could not write the audit programs: {e}"))?;
        let mut spec = job_spec(
            format!("Lean axiom audit: prepare ({name})"),
            project,
            toolchain.lean.display().to_string(),
            vec![
                "-R".into(),
                root.display().to_string(),
                "-o".into(),
                olean.display().to_string(),
                lean.display().to_string(),
            ],
            Some(&lean_path),
        );
        spec.timeout_ms = Some(TOOLS_TIMEOUT_MS);
        specs.push(spec);
        digested.extend([stub, olean]);
    }
    // Not cancelable by one audit: other audits may be waiting for the same programs.
    let (_never, mut never_a) = watch::channel(false);
    let mut never_b = never_a.clone();
    let check_spec = specs.pop().expect("two programs");
    let compile_spec = specs.pop().expect("two programs");
    let (compile, check) = tokio::join!(run(exec, compile_spec, &mut never_a), run(exec, check_spec, &mut never_b));
    for result in [compile, check] {
        if let Ran::Failed(e) = result {
            return Err(e);
        }
    }
    let mut digests = Vec::new();
    for path in digested {
        let bytes = std::fs::read(&path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
        digests.push((path, Sha256::digest(&bytes).to_vec()));
    }
    Ok(Tools { compile: dir.join("compile"), check: dir.join("check"), digests })
}

/// `<toolchain>/lib/lean`: the toolchain's own module library.
fn toolchain_lib(toolchain: &ResolvedToolchain) -> PathBuf {
    toolchain.dir.join("lib").join("lean")
}

/// `LEAN_PATH` for the audit jobs: the toolchain's library first (so nothing
/// can shadow `Init`/`Std`/`Lean`), then the application's directories, then
/// the project's module path.
fn lean_path(lib: &Path, own: &[&Path], project: &[PathBuf]) -> Result<String, String> {
    let mut entries: Vec<PathBuf> = vec![lib.to_path_buf()];
    entries.extend(own.iter().map(|p| p.to_path_buf()));
    entries.extend(project.iter().filter(|p| !same_dir(p, lib)).cloned());
    std::env::join_paths(entries)
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|e| format!("invalid module path: {e}"))
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.display().to_string().trim_end_matches(['/', '\\']).replace('/', "\\").to_lowercase();
    if cfg!(windows) { norm(a) == norm(b) } else { a == b }
}

/// The project's `LEAN_PATH` as Lake computes it (`lake env`), or nothing for
/// a standalone file.
async fn project_module_path(
    exec: &ExecutionService,
    project: &LeanProject,
    toolchain: &ResolvedToolchain,
    cancel: &mut watch::Receiver<bool>,
) -> Result<Vec<PathBuf>, String> {
    if !matches!(project.kind, ProjectKind::Lake { .. }) {
        return Ok(Vec::new());
    }
    let mut spec = job_spec(
        "Lean axiom audit: module path (lake env)".into(),
        project,
        toolchain.lake.display().to_string(),
        vec!["env".into()],
        None,
    );
    spec.timeout_ms = Some(LAKE_ENV_TIMEOUT_MS);
    match run(exec, spec, cancel).await {
        Ran::Exited { id, .. } => {
            let out = exec.output_text(&id, OutputStream::Stdout).unwrap_or_default();
            parse_lake_env_lean_path(&out).ok_or_else(|| "lake env printed no LEAN_PATH".to_owned())
        }
        Ran::Failed(e) => Err(e),
    }
}

/// The last `LEAN_PATH=` line of `lake env` output, split into entries.
pub fn parse_lake_env_lean_path(stdout: &str) -> Option<Vec<PathBuf>> {
    let value = stdout.lines().rev().find_map(|l| l.trim_end_matches('\r').strip_prefix("LEAN_PATH="))?;
    Some(std::env::split_paths(value).filter(|p| !p.as_os_str().is_empty()).collect())
}

fn job_spec(
    label: String,
    project: &LeanProject,
    program: String,
    args: Vec<String>,
    lean_path: Option<&str>,
) -> JobSpec {
    let mut environment = EnvironmentSpec::default();
    if let Some(path) = lean_path {
        environment.set.insert("LEAN_PATH".into(), path.to_owned());
    }
    JobSpec {
        label,
        program,
        args,
        environment,
        working_directory: project.root.display().to_string(),
        policy: ExecutionPolicy::TrustedNative,
        timeout_ms: Some(AUDIT_TIMEOUT_MS),
        log_limit_bytes: Some(AUDIT_LOG_LIMIT),
        stdin: None,
    }
}

enum Ran {
    /// Exited with code 0.
    Exited {
        id: JobId,
    },
    Failed(String),
}

/// Run a job; cancel it if `cancel` turns true.
async fn run(exec: &ExecutionService, spec: JobSpec, cancel: &mut watch::Receiver<bool>) -> Ran {
    let id = exec.launch(spec);
    let result = tokio::select! {
        r = exec.wait(&id) => r,
        _ = cancel.wait_for(|c| *c) => {
            exec.cancel(&id);
            return Ran::Failed("canceled".into());
        }
    };
    match result.map(|r| r.outcome) {
        Some(JobOutcome::Exited { code: 0 }) => Ran::Exited { id },
        Some(JobOutcome::Exited { code }) => {
            let stdout = exec.output_text(&id, OutputStream::Stdout).unwrap_or_default();
            let stderr = exec.output_text(&id, OutputStream::Stderr).unwrap_or_default();
            let detail = stdout
                .lines()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                .filter(|m| m.get("severity").and_then(Value::as_str) == Some("error"))
                .filter_map(|m| m.get("data").and_then(Value::as_str).map(str::to_owned))
                .next()
                .or_else(|| stderr.lines().find(|l| !l.trim().is_empty()).map(str::to_owned))
                .map(|d| d.chars().take(300).collect::<String>())
                .unwrap_or_default();
            Ran::Failed(format!(
                "exited with code {code}{}",
                if detail.is_empty() { String::new() } else { format!(": {detail}") }
            ))
        }
        Some(JobOutcome::FailedToStart { reason }) => Ran::Failed(reason),
        Some(other) => Ran::Failed(format!("{other:?}")),
        None => Ran::Failed("the job disappeared".into()),
    }
}

type Range = ((u32, u32), (u32, u32));

/// What step 1 reported (only the line tagged with the run's secret counts).
#[derive(Debug, PartialEq, Eq)]
pub struct CompileResult {
    /// Lean elaborated the text to its end (no terminal command before it).
    pub complete: bool,
    /// Zero-based (line, UTF-16 column) of the command that ended elaboration.
    pub stop: Option<(u32, u32)>,
    /// Declaration name ranges by constant name (for locating declarations).
    pub ranges: HashMap<String, Range>,
}

/// Parse step 1's stdout. Exactly one line may carry the secret.
pub fn parse_compile_output(stdout: &str, secret: &str) -> Result<CompileResult, String> {
    let prefix = format!("{secret} ");
    let mut tagged = stdout.lines().filter_map(|l| l.strip_prefix(&prefix));
    let (Some(line), None) = (tagged.next(), tagged.next()) else {
        return Err("no single result from the compile step".into());
    };
    let v: Value = serde_json::from_str(line).map_err(|_| "malformed result from the compile step".to_owned())?;
    if let Some(error) = v.get("error").and_then(Value::as_str) {
        return Err(error.chars().take(300).collect());
    }
    let complete = v.get("complete").and_then(Value::as_bool).ok_or("the compile step reported no completion")?;
    let stop = v.get("stop").and_then(Value::as_array).and_then(|a| {
        let n: Vec<u32> = a.iter().filter_map(Value::as_u64).map(|x| x as u32).collect();
        (n.len() == 2).then(|| (n[0], n[1]))
    });
    let mut ranges = HashMap::new();
    for r in v.get("ranges").and_then(Value::as_array).into_iter().flatten() {
        let Some(r) = r.as_array() else { continue };
        let (Some(name), nums) = (r.first().and_then(Value::as_str), &r[1.min(r.len())..]) else { continue };
        let n: Vec<u32> = nums.iter().filter_map(Value::as_u64).map(|x| x as u32).collect();
        if n.len() == 4 {
            // Declaration ranges use one-based lines.
            ranges.insert(name.to_owned(), ((n[0].saturating_sub(1), n[1]), (n[2].saturating_sub(1), n[3])));
        }
    }
    Ok(CompileResult { complete, stop, ranges })
}

/// One declaration as step 2 reported it.
#[derive(Debug, PartialEq, Eq)]
pub struct CheckedDecl {
    pub name: String,
    pub internal: bool,
    /// Sent to the kernel (unsafe and partial declarations are not).
    pub replayed: bool,
    pub axioms: Vec<AxiomRef>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CheckResult {
    Decls(Vec<CheckedDecl>),
    KernelRejected(String),
}

/// Parse step 2's stdout: `<tag> <json>` lines, the first with the kernel
/// result, the last `{"end":true}`.
pub fn parse_check_output(stdout: &str, tag: &str) -> Result<CheckResult, String> {
    let prefix = format!("{tag} ");
    let lines: Vec<Value> = stdout
        .lines()
        .filter_map(|l| l.strip_prefix(&prefix))
        .map(|l| serde_json::from_str(l).map_err(|_| "malformed audit output".to_owned()))
        .collect::<Result<_, _>>()?;
    let (Some(first), Some(last)) = (lines.first(), lines.last()) else {
        return Err("the audit produced no result".into());
    };
    if let Some(error) = first.get("error").and_then(Value::as_str) {
        return Err(error.chars().take(300).collect());
    }
    if last.get("end").and_then(Value::as_bool) != Some(true) || lines.len() < 2 {
        return Err("the audit output was incomplete".into());
    }
    match first.get("kernel") {
        Some(Value::Null) => {}
        Some(Value::String(message)) => return Ok(CheckResult::KernelRejected(message.chars().take(2000).collect())),
        _ => return Err("the audit reported no kernel result".into()),
    }
    let mut decls = Vec::new();
    for v in &lines[1..lines.len() - 1] {
        let name = v.get("n").and_then(Value::as_str).ok_or("malformed audit output")?;
        let axioms = v
            .get("ax")
            .and_then(Value::as_array)
            .ok_or("malformed audit output")?
            .iter()
            .map(|a| {
                Some(AxiomRef {
                    name: a.get("n").and_then(Value::as_str)?.to_owned(),
                    toolchain: a.get("t").and_then(Value::as_bool)?,
                })
            })
            .collect::<Option<Vec<_>>>()
            .ok_or("malformed audit output")?;
        decls.push(CheckedDecl {
            name: name.to_owned(),
            internal: v.get("i").and_then(Value::as_bool).unwrap_or(false),
            replayed: v.get("k").and_then(Value::as_bool).unwrap_or(false),
            axioms,
        });
    }
    Ok(CheckResult::Decls(decls))
}

/// Join step 2's declarations with step 1's ranges; strip private prefixes.
fn combine(decls: Vec<CheckedDecl>, ranges: &HashMap<String, Range>, module: &str) -> Vec<DeclAxioms> {
    let private_prefix = format!("_private.{module}.");
    decls
        .into_iter()
        .map(|d| {
            let range = ranges.get(&d.name).copied();
            // `_private.<module>.<n>.X.y` → `X.y`
            let name = d
                .name
                .strip_prefix(&private_prefix)
                .and_then(|rest| rest.split_once('.').map(|(_, n)| n.to_owned()))
                .unwrap_or(d.name);
            DeclAxioms { name, internal: d.internal, axioms: d.axioms, range }
        })
        .collect()
}

/// The lines of `text` that decide what it imports (`prelude`, `module`,
/// `import …` with its modifiers). A speculative audit (started before Lean
/// finished a version) needs these unchanged since Lean last finished the
/// file: otherwise Lake may still be building the imports the audit reads.
pub fn imports_key(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|line| {
            let words: Vec<&str> = line.split_whitespace().take(4).collect();
            matches!(words.first(), Some(&"prelude") | Some(&"module"))
                || words
                    .iter()
                    .position(|w| *w == "import")
                    .is_some_and(|i| words[..i].iter().all(|w| matches!(*w, "public" | "meta" | "private")))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A fresh random hex token (run ids and secrets), unpredictable to the
/// audited code.
pub fn nonce() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ax(name: &str, toolchain: bool) -> AxiomRef {
        AxiomRef { name: name.into(), toolchain }
    }

    #[test]
    fn classifies_axioms_by_name_and_origin() {
        assert_eq!(ax("propext", true).kind(), AxiomKind::Standard);
        assert_eq!(ax("sorryAx", true).kind(), AxiomKind::Sorry);
        assert_eq!(ax("Lean.ofReduceBool", true).kind(), AxiomKind::CompiledCode);
        assert_eq!(ax("X.bad._native.native_decide.ax_1_1", false).kind(), AxiomKind::CompiledCode);
        assert_eq!(ax("X.cheat", false).kind(), AxiomKind::Other);
        // A project axiom that reuses a toolchain name is never standard.
        assert_eq!(ax("propext", false).kind(), AxiomKind::Other);
        assert_eq!(ax("sorryAx", false).kind(), AxiomKind::Other);
        assert_eq!(ax("Lean.ofReduceBool", false).kind(), AxiomKind::Other);
        assert_eq!(ax("propext", false).display(), "propext (declared outside the toolchain)");
        assert_eq!(ax("propext", true).display(), "propext");
        assert_eq!(ax("t._native.by_evaluation.ax_1_1", false).kind(), AxiomKind::CompiledCode);
        assert_eq!(ax("_native.native_decide.ax_2", false).kind(), AxiomKind::CompiledCode);
        assert_eq!(ax("X._native", false).kind(), AxiomKind::Other);
        assert!(is_compiled_code_axiom("Lean.trustCompiler"));
        assert!(is_compiled_code_axiom("X.bad._native.native_decide.ax_1_1"));
        assert!(!is_compiled_code_axiom("propext"));
    }

    #[test]
    fn compile_output_counts_only_the_secret_line() {
        let secret = "0123456789abcdef0123456789abcdef";
        let good = format!(
            "{{\"severity\":\"warning\",\"data\":\"x\"}}\nnoise\n\n{secret} {{\"complete\":false,\"stop\":[1,0],\"ranges\":[[\"X.a\",3,8,3,9],[\"bad\"]]}}\n"
        );
        let r = parse_compile_output(&good, secret).unwrap();
        assert!(!r.complete);
        assert_eq!(r.stop, Some((1, 0)));
        assert_eq!(r.ranges.get("X.a"), Some(&((2, 8), (2, 9))));
        // Printed by the file: another tag, a forged copy, or nothing.
        assert!(parse_compile_output(&good.replace(secret, "forged"), secret).is_err());
        let twice = format!("{good}{secret} {{\"complete\":true}}\n");
        assert!(parse_compile_output(&twice, secret).is_err());
        let prefixed = good.replace(&format!("\n\n{secret}"), &format!("\nx{secret}"));
        assert!(parse_compile_output(&prefixed, secret).is_err());
        let error = format!("{secret} {{\"error\":\"Lean reported errors\"}}\n");
        assert_eq!(parse_compile_output(&error, secret), Err("Lean reported errors".into()));
    }

    #[test]
    fn check_output_requires_kernel_result_and_end() {
        let tag = "t";
        let body = [
            r#"t {"kernel":null,"replayed":3}"#,
            r#"t {"n":"X.a","i":false,"k":true,"ax":[{"n":"propext","t":true},{"n":"cheat","t":false}]}"#,
            r#"t {"n":"_private.ExpositionAudit.Tq.0.X.h","i":true,"k":true,"ax":[]}"#,
            r#"t {"end":true}"#,
        ]
        .join("\n");
        let Ok(CheckResult::Decls(decls)) = parse_check_output(&body, tag) else { panic!() };
        assert_eq!(decls.len(), 2);
        assert_eq!(decls[0].axioms, vec![ax("propext", true), ax("cheat", false)]);
        let ranges = HashMap::from([("X.a".to_owned(), ((0, 1), (0, 2)))]);
        let combined = combine(decls, &ranges, "ExpositionAudit.Tq");
        assert_eq!(combined[0].range, Some(((0, 1), (0, 2))));
        assert_eq!(combined[1].name, "X.h");
        // Truncated output is a failure, not a result.
        assert!(parse_check_output(&body.replace(r#"t {"end":true}"#, ""), tag).is_err());
        // A kernel rejection is reported as such.
        let rejected = "t {\"kernel\":\"(kernel) declaration type mismatch\",\"replayed\":2}\nt {\"end\":true}";
        assert!(
            matches!(parse_check_output(rejected, tag), Ok(CheckResult::KernelRejected(m)) if m.contains("mismatch"))
        );
        let error = "t {\"error\":\"the checked module was not found\"}\nt {\"end\":true}";
        assert!(parse_check_output(error, tag).is_err());
        assert!(parse_check_output("", tag).is_err());
    }

    #[test]
    fn module_path_puts_the_toolchain_first() {
        let lake = if cfg!(windows) {
            "PATH=x\nLEAN_PATH=C:\\p\\.lake\\build\\lib\\lean;C:\\tc\\lib\\lean\nLEAN_SRC_PATH=y\n"
        } else {
            "PATH=x\nLEAN_PATH=/p/.lake/build/lib/lean:/tc/lib/lean\nLEAN_SRC_PATH=y\n"
        };
        let project = parse_lake_env_lean_path(lake).unwrap();
        assert_eq!(project.len(), 2);
        let lib = if cfg!(windows) { PathBuf::from("C:\\tc\\lib\\lean") } else { PathBuf::from("/tc/lib/lean") };
        let tools = PathBuf::from("tools");
        let joined = lean_path(&lib, &[&tools], &project).unwrap();
        let entries: Vec<PathBuf> = std::env::split_paths(&joined).collect();
        assert_eq!(entries, vec![lib.clone(), tools, project[0].clone()]);
        assert!(parse_lake_env_lean_path("PATH=x\n").is_none());
    }

    #[test]
    fn imports_key_tracks_only_the_header() {
        let a = "module\npublic import A.B\nimport all C\n\ntheorem t : True := trivial\n";
        assert_eq!(imports_key(a), "module\npublic import A.B\nimport all C");
        assert_eq!(imports_key(a), imports_key(&a.replace("trivial", "by trivial")));
        assert_ne!(imports_key(a), imports_key(&a.replace("A.B", "A.D")));
        assert_eq!(imports_key("public theorem x : True := trivial\n"), "");
    }

    #[test]
    fn nonces_are_distinct() {
        let a = nonce();
        let b = nonce();
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
    }
}
