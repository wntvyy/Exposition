// Editor-page dependency inspector (auxiliary view "Dependencies"): what the
// project graph knows at the caret of the active source editor — the entities
// bound there (innermost first), their typed relationships with mathematical
// dependencies and references kept apart, evidence and freshness, extraction
// coverage and unresolved bindings — with navigation to the Graph page.
// Answers from an unsaved buffer are labelled as such. Region states render
// compact (the shell's auxiliary region).
import { Network, Sigma } from "lucide-react";
import { useEffect, useState } from "react";

import type { Entity } from "../../bindings/Entity";
import type { InspectedEntity } from "../../bindings/InspectedEntity";
import type { Relation } from "../../bindings/Relation";
import type { SourceInspection } from "../../bindings/SourceInspection";
import { Badge, Button, EmptyState, ErrorState, LoadingState, Section, StatusText, UnavailableState } from "../../ui/components";
import { useActiveSourcePosition } from "../editor";
import { useWorkspace } from "../workspace/store";
import { graphApi } from "./api";
import { evidenceKindLabel, OutcomeText, RelationStatus, TypeLabel } from "./Inspector";
import { entityDisplay, FAMILY_LABELS, FAMILY_ORDER, freshnessView, kindDisplay, relationDisplay, type VocabularyIndex } from "./model";
import { openInGraph } from "./navigation";
import { FreshnessPill } from "./status";
import "./graph.css";
import { canRead, graph, useGraph } from "./store";
import { useGraphLoad } from "./useGraphData";
import type { RelationFamily } from "./canvas/types";

export const DEPENDENCIES_VIEW_ID = "graph.dependencies";

const LIFECYCLE_LABEL: Record<Exclude<Entity["lifecycle"], "active">, string> = {
  tombstoned: "Removed from source",
  archived: "Archived",
  discarded: "Discarded",
};

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
        <TypeLabel category={display.category} icon={display.icon} label={display.typeLabel} />
        <h3 className="xp-deps-entity__title">{entity.label}</h3>
        {(binding.state !== "resolved" || entity.lifecycle !== "active") && (
          <div className="xp-graph-inline">
            {binding.state !== "resolved" && (
              <Badge tone="warning" dashed>
                {binding.state === "missing" ? "Binding missing" : binding.state === "ambiguous" ? "Ambiguous binding" : "Unresolved binding"}
              </Badge>
            )}
            {entity.lifecycle !== "active" && <Badge dashed>{LIFECYCLE_LABEL[entity.lifecycle]}</Badge>}
          </div>
        )}
        {binding.detail && <p className="ui-meta">{binding.detail}</p>}
      </header>
      {[...groups.keys()].length === 0 ? (
        <p className="ui-muted">No relationships recorded.</p>
      ) : (
        FAMILY_ORDER.filter((f) => groups.has(f)).map((family) => (
          <div key={family} className="xp-graph-group" data-family={family} data-testid="deps-family">
            <h4 className="ui-group-label">{FAMILY_LABELS[family]}</h4>
            <ul className="xp-graph-rows">
              {groups.get(family)!.map(({ relation, outgoing, other }) => (
                <li key={`${relation.id}-${outgoing}`} className="xp-graph-row" data-testid="deps-relation" data-kind={relation.kind} data-status={relation.status}>
                  <p className="xp-graph-row__line xp-graph-row__main">
                    {outgoing ? relationDisplay(vocabulary, relation.kind).reading : relationDisplay(vocabulary, relation.kind).inverse} {other?.label ?? "an item"}
                    {other && <span className="ui-muted"> ({entityDisplay(vocabulary, other).typeLabel})</span>} <RelationStatus status={relation.status} />
                  </p>
                </li>
              ))}
            </ul>
          </div>
        ))
      )}
      {primary && <EvidenceSummary entityId={entity.id} />}
      <div className="xp-graph-inline">
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
  if (detail.evidence.length === 0) return <p className="ui-muted" data-testid="deps-evidence">No evidence recorded for this item.</p>;
  return (
    <div className="xp-graph-group">
      <h4 className="ui-group-label">Evidence</h4>
      <ul className="xp-graph-rows" data-testid="deps-evidence" aria-label="Evidence">
        {detail.evidence.map((ev) => {
          const fresh = freshnessView(detail.evidenceFreshness.find(([id]) => id === ev.id)?.[1] ?? null);
          return (
            <li key={ev.id} className="xp-graph-row">
              <p className="xp-graph-row__line xp-graph-inline">
                <span>{evidenceKindLabel(ev.kind)}</span>
                {ev.outcome.kind === "verdict" && <OutcomeText outcome={ev.outcome} />}
                <StatusText status={fresh.status} label={fresh.label} />
              </p>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

function CoverageNote({ inspection }: { inspection: SourceInspection }) {
  const coverage = inspection.coverage;
  if (!coverage) {
    return (
      <p className="ui-meta xp-deps__note" data-testid="deps-coverage" data-state="unknown">
        Extraction coverage of this file is unknown; absence of results is not evidence of absence.
      </p>
    );
  }
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
    <p className="ui-meta xp-deps__note" data-testid="deps-coverage" data-state={coverage.state}>
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
  if (statusError) return <ErrorState title="The project graph could not be read" description={statusError} actions={<Button size="sm" onPress={() => void graph.resync()}>Retry</Button>} />;
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
        <p className="xp-deps__where">
          <span className="ui-code">{position.path}</span> <span className="ui-muted">· line {position.line}</span>
        </p>
        {data && (
          <div className="xp-graph-inline">
            {data.unsaved && (
              <Badge tone="info" dashed title="Answered from the unsaved editor buffer, not the saved file">
                Unsaved buffer (revision {data.revision})
              </Badge>
            )}
            <FreshnessPill freshness={data.freshness} size="sm" />
          </div>
        )}
      </header>
      {!data && inspection.loading && <LoadingState title="Inspecting…" />}
      {inspection.error && (
        <ErrorState title="Could not inspect this position" description={inspection.error} actions={<Button size="sm" onPress={inspection.reload}>Retry</Button>} live={false} />
      )}
      {data && current && (
        <>
          <CoverageNote inspection={data} />
          <Section title="At the caret" count={data.entities.length}>
            {data.entities.length === 0 ? (
              <p className="ui-muted" data-testid="deps-none">
                No graph item is bound at this position.
              </p>
            ) : (
              data.entities.map((item, i) => <InspectedEntityView key={item.entity.id} item={item} vocabulary={vocabulary} primary={i === 0} />)
            )}
          </Section>
          {data.resource ? (
            <Section title="File">
              <div className="xp-deps-file" data-testid="deps-resource" data-entity-id={data.resource.id}>
                <span>
                  {data.resource.label} <span className="ui-muted">({entityDisplay(vocabulary, data.resource).typeLabel})</span>
                </span>
                <Button size="sm" variant="ghost" icon={Network} onPress={() => openInGraph(data.resource!.id, "project")}>
                  Open in Graph
                </Button>
              </div>
            </Section>
          ) : (
            <p className="ui-meta xp-deps__note">This file is not a graph resource (not indexed, or excluded).</p>
          )}
        </>
      )}
    </div>
  );
}
