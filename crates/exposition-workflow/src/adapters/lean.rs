//! `leanAudit` (lean.audit): the existing complete audit of one Lean file
//! through the Lean service (trusted native toolchain; never in a restricted
//! context). The only producer of `FormalEvidence`: exactly the per-declaration
//! verdicts, statements, and axioms the audit reported, with the digest of the
//! text audited. A file-level Checked badge never stands in for declaration
//! evidence: a truncated audit says so in its verdict, and only listed
//! declarations can later be associated.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use exposition_core::path::WorkspacePath;
use exposition_graph::workflow::registry::PortType;
use exposition_graph::{EvidenceId, GraphService, OperationRef};
use exposition_lean::types::{DeclarationVerdict, LeanAuditRecord, LeanCheckRequest, LeanCheckTarget, LeanStatus};
use exposition_lean::{LeanService, evidence};
use exposition_workspace::{DocumentService, WorkspaceService};

use super::{StepAdapter, StepControl, StepFailure, StepRequest, StepResult, bare_hash};

/// The Lean service bounds one check at this many milliseconds.
const MAX_AUDIT_MS: u64 = 600_000;
use crate::model::{WorkflowAuditedDeclaration, WorkflowOutputValue, WorkflowRunError};

/// Lean audits and evidence recording (a seam for tests).
pub trait LeanAuditor: Send + Sync {
    fn availability(&self, root: &Path) -> Result<(), String>;
    /// Run the complete audit of `path` and return the service's own record
    /// of exactly the text audited.
    fn audit(
        &self,
        path: &WorkspacePath,
        timeout: Duration,
        canceled: &(dyn Fn() -> bool + Sync),
    ) -> Result<LeanAuditRecord, StepFailure>;
    /// Record service-owned evidence for `names` from the Lean service's own
    /// audit record of `path`, which must still describe `content_hash`.
    fn record_evidence(
        &self,
        path: &WorkspacePath,
        content_hash: &str,
        names: &[String],
    ) -> Result<Vec<EvidenceId>, WorkflowRunError>;
}

pub struct LeanAuditAdapter {
    pub auditor: Option<Arc<dyn LeanAuditor>>,
}

const NO_LEAN: &str = "The Lean verification service is not available.";

pub fn status_label(status: &LeanStatus) -> String {
    match status {
        LeanStatus::Checked { .. } => "Checked".into(),
        LeanStatus::CheckedWithAxioms { .. } => "Checked with nonstandard axioms".into(),
        LeanStatus::CheckedWithSorry { count, .. } => format!("Incomplete ({count} sorry)"),
        LeanStatus::ReliesOnCompiledCode { .. } => "Relies on compiled code".into(),
        LeanStatus::PartiallyChecked { reason, .. } => format!("Partially checked: {reason}"),
        LeanStatus::Unverified { reason, .. } => format!("Unverified: {reason}"),
        LeanStatus::Errors { count, .. } => format!("Errors ({count})"),
        LeanStatus::NotStarted => "Not checked".into(),
        LeanStatus::Unavailable { reason } => format!("Unavailable: {reason}"),
        LeanStatus::ToolchainMissing { toolchain, .. } => format!("Toolchain {toolchain} is not installed"),
        LeanStatus::Untrusted { .. } => "The folder is not trusted".into(),
        LeanStatus::Checking { .. } => "Still checking".into(),
        LeanStatus::ServerUnavailable { reason } => format!("Lean server unavailable: {reason}"),
        LeanStatus::ServerCrashed { message, .. } => format!("Lean server crashed: {message}"),
    }
}

pub fn verdict_label(verdict: DeclarationVerdict) -> &'static str {
    match verdict {
        DeclarationVerdict::Checked => "Checked",
        DeclarationVerdict::CheckedWithAxioms => "Checked with nonstandard axioms",
        DeclarationVerdict::Incomplete => "Incomplete",
        DeclarationVerdict::ReliesOnCompiledCode => "Relies on compiled code",
    }
}

/// The `FormalEvidence` value of an audit record (exact, never embellished).
pub fn evidence_value(record: &LeanAuditRecord) -> WorkflowOutputValue {
    let mut verdict = status_label(&record.status);
    if record.truncated {
        verdict.push_str(&format!(
            " — audit list truncated: {} of {} declarations listed; unlisted declarations have no evidence of their own",
            record.declarations.len(),
            record.declaration_count
        ));
    }
    WorkflowOutputValue::FormalEvidence {
        file: record.path.clone(),
        file_sha256: bare_hash(&record.content_hash),
        verdict,
        declarations: record
            .declarations
            .iter()
            .map(|d| WorkflowAuditedDeclaration {
                name: d.name.clone(),
                verdict: verdict_label(d.verdict).to_owned(),
                statement: (!d.statement.is_empty()).then(|| d.statement.clone()),
                axioms: d.axioms.clone(),
            })
            .collect(),
        evidence: Vec::new(),
    }
}

pub fn audit_operation(record: &LeanAuditRecord) -> OperationRef {
    OperationRef { kind: "leanAudit".into(), id: format!("{}@{}", record.path, bare_hash(&record.content_hash)) }
}

impl StepAdapter for LeanAuditAdapter {
    fn availability(&self, root: &Path) -> Result<(), String> {
        self.auditor.as_ref().ok_or_else(|| NO_LEAN.to_owned())?.availability(root)
    }

    fn run(&self, request: &StepRequest<'_>, control: &StepControl<'_>) -> Result<StepResult, StepFailure> {
        let auditor = self.auditor.as_ref().ok_or_else(|| StepFailure::new("workflow.runnerUnavailable", NO_LEAN))?;
        let sources: Vec<_> = request.inputs_on("source").collect();
        let [source] = sources.as_slice() else {
            return Err(StepFailure::new("workflow.invalidInput", "the step needs exactly one Lean file"));
        };
        let WorkflowOutputValue::File { path, sha256, .. } = &source.value else {
            return Err(StepFailure::new("workflow.invalidInput", "the Lean input is not a file"));
        };
        // The audit of exactly the approved text (persisted before Lean runs).
        let operation = OperationRef { kind: "leanAudit".into(), id: format!("{path}@{}", bare_hash(sha256)) };
        (control.on_dispatched)(&operation);
        let record = auditor
            .audit(path, request.time_limit, control.canceled)
            .map_err(|f| StepFailure { operation: Some(operation.clone()), ..f })?;
        if bare_hash(&record.content_hash) != bare_hash(sha256) {
            return Err(StepFailure {
                operation: Some(operation),
                ..StepFailure::new(
                    "workflow.inputChanged",
                    format!(
                        "The text Lean audited (sha256 {}) is not the file the plan approved (sha256 {}); an unsaved or changed buffer is not audited on the plan's behalf.",
                        short(&record.content_hash),
                        short(sha256)
                    ),
                )
            });
        }
        if record.declarations.is_empty() {
            return Err(StepFailure {
                operation: Some(operation),
                ..StepFailure::new(
                    "lean.noDeclarations",
                    format!("The audit of {path} ({}) reported no declarations.", status_label(&record.status)),
                )
            });
        }
        let value = evidence_value(&record);
        Ok(StepResult {
            operation: Some(operation.clone()),
            outputs: vec![request.output("evidence", PortType::FormalEvidence, value, &operation)],
            log: Some(format!(
                "{}: {} of {} declarations audited with {}",
                record.path,
                record.declarations.len(),
                record.declaration_count,
                record.toolchain
            )),
            ..Default::default()
        })
    }
}

fn short(hash: &str) -> String {
    bare_hash(hash).chars().take(12).collect()
}

/// The Lean service of the application.
pub struct LeanServiceAuditor {
    pub lean: LeanService,
    pub workspace: Arc<WorkspaceService>,
    pub documents: Arc<DocumentService>,
    pub graph: Arc<GraphService>,
    pub runtime: tokio::runtime::Handle,
}

impl LeanServiceAuditor {
    fn document(&self, path: &WorkspacePath) -> Result<exposition_core::document::DocumentId, StepFailure> {
        let opened = self
            .workspace
            .open_document(path)
            .map_err(|e| StepFailure::new(e.code, format!("{path} could not be opened: {}", e.message)))?;
        Ok(opened.state.info.id)
    }
}

impl LeanAuditor for LeanServiceAuditor {
    fn availability(&self, _root: &Path) -> Result<(), String> {
        // The toolchain is resolved per file when the audit runs; a missing
        // toolchain fails the step with the Lean service's own status.
        Ok(())
    }

    fn audit(
        &self,
        path: &WorkspacePath,
        timeout: Duration,
        canceled: &(dyn Fn() -> bool + Sync),
    ) -> Result<LeanAuditRecord, StepFailure> {
        let document = self.document(path)?;
        let timeout_ms = (timeout.as_millis().min(MAX_AUDIT_MS as u128)) as u64;
        let check = self.lean.check(LeanCheckRequest {
            target: LeanCheckTarget::Document { document: document.clone() },
            goals_at: Vec::new(),
            timeout_ms: Some(timeout_ms.max(1000)),
        });
        let report = self.runtime.block_on(async {
            tokio::pin!(check);
            loop {
                tokio::select! {
                    report = &mut check => break Some(report),
                    _ = tokio::time::sleep(Duration::from_millis(200)) => {
                        if canceled() {
                            break None;
                        }
                    }
                }
            }
        });
        let report = match report {
            None => return Err(StepFailure::canceled()),
            Some(Err(e)) => return Err(StepFailure::new(&e.code, e.message)),
            Some(Ok(report)) => report,
        };
        let record = self
            .runtime
            .block_on(self.lean.audit_record(document))
            .map_err(|e| StepFailure::new(&e.code, e.message))?;
        match record {
            Some(record)
                if Some(record.revision) == report.revision
                    && bare_hash(&record.content_hash) == bare_hash(&report.content_hash) =>
            {
                Ok(record)
            }
            _ => Err(StepFailure::new(
                "lean.noAudit",
                format!(
                    "Lean finished {path} without a complete declaration audit ({}); no formal evidence is produced.",
                    status_label(&report.status)
                ),
            )),
        }
    }

    fn record_evidence(
        &self,
        path: &WorkspacePath,
        content_hash: &str,
        names: &[String],
    ) -> Result<Vec<EvidenceId>, WorkflowRunError> {
        let document = self.document(path).map_err(|f| WorkflowRunError::new(&f.code, f.message))?;
        let record = self
            .runtime
            .block_on(self.lean.audit_record(document.clone()))
            .map_err(|e| WorkflowRunError::new(&e.code, e.message))?
            .ok_or_else(|| {
                WorkflowRunError::new(
                    "lean.noAudit",
                    "The Lean service no longer holds this file's audit; run the workflow again.",
                )
            })?;
        if bare_hash(&record.content_hash) != bare_hash(content_hash) {
            return Err(WorkflowRunError::new(
                "workflow.inputChanged",
                format!("{path} changed since the audit; run the workflow again to audit the current text."),
            ));
        }
        let saved =
            self.documents.info(&document).ok().is_some_and(|info| info.saved_revision == Some(record.revision));
        let now = crate::now_ms();
        let report = evidence::record_evidence(&self.graph, &record, names, saved, now)
            .map_err(|e| WorkflowRunError::new(&e.code, e.message))?;
        if let Some(refused) = report.refused.first() {
            return Err(WorkflowRunError::new(
                "workflow.declarationNotAudited",
                format!("“{}”: {}", refused.declaration, refused.reason),
            ));
        }
        Ok(report.recorded.into_iter().map(|r| r.evidence).collect())
    }
}

#[cfg(test)]
mod tests {
    use exposition_graph::workflow::registry::{LEAN_AUDIT, operations};

    use super::MAX_AUDIT_MS;

    #[test]
    fn the_registered_audit_limit_is_one_the_lean_service_honours() {
        let audit = operations().into_iter().find(|o| o.id == LEAN_AUDIT).unwrap();
        assert!(
            u64::from(audit.limits.max_seconds) * 1000 <= MAX_AUDIT_MS,
            "the plan shows {} s but an audit is stopped after {} s",
            audit.limits.max_seconds,
            MAX_AUDIT_MS / 1000
        );
    }
}
