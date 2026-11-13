//! Reviewed promotion of a retained result into a manuscript.
//!
//! Wraps the existing format-owned attachment path (`exposition-attach`):
//!
//! 1. **Prepare**: verify the artifact against the digest its run recorded,
//!    plan the exact markup the format adapter will insert against one
//!    document revision, and keep the plan in Rust (the webview only holds an
//!    opaque id).
//! 2. **Approve**: re-verify the digest (an altered artifact is refused) and
//!    the revision (a stale approval is refused), re-plan and compare, write
//!    the intent to the app-owned operation journal, then apply exactly one
//!    undoable document transaction (rejected if the document moved on).
//! 3. **Record**: retain the artifact (one resource identity, computational
//!    evidence) and record `manuscript usesAsset artifact`. If that graph write
//!    fails, the journal keeps a visible pending-reconciliation record; nothing
//!    claims a completed attachment, and the source edit is never re-applied.
//!
//! Undo/redo of the insertion is followed here: when the attachment's markers
//! leave (or return to) the document, the `usesAsset` relation research
//! recorded is removed (or restored). The graph's own source reconciler
//! observes markers too; both follow the same rule (markers present ⇔ use).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use exposition_attach::{Format, PlanInput};
use exposition_core::artifact::{ArtifactKind, ArtifactRecord, AttachArtifactRequest, RunId};
use exposition_core::document::{ConflictPolicy, EditOrigin, TextPatch};
use exposition_core::path::WorkspacePath;
use exposition_core::text::{TextRange, utf16_len, utf16_to_byte};
use exposition_graph::{
    EntityId, OperationId, OperationKind, OperationRecord, OperationState, RelationId, RelationKind, ServiceBatch,
};

use super::types::{
    AttachmentNotice, AttachmentRecorded, PreviewEdit, PromotionGraphState, PromotionOutcome, PromotionPreview,
    PromotionRequest, PromotionsState, ReconcileOutcome, RetainState, TrackedAttachment,
};
use super::{ResearchProject, now_ms, origin};
use crate::error::ResearchError;
use crate::types::RunManifest;

/// How long a prepared review stays approvable.
const REVIEW_TTL_MS: u64 = 30 * 60 * 1000;
const MAX_PENDING: usize = 32;
const MAX_TRACKED: usize = 200;
/// Characters of replaced text shown in a preview.
const REPLACED_PREVIEW: usize = 4000;

struct Pending {
    request: PromotionRequest,
    root: PathBuf,
    artifact: ArtifactRecord,
    path: WorkspacePath,
    revision: u64,
    patches: Vec<TextPatch>,
    attachment_key: String,
    created_ms: u64,
    /// What the user reviews (returned again by [`ResearchProject::promotion_preview`]).
    preview: PromotionPreview,
}

#[derive(Default)]
struct Tracked {
    root: Option<PathBuf>,
    items: Vec<(TrackedAttachment, Format)>,
}

#[derive(Default)]
pub(crate) struct Promotions {
    pending: Mutex<HashMap<String, Pending>>,
    tracked: Mutex<Tracked>,
    /// Test hook (debug builds): fail the next graph step of a promotion.
    fail_next_graph_write: AtomicBool,
}

fn format_name(format: Format) -> &'static str {
    match format {
        Format::Typst => "typst",
        Format::Latex => "latex",
        Format::Markdown => "markdown",
    }
}

fn format_from(name: &str) -> Option<Format> {
    match name {
        "typst" => Some(Format::Typst),
        "latex" => Some(Format::Latex),
        "markdown" => Some(Format::Markdown),
        _ => None,
    }
}

/// The begin/end marker lines of an attachment (as the format adapter writes them).
pub fn marker_lines(format: Format, key: &str) -> (String, String) {
    match format {
        Format::Typst => (format!("// exposition:begin {key}"), format!("// exposition:end {key}")),
        Format::Latex => (format!("% exposition:begin {key}"), format!("% exposition:end {key}")),
        Format::Markdown => (format!("<!-- exposition:begin {key} -->"), format!("<!-- exposition:end {key} -->")),
    }
}

/// Whether the attachment `key` is in `text`: exactly one begin marker
/// followed by its end marker.
pub fn attachment_present(format: Format, text: &str, key: &str) -> bool {
    let (begin, end) = marker_lines(format, key);
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let begins: Vec<usize> = lines.iter().enumerate().filter(|(_, l)| **l == begin).map(|(i, _)| i).collect();
    let ends: Vec<usize> = lines.iter().enumerate().filter(|(_, l)| **l == end).map(|(i, _)| i).collect();
    matches!((begins.as_slice(), ends.as_slice()), ([b], [e]) if b < e)
}

fn refs(pairs: &[(&str, String)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect()
}

fn step_refs<'a>(record: &'a OperationRecord, name: &str) -> Option<&'a BTreeMap<String, String>> {
    record.steps.iter().rev().find(|s| s.name == name && s.completed).map(|s| &s.refs)
}

impl ResearchProject {
    fn journal_project(&self) -> Option<exposition_graph::ProjectId> {
        self.graph.status().project
    }

    /// Test hook: make the next promotion's graph step fail (debug builds wire it).
    pub fn fail_next_graph_write(&self) {
        self.promotions.fail_next_graph_write.store(true, Ordering::SeqCst);
    }

    /// Plan the attachment against the document's current revision.
    fn plan_promotion(
        &self,
        root: &std::path::Path,
        request: &PromotionRequest,
        artifact: &ArtifactRecord,
    ) -> Result<(WorkspacePath, Format, u64, exposition_attach::AttachPlan, String), ResearchError> {
        if !matches!(artifact.kind, ArtifactKind::Figure | ArtifactKind::Table) {
            return Err(ResearchError::coded(
                "attach.unsupported",
                "Only figures and tables can be attached to documents.",
            ));
        }
        let snapshot = self
            .documents
            .snapshot(&request.document)
            .map_err(|e| ResearchError::coded("promotion.document", e.to_string()))?;
        let path = snapshot
            .path
            .clone()
            .ok_or_else(|| ResearchError::coded("attach.unsupportedFormat", "The document is not a project file."))?;
        let format = Format::from_path(&path).ok_or_else(|| {
            ResearchError::coded(
                "attach.unsupportedFormat",
                "documents of this type do not support attachments (Typst, LaTeX, and Markdown do)",
            )
        })?;
        let (content, mut warnings) =
            exposition_attach::load_content_with(root, format, &path, Some(&snapshot.text), artifact)
                .map_err(|e| ResearchError::coded(e.code(), e.to_string()))?;
        let attach = AttachArtifactRequest {
            document: request.document.clone(),
            base_revision: request_base(request, snapshot.revision),
            artifact: artifact.clone(),
            run: Some(RunId(request.run_id.clone())),
            placement: request.placement.clone(),
            caption: request.caption.clone(),
            attachment_key: request.attachment_key.clone(),
            origin: EditOrigin::Research { run_id: request.run_id.clone() },
        };
        let input = PlanInput {
            format,
            document: &request.document,
            document_path: &path,
            revision: snapshot.revision,
            text: &snapshot.text,
        };
        let mut plan = exposition_attach::plan(&input, &attach, &content)
            .map_err(|e| ResearchError::coded(e.code(), e.to_string()))?;
        plan.transaction.conflict = ConflictPolicy::Reject;
        warnings.append(&mut plan.warnings);
        plan.warnings = warnings;
        Ok((path, format, snapshot.revision, plan, snapshot.text.to_string()))
    }

    /// Step 1: compute exactly what will be inserted, for review.
    pub fn prepare_promotion(&self, request: &PromotionRequest) -> Result<PromotionPreview, ResearchError> {
        let root = self.root()?;
        let (_, artifact) = self.verified_artifact(&request.run_id, &request.artifact_id)?;
        let (path, format, revision, plan, text) = self.plan_promotion(&root, request, &artifact)?;
        let patches = plan.transaction.edits.first().map(|e| e.patches.clone()).unwrap_or_default();
        let edits = patches
            .iter()
            .map(|p| {
                let start = utf16_to_byte(&text, p.range.start as usize).unwrap_or(0);
                let end = utf16_to_byte(&text, p.range.end as usize).unwrap_or(start);
                let mut replaced: String = text[start..end].chars().take(REPLACED_PREVIEW).collect();
                if text[start..end].chars().count() > REPLACED_PREVIEW {
                    replaced.push('…');
                }
                PreviewEdit {
                    range: p.range,
                    line: text[..start].matches('\n').count() as u32 + 1,
                    replaced,
                    text: p.text.clone(),
                }
            })
            .collect();
        let retained = self
            .graph
            .resource(artifact.path.as_ref().expect("project artifact"))
            .ok()
            .flatten()
            .filter(|e| e.roles.contains(&exposition_graph::ResourceRole::Artifact))
            .map(|e| e.id);
        let id = format!("promo-{}", uuid::Uuid::new_v4().simple());
        let created_ms = now_ms();
        let preview = PromotionPreview {
            promotion: id.clone(),
            document: request.document.clone(),
            path: path.clone(),
            format: format_name(format).into(),
            revision,
            attachment_key: plan.attachment_key.clone(),
            update: matches!(request.placement, exposition_core::artifact::AttachmentPlacement::Update { .. }),
            edits,
            run_id: request.run_id.clone(),
            artifact: artifact.clone(),
            retained,
            warnings: plan.warnings.clone(),
            expires_at_ms: created_ms + REVIEW_TTL_MS,
        };
        let mut pending = self.promotions.pending.lock().unwrap_or_else(|p| p.into_inner());
        pending.retain(|_, p| created_ms.saturating_sub(p.created_ms) < REVIEW_TTL_MS);
        if pending.len() >= MAX_PENDING
            && let Some(oldest) = pending.iter().min_by_key(|(_, p)| p.created_ms).map(|(k, _)| k.clone())
        {
            pending.remove(&oldest);
        }
        pending.insert(
            id,
            Pending {
                request: request.clone(),
                root,
                artifact,
                path,
                revision,
                patches,
                attachment_key: plan.attachment_key,
                created_ms,
                preview: preview.clone(),
            },
        );
        Ok(preview)
    }

    /// A prepared review that is still pending (e.g. one a workflow step
    /// prepared), exactly as it was computed; `None` once applied, canceled,
    /// or expired.
    pub fn promotion_preview(&self, promotion: &str) -> Option<PromotionPreview> {
        let pending = self.promotions.pending.lock().unwrap_or_else(|p| p.into_inner());
        pending
            .get(promotion)
            .filter(|p| now_ms().saturating_sub(p.created_ms) < REVIEW_TTL_MS)
            .map(|p| p.preview.clone())
    }

    /// Drop a prepared review without applying it.
    pub fn cancel_promotion(&self, promotion: &str) -> bool {
        self.promotions.pending.lock().unwrap_or_else(|p| p.into_inner()).remove(promotion).is_some()
    }

    /// Step 2 and 3: apply the reviewed insertion and record it in the graph.
    pub fn approve_promotion(&self, promotion: &str) -> Result<PromotionOutcome, ResearchError> {
        let pending =
            self.promotions.pending.lock().unwrap_or_else(|p| p.into_inner()).remove(promotion).ok_or_else(|| {
                ResearchError::coded(
                    "promotion.unknown",
                    "This review expired or was already used; review the insertion again.",
                )
            })?;
        if now_ms().saturating_sub(pending.created_ms) >= REVIEW_TTL_MS {
            return Err(ResearchError::coded("promotion.expired", "This review expired; review the insertion again."));
        }
        let root = self.root()?;
        if root != pending.root {
            return Err(ResearchError::coded("promotion.projectChanged", "The project changed since this review."));
        }
        // The artifact must still be exactly what was reviewed.
        let (manifest, artifact) = self.verified_artifact(&pending.request.run_id, &pending.request.artifact_id)?;
        if artifact.content_hash != pending.artifact.content_hash {
            return Err(ResearchError::coded(
                "research.artifactChanged",
                "The artifact is not the one you reviewed; review the insertion again.",
            ));
        }
        // The document must still be at the reviewed revision.
        let current = self
            .documents
            .snapshot(&pending.request.document)
            .map_err(|e| ResearchError::coded("promotion.document", e.to_string()))?
            .revision;
        if current != pending.revision {
            return Err(ResearchError::coded(
                "promotion.stale",
                format!(
                    "{} changed since you reviewed this insertion (revision {} is now {}); nothing was inserted. Review it again.",
                    pending.path, pending.revision, current
                ),
            ));
        }
        let (path, format, revision, plan, _) = self.plan_promotion(&root, &pending.request, &artifact)?;
        let patches = plan.transaction.edits.first().map(|e| e.patches.clone()).unwrap_or_default();
        if path != pending.path
            || revision != pending.revision
            || patches != pending.patches
            || plan.attachment_key != pending.attachment_key
        {
            return Err(ResearchError::coded(
                "promotion.planChanged",
                "The insertion would differ from what you reviewed; nothing was inserted. Review it again.",
            ));
        }
        // Intent first (app-owned journal; survives a damaged graph).
        let journal = self.graph.journal();
        let artifact_path = artifact.path.clone().expect("project artifact");
        let record = journal
            .begin(
                OperationKind::Promotion,
                &root,
                self.journal_project(),
                &format!("Insert {} into {}", artifact_path.file_name(), path),
                vec!["attach".into(), "graph".into()],
            )
            .map_err(|e| {
                ResearchError::coded(
                    "promotion.journal",
                    format!("The operation could not be recorded ({e}); nothing was inserted."),
                )
            })?;
        let op = record.id.clone();
        let _ = journal.record_step(
            &root,
            &op,
            "prepared",
            refs(&[
                ("document", pending.request.document.0.clone()),
                ("path", path.to_string()),
                ("format", format_name(format).into()),
                ("revision", revision.to_string()),
                ("attachmentKey", plan.attachment_key.clone()),
                ("runId", pending.request.run_id.clone()),
                ("artifactId", pending.request.artifact_id.clone()),
                ("artifactPath", artifact_path.to_string()),
                ("artifactHash", artifact.content_hash.clone()),
            ]),
            true,
            None,
        );
        let transaction = plan.transaction.id.0.clone();
        let applied = match self.documents.apply(plan.transaction.clone()) {
            Ok(outcome) => outcome.applied.into_iter().find(|e| e.document == pending.request.document),
            Err(e) => {
                let message = format!("The document refused the insertion: {e}");
                let _ = journal.fail(&root, &op, &message);
                return Err(ResearchError::coded("promotion.document", message));
            }
        };
        let Some(applied) = applied else {
            let _ = journal.fail(&root, &op, "the document service applied no edit");
            return Err(ResearchError::coded("promotion.document", "The document service applied no edit."));
        };
        // Locate the block in the new revision (as `attach_artifact` does).
        let mut shift: i64 = 0;
        let mut start = 0u32;
        for (index, patch) in applied.patches.iter().enumerate() {
            if index == plan.block_patch {
                start = (i64::from(patch.range.start) + shift) as u32 + plan.block_offset;
                break;
            }
            shift += utf16_len(&patch.text) as i64 - i64::from(patch.range.len());
        }
        let range = TextRange::new(start, start + plan.block_len);
        let _ = journal.record_step(
            &root,
            &op,
            "attach",
            refs(&[
                ("transaction", transaction.clone()),
                ("previousRevision", applied.previous_revision.to_string()),
                ("revision", applied.revision.to_string()),
                ("attachmentKey", plan.attachment_key.clone()),
            ]),
            true,
            None,
        );
        let graph = self.finish_attachment(
            &root,
            &op,
            &manifest,
            &artifact,
            &path,
            format,
            &plan.attachment_key,
            applied.revision,
        );
        Ok(PromotionOutcome {
            operation: op,
            document: pending.request.document.clone(),
            path,
            revision: applied.revision,
            transaction,
            attachment_key: plan.attachment_key,
            range,
            warnings: plan.warnings,
            graph,
        })
    }

    /// The graph step of an applied attachment: record it (and follow it for
    /// undo/redo) or leave a pending-reconciliation record in the journal.
    #[allow(clippy::too_many_arguments)]
    fn finish_attachment(
        &self,
        root: &std::path::Path,
        op: &OperationId,
        manifest: &RunManifest,
        artifact: &ArtifactRecord,
        path: &WorkspacePath,
        format: Format,
        key: &str,
        revision: u64,
    ) -> PromotionGraphState {
        let journal = self.graph.journal();
        match self.record_promotion_graph(manifest, artifact, path, key) {
            Ok((graph_revision, manuscript, artifact_entity, relation)) => {
                let _ = journal.record_step(
                    root,
                    op,
                    "graph",
                    refs(&[
                        ("graphRevision", graph_revision.to_string()),
                        ("manuscript", manuscript.to_string()),
                        ("artifact", artifact_entity.to_string()),
                        ("relation", relation.as_ref().map(|r| r.to_string()).unwrap_or_default()),
                    ]),
                    true,
                    None,
                );
                let _ = journal.complete(root, op);
                self.track(
                    root,
                    TrackedAttachment {
                        operation: op.clone(),
                        path: path.clone(),
                        attachment_key: key.to_owned(),
                        manuscript: manuscript.clone(),
                        artifact: artifact_entity.clone(),
                        present: true,
                    },
                    format,
                );
                PromotionGraphState::Recorded { graph_revision, manuscript, artifact: artifact_entity, relation }
            }
            Err(e) => {
                let reason = format!(
                    "The attachment was inserted into {path} (revision {revision}), but the project graph was not updated: {e}"
                );
                let _ = journal.record_step(root, op, "graph", BTreeMap::new(), false, Some(e.to_string()));
                let _ = journal.needs_reconciliation(root, op, &reason);
                PromotionGraphState::PendingReconciliation { reason }
            }
        }
    }

    /// Record an attachment another service already inserted through the
    /// format-owned attachment path (the agent `insert_artifact` tool, after
    /// its own review): the artifact is retained (one resource identity,
    /// computational evidence, provenance), `manuscript usesAsset artifact` is
    /// recorded, the operation is journaled like a promotion, and the
    /// attachment is followed through undo/redo. The document is never
    /// changed here; the attachment's markers must be in its current text.
    /// A failed graph write leaves a pending-reconciliation record.
    pub fn record_external_attachment(&self, notice: &AttachmentNotice) -> Result<AttachmentRecorded, ResearchError> {
        let root = self.root()?;
        let (manifest, artifact) = self.verified_artifact(&notice.run_id, &notice.artifact_id)?;
        let snapshot = self
            .documents
            .snapshot(&notice.document)
            .map_err(|e| ResearchError::coded("promotion.document", e.to_string()))?;
        let path = snapshot
            .path
            .clone()
            .ok_or_else(|| ResearchError::coded("attach.unsupportedFormat", "The document is not a project file."))?;
        let format = Format::from_path(&path).ok_or_else(|| {
            ResearchError::coded("attach.unsupportedFormat", "documents of this type do not support attachments")
        })?;
        if !attachment_present(format, &snapshot.text, &notice.attachment_key) {
            return Err(ResearchError::coded(
                "promotion.notAttached",
                format!("The attachment {} is not in {path}; nothing was recorded.", notice.attachment_key),
            ));
        }
        let journal = self.graph.journal();
        let artifact_path = artifact.path.clone().expect("project artifact");
        let record = journal
            .begin(
                OperationKind::Promotion,
                &root,
                self.journal_project(),
                &format!("{}: insert {} into {}", notice.actor, artifact_path.file_name(), path),
                vec!["attach".into(), "graph".into()],
            )
            .map_err(|e| {
                ResearchError::coded("promotion.journal", format!("The operation could not be recorded ({e})."))
            })?;
        let op = record.id.clone();
        let _ = journal.record_step(
            &root,
            &op,
            "prepared",
            refs(&[
                ("document", notice.document.0.clone()),
                ("path", path.to_string()),
                ("format", format_name(format).into()),
                ("revision", notice.revision.to_string()),
                ("attachmentKey", notice.attachment_key.clone()),
                ("runId", notice.run_id.clone()),
                ("artifactId", notice.artifact_id.clone()),
                ("artifactPath", artifact_path.to_string()),
                ("artifactHash", artifact.content_hash.clone()),
                ("actor", notice.actor.clone()),
            ]),
            true,
            None,
        );
        let _ = journal.record_step(
            &root,
            &op,
            "attach",
            refs(&[
                ("transaction", notice.transaction.clone().unwrap_or_default()),
                ("revision", notice.revision.to_string()),
                ("attachmentKey", notice.attachment_key.clone()),
            ]),
            true,
            None,
        );
        let graph = self.finish_attachment(
            &root,
            &op,
            &manifest,
            &artifact,
            &path,
            format,
            &notice.attachment_key,
            notice.revision,
        );
        Ok(AttachmentRecorded { operation: op, path, graph })
    }

    /// Retain the artifact and record `manuscript usesAsset artifact`.
    fn record_promotion_graph(
        &self,
        manifest: &RunManifest,
        artifact: &ArtifactRecord,
        path: &WorkspacePath,
        key: &str,
    ) -> Result<(u64, EntityId, EntityId, Option<RelationId>), ResearchError> {
        if self.promotions.fail_next_graph_write.swap(false, Ordering::SeqCst) {
            return Err(ResearchError::coded(
                "graph.injectedFailure",
                "the graph write failed (injected test failure)",
            ));
        }
        let _ = self.graph_root()?;
        let retained = self.retain_verified(manifest, artifact, RetainState::Active, None)?;
        let manuscript = self.graph.ensure_resource(path, None, false, super::SERVICE)?;
        let graph_revision = self.add_relations(
            vec![(
                RelationKind::UsesAsset,
                manuscript.id.clone(),
                retained.result.entity.clone(),
                None,
                Some(format!("attachment {key}")),
            )],
            &format!("{path} uses {}", retained.result.path),
        )?;
        let relation = self.graph.with_store(|s| {
            Ok(s.find_relation(RelationKind::UsesAsset, &manuscript.id, &retained.result.entity)?.map(|r| r.id))
        })?;
        Ok((graph_revision, manuscript.id, retained.result.entity, relation))
    }

    /// Current text of a project file: the open buffer, else the file.
    fn current_text(&self, root: &std::path::Path, path: &WorkspacePath) -> Option<String> {
        if let Some(id) = self.documents.find_by_path(path) {
            return self.documents.snapshot(&id).ok().map(|s| s.text.to_string());
        }
        let file = crate::store::resolve_existing(root, path).ok()?;
        if std::fs::metadata(&file).ok()?.len() > 64 * 1024 * 1024 {
            return None;
        }
        std::fs::read_to_string(file).ok()
    }

    /// Promotions of the open project: unresolved records and followed attachments.
    pub fn promotions(&self) -> Result<PromotionsState, ResearchError> {
        let root = self.root()?;
        let unresolved = self
            .graph
            .journal()
            .unresolved(&root, false)
            .into_iter()
            .filter(|r| r.kind == OperationKind::Promotion)
            .collect();
        let tracked = self.promotions.tracked.lock().unwrap_or_else(|p| p.into_inner());
        let items = if tracked.root.as_deref() == Some(root.as_path()) {
            tracked.items.iter().map(|(t, _)| t.clone()).collect()
        } else {
            Vec::new()
        };
        Ok(PromotionsState { unresolved, tracked: items })
    }

    /// Reconcile an interrupted promotion against the actual document: if the
    /// attachment is in the document, record it in the graph now; if not,
    /// close the record. The source edit is never re-applied.
    pub fn reconcile_promotion(&self, operation: &OperationId) -> Result<ReconcileOutcome, ResearchError> {
        let root = self.root()?;
        let journal = self.graph.journal();
        let record = journal.get(&root, operation).map_err(|_| {
            ResearchError::coded("promotion.unknownOperation", "That operation is not recorded for this project.")
        })?;
        if record.kind != OperationKind::Promotion {
            return Err(ResearchError::coded("promotion.unknownOperation", "That operation is not a promotion."));
        }
        if !matches!(record.state, OperationState::Pending | OperationState::NeedsReconciliation { .. }) {
            return Ok(ReconcileOutcome { attachment_present: false, record });
        }
        let prepared = step_refs(&record, "prepared").cloned().ok_or_else(|| {
            ResearchError::coded("promotion.unreadable", "The operation record has no prepared step.")
        })?;
        let get = |k: &str| prepared.get(k).cloned().unwrap_or_default();
        let path = WorkspacePath::parse(&get("path"))
            .map_err(|_| ResearchError::coded("promotion.unreadable", "The operation record names no document."))?;
        let format = format_from(&get("format"))
            .ok_or_else(|| ResearchError::coded("promotion.unreadable", "The operation record names no format."))?;
        let key = get("attachmentKey");
        let present = self.current_text(&root, &path).is_some_and(|t| attachment_present(format, &t, &key));
        if !present {
            let message = format!(
                "The attachment {key} is not in {path} (the insertion was undone, removed, or never applied); nothing was recorded in the graph."
            );
            let _ =
                journal.record_step(&root, operation, "reconcile", refs(&[("present", "false".into())]), true, None);
            let record = journal
                .resolve(&root, operation, &message)
                .map_err(|e| ResearchError::coded("promotion.journal", e.to_string()))?;
            return Ok(ReconcileOutcome { record, attachment_present: false });
        }
        let (manifest, artifact) = self.verified_artifact(&get("runId"), &get("artifactId"))?;
        if artifact.content_hash != get("artifactHash") {
            return Err(ResearchError::coded(
                "research.artifactChanged",
                "The artifact differs from the one that was inserted; the record stays open for review.",
            ));
        }
        let (graph_revision, manuscript, artifact_entity, relation) =
            self.record_promotion_graph(&manifest, &artifact, &path, &key)?;
        let _ = journal.record_step(
            &root,
            operation,
            "graph",
            refs(&[
                ("graphRevision", graph_revision.to_string()),
                ("manuscript", manuscript.to_string()),
                ("artifact", artifact_entity.to_string()),
                ("relation", relation.as_ref().map(|r| r.to_string()).unwrap_or_default()),
                ("reconciled", "true".into()),
            ]),
            true,
            None,
        );
        let record = journal
            .resolve(
                &root,
                operation,
                &format!("The attachment {key} is in {path}; it was recorded in the project graph."),
            )
            .map_err(|e| ResearchError::coded("promotion.journal", e.to_string()))?;
        self.track(
            &root,
            TrackedAttachment {
                operation: operation.clone(),
                path,
                attachment_key: key,
                manuscript,
                artifact: artifact_entity,
                present: true,
            },
            format,
        );
        Ok(ReconcileOutcome { record, attachment_present: true })
    }

    fn track(&self, root: &std::path::Path, item: TrackedAttachment, format: Format) {
        let mut tracked = self.promotions.tracked.lock().unwrap_or_else(|p| p.into_inner());
        if tracked.root.as_deref() != Some(root) {
            tracked.root = Some(root.to_path_buf());
            tracked.items.clear();
        }
        tracked.items.retain(|(t, _)| t.operation != item.operation);
        tracked.items.push((item, format));
        if tracked.items.len() > MAX_TRACKED {
            tracked.items.remove(0);
        }
    }

    /// Project opened: mark promotions left pending by an earlier run as
    /// needing reconciliation (never re-run), load the attachments research
    /// follows, and bring their `usesAsset` relations in line with the text.
    pub fn load_promotions(&self) -> Result<PromotionsState, ResearchError> {
        let root = self.root()?;
        let journal = self.graph.journal();
        let _ = journal.unresolved(&root, true);
        let mut items = Vec::new();
        for record in journal.list(&root).into_iter().take(MAX_TRACKED) {
            if record.kind != OperationKind::Promotion
                || !matches!(record.state, OperationState::Completed | OperationState::Resolved { .. })
            {
                continue;
            }
            let (Some(prepared), Some(graph)) = (step_refs(&record, "prepared"), step_refs(&record, "graph")) else {
                continue;
            };
            let (Ok(path), Some(format), Ok(manuscript), Ok(artifact)) = (
                WorkspacePath::parse(prepared.get("path").map(String::as_str).unwrap_or("")),
                format_from(prepared.get("format").map(String::as_str).unwrap_or("")),
                EntityId::parse(graph.get("manuscript").map(String::as_str).unwrap_or("")),
                EntityId::parse(graph.get("artifact").map(String::as_str).unwrap_or("")),
            ) else {
                continue;
            };
            let key = prepared.get("attachmentKey").cloned().unwrap_or_default();
            items.push((
                TrackedAttachment {
                    operation: record.id.clone(),
                    path,
                    attachment_key: key,
                    manuscript,
                    artifact,
                    present: true,
                },
                format,
            ));
        }
        {
            let mut tracked = self.promotions.tracked.lock().unwrap_or_else(|p| p.into_inner());
            tracked.root = Some(root.clone());
            tracked.items = items;
        }
        let paths: Vec<WorkspacePath> = {
            let tracked = self.promotions.tracked.lock().unwrap_or_else(|p| p.into_inner());
            let mut p: Vec<WorkspacePath> = tracked.items.iter().map(|(t, _)| t.path.clone()).collect();
            p.sort();
            p.dedup();
            p
        };
        for path in paths {
            if let Some(text) = self.current_text(&root, &path) {
                self.observe_text(&path, &text, true);
            }
        }
        self.promotions()
    }

    /// Whether research follows attachments in `path` (cheap; for event filtering).
    pub fn follows(&self, path: &WorkspacePath) -> bool {
        self.promotions.tracked.lock().unwrap_or_else(|p| p.into_inner()).items.iter().any(|(t, _)| &t.path == path)
    }

    /// The text of `path` changed (edit, undo, redo, reload): follow the
    /// attachments in it. Returns the attachments whose presence changed.
    pub fn observe_text(&self, path: &WorkspacePath, text: &str, force: bool) -> Vec<TrackedAttachment> {
        let Ok(root) = self.root() else { return Vec::new() };
        let mut changed = Vec::new();
        let pairs: Vec<(EntityId, EntityId, bool)> = {
            let mut tracked = self.promotions.tracked.lock().unwrap_or_else(|p| p.into_inner());
            if tracked.root.as_deref() != Some(root.as_path()) {
                return Vec::new();
            }
            for (item, format) in tracked.items.iter_mut().filter(|(t, _)| &t.path == path) {
                let present = attachment_present(*format, text, &item.attachment_key);
                if present != item.present {
                    item.present = present;
                    changed.push(item.clone());
                }
            }
            // A manuscript uses an artifact while any of its attachments is present.
            let mut pairs: Vec<(EntityId, EntityId, bool)> = Vec::new();
            for (item, _) in tracked.items.iter().filter(|(t, _)| &t.path == path) {
                match pairs.iter_mut().find(|(m, a, _)| *m == item.manuscript && *a == item.artifact) {
                    Some(p) => p.2 |= item.present,
                    None => pairs.push((item.manuscript.clone(), item.artifact.clone(), item.present)),
                }
            }
            pairs
        };
        if changed.is_empty() && !force {
            return changed;
        }
        for (manuscript, artifact, present) in pairs {
            let existing = self
                .graph
                .with_store(|s| Ok(s.find_relation(RelationKind::UsesAsset, &manuscript, &artifact)?))
                .ok()
                .flatten();
            let result = match (present, existing) {
                // Only the relation research recorded; others belong to their owner.
                (false, Some(r)) if r.origin == origin() => self
                    .graph
                    .apply_service_batch(
                        &origin(),
                        &ServiceBatch {
                            remove_relations: vec![r.id],
                            summary: format!("The attachment left {path} (undone or removed)"),
                            ..Default::default()
                        },
                    )
                    .map(|_| ()),
                (true, None) => self
                    .add_relations(
                        vec![(RelationKind::UsesAsset, manuscript, artifact, None, Some("attachment restored".into()))],
                        &format!("The attachment is back in {path}"),
                    )
                    .map(|_| ())
                    .map_err(|e| exposition_graph::GraphError::new("graph.storage", e.to_string())),
                _ => Ok(()),
            };
            if let Err(e) = result {
                eprintln!("research: could not follow an attachment in {path}: {e}");
            }
        }
        let journal = self.graph.journal();
        for item in &changed {
            let _ = journal.record_step(
                &root,
                &item.operation,
                if item.present { "restored" } else { "removed" },
                refs(&[("path", item.path.to_string()), ("attachmentKey", item.attachment_key.clone())]),
                true,
                Some(if item.present {
                    "The attachment is in the document again (redo or restore).".into()
                } else {
                    "The attachment left the document (undo, revert, or edit).".into()
                }),
            );
        }
        changed
    }
}

/// The base revision an attachment plan is computed against: the request's
/// (an insert offset only makes sense for the revision it was chosen in).
fn request_base(request: &PromotionRequest, current: u64) -> u64 {
    request.base_revision.unwrap_or(current)
}
