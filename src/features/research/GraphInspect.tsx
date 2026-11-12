// Graph inspection of one entity: its typed relations, evidence (with
// applicability to current sources), and change impact. Reads bounded graph
// projections through `graphIpc`; never interprets relations itself.
import { useEffect, useState } from "react";

import type { EntityDetail } from "../../bindings/EntityDetail";
import type { EntityId } from "../../bindings/EntityId";
import type { Evidence } from "../../bindings/Evidence";
import type { Freshness } from "../../bindings/Freshness";
import type { ImpactReport } from "../../bindings/ImpactReport";
import type { Relation } from "../../bindings/Relation";
import { errorMessage } from "../../lib/ipc";
import { graphCommands } from "../../lib/graphIpc";
import { Banner, Section, Spinner, StatusPill, type Status } from "../../ui/components";

const READING: Record<string, string> = {
  contains: "contains",
  memberOf: "is a member of",
  anchoredIn: "is declared in",
  imports: "imports",
  usesAsset: "uses",
  dependsOn: "depends on",
  assumes: "assumes",
  proofOf: "is a proof of",
  references: "references",
  evidenceFor: "is evidence for",
  derivedFrom: "is derived from",
  formalizes: "formalizes",
};

export function freshnessPill(f: Freshness | undefined): { status: Status; label: string; detail?: string } {
  if (!f) return { status: "idle", label: "Freshness not computed" };
  switch (f.kind) {
    case "current":
      return { status: "succeeded", label: "Current" };
    case "stale":
      return { status: "stale", label: "Stale (history)", detail: f.reasons[0] };
    case "unknown":
      return { status: "unavailable", label: "Freshness unknown", detail: f.reason };
  }
}

function outcomeText(e: Evidence): string {
  switch (e.outcome.kind) {
    case "succeeded":
      return "run succeeded";
    case "failed":
      return `run failed: ${e.outcome.message}`;
    case "canceled":
      return "run canceled";
    case "interrupted":
      return "run interrupted";
    case "verdict":
      return `verdict: ${e.outcome.verdict}`;
    case "reported":
      return e.outcome.summary;
  }
}

/** One evidence record: what it pins (inputs, outputs, environment) and its applicability. */
export function EvidenceRecord({ evidence, freshness }: { evidence: Evidence; freshness?: Freshness }) {
  const pill = freshnessPill(freshness);
  return (
    <li className="exp-graph__evidence" data-testid="graph-evidence" data-kind={evidence.kind} data-freshness={freshness?.kind ?? "none"}>
      <div className="exp-graph__evidence-head">
        <StatusPill size="sm" status={pill.status} label={pill.label} />
        <span>
          {evidence.kind === "computational" ? "Computational" : evidence.kind === "formalCheck" ? "Formal check" : evidence.kind} ·{" "}
          {evidence.operation.kind} <code>{evidence.operation.id}</code> · {outcomeText(evidence)}
        </span>
      </div>
      {pill.detail && <p className="exp-muted">{pill.detail}</p>}
      <ul className="exp-graph__pins">
        {evidence.inputs.map((i) => (
          <li key={`in:${i.path}`}>
            input <code>{i.path}</code>
            {i.revision !== null && ` · revision ${i.revision}`}
            {i.contentHash && (
              <>
                {" · "}
                <code title={i.contentHash}>sha256 {i.contentHash.slice(0, 12)}…</code>
              </>
            )}
            {i.unsaved && " · unsaved buffer (digest only)"}
          </li>
        ))}
        {evidence.outputs.map((o) => (
          <li key={`out:${o.path}`}>
            output <code>{o.path}</code> · <code title={o.contentHash}>sha256 {o.contentHash.slice(0, 12)}…</code>
          </li>
        ))}
        {evidence.environment.length > 0 && <li>environment: {evidence.environment.map(([k, v]) => `${k} ${v}`).join(" · ")}</li>}
      </ul>
    </li>
  );
}

function RelationRow({ relation, detail, outgoing }: { relation: Relation; detail: EntityDetail; outgoing: boolean }) {
  const otherId = outgoing ? relation.to : relation.from;
  const other = detail.neighbors.find((n) => n.id === otherId);
  const self = detail.entity.label;
  const text = outgoing
    ? `${self} ${READING[relation.kind] ?? relation.kind} ${other?.label ?? otherId}`
    : `${other?.label ?? otherId} ${READING[relation.kind] ?? relation.kind} ${self}`;
  return (
    <li data-testid="graph-relation" data-kind={relation.kind} data-direction={outgoing ? "out" : "in"} data-status={relation.status}>
      <span className="exp-graph__relation">{text}</span>
      <span className="exp-graph__badge" data-status={relation.status}>
        {relation.status}
      </span>
      {relation.note && <span className="exp-muted"> · {relation.note}</span>}
    </li>
  );
}

/** Relations, evidence, and impact of an entity (refreshes when `revision` changes). */
export function GraphDependencies({ entity, revision }: { entity: EntityId; revision?: number | null }) {
  const [detail, setDetail] = useState<EntityDetail | null>(null);
  const [impact, setImpact] = useState<ImpactReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    setError(null);
    Promise.all([graphCommands.entity(entity), graphCommands.impact({ entities: [entity], maxDepth: 4, maxItems: 100 })]).then(
      ([d, i]) => {
        if (!live) return;
        setDetail(d);
        setImpact(i);
      },
      (e: unknown) => live && setError(errorMessage(e)),
    );
    return () => {
      live = false;
    };
  }, [entity, revision]);
  if (error) {
    return (
      <Banner tone="warning" title="Graph unavailable">
        {error}
      </Banner>
    );
  }
  if (!detail) return <Spinner size="sm" aria-label="Loading graph dependencies" />;
  const freshness = new Map(detail.evidenceFreshness.map(([id, f]) => [id, f]));
  const outgoing = detail.outgoing.filter((r) => r.status !== "rejected");
  const incoming = detail.incoming.filter((r) => r.status !== "rejected");
  return (
    <div className="exp-graph" data-testid="graph-dependencies" data-entity={entity} data-graph-revision={detail.graphRevision}>
      <Section title={`Relations (${outgoing.length + incoming.length})`}>
        {outgoing.length + incoming.length === 0 ? (
          <p className="exp-muted">No relations yet.</p>
        ) : (
          <ul className="exp-graph__list">
            {outgoing.map((r) => (
              <RelationRow key={r.id} relation={r} detail={detail} outgoing />
            ))}
            {incoming.map((r) => (
              <RelationRow key={r.id} relation={r} detail={detail} outgoing={false} />
            ))}
          </ul>
        )}
      </Section>
      <Section title={`Evidence (${detail.evidence.length})`}>
        {detail.evidence.length === 0 ? (
          <p className="exp-muted">No evidence recorded.</p>
        ) : (
          <ul className="exp-graph__list">
            {detail.evidence.map((e) => (
              <EvidenceRecord key={e.id} evidence={e} freshness={freshness.get(e.id)} />
            ))}
          </ul>
        )}
      </Section>
      <Section title={`If this changes (${impact?.items.length ?? 0})`} collapsible defaultExpanded={false}>
        {!impact || impact.items.length === 0 ? (
          <p className="exp-muted">Nothing in the graph depends on it.</p>
        ) : (
          <ul className="exp-graph__list" data-testid="graph-impact">
            {impact.items.map((i) => (
              <li key={i.entity.id} data-testid="graph-impact-item" data-action={i.action} data-entity={i.entity.id}>
                <strong>{i.entity.label}</strong> <span className="exp-muted">{i.reason}</span>
              </li>
            ))}
          </ul>
        )}
        {impact && (impact.truncated || impact.workLimitReached) && <p className="exp-muted">Impact was cut off (bounded traversal).</p>}
        {impact && impact.unknownCoverage.length > 0 && (
          <p className="exp-muted">Extraction is incomplete for {impact.unknownCoverage.join(", ")}; impact through them is unknown.</p>
        )}
      </Section>
    </div>
  );
}
