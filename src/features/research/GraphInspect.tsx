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
import { evidenceKindLabel, freshnessView, READINGS } from "../graph/model";
import { RelationStatus } from "../graph/RelationStatus";
import { verdictView } from "../lean/evidence";
import { Banner, Section, Spinner, StatusText, type Status } from "../../ui/components";

/**
 * Freshness of one evidence record as a quiet status, in the Graph's words
 * and tones (`freshnessView`, standalone form: Current, Stale, Freshness
 * unknown; never green). A record
 * whose freshness was not part of the answer says so; that is not "No
 * evidence".
 */
export function freshnessStatus(f: Freshness | undefined): { status: Status; label: string; detail?: string } {
  if (!f) return { status: "idle", label: "Freshness not computed" };
  const view = freshnessView(f);
  switch (f.kind) {
    case "current":
      return { status: view.status, label: view.standalone };
    case "stale":
      return { status: view.status, label: view.standalone, detail: f.reasons[0] };
    case "unknown":
      return { status: view.status, label: view.standalone, detail: f.reason };
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
      return `verdict: ${verdictView(e.outcome.verdict).label}`;
    case "reported":
      return e.outcome.summary;
  }
}

/** One evidence record: what it pins (inputs, outputs, environment) and its applicability. */
export function EvidenceRecord({ evidence, freshness }: { evidence: Evidence; freshness?: Freshness }) {
  const fresh = freshnessStatus(freshness);
  return (
    <li className="exp-graph__evidence" data-testid="graph-evidence" data-kind={evidence.kind} data-freshness={freshness?.kind ?? "none"}>
      <div className="exp-graph__evidence-head">
        <StatusText status={fresh.status} label={fresh.label} />
        <span>
          {evidenceKindLabel(evidence.kind)} ·{" "}
          {evidence.operation.kind} <code className="ui-code">{evidence.operation.id}</code> · {outcomeText(evidence)}
        </span>
      </div>
      {fresh.detail && <p className="ui-meta">{fresh.detail}</p>}
      <ul className="exp-graph__pins ui-meta">
        {evidence.inputs.map((i) => (
          <li key={`in:${i.path}`}>
            input <code className="ui-code">{i.path}</code>
            {i.revision !== null && ` · revision ${i.revision}`}
            {i.contentHash && (
              <>
                {" · "}
                <code className="ui-code" title={i.contentHash}>
                  sha256 {i.contentHash.slice(0, 12)}…
                </code>
              </>
            )}
            {i.unsaved && " · unsaved buffer (digest only)"}
          </li>
        ))}
        {evidence.outputs.map((o) => (
          <li key={`out:${o.path}`}>
            output <code className="ui-code">{o.path}</code> ·{" "}
            <code className="ui-code" title={o.contentHash}>
              sha256 {o.contentHash.slice(0, 12)}…
            </code>
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
    ? `${self} ${READINGS[relation.kind]} ${other?.label ?? otherId}`
    : `${other?.label ?? otherId} ${READINGS[relation.kind]} ${self}`;
  return (
    <li data-testid="graph-relation" data-kind={relation.kind} data-direction={outgoing ? "out" : "in"} data-status={relation.status}>
      <span className="exp-graph__relation">{text}</span> <RelationStatus status={relation.status} review={relation.review} />
      {relation.review?.reason && (
        <span className="ui-meta" data-testid="graph-link-review" data-review={relation.review.state}>
          {" "}
          · {relation.review.reason}
        </span>
      )}
      {relation.note && <span className="ui-meta"> · {relation.note}</span>}
    </li>
  );
}

/**
 * Relations, evidence, and impact of an entity as inspector sections
 * (refreshes when `revision` changes).
 */
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
      <div className="exp-graph exp-graph--state">
        <Banner tone="warning" title="Graph unavailable">
          {error}
        </Banner>
      </div>
    );
  }
  if (!detail) {
    return (
      <div className="exp-graph exp-graph--state">
        <Spinner size="sm" aria-label="Loading graph dependencies" />
      </div>
    );
  }
  const freshness = new Map(detail.evidenceFreshness.map(([id, f]) => [id, f]));
  const outgoing = detail.outgoing.filter((r) => r.status !== "rejected");
  const incoming = detail.incoming.filter((r) => r.status !== "rejected");
  return (
    <div className="exp-graph" data-testid="graph-dependencies" data-entity={entity} data-graph-revision={detail.graphRevision}>
      <Section title="Relationships" count={outgoing.length + incoming.length}>
        {outgoing.length + incoming.length === 0 ? (
          <p className="ui-meta">No relationships yet.</p>
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
      <Section title="Evidence" count={detail.evidence.length}>
        {detail.evidence.length === 0 ? (
          <p className="ui-meta">No evidence recorded.</p>
        ) : (
          <ul className="exp-graph__list">
            {detail.evidence.map((e) => (
              <EvidenceRecord key={e.id} evidence={e} freshness={freshness.get(e.id)} />
            ))}
          </ul>
        )}
      </Section>
      <Section title="Change impact" count={impact?.items.length ?? 0} collapsible defaultExpanded={false}>
        {!impact || impact.items.length === 0 ? (
          <p className="ui-meta">Nothing in the graph depends on it.</p>
        ) : (
          <ul className="exp-graph__list" data-testid="graph-impact">
            {impact.items.map((i) => (
              <li key={i.entity.id} data-testid="graph-impact-item" data-action={i.action} data-entity={i.entity.id}>
                <span className="exp-graph__name">{i.entity.label}</span> <span className="ui-meta">{i.reason}</span>
              </li>
            ))}
          </ul>
        )}
        {impact && (impact.truncated || impact.workLimitReached) && <p className="ui-meta">Impact was cut off (bounded traversal).</p>}
        {impact && impact.unknownCoverage.length > 0 && (
          <p className="ui-meta">Extraction is incomplete for {impact.unknownCoverage.join(", ")}; impact through them is unknown.</p>
        )}
      </Section>
    </div>
  );
}
