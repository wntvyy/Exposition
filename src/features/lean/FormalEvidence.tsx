// Evidence and reviewed associations for audited Lean declarations, shown in
// the Lean infoview. The verifier's verdict, the reviewed informal
// correspondence, and freshness are separate fields; none implies another.
import { Link2, ShieldCheck } from "lucide-react";
import { useEffect, useState } from "react";

import { notify } from "../../app/notifications";
import type { AuditedDeclaration } from "../../bindings/AuditedDeclaration";
import type { DeclarationEvidence } from "../../bindings/DeclarationEvidence";
import type { Entity } from "../../bindings/Entity";
import type { EntityKind } from "../../bindings/EntityKind";
import type { LeanAuditRecord } from "../../bindings/LeanAuditRecord";
import type { LeanDocumentState } from "../../bindings/LeanDocumentState";
import { errorMessage } from "../../lib/ipc";
import { graphCommands } from "../../lib/graphIpc";
import { Banner, Button, Dialog, Section, Select, Spinner, StatusPill, TextField } from "../../ui/components";
import { freshnessPill } from "../research/GraphInspect";
import { associate, evidenceFor, leanEvidenceCommands, refreshEvidence, useLeanEvidence, verdictView } from "./evidence";
import { revealInEditor } from "./leanMonaco";

const TARGET_KINDS: EntityKind[] = [
  "theorem",
  "lemma",
  "proposition",
  "corollary",
  "claim",
  "conjecture",
  "definition",
  "assumption",
  "mathObject",
];

const KIND_TITLE: Record<string, string> = {
  theorem: "Theorem",
  lemma: "Lemma",
  proposition: "Proposition",
  corollary: "Corollary",
  claim: "Claim",
  conjecture: "Conjecture",
  definition: "Definition",
  assumption: "Assumption",
  mathObject: "Mathematical object",
};

/** Follow a document's audit record and evidence (refreshes when its status changes). */
export function useDeclarationEvidence(state: LeanDocumentState) {
  const docs = useLeanEvidence((s) => s.docs[state.document]);
  const statusKey = `${state.status.kind}:${"revision" in state.status ? state.status.revision : ""}`;
  useEffect(() => {
    void refreshEvidence(state.document);
  }, [state.document, statusKey]);
  return docs;
}

/** Evidence summary and actions for one declaration. */
export function DeclarationEvidenceControls({
  state,
  name,
  audit,
  evidence,
}: {
  state: LeanDocumentState;
  name: string;
  audit: LeanAuditRecord | null;
  evidence: DeclarationEvidence | undefined;
}) {
  const [busy, setBusy] = useState(false);
  const [associating, setAssociating] = useState(false);
  const audited = Boolean(audit?.declarations.some((d) => d.name === name));
  const latest = evidence?.evidence[0];
  const formal = latest?.evidence.formal;
  const recordedForAudit =
    Boolean(audit && formal && formal.sourceDigest === audit.contentHash && formal.revision === audit.revision);
  const record = async () => {
    setBusy(true);
    try {
      const report = await leanEvidenceCommands.recordEvidence(state.document, [name]);
      if (report.refused.length > 0) notify({ tone: "warning", title: `No evidence for ${name}`, body: report.refused[0]?.reason });
      await refreshEvidence(state.document);
    } catch (e) {
      notify({ tone: "danger", title: "Could not record evidence", body: errorMessage(e) });
    }
    setBusy(false);
  };
  return (
    <div className="exp-lean__evidence" data-testid="lean-decl-evidence" data-name={name} data-recorded={latest ? "true" : "false"}>
      {latest && formal ? (
        <div className="exp-lean__evidence-latest" data-testid="lean-evidence-latest" data-freshness={latest.freshness.kind}>
          <StatusPill size="sm" {...verdictView(formal.declarationVerdict ?? formal.fileVerdict)} />
          <span className="exp-muted">
            evidence of revision {formal.revision ?? "?"} · file {verdictView(formal.fileVerdict).label.toLowerCase()} ·{" "}
            {formal.toolchain}
          </span>
          <StatusPill size="sm" {...freshnessPill(latest.freshness)} />
          {evidence && evidence.evidence.length > 1 && <span className="exp-muted">{evidence.evidence.length - 1} older</span>}
          <span className="exp-muted">dependencies: {formal.dependencies.kind === "unavailable" ? "not reported by the audit" : formal.dependencies.names.join(", ")}</span>
        </div>
      ) : (
        <span className="exp-muted">No evidence recorded in the project graph.</span>
      )}
      {latest?.freshness.kind === "stale" && (
        <p className="exp-muted" data-testid="lean-evidence-history">
          Historical: {latest.freshness.reasons[0]}
        </p>
      )}
      {evidence?.associations.map((a) => (
        <p key={a.relation} className="exp-lean__association" data-testid="lean-decl-association" data-target={a.target.id} data-status={a.status}>
          <Link2 size={12} aria-hidden /> Formalizes {KIND_TITLE[a.target.kind] ?? a.target.kind} “{a.target.label}” ·{" "}
          {a.status === "accepted" ? "reviewed correspondence" : a.status} (not a proof of equivalence)
        </p>
      ))}
      <div className="exp-lean__evidence-actions">
        {audited && !recordedForAudit && (
          <Button size="sm" variant="ghost" icon={ShieldCheck} isPending={busy} onPress={() => void record()}>
            Record evidence
          </Button>
        )}
        <Button size="sm" variant="ghost" icon={Link2} isDisabled={!audited && !evidence} onPress={() => setAssociating(true)}>
          Associate with…
        </Button>
      </div>
      {associating && (
        <AssociateDialog state={state} name={name} audited={audited} onClose={() => setAssociating(false)} />
      )}
    </div>
  );
}

function AssociateDialog({
  state,
  name,
  audited,
  onClose,
}: {
  state: LeanDocumentState;
  name: string;
  audited: boolean;
  onClose: () => void;
}) {
  const [candidates, setCandidates] = useState<Entity[] | null>(null);
  const [graphRevision, setGraphRevision] = useState(0);
  const [target, setTarget] = useState<string | null>(null);
  const [newClaim, setNewClaim] = useState("");
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const evidence = useLeanEvidence((s) => evidenceFor(s.docs[state.document]?.declarations, name));

  const load = async () => {
    try {
      const snapshot = await graphCommands.snapshot({ kinds: TARGET_KINDS, text: null, around: null, includeInactive: false, limit: 500 });
      setCandidates(snapshot.entities);
      setGraphRevision(snapshot.revision);
      setTarget((t) => t ?? snapshot.entities[0]?.id ?? null);
    } catch (e) {
      setCandidates([]);
      setError(errorMessage(e));
    }
  };
  useEffect(() => {
    void load();
  }, []);

  const createClaim = async () => {
    setError(null);
    try {
      const proposal = await graphCommands.proposeChange({
        baseRevision: graphRevision,
        operations: [
          { op: "createEntity", tempId: "claim", kind: "claim", roles: [], label: newClaim.trim(), key: null, attributes: {}, binding: null },
        ],
        summary: "Create a claim to associate with a formal declaration",
      });
      if (!proposal.valid) throw new Error(proposal.diagnostics[0]?.message ?? "the claim is invalid");
      const outcome = await graphCommands.applyChange({ proposal: proposal.id, expectedRevision: graphRevision });
      if (outcome.kind !== "applied") throw new Error(outcome.diagnostics[0]?.message ?? "the claim was not created");
      const id = outcome.created[0]?.id ?? null;
      setNewClaim("");
      await load();
      setTarget(id);
    } catch (e) {
      setError(errorMessage(e));
    }
  };

  const submit = async () => {
    if (!target) return;
    setBusy(true);
    setError(null);
    try {
      const result = audited
        ? await associate(state.document, name, target, note.trim() ? note.trim() : null)
        : await leanEvidenceCommands.associate({
            declaration: { kind: "entity", entity: evidence?.entity ?? "" },
            target,
            note: note.trim() ? note.trim() : null,
          });
      await refreshEvidence(state.document);
      const label = candidates?.find((c) => c.id === target)?.label ?? "the statement";
      notify({
        tone: "success",
        title: `${name} associated with “${label}”`,
        body: result.association.existing
          ? "The reviewed association already existed."
          : "Recorded as a reviewed correspondence (not a proof of equivalence).",
      });
      onClose();
    } catch (e) {
      setError(errorMessage(e));
    }
    setBusy(false);
  };

  return (
    <Dialog
      title={`Associate ${name}`}
      isOpen
      onOpenChange={(open) => !open && onClose()}
      size="lg"
      footer={
        <>
          <Button variant="secondary" onPress={onClose}>
            Cancel
          </Button>
          <Button variant="primary" isPending={busy} isDisabled={!target} onPress={() => void submit()}>
            Record reviewed association
          </Button>
        </>
      }
    >
      <div data-testid="lean-associate-dialog" data-name={name}>
        <p>
          Record that you reviewed how the formal declaration <code>{name}</code> corresponds to an informal statement. This is a
          correspondence assertion, not a machine proof that the two say the same thing; Lean&rsquo;s verdict stays a separate record.
        </p>
        {error && (
          <Banner tone="danger" title="Not recorded">
            {error}
          </Banner>
        )}
        {candidates === null ? (
          <Spinner size="sm" aria-label="Loading statements" />
        ) : (
          <Select<string>
            label="Informal statement"
            placeholder={candidates.length ? "Choose a statement" : "No statements in the project graph yet"}
            options={candidates.map((c) => ({ id: c.id, label: `${KIND_TITLE[c.kind] ?? c.kind}: ${c.label}` }))}
            value={target}
            onChange={setTarget}
          />
        )}
        <div className="exp-lean__new-claim">
          <TextField label="Or create a claim" value={newClaim} onChange={setNewClaim} placeholder="e.g. Gauss sum formula" />
          <Button size="sm" isDisabled={!newClaim.trim()} onPress={() => void createClaim()}>
            Create claim
          </Button>
        </div>
        <TextField label="Note (optional)" value={note} onChange={setNote} placeholder="Why the statements correspond" />
      </div>
    </Dialog>
  );
}

/** Declarations of a non-Checked audited revision (Checked results list theirs in CheckedDeclarations). */
export function AuditedDeclarations({ state, audit, evidence }: { state: LeanDocumentState; audit: LeanAuditRecord; evidence: DeclarationEvidence[] | undefined }) {
  const written = audit.declarations.filter((d) => d.range);
  return (
    <Section title={`Audited declarations (${audit.declarationCount})`}>
      <div data-testid="lean-audited-declarations" data-revision={audit.revision}>
        <p className="exp-muted exp-lean__note">
          What the axiom audit reported for revision {audit.revision}
          {audit.revision !== state.revision ? ` (the file is now at revision ${state.revision})` : ""}. Each declaration&rsquo;s own
          verdict comes from the axioms it depends on; the file&rsquo;s verdict still applies to the whole file.
        </p>
        <ul className="exp-lean__list">
          {written.map((d) => (
            <AuditedRow key={d.name} state={state} d={d} audit={audit} evidence={evidenceFor(evidence, d.name)} />
          ))}
        </ul>
        {audit.truncated && <p className="exp-muted">More declarations were audited than are listed; those have no evidence of their own.</p>}
      </div>
    </Section>
  );
}

function AuditedRow({
  state,
  d,
  audit,
  evidence,
}: {
  state: LeanDocumentState;
  d: AuditedDeclaration;
  audit: LeanAuditRecord;
  evidence: DeclarationEvidence | undefined;
}) {
  const range = d.range;
  return (
    <li className="exp-lean__decl" data-testid="lean-audited-declaration" data-name={d.name} data-verdict={d.verdict}>
      <button
        type="button"
        className="exp-lean__diag-jump"
        disabled={!range}
        onClick={() => range && void revealInEditor(state.path, { range })}
      >
        <StatusPill size="sm" {...verdictView(d.verdict)} />
        <span className="exp-lean__kind">{d.kind}</span>
        {d.line != null && <span className="exp-lean__line">Ln {d.line + 1}</span>}
        <code>{d.name}</code>
      </button>
      <pre className="exp-lean__message">{d.statement}</pre>
      <p className="exp-lean__decl-axioms exp-muted">{d.axioms.length ? `Axioms: ${d.axioms.join(", ")}` : "No axioms"}</p>
      <DeclarationEvidenceControls state={state} name={d.name} audit={audit} evidence={evidence} />
    </li>
  );
}
