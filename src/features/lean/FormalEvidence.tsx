// Evidence and reviewed associations for audited Lean declarations, shown in
// the Lean infoview. The verifier's verdict, the reviewed informal
// correspondence, and freshness are separate fields; none implies another.
import { Link2, ShieldCheck } from "lucide-react";
import { useEffect, useState } from "react";

import { notify } from "../../app/notifications";
import type { AuditedDeclaration } from "../../bindings/AuditedDeclaration";
import type { DeclarationEvidence } from "../../bindings/DeclarationEvidence";
import type { DeclarationVerdict } from "../../bindings/DeclarationVerdict";
import type { FormalVerdict } from "../../bindings/FormalVerdict";
import type { Entity } from "../../bindings/Entity";
import type { EntityKind } from "../../bindings/EntityKind";
import type { LeanAuditRecord } from "../../bindings/LeanAuditRecord";
import type { LeanDocumentState } from "../../bindings/LeanDocumentState";
import { errorMessage } from "../../lib/ipc";
import { graphCommands } from "../../lib/graphIpc";
import { Badge, Banner, Button, Dialog, Section, Select, Spinner, StatusText, TextField } from "../../ui/components";
import { freshnessStatus } from "../research/GraphInspect";
import {
  associate,
  evidenceFor,
  isRecordOfAudit,
  leanEvidenceCommands,
  refreshEvidence,
  useLeanEvidence,
  verdictTone,
  verdictView,
} from "./evidence";
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

/** How an association reads on its declaration: a suggestion, a reviewed correspondence, or one whose review no longer stands. */
function associationStanding(a: { status: string; review: { state: string } | null }): string {
  if (a.status !== "accepted") return a.status;
  switch (a.review?.state) {
    case "statementChanged":
    case "statementMissing":
      return "reviewed correspondence needs re-review";
    case "unrecorded":
      return "reviewed correspondence, reviewed statement not recorded";
    default:
      return "reviewed correspondence";
  }
}

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

/** The verifier's exact verdict as a toned badge (never color alone: the label is the verdict). */
export function VerdictBadge({ verdict }: { verdict: FormalVerdict | DeclarationVerdict }) {
  return <Badge tone={verdictTone(verdict)}>{verdictView(verdict).label}</Badge>;
}

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
  // A stale record's reason is shown once, in the history line below.
  const fresh = latest ? freshnessStatus(latest.freshness) : null;
  // The package context the record was checked in (Lake packages only).
  const environment = new Map(latest?.evidence.environment ?? []);
  const manifestDigest = environment.get("lake-manifest.json sha256") ?? null;
  const mathlibRevision = environment.get("mathlib revision") ?? null;
  const recordedForAudit = Boolean(audit && latest && isRecordOfAudit(latest.evidence, audit));
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
          <VerdictBadge verdict={formal.declarationVerdict ?? formal.fileVerdict} />
          <span className="ui-meta">
            evidence of revision {formal.revision ?? "?"} · file: {verdictView(formal.fileVerdict).label} · {formal.toolchain}
          </span>
          {fresh && (
            <StatusText
              status={fresh.status}
              label={fresh.label}
              detail={latest.freshness.kind === "stale" ? undefined : fresh.detail}
              wrap
            />
          )}
          {evidence && evidence.evidence.length > 1 && <span className="ui-meta">{evidence.evidence.length - 1} older</span>}
          <span className="ui-meta">dependencies: {formal.dependencies.kind === "unavailable" ? "not reported by the audit" : formal.dependencies.names.join(", ")}</span>
          {manifestDigest && (
            <span className="ui-meta" data-testid="lean-evidence-package" title={`lake-manifest.json sha256 ${manifestDigest}${mathlibRevision ? `, Mathlib ${mathlibRevision}` : ""}`}>
              {[mathlibRevision ? `Mathlib ${mathlibRevision.slice(0, 12)}` : null, `lake-manifest.json ${manifestDigest.slice(0, 12)}`].filter(Boolean).join(" · ")}
            </span>
          )}
        </div>
      ) : (
        <span className="ui-meta">No evidence recorded in the project graph.</span>
      )}
      {latest?.freshness.kind === "stale" && (
        <p className="ui-meta" data-testid="lean-evidence-history">
          Historical: {latest.freshness.reasons[0]}
        </p>
      )}
      {evidence?.associations.map((a) => (
        <p
          key={a.relation}
          className="exp-lean__association"
          data-testid="lean-decl-association"
          data-target={a.target.id}
          data-status={a.status}
          data-review={a.review?.state}
        >
          <Link2 size={12} aria-hidden /> Formalizes {KIND_TITLE[a.target.kind] ?? a.target.kind} “{a.target.label}” ·{" "}
          {associationStanding(a)} (not a proof of equivalence)
          {a.review?.reason && <span className="ui-meta"> {a.review.reason}</span>}
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
          Records that you reviewed how the formal declaration <code>{name}</code> corresponds to an informal statement. It is
          not a machine proof that they are equivalent, and it does not change Lean&rsquo;s verdict.
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
    <Section title="Audited declarations" count={audit.declarationCount}>
      <div data-testid="lean-audited-declarations" data-revision={audit.revision}>
        <p className="ui-meta">
          What the axiom audit reported for revision {audit.revision}
          {audit.revision !== state.revision ? ` (the file is now at revision ${state.revision})` : ""}. Each declaration&rsquo;s own
          verdict comes from the axioms it depends on; the file&rsquo;s verdict still applies to the whole file.
        </p>
        <ul className="exp-lean__list">
          {written.map((d) => (
            <AuditedRow key={d.name} state={state} d={d} audit={audit} evidence={evidenceFor(evidence, d.name)} />
          ))}
        </ul>
        {audit.truncated && <p className="ui-meta">More declarations were audited than are listed; those have no evidence of their own.</p>}
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
        <VerdictBadge verdict={d.verdict} />
        <span className="ui-meta">{d.kind}</span>
        {d.line != null && <span className="ui-meta">Ln {d.line + 1}</span>}
        <code className="ui-code">{d.name}</code>
      </button>
      <pre className="exp-lean__message">{d.statement}</pre>
      <p className="exp-lean__decl-axioms ui-meta">{d.axioms.length ? `Axioms: ${d.axioms.join(", ")}` : "No axioms"}</p>
      <DeclarationEvidenceControls state={state} name={d.name} audit={audit} evidence={evidence} />
    </li>
  );
}
