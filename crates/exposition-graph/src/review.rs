//! Review standing of a `formalizes` link.
//!
//! A person reviews the correspondence between a formal declaration and a
//! statement as the statement read at that time. When the link is accepted
//! or confirmed, a digest of the statement's bound text is recorded on the
//! relation; every read compares it with the statement as it is now. A
//! differing or unlocatable statement qualifies the review until a person
//! confirms the link again (which records the new digest) or rejects it.
//! Nothing here re-reviews anything, and the declaration's Lean evidence is
//! never touched.
//!
//! The digest is built from what the binding already uses to identify its
//! text, so nothing is re-read from disk and the comparison uses the same
//! normalization as re-anchoring:
//! - a region binding: its fingerprint (the exact bytes of the bound text);
//! - an extracted declaration: its body fingerprint (whitespace-normalized
//!   text of the construct), so re-wrapping or edits elsewhere in the file
//!   do not change it;
//! - a whole-file binding: the file's content hash;
//! - an entity without a source binding: its label (all the text it has).
//!
//! Paths and ranges are left out: moving or renaming the file, or editing
//! around the statement, is not a change of the statement.

use sha2::{Digest, Sha256};

use crate::model::{BindingState, Entity, Lifecycle, LinkReview, LinkReviewState, SourceBinding, SourceSelector};
use crate::reconcile::matching::decode_fingerprint;

const PREFIX: &str = "statement:v1:";

/// The statement an entity stands for, as the graph can identify it now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatementNow {
    /// Located; the digest identifies its text.
    Located(String),
    /// Cannot be located in the current sources: why, in plain words, and
    /// whether binding the item to its text again would locate it (a binding
    /// that no longer resolves) or not (the item itself is gone).
    Unlocated { why: String, rebindable: bool },
}

impl StatementNow {
    /// The digest to record at review time (`None` when unlocatable: what
    /// the review covered is then unknown).
    pub fn digest(&self) -> Option<String> {
        match self {
            StatementNow::Located(d) => Some(d.clone()),
            StatementNow::Unlocated { .. } => None,
        }
    }
}

fn unlocated(binding: &SourceBinding, fallback: &str) -> StatementNow {
    let detail = binding.detail.as_deref().map(str::trim).filter(|d| !d.is_empty()).unwrap_or(fallback);
    StatementNow::Unlocated { why: format!("{}: {detail}", binding.path), rebindable: true }
}

/// Identify the current statement of `entity` from its bindings.
pub fn statement_now(entity: &Entity, bindings: &[SourceBinding]) -> StatementNow {
    const NOT_INDEXED: &str = "The source has not been indexed yet.";
    if entity.lifecycle == Lifecycle::Tombstoned {
        let reason = entity.tombstone_reason.as_deref().map(str::trim).filter(|r| !r.is_empty());
        return StatementNow::Unlocated {
            why: reason.unwrap_or("It no longer exists in the project sources.").to_owned(),
            rebindable: false,
        };
    }
    let mut h = Sha256::new();
    if bindings.is_empty() {
        h.update(b"label\n");
        h.update(crate::reconcile::extract::fingerprint(&entity.label).as_bytes());
    }
    for b in bindings {
        match b.state {
            BindingState::Resolved => {}
            BindingState::Missing => return unlocated(b, "The bound text is missing."),
            BindingState::Ambiguous => return unlocated(b, "The bound text matches more than one place."),
            BindingState::Unresolved => return unlocated(b, NOT_INDEXED),
        }
        match &b.selector {
            SourceSelector::Region => {
                let Some(fp) = &b.fingerprint else { return unlocated(b, NOT_INDEXED) };
                h.update(b"region\n");
                h.update(fp.as_bytes());
            }
            SourceSelector::Declaration { syntax, .. } => {
                let Some((_, body, _)) = b.fingerprint.as_deref().and_then(decode_fingerprint) else {
                    return unlocated(b, NOT_INDEXED);
                };
                h.update(b"declaration\n");
                h.update(syntax.as_bytes());
                h.update(b"\n");
                h.update(body.as_bytes());
            }
            SourceSelector::WholeFile => {
                let Some(v) = &b.version else { return unlocated(b, NOT_INDEXED) };
                h.update(b"file\n");
                h.update(v.content_hash.as_bytes());
            }
        }
        h.update(b"\n");
    }
    let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    StatementNow::Located(format!("{PREFIX}{hex}"))
}

/// What a person is told when the statement of a formal link cannot be
/// located: why, and what to do. Confirming needs a statement to record, so
/// a binding that no longer resolves is bound again first.
pub fn unlocated_reason(why: &str, rebindable: bool) -> String {
    let why = why.trim_end_matches('.');
    if rebindable {
        format!(
            "The statement cannot be located, so the review cannot be compared with it ({why}). Rebind the \
             statement to its current text, then confirm the link. Until it is rebound the link cannot be \
             confirmed, only rejected."
        )
    } else {
        format!(
            "The statement cannot be located, so the review cannot be compared with it ({why}). The link cannot \
             be confirmed while the statement is not in the sources; reject it, or restore the statement."
        )
    }
}

/// The standing of an accepted `formalizes` link whose review recorded
/// `reviewed` (if anything), against the statement `entity` stands for now.
/// A missing entity reads as unlocatable.
pub fn link_review(reviewed: Option<&str>, entity: Option<&Entity>, bindings: &[SourceBinding]) -> LinkReview {
    let now = match entity {
        Some(e) => statement_now(e, bindings),
        None => StatementNow::Unlocated { why: "It is no longer in the project graph.".into(), rebindable: false },
    };
    match (reviewed, now) {
        // Unlocatable is reported as such whether or not a digest exists:
        // it is never taken for unchanged.
        (_, StatementNow::Unlocated { why, rebindable }) => {
            LinkReview { state: LinkReviewState::StatementMissing, reason: Some(unlocated_reason(&why, rebindable)) }
        }
        (Some(recorded), StatementNow::Located(current)) if recorded == current => {
            LinkReview { state: LinkReviewState::Current, reason: None }
        }
        (Some(_), StatementNow::Located(_)) => LinkReview {
            state: LinkReviewState::StatementChanged,
            reason: Some(
                "The statement changed since this link was reviewed. Confirm the link to record the current \
                 statement, or reject it."
                    .into(),
            ),
        },
        (None, StatementNow::Located(_)) => LinkReview {
            state: LinkReviewState::Unrecorded,
            reason: Some(
                "The statement text this review covered was not recorded, so whether the statement changed since \
                 is unknown. Confirm the link to record the current statement."
                    .into(),
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use exposition_core::path::WorkspacePath;

    use super::*;
    use crate::ids::EntityId;
    use crate::model::{Origin, SourceVersion};
    use crate::vocabulary::EntityKind;

    fn entity(label: &str) -> Entity {
        Entity {
            id: EntityId::mint(),
            kind: EntityKind::Theorem,
            roles: Vec::new(),
            label: label.into(),
            key: None,
            lifecycle: Lifecycle::Active,
            tombstone_reason: None,
            revision: 1,
            origin: Origin::User,
            attributes: BTreeMap::new(),
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    fn binding(e: &Entity, path: &str, selector: SourceSelector, fingerprint: Option<&str>) -> SourceBinding {
        SourceBinding {
            entity: e.id.clone(),
            path: WorkspacePath::parse(path).unwrap(),
            selector,
            range: None,
            version: Some(SourceVersion { content_hash: "h1".into(), revision: None, saved: true }),
            fingerprint: fingerprint.map(str::to_owned),
            state: BindingState::Resolved,
            detail: None,
        }
    }

    fn digest(e: &Entity, b: &[SourceBinding]) -> String {
        statement_now(e, b).digest().expect("located")
    }

    #[test]
    fn a_region_is_identified_by_its_text_not_by_its_file_or_place() {
        let e = entity("T");
        let a = binding(&e, "main.typ", SourceSelector::Region, Some("region:v1:5:00:aa"));
        let mut moved = binding(&e, "chapters/one.typ", SourceSelector::Region, Some("region:v1:5:00:aa"));
        moved.version = Some(SourceVersion { content_hash: "h2".into(), revision: None, saved: true });
        assert_eq!(digest(&e, std::slice::from_ref(&a)), digest(&e, &[moved]));
        let other = binding(&e, "main.typ", SourceSelector::Region, Some("region:v1:5:00:bb"));
        assert_ne!(digest(&e, &[a]), digest(&e, &[other]));
    }

    #[test]
    fn a_declaration_is_identified_by_its_body_fingerprint_only() {
        let e = entity("T");
        let sel = || SourceSelector::Declaration { syntax: "latex.env:theorem".into(), name: None, ordinal: Some(0) };
        let a = binding(&e, "a.tex", sel(), Some("decl:v1:ctx1:body1:scope1"));
        let context_changed = binding(&e, "a.tex", sel(), Some("decl:v1:ctx2:body1:scope2"));
        let body_changed = binding(&e, "a.tex", sel(), Some("decl:v1:ctx1:body2:scope1"));
        assert_eq!(digest(&e, std::slice::from_ref(&a)), digest(&e, &[context_changed]));
        assert_ne!(digest(&e, &[a]), digest(&e, &[body_changed]));
    }

    #[test]
    fn an_unresolved_binding_or_a_tombstoned_entity_is_unlocated_with_the_reason() {
        let mut e = entity("T");
        let mut b = binding(&e, "main.typ", SourceSelector::Region, Some("region:v1:5:00:aa"));
        b.state = BindingState::Missing;
        b.detail = Some("The bound text was changed or removed.".into());
        assert_eq!(
            statement_now(&e, &[b]),
            StatementNow::Unlocated {
                why: "main.typ: The bound text was changed or removed.".into(),
                rebindable: true
            }
        );
        let first_sight = binding(&e, "main.typ", SourceSelector::Region, None);
        assert!(matches!(statement_now(&e, &[first_sight]), StatementNow::Unlocated { rebindable: true, .. }));
        e.lifecycle = Lifecycle::Tombstoned;
        assert!(matches!(statement_now(&e, &[]), StatementNow::Unlocated { rebindable: false, .. }));
    }

    #[test]
    fn an_unbound_entity_is_its_label() {
        let a = entity("f is continuous");
        let mut b = entity("f  is\ncontinuous");
        assert_eq!(digest(&a, &[]), digest(&b, &[]), "whitespace-normalized like declarations");
        b.label = "f is differentiable".into();
        assert_ne!(digest(&a, &[]), digest(&b, &[]));
    }

    #[test]
    fn standing_never_reads_unknown_or_unlocated_as_current() {
        let e = entity("T");
        let b = binding(&e, "main.typ", SourceSelector::Region, Some("region:v1:5:00:aa"));
        let one = std::slice::from_ref(&b);
        let now = digest(&e, one);
        assert_eq!(link_review(Some(&now), Some(&e), one).state, LinkReviewState::Current);
        assert_eq!(link_review(Some("statement:v1:00"), Some(&e), one).state, LinkReviewState::StatementChanged);
        let unknown = link_review(None, Some(&e), one);
        assert_eq!(unknown.state, LinkReviewState::Unrecorded);
        assert!(!unknown.needs_re_review(), "unknown is stated, not flagged as changed");
        let mut missing = b.clone();
        missing.state = BindingState::Missing;
        for reviewed in [Some(now.as_str()), None] {
            let r = link_review(reviewed, Some(&e), std::slice::from_ref(&missing));
            assert_eq!(r.state, LinkReviewState::StatementMissing);
            assert!(r.needs_re_review());
            let reason = r.reason.unwrap();
            assert!(reason.contains("Rebind the statement to its current text, then confirm the link."), "{reason}");
        }
        let gone = link_review(Some(&now), None, &[]).reason.unwrap();
        assert!(gone.contains("reject it, or restore the statement") && !gone.contains("Rebind"), "{gone}");
        assert_eq!(link_review(Some(&now), None, &[]).state, LinkReviewState::StatementMissing);
    }
}
