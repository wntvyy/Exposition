// Formal evidence and reviewed associations for Lean declarations.
//
// Evidence comes only from the Lean service's own audit record of a finished
// revision (Rust reads it; the webview names a document and declarations).
// Associating a declaration with an informal statement records a reviewed
// correspondence (`formalizes`, accepted) — never a proof of equivalence —
// separately from the verifier's verdict. Freshness is computed in Rust
// against the current text and is never "current" (dependencies are not
// fingerprinted by the audit).
import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";

import type { DeclarationEvidence } from "../../bindings/DeclarationEvidence";
import type { DeclarationVerdict } from "../../bindings/DeclarationVerdict";
import type { DocumentId } from "../../bindings/DocumentId";
import type { EntityId } from "../../bindings/EntityId";
import type { EvidenceOutcomeReport } from "../../bindings/EvidenceOutcomeReport";
import type { FormalVerdict } from "../../bindings/FormalVerdict";
import type { LeanAssociateRequest } from "../../bindings/LeanAssociateRequest";
import type { LeanAssociation } from "../../bindings/LeanAssociation";
import type { LeanAuditRecord } from "../../bindings/LeanAuditRecord";
import { errorMessage } from "../../lib/ipc";
import type { Status } from "../../ui/components";

export const leanEvidenceCommands = {
  /** The audit of the last finished revision (declarations, statements, axioms, verdicts). */
  auditRecord: (document: DocumentId) => invoke<LeanAuditRecord | null>("lean_audit_record", { document }),
  /** Record FormalCheck evidence for audited declarations (unlisted names are refused). */
  recordEvidence: (document: DocumentId, declarations: string[]) =>
    invoke<EvidenceOutcomeReport>("lean_record_evidence", { document, declarations }),
  /**
   * Reviewed association `declaration formalizes target` (accepted). Also used
   * by the Mathematics view: `declaration` is `{ kind: "audited", document, name }`
   * or `{ kind: "entity", entity }`.
   */
  associate: (request: LeanAssociateRequest) => invoke<LeanAssociation>("lean_associate_declaration", { request }),
  /** Declarations of the document with evidence history and associations. */
  declarationEvidence: (document: DocumentId) =>
    invoke<DeclarationEvidence[]>("lean_declaration_evidence", { document }),
};

export function verdictView(verdict: FormalVerdict | DeclarationVerdict): { status: Status; label: string } {
  switch (verdict) {
    case "checked":
      return { status: "succeeded", label: "Checked" };
    case "checkedWithAxioms":
      return { status: "warning", label: "Checked with axioms" };
    case "incomplete":
      return { status: "warning", label: "Incomplete (sorry)" };
    case "reliesOnCompiledCode":
      return { status: "warning", label: "Relies on compiled code" };
    case "partiallyChecked":
      return { status: "warning", label: "Partially checked" };
    case "notVerified":
      return { status: "warning", label: "Not verified" };
    case "errors":
      return { status: "failed", label: "Errors" };
  }
}

interface DocEvidence {
  audit: LeanAuditRecord | null;
  declarations: DeclarationEvidence[];
  error: string | null;
}

interface EvidenceStore {
  docs: Record<string, DocEvidence>;
}

export const useLeanEvidence = create<EvidenceStore>(() => ({ docs: {} }));

const inflight = new Map<string, number>();

/** Re-read the audit record and the recorded evidence of a document. */
export async function refreshEvidence(document: DocumentId): Promise<void> {
  const seq = (inflight.get(document) ?? 0) + 1;
  inflight.set(document, seq);
  let audit: LeanAuditRecord | null = null;
  let declarations: DeclarationEvidence[] = [];
  let error: string | null = null;
  try {
    audit = await leanEvidenceCommands.auditRecord(document);
  } catch (e) {
    error = errorMessage(e);
  }
  try {
    declarations = await leanEvidenceCommands.declarationEvidence(document);
  } catch (e) {
    // The graph may be unavailable; the audit itself is still shown.
    error = error ?? errorMessage(e);
  }
  if (inflight.get(document) !== seq) return;
  useLeanEvidence.setState((s) => ({ docs: { ...s.docs, [document]: { audit, declarations, error } } }));
}

export function evidenceFor(docs: DeclarationEvidence[] | undefined, name: string): DeclarationEvidence | undefined {
  return docs?.find((d) => d.name === name);
}

export async function associate(
  document: DocumentId,
  name: string,
  target: EntityId,
  note: string | null,
): Promise<LeanAssociation> {
  const result = await leanEvidenceCommands.associate({
    declaration: { kind: "audited", document, name },
    target,
    note,
  });
  await refreshEvidence(document);
  return result;
}
