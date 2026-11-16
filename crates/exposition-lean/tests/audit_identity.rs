//! The identity of a Lean audit on formal evidence, through the public
//! evidence API and a real graph store: the Lean toolchain an audit ran
//! under is part of what identifies it.

use exposition_core::document::DocumentId;
use exposition_core::path::WorkspacePath;
use exposition_graph::{GraphConfig, GraphService};
use exposition_lean::evidence::{audit_id, record_evidence};
use exposition_lean::{
    AuditedDeclaration, DeclarationVerdict, LeanAuditFile, LeanAuditPackage, LeanAuditRecord, LeanStatus,
};

fn package(toolchain_digest: &str, manifest_digest: &str) -> LeanAuditPackage {
    LeanAuditPackage {
        folder: "Proofs".into(),
        toolchain_file: Some(LeanAuditFile {
            path: WorkspacePath::parse("Proofs/lean-toolchain").unwrap(),
            sha256: toolchain_digest.repeat(32),
        }),
        manifest: Some(LeanAuditFile {
            path: WorkspacePath::parse("Proofs/lake-manifest.json").unwrap(),
            sha256: manifest_digest.repeat(32),
        }),
        mathlib_revision: Some("56a005855ea17e0b0958f73ec801ae140b0a1679".into()),
        dependency_count: 9,
    }
}

fn record(
    toolchain: &str,
    status: LeanStatus,
    verdict: DeclarationVerdict,
    package: LeanAuditPackage,
) -> LeanAuditRecord {
    LeanAuditRecord {
        document: DocumentId("doc-1".into()),
        path: WorkspacePath::parse("Proofs/Main.lean").unwrap(),
        revision: 7,
        content_hash: "ab".repeat(32),
        toolchain: toolchain.into(),
        toolchain_version: Some(format!("Lean ({toolchain})")),
        status,
        declaration_count: 1,
        declarations: vec![AuditedDeclaration {
            name: "Main.a".into(),
            kind: "theorem".into(),
            statement: "Main.a : 1 + 1 = 2".into(),
            range: None,
            line: Some(3),
            axioms: Vec::new(),
            verdict,
        }],
        truncated: false,
        finished_at_ms: 1,
        package: Some(package),
    }
}

/// The same text at the same revision, audited again after `lean-toolchain`
/// changed, is another audit: recording it never answers "already recorded"
/// with the record of the other Lean version (here a Checked record, while
/// the audit being recorded found `sorry`).
#[test]
fn an_audit_under_another_toolchain_is_another_audit() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("project");
    std::fs::create_dir_all(root.join("Proofs")).unwrap();
    std::fs::write(root.join("Proofs/Main.lean"), "theorem a : 1 + 1 = 2 := rfl\n").unwrap();
    let graph = GraphService::new(GraphConfig { data_dir: dir.path().join("data") });
    assert!(graph.attach(&std::fs::canonicalize(&root).unwrap()).is_usable());

    let first = record(
        "leanprover/lean4:v4.35.0",
        LeanStatus::Checked {
            revision: 7,
            toolchain: "leanprover/lean4:v4.35.0".into(),
            warnings: 0,
            axioms: Vec::new(),
            declarations: Vec::new(),
            declaration_count: 0,
        },
        DeclarationVerdict::Checked,
        package("11", "aa"),
    );
    // lean-toolchain was changed; the manifest, the text, and the revision are the same.
    let second = record(
        "leanprover/lean4:v4.36.0",
        LeanStatus::CheckedWithSorry {
            revision: 7,
            toolchain: "leanprover/lean4:v4.36.0".into(),
            count: 1,
            locations: Vec::new(),
            warnings: 0,
        },
        DeclarationVerdict::Incomplete,
        package("22", "aa"),
    );
    assert_ne!(audit_id(&first), audit_id(&second));
    let a = record_evidence(&graph, &first, &["Main.a".into()], true, 1).unwrap();
    assert!(!a.recorded[0].existing);
    let b = record_evidence(&graph, &second, &["Main.a".into()], true, 2).unwrap();
    assert!(
        b.recorded.first().is_some_and(|r| !r.existing && r.evidence != a.recorded[0].evidence),
        "the audit under the other toolchain (verdict: depends on sorry) was answered with the Checked record of \
         the earlier toolchain: {:?} refused: {:?}",
        b.recorded,
        b.refused
    );
    // Each audit recorded again is its own record.
    let again = record_evidence(&graph, &first, &["Main.a".into()], true, 3).unwrap();
    assert!(again.recorded[0].existing && again.recorded[0].evidence == a.recorded[0].evidence);
    let again = record_evidence(&graph, &second, &["Main.a".into()], true, 4).unwrap();
    assert!(again.recorded[0].existing && again.recorded[0].evidence == b.recorded[0].evidence);
}
