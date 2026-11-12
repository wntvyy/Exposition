//! Axiom audit: what the declarations of a checked revision really rest on.
//!
//! Lean's diagnostics alone cannot establish a checked proof: a `sorry` can be
//! imported, its warning can be silenced (`#guard_msgs`, `warn.sorry false`),
//! a proof can use a user `axiom` or `native_decide` (trusting compiled code),
//! `#exit` can stop elaboration early, and `debug.skipKernelTC` disables the
//! kernel. After Lean reports a revision free of errors, the service audits
//! exactly that text:
//!
//! 1. **Compile** an app-owned copy of the text as a temporary module
//!    (`lake env lean -R <dir> -o <olean>`), with a nonce-named sentinel
//!    declaration appended at the end. A missing sentinel means elaboration
//!    stopped before the end of the file (`#exit`).
//! 2. **Audit** from a separate file that imports `Lean` and the module and,
//!    for every constant of the module, runs `Lean.collectAxioms` (which
//!    follows dependencies into imported modules) and looks up its declaration
//!    range. Output is tagged with the same per-run nonce, so text printed by
//!    the audited file cannot impersonate it.
//!
//! Both steps are supervised `TrustedNative` jobs in the project root (they run
//! project code, like the language server). Nothing is written to the project.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use exposition_core::job::{EnvironmentSpec, ExecutionPolicy, JobId, JobOutcome, JobSpec, OutputStream};
use exposition_execution::ExecutionService;
use serde_json::Value;
use tokio::sync::watch;

use crate::project::{LeanProject, ProjectKind};
use crate::toolchain::ResolvedToolchain;

/// The axioms of Lean's standard foundations. Anything else is reported.
pub const STANDARD_AXIOMS: &[&str] = &["propext", "Classical.choice", "Quot.sound"];
pub const SORRY_AXIOM: &str = "sorryAx";
const AUDIT_TIMEOUT_MS: u64 = 15 * 60 * 1000;
const AUDIT_LOG_LIMIT: u64 = 32 * 1024 * 1024;

/// True for axioms through which a proof trusts compiled code instead of the
/// kernel (`native_decide`, `Lean.ofReduceBool`, `Lean.trustCompiler`).
pub fn is_compiled_code_axiom(name: &str) -> bool {
    matches!(name, "Lean.ofReduceBool" | "Lean.ofReduceNat" | "Lean.trustCompiler")
        || name.contains("._native.native_decide.")
        || name.contains("native_decide.ax")
}

/// One constant of the audited module and the axioms it depends on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclAxioms {
    /// Fully qualified name (private names shown without their `_private` prefix).
    pub name: String,
    pub internal: bool,
    pub axioms: Vec<String>,
    /// Zero-based (line, UTF-16 column) of the declaration name's start and end.
    pub range: Option<((u32, u32), (u32, u32))>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuditOutcome {
    /// Every declaration of the file was audited.
    Complete(Vec<DeclAxioms>),
    /// Elaboration stopped before the end of the file (e.g. `#exit`); the
    /// declarations before that point were audited.
    Stopped(Vec<DeclAxioms>),
    /// The audit could not run or its output was not trustworthy.
    Failed(String),
}

pub struct AuditRequest<'a> {
    pub project: &'a LeanProject,
    pub toolchain: &'a ResolvedToolchain,
    pub text: Arc<str>,
    /// Application-owned scratch directory (created; removed afterwards).
    pub scratch: PathBuf,
    /// Random hex nonce (also names the module and the sentinel).
    pub nonce: String,
    /// Display name of the audited file (for job labels).
    pub file_name: String,
}

fn sentinel(nonce: &str) -> String {
    format!("exposition_audit_end_{nonce}")
}

/// The text compiled: the exact checked text plus the sentinel declaration.
pub fn audited_text(text: &str, nonce: &str) -> String {
    format!("{text}\n\ndef «{}» : Unit := ()\n", sentinel(nonce))
}

/// The audit program (a separate module importing `Lean` and the target).
pub fn audit_program(module: &str, nonce: &str) -> String {
    format!(
        r#"import Lean
import {module}
open Lean in
#eval show Lean.Elab.Command.CommandElabM Unit from do
  let env ← getEnv
  let some idx := env.getModuleIdx? `{module} | throwError "{nonce}-MISSING"
  let mut out := "{nonce}-BEGIN"
  for n in env.header.moduleData[idx.toNat]!.constNames do
    let axs ← Lean.collectAxioms n
    let r ← Lean.findDeclarationRanges? n
    let pos : Json := match r with
      | some r => toJson [r.selectionRange.pos.line, r.selectionRange.charUtf16, r.selectionRange.endPos.line, r.selectionRange.endCharUtf16]
      | none => Json.null
    let j := Json.mkObj [("n", toJson n.toString), ("i", toJson n.isInternal), ("ax", toJson (axs.toList.map (·.toString))), ("r", pos)]
    out := out ++ "\n{nonce} " ++ j.compress
  logInfo (out ++ "\n{nonce}-END")
"#
    )
}

/// Parse `lean --json` output of the audit program.
pub fn parse_audit_output(stdout: &str, nonce: &str, module: &str) -> AuditOutcome {
    let begin = format!("{nonce}-BEGIN");
    let end = format!("{nonce}-END");
    let prefix = format!("{nonce} ");
    for line in stdout.lines() {
        let Ok(message) = serde_json::from_str::<Value>(line.trim()) else { continue };
        let Some(data) = message.get("data").and_then(Value::as_str) else { continue };
        if !data.starts_with(&begin) {
            continue;
        }
        if !data.trim_end().ends_with(&end) {
            return AuditOutcome::Failed("the axiom audit output was incomplete".into());
        }
        let mut decls = Vec::new();
        let mut reached_end = false;
        let private_prefix = format!("_private.{module}.");
        for entry in data.lines().filter_map(|l| l.strip_prefix(&prefix)) {
            let Ok(v) = serde_json::from_str::<Value>(entry) else {
                return AuditOutcome::Failed("the axiom audit output was malformed".into());
            };
            let raw = v.get("n").and_then(Value::as_str).unwrap_or_default();
            if raw.rsplit('.').next().is_some_and(|last| last.trim_matches(['«', '»']) == sentinel(nonce)) {
                reached_end = true;
                continue;
            }
            // `_private.<module>.<n>.X.y` → `X.y`
            let name = raw
                .strip_prefix(&private_prefix)
                .and_then(|rest| rest.split_once('.').map(|(_, n)| n.to_owned()))
                .unwrap_or_else(|| raw.to_owned());
            let axioms = v
                .get("ax")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                .unwrap_or_default();
            let range = v.get("r").and_then(Value::as_array).and_then(|r| {
                let n: Vec<u32> = r.iter().filter_map(Value::as_u64).map(|x| x as u32).collect();
                (n.len() == 4).then(|| ((n[0].saturating_sub(1), n[1]), (n[2].saturating_sub(1), n[3])))
            });
            decls.push(DeclAxioms {
                name,
                internal: v.get("i").and_then(Value::as_bool).unwrap_or(false),
                axioms,
                range,
            });
        }
        return if reached_end { AuditOutcome::Complete(decls) } else { AuditOutcome::Stopped(decls) };
    }
    AuditOutcome::Failed("the axiom audit produced no result".into())
}

fn job_spec(
    label: String,
    project: &LeanProject,
    toolchain: &ResolvedToolchain,
    lean_args: Vec<String>,
    lean_path: Option<&Path>,
) -> JobSpec {
    let (program, args) = match project.kind {
        ProjectKind::Lake { .. } => {
            let mut args = vec!["env".to_owned(), "lean".to_owned()];
            args.extend(lean_args);
            (toolchain.lake.display().to_string(), args)
        }
        ProjectKind::Standalone => (toolchain.lean.display().to_string(), lean_args),
    };
    let mut environment = EnvironmentSpec::default();
    if let Some(path) = lean_path {
        // `lake env` keeps an existing LEAN_PATH after the package's own entries.
        environment.set.insert("LEAN_PATH".into(), path.display().to_string());
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

/// Run a job; cancel it if `cancel` turns true. `Err` describes a failure.
async fn run(exec: &ExecutionService, spec: JobSpec, cancel: &mut watch::Receiver<bool>) -> Result<JobId, String> {
    let id = exec.launch(spec);
    let result = tokio::select! {
        r = exec.wait(&id) => r,
        _ = cancel.wait_for(|c| *c) => {
            exec.cancel(&id);
            return Err("canceled".into());
        }
    };
    match result.map(|r| r.outcome) {
        Some(JobOutcome::Exited { code: 0 }) => Ok(id),
        Some(JobOutcome::Exited { code }) => {
            let detail = exec
                .output_text(&id, OutputStream::Stdout)
                .unwrap_or_default()
                .lines()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                .filter(|m| m.get("severity").and_then(Value::as_str) == Some("error"))
                .filter_map(|m| m.get("data").and_then(Value::as_str).map(|d| d.chars().take(300).collect::<String>()))
                .next()
                .unwrap_or_default();
            Err(format!(
                "exited with code {code}{}",
                if detail.is_empty() { String::new() } else { format!(": {detail}") }
            ))
        }
        Some(JobOutcome::FailedToStart { reason }) => Err(reason),
        Some(other) => Err(format!("{other:?}")),
        None => Err("the job disappeared".into()),
    }
}

/// Run the two-step audit. `cancel` aborts both jobs.
pub async fn run_audit(
    exec: &ExecutionService,
    request: AuditRequest<'_>,
    mut cancel: watch::Receiver<bool>,
) -> AuditOutcome {
    let module_leaf = format!("T{}", request.nonce);
    let module = format!("ExpositionAudit.{module_leaf}");
    let dir = request.scratch.join(&request.nonce);
    let package = dir.join("ExpositionAudit");
    let source = package.join(format!("{module_leaf}.lean"));
    let olean = package.join(format!("{module_leaf}.olean"));
    let main = dir.join("AuditMain.lean");
    let prepared = std::fs::create_dir_all(&package)
        .and_then(|_| std::fs::write(&source, audited_text(&request.text, &request.nonce)))
        .and_then(|_| std::fs::write(&main, audit_program(&module, &request.nonce)));
    if let Err(e) = prepared {
        return AuditOutcome::Failed(format!("could not prepare the axiom audit: {e}"));
    }
    let outcome = async {
        let compile = job_spec(
            format!("Lean axiom audit: compile {}", request.file_name),
            request.project,
            request.toolchain,
            vec![
                "-R".into(),
                dir.display().to_string(),
                "-o".into(),
                olean.display().to_string(),
                source.display().to_string(),
            ],
            None,
        );
        if let Err(e) = run(exec, compile, &mut cancel).await {
            return AuditOutcome::Failed(format!("compiling the checked text failed ({e})"));
        }
        let audit = job_spec(
            format!("Lean axiom audit: {}", request.file_name),
            request.project,
            request.toolchain,
            vec!["--json".into(), main.display().to_string()],
            Some(&dir),
        );
        match run(exec, audit, &mut cancel).await {
            Ok(id) => parse_audit_output(
                &exec.output_text(&id, OutputStream::Stdout).unwrap_or_default(),
                &request.nonce,
                &module,
            ),
            Err(e) => AuditOutcome::Failed(format!("the axiom audit failed ({e})")),
        }
    }
    .await;
    let _ = std::fs::remove_dir_all(&dir);
    outcome
}

/// A fresh random nonce (hex), unpredictable to the audited code.
pub fn nonce() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Trust-weakening options found in the text (checked before trusting a result).
pub fn trust_weakening(text: &str) -> Option<String> {
    text.contains("skipKernelTC").then(|| {
        "The file disables kernel type checking (debug.skipKernelTC), so its declarations were not verified by Lean's kernel.".to_owned()
    })
}

/// Zero-based line of the first `#exit` command, if any (for messages).
pub fn exit_line(text: &str) -> Option<u32> {
    text.lines().position(|l| l.trim_start().starts_with("#exit")).map(|l| l as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_compiled_code_axioms() {
        assert!(is_compiled_code_axiom("X.bad._native.native_decide.ax_1_1"));
        assert!(is_compiled_code_axiom("Lean.ofReduceBool"));
        assert!(is_compiled_code_axiom("Lean.trustCompiler"));
        assert!(!is_compiled_code_axiom("propext"));
        assert!(!is_compiled_code_axiom("X.cheat"));
    }

    #[test]
    fn parses_audit_output_and_detects_stopped_files() {
        let nonce = "abc123";
        let module = "ExpositionAudit.Tabc123";
        let body = [
            format!("{nonce}-BEGIN"),
            format!(r#"{nonce} {{"ax":["sorryAx"],"i":false,"n":"X.guarded","r":[6,8,6,15]}}"#),
            format!(r#"{nonce} {{"ax":[],"i":true,"n":"_private.{module}.0.X.hidden","r":[14,16,14,22]}}"#),
            format!(r#"{nonce} {{"ax":[],"i":false,"n":"X.{}","r":[16,4,16,30]}}"#, sentinel(nonce)),
            format!("{nonce}-END"),
        ]
        .join("\n");
        let message = serde_json::json!({ "severity": "information", "data": body }).to_string();
        let stdout = format!("noise\n{{\"severity\":\"warning\",\"data\":\"x\"}}\n{message}\n");
        match parse_audit_output(&stdout, nonce, module) {
            AuditOutcome::Complete(decls) => {
                assert_eq!(decls.len(), 2);
                assert_eq!(decls[0].name, "X.guarded");
                assert_eq!(decls[0].axioms, vec!["sorryAx"]);
                assert_eq!(decls[0].range, Some(((5, 8), (5, 15))));
                assert_eq!(decls[1].name, "X.hidden");
            }
            other => panic!("{other:?}"),
        }
        // No sentinel: elaboration stopped early.
        let stopped = body.replace(&sentinel(nonce), "other");
        let message = serde_json::json!({ "data": stopped }).to_string();
        assert!(matches!(parse_audit_output(&message, nonce, module), AuditOutcome::Stopped(_)));
        // Output without the nonce (e.g. printed by the audited file) is not trusted.
        let forged = serde_json::json!({ "data": body.replace(nonce, "forged") }).to_string();
        assert!(matches!(parse_audit_output(&forged, nonce, module), AuditOutcome::Failed(_)));
        // Truncated output is a failure, not a result.
        let truncated = serde_json::json!({ "data": body.replace(&format!("{nonce}-END"), "") }).to_string();
        assert!(matches!(parse_audit_output(&truncated, nonce, module), AuditOutcome::Failed(_)));
    }

    #[test]
    fn nonces_are_distinct_and_detect_trust_weakening() {
        let a = nonce();
        let b = nonce();
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
        assert!(trust_weakening("set_option debug.skipKernelTC true").is_some());
        assert!(trust_weakening("theorem t : True := trivial").is_none());
        assert_eq!(exit_line("theorem a : 1 = 1 := rfl\n#exit\n"), Some(1));
        assert!(audited_text("x", "n").ends_with("def «exposition_audit_end_n» : Unit := ()\n"));
    }
}
