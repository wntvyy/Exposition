// Editor-page dependency inspector (auxiliary view "Dependencies"): what the
// project graph knows at the caret of the active source editor — the entities
// bound there (innermost first), their typed relationships with mathematical
// dependencies and references kept apart, evidence and freshness, extraction
// coverage and unresolved bindings — with navigation to the Graph page.
// Answers from an unsaved buffer are labelled as such.
import { Network, Sigma } from "lucide-react";
import { useEffect, useState } from "react";

import type { Entity } from "../../bindings/Entity";
import type { InspectedEntity } from "../../bindings/InspectedEntity";
import type { Relation } from "../../bindings/Relation";
import type { SourceInspection } from "../../bindings/SourceInspection";
import { Badge, Button, EmptyState, ErrorState, LoadingState, Section, StatusPill, UnavailableState } from "../../ui/components";
import { useActiveSourcePosition } from "../editor";
import { useWorkspace } from "../workspace/store";
import { graphApi } from "./api";
import { RelationStatusBadge } from "./Inspector";
import { entityDisplay, FAMILY_LABELS, freshnessView, kindDisplay, relationDisplay, VERDICT_VIEW, type VocabularyIndex } from "./model";
import { openInGraph } from "./navigation";
import { FreshnessPill } from "./status";
import "./graph.css";
import { canRead, graph, useGraph } from "./store";
import { useGraphLoad } from "./useGraphData";
import type { RelationFamily } from "./canvas/types";

export const DEPENDENCIES_VIEW_ID = "graph.dependencies";

/** Order of relationship groups: mathematics first, references kept separate. */
const FAMILY_ORDER: RelationFamily[] = ["math", "citation", "formal", "evidence", "structure", "resource", "execution"];

function useDebounced<T>(value: T, delay: number): T {
  const [current, setCurrent] = useState(value);
  useEffect(() => {
    const t = setTimeout(() => setCurrent(value), delay);
    return () => clearTimeout(t);
  }, [value, delay]);
  return current;
}

function InspectedEntityView({ item, vocabulary, primary }: { item: InspectedEntity; vocabulary: VocabularyIndex | null; primary: boolean }) {
  const { entity, binding } = item;
  const display = entityDisplay(vocabulary, entity);
  const related = new Map(item.related.map((e) => [e.id, e]));
  const groups = new Map<RelationFamily, { relation: Relation; outgoing: boolean; other: Entity | undefined }[]>();
  for (const relation of item.relations) {
    const outgoing = relation.from === entity.id;
    const family = relationDisplay(vocabulary, relation.kind).family;
    const list = groups.get(family) ?? [];
    list.push({ relation, outgoing, other: related.get(outgoing ? relation.to : relation.from) });
    groups.set(family, list);
  }
  const mathematical = kindDisplay(vocabulary, entity.kind).mathematical;
  return (
    <article className="xp-deps-entity" data-testid="deps-entity" data-entity-id={entity.id} data-kind={entity.kind} aria-label={`${display.typeLabel} ${entity.label}`}>
      <header className="xp-deps-entity__header">
        <span className="xp-graph-inspector__type" data-category={display.category}>
          <display.icon size={14} aria-hidden /> {display.typeLabel}
        </span>
        <h3 className="xp-deps-entity__title">{entity.label}</h3>
        <div className="xp-graph-badges">
          {binding.state !== "resolved" && (
            <Badge tone="warning" dashed>
              {binding.state === "missing" ? "Binding missing" : binding.state === "ambiguous" ? "Ambiguous binding" : "Unresolved binding"}
            </Badge>
          )}
          {entity.lifecycle !== "active" && <Badge dashed>{entity.lifecycle}</Badge>}
        </div>
        {binding.detail && <p className="xp-muted">{binding.detail}</p>}
      </header>
      {[...groups.keys()].length === 0 ? (
        <p className="xp-muted">No relationships recorded.</p>
      ) : (
        FAMILY_ORDER.filter((f) => groups.has(f)).map((family) => (
          <div key={family} className="xp-graph-family" data-family={family} data-testid="deps-family">
            <h4 className="xp-graph-family__title">{FAMILY_LABELS[family]}</h4>
            <ul className="xp-graph-list">
              {groups.get(family)!.map(({ relation, outgoing, other }) => (
                <li key={`${relation.id}-${outgoing}`} className="xp-graph-list__item" data-testid="deps-relation" data-kind={relation.kind} data-status={relation.status}>
                  <div className="xp-graph-list__main">
                    <span className="xp-graph-relation__reading">
                      {outgoing ? relationDisplay(vocabulary, relation.kind).reading : relationDisplay(vocabulary, relation.kind).inverse}
                    </span>{" "}
                    {other?.label ?? "an item"}
                    {other && <span className="xp-muted"> ({entityDisplay(vocabulary, other).typeLabel})</span>}
                    <div className="xp-graph-badges">
                      <RelationStatusBadge status={relation.status} />
                    </div>
                  </div>
                </li>
              ))}
            </ul>
          </div>
        ))
      )}
      {primary && <EvidenceSummary entityId={entity.id} />}
      <div className="xp-deps-entity__actions">
        <Button size="sm" icon={Network} onPress={() => openInGraph(entity.id, "project")}>
          Open in Graph
        </Button>
        {mathematical && (
          <Button size="sm" icon={Sigma} onPress={() => openInGraph(entity.id, "mathematics")}>
            Open in Mathematics
          </Button>
        )}
      </div>
    </article>
  );
}

/** Evidence and its freshness for the innermost entity (loaded on demand). */
function EvidenceSummary({ entityId }: { entityId: string }) {
  const loaded = useGraphLoad(`deps-evidence:${entityId}`, () => graphApi().entity(entityId));
  const detail = loaded.data;
  if (!detail) return null;
  if (detail.evidence.length === 0) return <p className="xp-muted" data-testid="deps-evidence">No evidence recorded for this item.</p>;
  return (
    <ul className="xp-graph-list" data-testid="deps-evidence" aria-label="Evidence">
      {detail.evidence.map((ev) => {
        const fresh = freshnessView(detail.evidenceFreshness.find(([id]) => id === ev.id)?.[1] ?? null);
        const verdict = ev.outcome.kind === "verdict" ? VERDICT_VIEW[ev.outcome.verdict] : null;
        return (
          <li key={ev.id} className="xp-graph-list__item">
            <div className="xp-graph-badges">
              <Badge>{ev.kind === "formalCheck" ? "Formal check" : ev.kind}</Badge>
              {verdict && <StatusPill size="sm" status={verdict.status} label={verdict.label} />}
              <StatusPill size="sm" status={fresh.status} label={fresh.label} />
            </div>
          </li>
        );
      })}
    </ul>
  );
}

function CoverageNote({ inspection }: { inspection: SourceInspection }) {
  const coverage = inspection.coverage;
  if (!coverage) return <p className="xp-muted" data-testid="deps-coverage" data-state="unknown">Extraction coverage of this file is unknown; absence of results is not evidence of absence.</p>;
  const label =
    coverage.state === "complete"
      ? "Complete"
      : coverage.state === "partial"
        ? "Partial"
        : coverage.state === "pending"
          ? "Pending"
          : coverage.state === "unsupported"
            ? "Not supported"
            : "Failed";
  return (
    <p className="xp-muted" data-testid="deps-coverage" data-state={coverage.state}>
      Extraction: {label}
      {coverage.extractor ? ` (${coverage.extractor})` : ""}
      {coverage.detail ? ` — ${coverage.detail}` : ""}
      {coverage.unresolved ? `. ${coverage.unresolved} unresolved ${coverage.unresolved === 1 ? "binding" : "bindings"} in this file.` : ""}
    </p>
  );
}

export function DependencyInspector() {
  useEffect(() => graph.start(), []);
  const folder = useWorkspace((s) => s.info);
  const status = useGraph((s) => s.status);
  const statusError = useGraph((s) => s.statusError);
  const vocabulary = useGraph((s) => s.vocabulary);
  const position = useActiveSourcePosition();
  const settled = useDebounced(position, 250);
  const readable = canRead(status);
  const key = readable && settled ? `${settled.path}:${settled.offset}:${settled.revision}` : null;
  const inspection = useGraphLoad<SourceInspection>(key, () =>
    graphApi().inspectAtSource({ path: settled!.path, offset: settled!.offset, revision: settled!.revision }),
  );

  if (!folder) return <EmptyState title="No folder open" description="Open a project folder, then place the caret in a source file to see what it depends on." />;
  if (statusError) return <ErrorState title="The project graph could not be read" description={statusError} actions={<Button onPress={() => void graph.resync()}>Retry</Button>} />;
  if (status && !readable) {
    return (
      <UnavailableState
        title="Dependencies unavailable"
        description={status.state.kind === "unavailable" ? `${status.state.reason} Editing keeps working.` : "The project graph is not attached."}
      />
    );
  }
  if (!position) return <EmptyState title="No source file active" description="Place the caret in a source file to see the graph items there and how they relate." />;
  const data = inspection.data;
  const current = data && data.path === position.path;
  return (
    <div className="xp-deps" data-testid="dependency-inspector" data-path={position.path} aria-busy={inspection.loading}>
      <header className="xp-deps__header">
        <code className="xp-graph-path">{position.path}</code>
        <span className="xp-muted"> · caret at character {position.offset}</span>
        <div className="xp-graph-badges">
          {data?.unsaved && (
            <Badge tone="info" dashed title="Answered from the unsaved editor buffer, not the saved file">
              Unsaved buffer (revision {data.revision})
            </Badge>
          )}
          {data && <FreshnessPill freshness={data.freshness} />}
        </div>
      </header>
      {!data && inspection.loading && <LoadingState title="Inspecting…" />}
      {inspection.error && <ErrorState title="Could not inspect this position" description={inspection.error} actions={<Button onPress={inspection.reload}>Retry</Button>} live={false} />}
      {data && current && (
        <>
          <CoverageNote inspection={data} />
          <Section title={`At the caret (${data.entities.length})`}>
            {data.entities.length === 0 ? (
              <p className="xp-muted" data-testid="deps-none">
                No graph item is bound at this position.
              </p>
            ) : (
              data.entities.map((item, i) => <InspectedEntityView key={item.entity.id} item={item} vocabulary={vocabulary} primary={i === 0} />)
            )}
          </Section>
          {data.resource && (
            <Section title="File">
              <div className="xp-deps-file" data-testid="deps-resource" data-entity-id={data.resource.id}>
                <span>
                  {data.resource.label} <span className="xp-muted">({entityDisplay(vocabulary, data.resource).typeLabel})</span>
                </span>
                <Button size="sm" variant="ghost" icon={Network} onPress={() => openInGraph(data.resource!.id, "project")}>
                  Open in Graph
                </Button>
              </div>
            </Section>
          )}
          {!data.resource && <p className="xp-muted">This file is not a graph resource (not indexed, or excluded).</p>}
        </>
      )}
    </div>
  );
}
