//! Agent process and build tools on the application's services.
//!
//! - `build_document`: Typst (`.typ`) and Markdown (`.md`, `.markdown`, the
//!   declared print profile laid out by Typst) entries through the
//!   typesetting service (in-process compiler, the same builds the page view
//!   uses; a task scoped to part of the project reads only its scope), LaTeX
//!   (`.tex`, `.ltx`) entries through the TeX build service (which runs on the
//!   execution service and requires workspace trust; a refusal is reported,
//!   never bypassed).
//! - `run_python`: approved, trusted-native runs go to the research service in
//!   a session owned by the agent task (never the user's sessions), with run
//!   provenance recorded in `.exposition/runs/`; unattended runs use the
//!   restricted Docker backend of the execution service. Never downgraded.
//! - `run_command`: execution-service jobs (restricted Docker, or trusted
//!   native after approval) with a clean environment and bounded output.
//!
//! - `check_lean`: the Lean service (the project's toolchain through
//!   `lake serve`), natively and only in a trusted folder after approval.
//!   `checked` is true only when Lean finished exactly that text with no
//!   errors and no `sorry`; anything else is reported as not checked.
//!
//! Every service result is real; genuine failures (build errors, exceptions,
//! refusals) come back as error results with their details.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use exposition_core::artifact::{ArtifactRecord, RunStatus};
use exposition_core::build::{BuildResult, BuildStatus, BuildTarget, DocumentFormat, Engine};
use exposition_core::job::{EnvironmentSpec, ExecutionPolicy, JobOutcome, JobSpec, OutputStream};
use exposition_core::text::utf16_to_byte;
use exposition_execution::ExecutionService;
use exposition_lean::{LeanCheckRequest, LeanCheckTarget, LeanService, LeanStatus};
use exposition_research::types::{
    Availability, ExecuteRequest, OutputItem, ResearchEvent, RunSource, RunSummary, RuntimeLanguage, SessionOwner,
    SessionState, StreamName,
};
use exposition_research::{ResearchService, StartSessionRequest};
use exposition_tex::{TexBuildMode, TexEvent, TexService};
use exposition_typesetting::TypesettingService;
use exposition_workspace::WorkspaceService;
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;

use super::scope;
use super::services::{
    BuildRequest, CommandRequest, LatexEngine, LeanRequest, PythonRequest, ServiceError, ServiceKind, ToolServices,
};
use crate::model::TaskScope;

/// Default wall-clock limit for one agent-launched process.
const JOB_TIMEOUT_MS: u64 = 10 * 60 * 1000;
/// Retained output per stream.
const LOG_LIMIT_BYTES: u64 = 256 * 1024;
/// Output returned to the model per stream (the tail is kept).
const RESULT_TAIL_BYTES: usize = 16 * 1024;
const MAX_DIAGNOSTICS: usize = 40;

/// The application's services as seen by agent tools.
pub struct AppToolServices {
    execution: ExecutionService,
    workspace: Option<Arc<WorkspaceService>>,
    typesetting: Option<Arc<TypesettingService>>,
    tex: Option<TexService>,
    research: Option<Arc<ResearchService>>,
    /// Research ↔ project graph integration (retained artifacts, promotions).
    research_project: Option<Arc<exposition_research::ResearchProject>>,
    lean: Option<LeanService>,
}

/// Former name, kept for callers that only need command execution.
pub type ExecutionToolServices = AppToolServices;

impl AppToolServices {
    pub fn new(execution: ExecutionService) -> Self {
        Self {
            execution,
            workspace: None,
            typesetting: None,
            tex: None,
            research: None,
            research_project: None,
            lean: None,
        }
    }

    pub fn with_workspace(mut self, workspace: Arc<WorkspaceService>) -> Self {
        self.workspace = Some(workspace);
        self
    }

    pub fn with_typesetting(mut self, typesetting: Arc<TypesettingService>) -> Self {
        self.typesetting = Some(typesetting);
        self
    }

    pub fn with_tex(mut self, tex: TexService) -> Self {
        self.tex = Some(tex);
        self
    }

    pub fn with_research(mut self, research: Arc<ResearchService>) -> Self {
        self.research = Some(research);
        self
    }

    /// Attachments the agent inserts are recorded in the project graph
    /// through the research project, like reviewed promotions.
    pub fn with_research_project(mut self, project: Arc<exposition_research::ResearchProject>) -> Self {
        self.research_project = Some(project);
        self
    }

    pub fn with_lean(mut self, lean: LeanService) -> Self {
        self.lean = Some(lean);
        self
    }

    // ----- jobs ----------------------------------------------------------------

    async fn run_job(&self, spec: JobSpec, root: &Path, cancel: CancellationToken) -> Result<Value, ServiceError> {
        // The task's project folder must be a registered workspace root for jobs
        // to run there (registration does not grant trust).
        self.execution
            .register_workspace_root(root)
            .map_err(|e| ServiceError::Failed(format!("the project folder cannot host jobs: {e}")))?;
        let policy = spec.policy.clone();
        let id = self.execution.launch(spec);
        let result = tokio::select! {
            result = self.execution.wait(&id) => result,
            _ = cancel.cancelled() => {
                self.execution.cancel(&id);
                let _ = self.execution.wait(&id).await;
                return Err(ServiceError::Canceled);
            }
        };
        let result = result.ok_or_else(|| ServiceError::Failed("the job record was lost".into()))?;
        let stdout = self.execution.output_text(&id, OutputStream::Stdout).unwrap_or_default();
        let stderr = self.execution.output_text(&id, OutputStream::Stderr).unwrap_or_default();
        let (status, exit_code, reason) = match &result.outcome {
            JobOutcome::Exited { code } => ("exited", Some(*code), None),
            JobOutcome::Terminated { description } => ("terminated", None, Some(description.clone())),
            JobOutcome::Canceled => return Err(ServiceError::Canceled),
            JobOutcome::TimedOut => ("timedOut", None, Some(format!("killed after {JOB_TIMEOUT_MS} ms"))),
            JobOutcome::FailedToStart { reason } => {
                return Err(ServiceError::Failed(format!("the process was not started: {reason}")));
            }
        };
        let mut value = json!({
            "status": status,
            "exitCode": exit_code,
            "stdout": tail(&stdout),
            "stderr": tail(&stderr),
            "durationMs": result.duration_ms,
            "outputTruncated": result.output_truncated || stdout.len() > RESULT_TAIL_BYTES || stderr.len() > RESULT_TAIL_BYTES,
            "policy": policy_label(&policy),
            "job": id.0,
        });
        if let Some(reason) = reason {
            value["reason"] = json!(reason);
        }
        if matches!(policy, ExecutionPolicy::Restricted { .. }) {
            value["note"] = json!(
                "Ran in the restricted container: the project is mounted read-only at /workspace; files written to /out \
                 are kept in the job directory, not in the project."
            );
        }
        Ok(value)
    }

    fn spec(
        label: String,
        program: String,
        args: Vec<String>,
        cwd: PathBuf,
        policy: ExecutionPolicy,
        stdin: Option<String>,
    ) -> JobSpec {
        JobSpec {
            label,
            program,
            args,
            // Built from empty: nothing is inherited, so no credential can leak.
            environment: EnvironmentSpec::default(),
            working_directory: cwd.display().to_string(),
            policy,
            timeout_ms: Some(JOB_TIMEOUT_MS),
            log_limit_bytes: Some(LOG_LIMIT_BYTES),
            stdin,
        }
    }

    // ----- builds --------------------------------------------------------------

    /// Line/column (1-based) of a UTF-16 offset in the current text of `path`.
    fn line_column(&self, path: &exposition_core::path::WorkspacePath, offset: u32) -> Option<(usize, usize)> {
        let snapshot = self.workspace.as_ref()?.snapshot().ok()?;
        let text = snapshot.read(path).ok()?.text?;
        let byte = utf16_to_byte(&text, offset as usize).ok()?;
        let before = &text[..byte];
        let line = before.matches('\n').count() + 1;
        let column = before.rsplit('\n').next().map(|l| l.chars().count()).unwrap_or(0) + 1;
        Some((line, column))
    }

    fn summarize(&self, result: &BuildResult) -> (bool, Value) {
        let diagnostics: Vec<Value> = result
            .diagnostics
            .iter()
            .take(MAX_DIAGNOSTICS)
            .map(|d| {
                let mut v = json!({"severity": d.severity, "message": d.message});
                if let Some(location) = &d.location {
                    v["path"] = json!(location.path.as_str());
                    v["range"] = json!({"start": location.range.start, "end": location.range.end});
                    if let Some((line, column)) = self.line_column(&location.path, location.range.start) {
                        v["line"] = json!(line);
                        v["column"] = json!(column);
                    }
                }
                if !d.hints.is_empty() {
                    v["hints"] = json!(d.hints);
                }
                v
            })
            .collect();
        let (ok, status) = match &result.status {
            BuildStatus::Succeeded => (true, json!("succeeded")),
            BuildStatus::Failed => (false, json!("failed")),
            BuildStatus::Canceled => (false, json!("canceled")),
            BuildStatus::ToolUnavailable { tool, guidance } => {
                (false, json!({"toolUnavailable": tool, "guidance": guidance}))
            }
            BuildStatus::Error { message } => (false, json!({"error": message})),
        };
        let value = json!({
            "status": status,
            "build": result.id.0,
            "entry": result.target.entry.as_str(),
            "engine": result.target.engine,
            "pages": result.pages.len(),
            "durationMs": result.duration_ms,
            "diagnostics": diagnostics,
            "diagnosticsOmitted": result.diagnostics.len().saturating_sub(MAX_DIAGNOSTICS),
        });
        (ok, value)
    }

    fn report(&self, result: &BuildResult) -> Result<Value, ServiceError> {
        let (ok, value) = self.summarize(result);
        if ok { Ok(value) } else { Err(ServiceError::Outcome(value)) }
    }

    /// In-process builds through the typesetting service: Typst, and Markdown
    /// through its declared print profile.
    async fn build_typesetting(
        &self,
        request: BuildRequest,
        format: DocumentFormat,
        engine: Engine,
    ) -> Result<Value, ServiceError> {
        let typesetting = self.typesetting.clone().ok_or(ServiceError::NotAvailable(ServiceKind::Typesetting))?;
        let target = BuildTarget { format, engine, entry: request.entry };
        // A task scoped to part of the project builds with reads confined to
        // its scope; other files read as "access denied".
        let filter = (request.scope != TaskScope::Workspace).then(|| scope::read_filter(request.root, request.scope));
        let record = tokio::task::spawn_blocking(move || match filter {
            Some(filter) => typesetting.build_scoped(&target, filter),
            None => typesetting.build(&target),
        })
        .await
        .map_err(|e| ServiceError::Failed(format!("the build stopped unexpectedly: {e}")))?
        .map_err(|e| ServiceError::Failed(e.to_string()))?;
        self.report(&record.result)
    }

    async fn build_latex(&self, request: BuildRequest, cancel: CancellationToken) -> Result<Value, ServiceError> {
        let tex = self.tex.clone().ok_or_else(|| ServiceError::Unsupported("LaTeX builds".into()))?;
        // TeX runs natively and can read any file the user can: only a launch
        // the user approved (trusted native) may run.
        if request.policy != Some(ExecutionPolicy::TrustedNative) {
            return Err(ServiceError::Failed(
                "LaTeX builds run TeX natively with your account's privileges and need the user's approval; nothing \
                 was run."
                    .into(),
            ));
        }
        let engine = match request.engine {
            LatexEngine::PdfLatex => Engine::PdfLatex,
            LatexEngine::LuaLatex => Engine::LuaLatex,
            LatexEngine::Auto => tex
                .default_target()
                .ok()
                .filter(|t| t.entry == request.entry)
                .map(|t| t.engine)
                .unwrap_or(Engine::PdfLatex),
        };
        let target = BuildTarget { format: DocumentFormat::Latex, engine, entry: request.entry };
        let mut events = tex.subscribe();
        let reference = tex
            .request_build(Some(target.clone()), TexBuildMode::Full)
            .map_err(|e| ServiceError::Failed(e.to_string()))?;
        let summary = loop {
            let event = tokio::select! {
                event = events.recv() => event,
                _ = cancel.cancelled() => {
                    tex.cancel(Some(&target));
                    return Err(ServiceError::Canceled);
                }
            };
            match event {
                Ok(TexEvent::Finished { target: finished, summary })
                    if finished == reference && !summary.superseded =>
                {
                    break *summary;
                }
                Ok(_) => {}
                Err(RecvError::Lagged(_)) => {
                    if let Some(status) = tex.status(Some(&target))
                        && status.running.is_none()
                        && status.pending.is_none()
                        && let Some(last) = status.last
                    {
                        break last;
                    }
                }
                Err(RecvError::Closed) => return Err(ServiceError::Failed("the TeX build service stopped".into())),
            }
        };
        if let Some(refusal) = &summary.refusal {
            return Err(ServiceError::Outcome(json!({
                "status": "refused",
                "reason": refusal.message,
                "note": "The LaTeX build was refused by the execution policy (for example, the project folder is not \
                         trusted). Nothing was run; ask the user to trust the folder.",
            })));
        }
        let mut value = self.summarize(&summary.result).1;
        value["passes"] =
            json!(summary.passes.iter().map(|p| json!({"tool": p.tool, "exitCode": p.exit_code})).collect::<Vec<_>>());
        if matches!(summary.result.status, BuildStatus::Succeeded) {
            Ok(value)
        } else {
            Err(ServiceError::Outcome(value))
        }
    }

    // ----- research --------------------------------------------------------------

    /// Point the research service at the task's folder (the open workspace).
    fn sync_research_workspace(&self, research: &ResearchService, root: &Path) -> Result<(), ServiceError> {
        let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        if research.workspace_root().as_deref() != Some(canonical.as_path()) {
            research.set_workspace(root, false).map_err(|e| ServiceError::Failed(e.to_string()))?;
        }
        Ok(())
    }

    async fn agent_session(&self, research: &ResearchService, owner: &SessionOwner) -> Result<String, ServiceError> {
        let live = research.sessions().into_iter().find(|s| {
            &s.owner == owner
                && !matches!(
                    s.state,
                    SessionState::Stopped | SessionState::Crashed { .. } | SessionState::Detached { .. }
                )
        });
        if let Some(session) = live {
            return Ok(session.id);
        }
        let report = research.environments(false).await.map_err(|e| ServiceError::Failed(e.to_string()))?;
        let python = report.runtimes.iter().filter(|r| r.language == RuntimeLanguage::Python).collect::<Vec<_>>();
        let usable = python.iter().find(|r| {
            matches!(r.availability, Availability::Available) && r.kernel.is_some() && r.host_interpreter.is_some()
        });
        let Some(environment) = usable else {
            let guidance = python
                .iter()
                .find_map(|r| match &r.availability {
                    Availability::Unavailable { reason, guidance } => Some(format!("{reason} {guidance}")),
                    Availability::Available => None,
                })
                .unwrap_or_else(|| "No Python environment was found.".into());
            return Err(ServiceError::Failed(format!("No usable Python research environment: {guidance}")));
        };
        let session = research
            .start_session(StartSessionRequest { environment_id: environment.id.clone(), owner: owner.clone() })
            .await
            .map_err(|e| ServiceError::Failed(e.to_string()))?;
        Ok(session.id)
    }

    async fn research_python(
        &self,
        research: Arc<ResearchService>,
        request: PythonRequest,
        cancel: CancellationToken,
    ) -> Result<Value, ServiceError> {
        self.sync_research_workspace(&research, &request.root)?;
        let owner = SessionOwner::Agent { task_id: request.task_id.clone() };
        let session_id = self.agent_session(&research, &owner).await?;
        let mut events = research.subscribe();
        let queued = research
            .execute(ExecuteRequest {
                session_id: session_id.clone(),
                source: RunSource::Input { code: request.code },
                requester: owner,
            })
            .map_err(|e| ServiceError::Failed(e.to_string()))?;
        let run_id = queued.id.0.clone();
        let finished: RunSummary = loop {
            let event = tokio::select! {
                event = events.recv() => event,
                _ = cancel.cancelled() => {
                    let _ = research.cancel_run(&run_id).await;
                    return Err(ServiceError::Canceled);
                }
            };
            match event {
                Ok(ResearchEvent::RunFinished { run }) if run.id.0 == run_id => break run,
                Ok(_) => {}
                Err(RecvError::Lagged(_)) => {
                    if let Some(run) = research.runs().ok().and_then(|runs| runs.into_iter().find(|r| r.id.0 == run_id))
                        && !matches!(run.status, RunStatus::Running)
                    {
                        break run;
                    }
                }
                Err(RecvError::Closed) => return Err(ServiceError::Failed("the research service stopped".into())),
            }
        };
        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut results = Vec::new();
        let mut error = Value::Null;
        if let Ok(page) = research.run_output(&run_id, None, 500) {
            for entry in page.entries {
                match entry.item {
                    OutputItem::Stream { name: StreamName::Stdout, text } => stdout.push_str(&text),
                    OutputItem::Stream { name: StreamName::Stderr, text } => stderr.push_str(&text),
                    OutputItem::Display { text, artifacts, .. } => results.push(json!({
                        "text": text.map(|t| tail(&t)),
                        "artifacts": artifacts.iter().map(|a| a.0.clone()).collect::<Vec<_>>(),
                    })),
                    OutputItem::Error { ename, evalue, traceback } => {
                        let traceback: Vec<String> =
                            traceback.iter().rev().take(12).rev().map(|l| strip_ansi(l)).collect();
                        error = json!({"ename": ename, "evalue": evalue, "traceback": traceback});
                    }
                    OutputItem::Notice { message } => stderr.push_str(&format!("[notice] {message}\n")),
                    OutputItem::ClearOutput { .. } => {}
                }
            }
        }
        let artifacts: Vec<Value> = finished
            .outputs
            .iter()
            .map(|a| json!({"id": a.id.0, "kind": a.kind, "path": a.path.as_ref().map(|p| p.as_str()), "mediaType": a.media_type}))
            .collect();
        let status = match &finished.status {
            RunStatus::Succeeded => "succeeded",
            RunStatus::Failed { .. } => "failed",
            RunStatus::Canceled => "canceled",
            RunStatus::Interrupted => "interrupted",
            RunStatus::Running => "running",
        };
        let value = json!({
            "status": status,
            "run": run_id,
            "session": session_id,
            "provenance": format!(".exposition/runs/{run_id}.json"),
            "stdout": tail(&stdout),
            "stderr": tail(&stderr),
            "results": results,
            "error": error,
            "artifacts": artifacts,
            "note": "Ran in this task's own research session. Artifacts can be inserted into a document with insert_artifact.",
        });
        if matches!(finished.status, RunStatus::Succeeded) { Ok(value) } else { Err(ServiceError::Outcome(value)) }
    }
}

fn policy_label(policy: &ExecutionPolicy) -> String {
    match policy {
        ExecutionPolicy::TrustedNative => "trustedNative".to_owned(),
        ExecutionPolicy::Restricted { backend } => format!("restricted:{backend}"),
    }
}

fn tail(text: &str) -> String {
    if text.len() <= RESULT_TAIL_BYTES {
        return text.to_owned();
    }
    let mut start = text.len() - RESULT_TAIL_BYTES;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("… ({start} earlier bytes omitted)\n{}", &text[start..])
}

/// Kernel tracebacks carry ANSI color codes.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[async_trait]
impl ToolServices for AppToolServices {
    fn available(&self, kind: ServiceKind) -> bool {
        match kind {
            ServiceKind::Execution | ServiceKind::Research => true,
            ServiceKind::Typesetting => self.typesetting.is_some() || self.tex.is_some(),
            ServiceKind::Lean => self.lean.is_some(),
        }
    }

    fn restricted_backend(&self) -> Option<String> {
        // The service still refuses at launch if Docker or the image is missing.
        self.execution.settings().docker.image.as_ref().map(|_| exposition_execution::docker::BACKEND.to_owned())
    }

    fn native_refusal(&self, root: &Path) -> Option<String> {
        match self.execution.trust_status(&root.display().to_string()) {
            Ok(status) if status.trusted => None,
            Ok(_) => Some(
                "This project folder is not trusted for native execution, so code cannot run with your account's \
                 privileges. Trust the folder in Exposition first, or run the task with the restricted sandbox."
                    .to_owned(),
            ),
            Err(e) => Some(format!("Native execution is not possible here: {e}")),
        }
    }

    async fn build_document(&self, request: BuildRequest, cancel: CancellationToken) -> Result<Value, ServiceError> {
        match request.entry.extension().map(str::to_ascii_lowercase).as_deref() {
            Some("typ") => self.build_typesetting(request, DocumentFormat::Typst, Engine::Typst).await,
            Some("md" | "markdown") => {
                self.build_typesetting(request, DocumentFormat::Markdown, Engine::MarkdownPrint).await
            }
            Some("tex" | "ltx") => self.build_latex(request, cancel).await,
            _ => Err(ServiceError::Failed(format!("{} is not a buildable entry (.typ, .md, or .tex)", request.entry))),
        }
    }

    async fn run_python(&self, request: PythonRequest, cancel: CancellationToken) -> Result<Value, ServiceError> {
        match (&request.policy, &self.research) {
            // Approved trusted-native runs use an agent-owned research session.
            (ExecutionPolicy::TrustedNative, Some(research)) => {
                let research = Arc::clone(research);
                self.research_python(research, request, cancel).await
            }
            // Unattended runs stay inside the restricted container.
            _ => {
                let spec = Self::spec(
                    format!("Agent Python ({})", request.session),
                    "python".into(),
                    vec!["-".into()],
                    request.working_directory.clone(),
                    request.policy.clone(),
                    Some(request.code),
                );
                self.run_job(spec, &request.root, cancel).await
            }
        }
    }

    async fn check_lean(&self, request: LeanRequest, cancel: CancellationToken) -> Result<Value, ServiceError> {
        let lean = self.lean.clone().ok_or(ServiceError::NotAvailable(ServiceKind::Lean))?;
        // Lean runs project code (lakefile, macros, #eval); it only runs natively.
        if !matches!(request.policy, ExecutionPolicy::TrustedNative) {
            return Err(ServiceError::Failed(
                "Lean checks run the project's own toolchain natively and cannot run in the restricted sandbox. \
                 Use per-command approval in a trusted folder to check Lean files."
                    .into(),
            ));
        }
        let check = lean.check(LeanCheckRequest {
            target: LeanCheckTarget::Path { path: request.path },
            goals_at: Vec::new(),
            timeout_ms: Some(300_000),
        });
        let report = tokio::select! {
            report = check => report.map_err(|e| ServiceError::Failed(e.to_string()))?,
            _ = cancel.cancelled() => return Err(ServiceError::Canceled),
        };
        let mut value = serde_json::to_value(&report).map_err(|e| ServiceError::Failed(e.to_string()))?;
        let note = match &report.status {
            _ if report.checked => "Lean finished exactly this text with no errors, no sorry (also none through \
                 imports or suppressed warnings), kernel checking on, and only the standard axioms: the file is checked."
                .to_owned(),
            LeanStatus::CheckedWithAxioms { axioms, .. } => format!(
                "Not checked: no errors, but declarations depend on axioms beyond Lean's standard ones ({}).",
                axioms.join(", ")
            ),
            LeanStatus::ReliesOnCompiledCode { .. } => {
                "Not checked: no errors, but declarations trust compiled code (native_decide) instead of the kernel."
                    .to_owned()
            }
            LeanStatus::PartiallyChecked { reason, .. }
            | LeanStatus::Unverified { reason, .. }
            | LeanStatus::ServerUnavailable { reason } => format!("Not checked: {reason}"),
            LeanStatus::Untrusted { workspace_root } => format!(
                "Not checked: checking runs project code and {workspace_root} is not trusted. Ask the user to trust \
                 the folder."
            ),
            LeanStatus::ToolchainMissing { toolchain, guidance, .. } => {
                format!("Not checked: the Lean toolchain {toolchain} is not installed. {guidance}")
            }
            LeanStatus::CheckedWithSorry { count, .. } => {
                format!("Not checked: {count} declaration(s) depend on sorry (directly, through an import, or with the warning suppressed), so the material is incomplete.")
            }
            LeanStatus::Errors { count, .. } => format!("Not checked: Lean reported {count} error(s)."),
            _ => "Not checked: Lean did not finish checking this exact text.".to_owned(),
        };
        value["note"] = json!(note);
        if report.checked { Ok(value) } else { Err(ServiceError::Outcome(value)) }
    }

    async fn run_command(&self, request: CommandRequest, cancel: CancellationToken) -> Result<Value, ServiceError> {
        let label = format!("Agent: {} {}", request.program, request.args.join(" "));
        let label: String = label.chars().take(120).collect();
        let spec = Self::spec(label, request.program, request.args, request.working_directory, request.policy, None);
        self.run_job(spec, &request.root, cancel).await
    }

    fn research_artifact(&self, run_id: &str, artifact_id: &str) -> Result<ArtifactRecord, ServiceError> {
        let research = self.research.as_ref().ok_or(ServiceError::NotAvailable(ServiceKind::Research))?;
        research
            .artifact(run_id, artifact_id)
            .map(|(record, _)| record)
            .map_err(|e| ServiceError::Failed(e.to_string()))
    }

    fn record_attachment(
        &self,
        notice: &exposition_research::project::types::AttachmentNotice,
    ) -> Result<exposition_research::project::types::AttachmentRecorded, ServiceError> {
        let project = self.research_project.as_ref().ok_or(ServiceError::NotAvailable(ServiceKind::Research))?;
        project
            .record_external_attachment(notice)
            .map_err(|e| super::services::research_outcome(&e.code(), e.to_string()))
    }

    async fn task_finished(&self, task_id: &str) {
        let Some(research) = &self.research else { return };
        let owner = SessionOwner::Agent { task_id: task_id.to_owned() };
        for session in research.sessions().into_iter().filter(|s| s.owner == owner) {
            let _ = research.shutdown(&session.id).await;
        }
    }
}
