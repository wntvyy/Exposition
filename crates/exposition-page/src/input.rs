//! Format-aware page input and its application through the document service.
//!
//! A page edit names a document revision, a selection in that revision, and
//! an operation. Format rules ([`PageInputRules`]) turn it into source patches;
//! [`apply_page_edit`] submits them as one transaction with
//! [`EditOrigin::Page`], merge-able undo for continuous typing, and
//! [`ConflictPolicy::RebaseDisjoint`]. A stale or conflicting edit is never
//! applied with old offsets: the caller resynchronises and retries.

use std::sync::Arc;

use exposition_core::document::{
    ConflictPolicy, DocumentEdit, DocumentId, EditOrigin, LineEnding, SelectionHint, TextPatch, TransactionError,
    TransactionId, TransactionRequest, UndoBehavior,
};
use exposition_core::text::TextRange;
use exposition_workspace::DocumentService;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::map::EditMode;

/// What the user did at the caret or selection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum PageEditOp {
    /// Typed or pasted plain text (newlines become paragraph breaks in prose).
    Insert {
        text: String,
    },
    DeleteBackward,
    DeleteForward,
    ParagraphBreak,
}

/// Why an edit was not applied.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS, thiserror::Error)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum EditRefusal {
    /// The position is in content that cannot be edited on the page.
    #[error("not editable on the page ({reason})")]
    NotEditable { reason: String },
    /// The edit would change document structure beyond the intended text.
    #[error("the edit would change document structure; edit in source")]
    WouldChangeStructure,
    /// The selection does not fall on character boundaries of the text.
    #[error("invalid position")]
    InvalidPosition,
    /// Nothing to change (e.g. deleting at the start of the document).
    #[error("nothing to change")]
    NoChange,
}

/// Planned source change for one page edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditPlan {
    /// Sorted, non-overlapping patches relative to the planned text.
    pub patches: Vec<TextPatch>,
    /// Caret after the edit, as a UTF-16 offset in the new text.
    pub caret: u32,
    pub mode: EditMode,
}

/// Format-specific translation of page input into source patches.
pub trait PageInputRules: Send + Sync {
    /// Plan `op` at `selection` (UTF-16, either order) over `text`.
    fn plan(
        &self,
        text: &str,
        selection: TextRange,
        op: &PageEditOp,
        line_ending: &str,
    ) -> Result<EditPlan, EditRefusal>;

    /// The input mode at an offset, or `None` where page input is not accepted.
    fn mode_at(&self, text: &str, offset: u32) -> Option<EditMode>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PageEditRequest {
    /// Client-generated transaction identifier.
    pub id: TransactionId,
    pub document: DocumentId,
    /// Revision the selection refers to.
    pub base_revision: u64,
    /// Selection anchor/head (equal for a caret), UTF-16 in `base_revision`.
    pub selection: TextRange,
    pub op: PageEditOp,
    /// Client typing-group counter: consecutive edits with the same group and
    /// kind merge into one undo step; the client bumps it when the caret jumps.
    pub group: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum PageEditOutcome {
    /// Applied as `revision`; the caret is at `caret` (UTF-16 in `revision`).
    Applied { revision: u64, caret: u32, mode: EditMode, rebased: bool },
    /// Nothing to change; the caret moves to `caret` in `revision`.
    Unchanged { revision: u64, caret: u32 },
    /// Refused without changing the document.
    Refused { revision: u64, refusal: EditRefusal },
    /// The base revision is no longer available or conflicts with newer
    /// changes: resynchronise (map the selection forward) and retry.
    Stale { current_revision: u64 },
}

/// Why a document with mixed line endings cannot be edited on the page.
pub const MIXED_LINE_ENDINGS: &str = "the file mixes line endings; normalize them to edit it";

fn line_ending_text(ending: LineEnding) -> &'static str {
    match ending {
        LineEnding::Crlf => "\r\n",
        _ => "\n",
    }
}

/// Apply a page edit through the document service.
///
/// `revision_text` returns the text of `(document, revision)` when the request
/// is based on an older revision whose text is still known (e.g. the snapshot
/// a page map was built from); the edit is then rebased over newer disjoint
/// changes by the document service.
pub fn apply_page_edit(
    documents: &DocumentService,
    rules: &dyn PageInputRules,
    request: &PageEditRequest,
    revision_text: &dyn Fn(&DocumentId, u64) -> Option<Arc<str>>,
) -> Result<PageEditOutcome, TransactionError> {
    let info = documents.info(&request.document)?;
    let snapshot = documents.snapshot(&request.document)?;
    let text = if request.base_revision == snapshot.revision {
        Arc::clone(&snapshot.text)
    } else if request.base_revision < snapshot.revision {
        match revision_text(&request.document, request.base_revision) {
            Some(text) => text,
            None => return Ok(PageEditOutcome::Stale { current_revision: snapshot.revision }),
        }
    } else {
        return Err(TransactionError::FutureRevision {
            document: request.document.clone(),
            base_revision: request.base_revision,
        });
    };
    if let Some(reason) = info.read_only {
        return Ok(PageEditOutcome::Refused {
            revision: snapshot.revision,
            refusal: EditRefusal::NotEditable { reason: format!("read-only: {reason}") },
        });
    }
    // The document service refuses these edits too; say why without trying.
    if info.line_ending == LineEnding::Mixed {
        return Ok(PageEditOutcome::Refused {
            revision: snapshot.revision,
            refusal: EditRefusal::NotEditable { reason: MIXED_LINE_ENDINGS.into() },
        });
    }
    let plan = match rules.plan(&text, request.selection, &request.op, line_ending_text(info.line_ending)) {
        Ok(plan) => plan,
        Err(refusal) => return Ok(PageEditOutcome::Refused { revision: snapshot.revision, refusal }),
    };
    if plan.patches.is_empty() {
        if request.base_revision != snapshot.revision {
            return Ok(PageEditOutcome::Stale { current_revision: snapshot.revision });
        }
        return Ok(PageEditOutcome::Unchanged { revision: snapshot.revision, caret: plan.caret });
    }
    let is_caret = request.selection.start == request.selection.end;
    let undo = match &request.op {
        PageEditOp::Insert { text } if is_caret && text.chars().count() == 1 && text != "\n" => {
            UndoBehavior::Merge { key: format!("page-type-{}", request.group) }
        }
        PageEditOp::DeleteBackward | PageEditOp::DeleteForward if is_caret => {
            UndoBehavior::Merge { key: format!("page-delete-{}", request.group) }
        }
        _ => UndoBehavior::NewGroup,
    };
    let label = match &request.op {
        PageEditOp::Insert { .. } => "Typing",
        PageEditOp::DeleteBackward | PageEditOp::DeleteForward => "Delete",
        PageEditOp::ParagraphBreak => "Paragraph break",
    };
    let caret_before = TextRange::new(
        request.selection.start.min(request.selection.end),
        request.selection.start.max(request.selection.end),
    );
    let transaction = TransactionRequest {
        id: request.id.clone(),
        origin: EditOrigin::Page,
        edits: vec![DocumentEdit {
            document: request.document.clone(),
            base_revision: request.base_revision,
            patches: plan.patches.clone(),
        }],
        undo,
        conflict: ConflictPolicy::RebaseDisjoint,
        label: Some(label.to_owned()),
        selection: Some(SelectionHint { before: vec![caret_before], after: vec![TextRange::empty(plan.caret)] }),
    };
    match documents.apply(transaction) {
        Ok(outcome) => {
            let applied = outcome.applied.into_iter().next().expect("one edit");
            // When rebased, shift the caret by the same amount as the patch
            // that ends at or before it.
            let caret = if applied.rebased {
                let delta = applied
                    .patches
                    .iter()
                    .zip(&plan.patches)
                    .map(|(a, p)| i64::from(a.range.start) - i64::from(p.range.start))
                    .next()
                    .unwrap_or(0);
                (i64::from(plan.caret) + delta).max(0) as u32
            } else {
                plan.caret
            };
            Ok(PageEditOutcome::Applied {
                revision: applied.revision,
                caret,
                mode: plan.mode,
                rebased: applied.rebased,
            })
        }
        Err(TransactionError::Conflict { current_revision, .. })
        | Err(TransactionError::StaleRevision { current_revision, .. }) => {
            Ok(PageEditOutcome::Stale { current_revision: current_revision.max(snapshot.revision) })
        }
        Err(TransactionError::HistoryUnavailable { .. }) => {
            Ok(PageEditOutcome::Stale { current_revision: documents.info(&request.document)?.revision })
        }
        Err(other) => Err(other),
    }
}

#[cfg(all(test, feature = "typst"))]
mod tests {
    use exposition_core::document::{TextEncoding, TransactionId};
    use exposition_core::path::WorkspacePath;
    use exposition_workspace::documents::OpenDocument;

    use super::*;
    use crate::TypstInput;

    #[test]
    fn mixed_line_endings_are_refused_on_the_page_with_the_reason() {
        let documents = DocumentService::new();
        let info = documents.open(OpenDocument {
            path: Some(WorkspacePath::parse("main.typ").unwrap()),
            text: "Hello\r\nworld\n".into(),
            language: "typst".into(),
            encoding: TextEncoding::Utf8,
            read_only: None,
        });
        let request = PageEditRequest {
            id: TransactionId("page-1".into()),
            document: info.id.clone(),
            base_revision: info.revision,
            selection: TextRange::new(1, 1),
            op: PageEditOp::Insert { text: "x".into() },
            group: 1,
        };
        let outcome = apply_page_edit(&documents, &TypstInput, &request, &|_, _| None).unwrap();
        assert_eq!(
            outcome,
            PageEditOutcome::Refused {
                revision: 1,
                refusal: EditRefusal::NotEditable { reason: MIXED_LINE_ENDINGS.into() }
            }
        );
        assert_eq!(documents.state(&info.id).unwrap().text, "Hello\r\nworld\n");
    }
}
