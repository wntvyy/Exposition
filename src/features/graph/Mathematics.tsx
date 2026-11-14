// Mathematics subsection of the Graph page: the same canonical entities,
// restricted to mathematics (definitions, assumptions, statements, proofs,
// questions, formal declarations), as a dependency canvas and a verification
// table. Informal proof/review state, formal association, the exact verifier
// verdict, and freshness are separate columns; stale or unknown is never
// shown as success, and a reviewed association is a correspondence, not a
// machine proof of equivalence.
import { FileSearch, LayoutGrid, Plus, Table2, Waypoints } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";

import type { Entity } from "../../bindings/Entity";
import type { EntityDetail } from "../../bindings/EntityDetail";
import type { GraphSnapshot } from "../../bindings/GraphSnapshot";
import type { MathOverview } from "../../bindings/MathOverview";
import type { MathRow } from "../../bindings/MathRow";
import type { Relation } from "../../bindings/Relation";
import {
  Badge,
  Banner,
  Button,
  DataTable,
  ErrorState,
  LoadingState,
  SearchField,
  SegmentedControl,
  Section,
  StatusPill,
  type BadgeTone,
  type DataTableColumn,
} from "../../ui/components";
import { graphApi } from "./api";
import type { CanvasBadge } from "./canvas/types";
import type { CanvasSelection } from "./canvas/GraphCanvas";
import { FiltersButton } from "./Filters";
import { EntityInspector, FormalEvidenceBody, InspectorEmpty, RelationInspector, RelationStatusBadge, type InspectorActions } from "./Inspector";
import { entityDisplay, freshnessView, kindDisplay, VERDICT_VIEW, type VocabularyIndex } from "./model";
import { GraphDialogs, InspectorPanel, useInspectorOpen, type DialogState } from "./ProjectGraph";
import { project } from "./projection";
import { revealSource } from "./reveal";
import { SaveIndicator } from "./SaveIndicator";
import { takeRequest, useGraphNavigation } from "./navigation";
import { coverageSentence } from "./status";
import { canWrite, useGraph } from "./store";
import { useGraphLoad } from "./useGraphData";
import { useGraphOps } from "./useGraphOps";
import { arrange, ViewCanvas } from "./ViewCanvas";
import { AssociateDeclarationAction } from "./AssociateDeclaration";
import type { ViewStore } from "./viewStore";

type MathMode = "dependencies" | "verification";
type Quick = "all" | "noProof" | "noFormal" | "notCurrent";

const INFORMAL: Record<MathRow["informal"], { label: string; tone: BadgeTone; dashed?: boolean } | null> = {
  notApplicable: null,
  noProof: { label: "No proof linked", tone: "warning", dashed: true },
  proofLinked: { label: "Proof linked", tone: "neutral" },
  proofSuggested: { label: "Proof suggested", tone: "accent", dashed: true },
};

const FORMAL: Record<MathRow["formalAssociation"], { label: string; tone: BadgeTone; dashed?: boolean }> = {
  none: { label: "None", tone: "neutral", dashed: true },
  suggested: { label: "Suggested association", tone: "accent", dashed: true },
  reviewed: { label: "Reviewed association", tone: "neutral" },
};

const VERDICT_TONE = { succeeded: "success", warning: "warning", failed: "danger" } as const;

/** Node badges in the dependency view: verdict, freshness, missing proof — as text. */
export function mathBadges(row: MathRow | undefined): CanvasBadge[] {
  if (!row) return [];
  const badges: CanvasBadge[] = [];
  if (row.verdict) {
    const v = VERDICT_VIEW[row.verdict];
    badges.push({ label: v.label, tone: VERDICT_TONE[v.status as keyof typeof VERDICT_TONE] ?? "neutral", title: v.sentence });
  }
  if (row.freshness && row.freshness.kind !== "current") {
    const f = freshnessView(row.freshness);
    badges.push({ label: f.label === "Stale" ? "Stale evidence" : "Freshness unknown", tone: row.freshness.kind === "stale" ? "stale" : "neutral", dashed: true, title: f.detail });
  }
  const informal = INFORMAL[row.informal];
  if (informal && row.informal !== "proofLinked") badges.push({ label: informal.label, tone: informal.tone, dashed: informal.dashed });
  return badges;
}

function matchesQuick(row: MathRow, quick: Quick): boolean {
  switch (quick) {
    case "all":
      return true;
    case "noProof":
      return row.informal === "noProof";
    case "noFormal":
      return row.entity.kind !== "formalDeclaration" && row.formalAssociation === "none";
    case "notCurrent":
      return row.freshness !== null && row.freshness.kind !== "current";
  }
}

export function VerificationTable({
  rows,
  vocabulary,
  selected,
  onSelect,
}: {
  rows: MathRow[];
  vocabulary: VocabularyIndex | null;
  selected: string | null;
  onSelect: (id: string | null) => void;
}) {
  const columns: DataTableColumn<MathRow>[] = [
    {
      id: "statement",
      title: "Statement",
      isRowHeader: true,
      sortValue: (r) => r.entity.label,
      render: (r) => (
        <span className="xp-graph-table__name">
          <strong>{r.entity.label}</strong> <span className="xp-muted">{entityDisplay(vocabulary, r.entity).typeLabel}</span>
        </span>
      ),
    },
    {
      id: "informal",
      title: "Informal proof",
      sortValue: (r) => r.informal,
      render: (r) => {
        const v = INFORMAL[r.informal];
        return v ? (
          <Badge tone={v.tone} dashed={v.dashed}>
            {v.label}
          </Badge>
        ) : (
          <span className="xp-muted">Not applicable</span>
        );
      },
    },
    {
      id: "formal",
      title: "Formal association",
      sortValue: (r) => r.formalAssociation,
      render: (r) =>
        r.entity.kind === "formalDeclaration" ? (
          <span className="xp-muted">Is a formal declaration</span>
        ) : (
          <Badge tone={FORMAL[r.formalAssociation].tone} dashed={FORMAL[r.formalAssociation].dashed}>
            {FORMAL[r.formalAssociation].label}
            {r.formalDeclarations.length ? ` (${r.formalDeclarations.length})` : ""}
          </Badge>
        ),
    },
    {
      id: "verdict",
      title: "Verifier verdict",
      sortValue: (r) => (r.verdict ? VERDICT_VIEW[r.verdict].label : "~"),
      render: (r) =>
        r.verdict ? (
          <StatusPill size="sm" status={VERDICT_VIEW[r.verdict].status} label={VERDICT_VIEW[r.verdict].label} />
        ) : (
          <span className="xp-muted">No formal evidence</span>
        ),
    },
    {
      id: "freshness",
      title: "Freshness",
      sortValue: (r) => r.freshness?.kind ?? "~",
      render: (r) => {
        if (!r.freshness) return <span className="xp-muted">—</span>;
        const f = freshnessView(r.freshness);
        return <StatusPill size="sm" status={f.status} label={f.label} />;
      },
    },
    {
      id: "source",
      title: "Source",
      sortValue: (r) => r.binding?.path ?? "~",
      render: (r) =>
        r.binding ? (
          <span className="xp-graph-badges">
            <code className="xp-graph-path">{r.binding.path}</code>
            {r.binding.state !== "resolved" && (
              <Badge tone="warning" dashed>
                {r.binding.state}
              </Badge>
            )}
          </span>
        ) : (
          <span className="xp-muted">Not bound</span>
        ),
    },
  ];
  return (
    <DataTable
      aria-label="Verification"
      className="xp-graph-table"
      columns={columns}
      rows={rows}
      rowKey={(r) => r.entity.id}
      selectedKey={selected}
      onSelectionChange={onSelect}
      defaultSort={{ column: "statement", direction: "ascending" }}
      rowProps={(r) => ({ "data-entity-id": r.entity.id, "data-verdict": r.verdict ?? "none", "data-freshness": r.freshness?.kind ?? "none" })}
      empty="No mathematical items match. Declarations, statements, and proofs appear as they are extracted or created."
    />
  );
}

/** Traversal: the assumptions and results this entity depends on (bounded). */
function DependsOnSection({ entityId, vocabulary, onSelectEntity }: { entityId: string; vocabulary: VocabularyIndex | null; onSelectEntity: (id: string) => void }) {
  const loaded = useGraphLoad<GraphSnapshot>(`depends:${entityId}`, () =>
    graphApi().snapshot({
      kinds: null,
      text: null,
      around: { entity: entityId, depth: 4, relationKinds: ["dependsOn", "assumes"], direction: "outgoing" },
      includeInactive: false,
      limit: 200,
    }),
  );
  const others = (loaded.data?.entities ?? []).filter((e) => e.id !== entityId);
  const assumptions = others.filter((e) => e.kind === "assumption" || e.kind === "conjecture");
  const results = others.filter((e) => !assumptions.includes(e));
  const list = (items: Entity[]) => (
    <ul className="xp-graph-list">
      {items.map((e) => (
        <li key={e.id} className="xp-graph-list__item" data-testid="graph-depends-on" data-entity-id={e.id}>
          <p className="xp-graph-list__line">
            <button type="button" className="xp-link" onClick={() => onSelectEntity(e.id)}>
              {e.label}
            </button>
            <span className="xp-muted"> ({kindDisplay(vocabulary, e.kind).title})</span>
          </p>
        </li>
      ))}
    </ul>
  );
  return (
    <Section title="Depends on">
      {loaded.loading && !loaded.data ? (
        <p className="xp-muted">Tracing dependencies…</p>
      ) : loaded.error ? (
        <p className="xp-inline-error">{loaded.error}</p>
      ) : others.length === 0 ? (
        <p className="xp-muted">No recorded dependencies or assumptions (through “depends on” and “assumes”).</p>
      ) : (
        <>
          <h4 className="xp-graph-family__title">Assumptions ({assumptions.length})</h4>
          {assumptions.length ? list(assumptions) : <p className="xp-muted">None recorded.</p>}
          <h4 className="xp-graph-family__title">Results and definitions ({results.length})</h4>
          {results.length ? list(results) : <p className="xp-muted">None recorded.</p>}
          {loaded.data?.truncated && <p className="xp-muted">Showing the first {others.length}; the dependency closure is larger.</p>}
        </>
      )}
    </Section>
  );
}

function MathSections({
  detail,
  row,
  vocabulary,
  onSelectEntity,
  declarations,
  canWrite,
}: {
  detail: EntityDetail;
  row: MathRow | undefined;
  vocabulary: VocabularyIndex | null;
  onSelectEntity: (id: string) => void;
  /** Formal declaration entities in the project (association candidates). */
  declarations: Entity[];
  canWrite: boolean;
}) {
  const neighbors = new Map(detail.neighbors.map((n) => [n.id, n]));
  const proofs = detail.incoming.filter((r) => r.kind === "proofOf");
  const targets = detail.outgoing.filter((r) => r.kind === "proofOf");
  const formalizations = detail.incoming.filter((r) => r.kind === "formalizes");
  const isStatement = kindDisplay(vocabulary, detail.entity.kind).statement;
  const relationList = (relations: Relation[], pick: (r: Relation) => string, testId: string) => (
    <ul className="xp-graph-list">
      {relations.map((r) => {
        const other = neighbors.get(pick(r));
        return (
          <li key={r.id} className="xp-graph-list__item" data-testid={testId} data-entity-id={pick(r)}>
            <p className="xp-graph-list__line">
              <button type="button" className="xp-link" onClick={() => onSelectEntity(pick(r))}>
                {other?.label ?? pick(r)}
              </button>{" "}
              <RelationStatusBadge status={r.status} />
            </p>
          </li>
        );
      })}
    </ul>
  );
  return (
    <>
      <DependsOnSection entityId={detail.entity.id} vocabulary={vocabulary} onSelectEntity={onSelectEntity} />
      {isStatement && (
        <Section title={`Proofs targeting this (${proofs.length})`}>
          {proofs.length ? relationList(proofs, (r) => r.from, "graph-proof") : <p className="xp-muted">No proof is linked to this statement.</p>}
        </Section>
      )}
      {detail.entity.kind === "proof" && (
        <Section title="Proves">
          {targets.length ? relationList(targets, (r) => r.to, "graph-proof-target") : <p className="xp-muted">This proof targets no statement yet.</p>}
        </Section>
      )}
      <Section title="Formal verification">
        <div data-testid="graph-formal-section">
          {detail.entity.kind !== "formalDeclaration" &&
            (formalizations.length ? (
              <>
                <p>Associated formal declarations:</p>
                {relationList(formalizations, (r) => r.from, "graph-formalization")}
                <p className="xp-muted" data-testid="graph-correspondence-note">
                  An association, even a reviewed one, records a correspondence; it is not a machine proof that the
                  declaration and this statement are equivalent.
                </p>
              </>
            ) : (
              <p className="xp-muted">No formal declaration is associated with this item.</p>
            ))}
          {detail.entity.kind !== "formalDeclaration" && (
            <AssociateDeclarationAction target={detail.entity} declarations={declarations} canWrite={canWrite} />
          )}
          {row?.verdict ? (
            <p>
              Verifier verdict: <StatusPill size="sm" status={VERDICT_VIEW[row.verdict].status} label={VERDICT_VIEW[row.verdict].label} />{" "}
              <span className="xp-muted">{VERDICT_VIEW[row.verdict].sentence}</span>
            </p>
          ) : (
            <p className="xp-muted">No formal evidence: nothing about this item has been checked by Lean.</p>
          )}
          {row?.freshness && (
            <p>
              Freshness: <StatusPill size="sm" status={freshnessView(row.freshness).status} label={freshnessView(row.freshness).label} />{" "}
              <span className="xp-muted">{freshnessView(row.freshness).detail}</span>
            </p>
          )}
          {row?.evidence?.formal ? (
            <FormalEvidenceBody formal={row.evidence.formal} />
          ) : row?.formalDependencies?.kind === "unavailable" ? (
            <p data-testid="graph-formal-deps-unavailable">Formal dependency edges unavailable: {row.formalDependencies.reason}</p>
          ) : row?.formalDependencies?.kind === "reported" ? (
            <p>Formal dependencies reported by the checker: {row.formalDependencies.names.join(", ") || "none"}.</p>
          ) : (
            <p className="xp-muted" data-testid="graph-formal-deps-unavailable">
              Formal dependency edges unavailable: no checker evidence exposes them for this item.
            </p>
          )}
        </div>
      </Section>
    </>
  );
}

export function Mathematics({ store }: { store: ViewStore }) {
  const vocabulary = useGraph((s) => s.vocabulary);
  const status = useGraph((s) => s.status);
  const writable = canWrite(status);
  const view = store.use((s) => s.view);
  const [mode, setMode] = useState<MathMode>("dependencies");
  const [quick, setQuick] = useState<Quick>("all");
  const [selection, setSelection] = useState<CanvasSelection>({ nodes: [], edge: null });
  const [dialog, setDialog] = useState<DialogState>(null);
  const [fitRequest, setFitRequest] = useState<{ nonce: number; nodes?: string[] }>({ nonce: 0 });
  const [inspectorOpen, setInspectorOpen] = useInspectorOpen();
  const overview = useGraphLoad<MathOverview>("math", () => graphApi().mathOverview());
  const data = overview.data;

  const rowsById = useMemo(() => new Map((data?.rows ?? []).map((r) => [r.entity.id, r])), [data]);
  const entities = useMemo(() => new Map((data?.rows ?? []).map((r) => [r.entity.id, r.entity])), [data]);
  const relations = useMemo(() => new Map((data?.relations ?? []).map((r) => [r.id, r])), [data]);
  const declarations = useMemo(() => (data?.rows ?? []).map((r) => r.entity).filter((e) => e.kind === "formalDeclaration" && e.lifecycle === "active"), [data]);
  const quickIds = useMemo(() => new Set((data?.rows ?? []).filter((r) => matchesQuick(r, quick)).map((r) => r.entity.id)), [data, quick]);
  const projection = useMemo(() => {
    if (!data || !view) return null;
    const input = {
      entities: data.rows.map((r) => r.entity).filter((e) => quickIds.has(e.id)),
      relations: data.relations,
    };
    return project(input, vocabulary, view, (e) => mathBadges(rowsById.get(e.id)));
  }, [data, view, vocabulary, rowsById, quickIds]);

  useEffect(() => {
    if (!projection) return;
    const nodes = selection.nodes.filter((id) => entities.has(id));
    if (nodes.length !== selection.nodes.length) setSelection({ nodes, edge: selection.edge });
  }, [projection, entities, selection]);

  const select = (next: CanvasSelection) => {
    setSelection(next);
    if (next.nodes.length || next.edge) setInspectorOpen(true);
  };
  const selectEntity = (id: string) => select({ nodes: [id], edge: null });

  // "Open in Mathematics" from elsewhere: show everything and select the entity.
  const request = useGraphNavigation((s) => s.request);
  const handled = useRef(0);
  useEffect(() => {
    const pending = takeRequest("mathematics", handled.current);
    if (!pending || !projection) return;
    handled.current = pending.nonce;
    setQuick("all");
    if (view?.hidden.includes(pending.entity)) store.unhide([pending.entity]);
    select({ nodes: [pending.entity], edge: null });
    setFitRequest((f) => ({ nonce: f.nonce + 1, nodes: [pending.entity] }));
  }, [request, projection]);

  // Copy / Paste / Duplicate / Delete for the focused selection (P1), with a context menu.
  const ops = useGraphOps({
    id: "graph-mathematics",
    selection,
    entities,
    relations,
    vocabulary,
    canWrite: writable,
    onSelect: select,
    onHide: (ids) => {
      store.hide(ids);
      setSelection({ nodes: [], edge: null });
    },
  });

  if (!view || (!data && overview.loading)) return <LoadingState title="Loading mathematics…" />;
  if (!data || !projection) {
    return <ErrorState title="Could not load the Mathematics view" description={overview.error ?? undefined} actions={<Button onPress={overview.reload}>Retry</Button>} />;
  }

  const visibleRows = data.rows.filter((r) => quickIds.has(r.entity.id) && projection.nodes.some((n) => n.id === r.entity.id));
  const selectedEntity = selection.nodes.length === 1 ? selection.nodes[0]! : null;
  const selectedRelation = selection.edge ? relations.get(selection.edge) : undefined;
  const actions: InspectorActions = {
    canWrite: writable,
    onSelectEntity: selectEntity,
    onRelate: (entity) => setDialog({ kind: "relate", from: entity, to: null }),
    onHide: (id) => {
      store.hide([id]);
      setSelection({ nodes: [], edge: null });
    },
    onUnhide: (id) => store.unhide([id]),
    isHidden: (id) => view.hidden.includes(id),
  };
  const formalUnavailable = data.rows.some((r) => r.formalDependencies?.kind === "unavailable");

  return (
    <div className="xp-graph-section" data-testid="graph-mathematics">
      <div className="xp-graph-toolbar" data-region="graph-toolbar" role="toolbar" aria-label="Mathematics tools">
        <SegmentedControl<MathMode>
          aria-label="Mathematics view"
          value={mode}
          onChange={setMode}
          options={[
            { id: "dependencies", label: "Dependencies", icon: LayoutGrid },
            { id: "verification", label: "Verification", icon: Table2 },
          ]}
        />
        <SegmentedControl<Quick>
          aria-label="Show"
          value={quick}
          onChange={setQuick}
          options={[
            { id: "all", label: "All" },
            { id: "noProof", label: "No proof" },
            { id: "noFormal", label: "No formal association" },
            { id: "notCurrent", label: "Stale or unknown" },
          ]}
        />
        <SearchField aria-label="Search mathematics" placeholder="Search" className="xp-graph-toolbar__search" value={view.filters.text} onChange={(text) => store.setFilters({ text })} />
        <FiltersButton
          vocabulary={vocabulary}
          filters={view.filters}
          onChange={(patch) => store.setFilters(patch)}
          entityKinds={(kind) => kindDisplay(vocabulary, kind).mathematical}
        />
        <Button size="sm" variant="ghost" icon={Plus} isDisabled={!writable} onPress={() => setDialog({ kind: "create" })}>
          New
        </Button>
        {mode === "dependencies" && (
          <Button size="sm" variant="ghost" icon={Waypoints} isDisabled={projection.nodes.length === 0} onPress={() => arrange(store, projection, () => setFitRequest((f) => ({ nonce: f.nonce + 1 })))}>
            Arrange
          </Button>
        )}
        <span className="xp-graph-toolbar__spacer" />
        {mode === "dependencies" && <SaveIndicator store={store} />}
        {!inspectorOpen && (
          <Button size="sm" variant="ghost" onPress={() => setInspectorOpen(true)}>
            Inspector
          </Button>
        )}
      </div>
      <p className="xp-graph-coverage xp-muted" data-testid="graph-math-coverage">
        Extraction coverage: {coverageSentence(data.coverage)} Unknown coverage is not verified.
        {formalUnavailable ? " Formal dependency edges are unavailable where the checker evidence does not expose them." : ""}
      </p>
      {data.truncated && (
        <Banner tone="info" title={`Showing the first ${data.rows.length} mathematical items`}>
          Search or filter to narrow the view.
        </Banner>
      )}
      <div className="xp-graph-body">
        <div className="xp-graph-main" data-region="graph-main" aria-label="Mathematics" ref={ops.ref} onContextMenu={ops.onContextMenu}>
          {mode === "dependencies" ? (
            <ViewCanvas
              aria-label="Mathematics dependency canvas"
              data-testid="graph-math-canvas"
              store={store}
              projection={projection}
              selection={selection}
              onSelectionChange={select}
              onItemContextMenu={ops.onItemContextMenu}
              onConnect={
                writable
                  ? (source, target) => {
                      const from = entities.get(source);
                      const to = entities.get(target);
                      if (from && to) setDialog({ kind: "relate", from, to });
                    }
                  : undefined
              }
              fitRequest={fitRequest}
              empty={
                <div className="xp-graph-empty">
                  <p className="xp-graph-empty__title">No mathematical items{quick !== "all" ? " match" : " yet"}</p>
                  <p className="xp-muted">Definitions, statements, proofs, and formal declarations appear here as they are extracted from sources or created with New.</p>
                </div>
              }
            />
          ) : (
            <VerificationTable rows={visibleRows} vocabulary={vocabulary} selected={selectedEntity} onSelect={(id) => select({ nodes: id ? [id] : [], edge: null })} />
          )}
          {ops.menu}
        </div>
        <InspectorPanel open={inspectorOpen} onClose={() => setInspectorOpen(false)}>
          {selectedEntity ? (
            <EntityInspector key={selectedEntity} entityId={selectedEntity} vocabulary={vocabulary} actions={actions}>
              {(detail) => (
                <MathSections
                  detail={detail}
                  row={rowsById.get(selectedEntity)}
                  vocabulary={vocabulary}
                  onSelectEntity={selectEntity}
                  declarations={declarations}
                  canWrite={writable}
                />
              )}
            </EntityInspector>
          ) : selectedRelation ? (
            <RelationInspector relation={selectedRelation} entities={entities} vocabulary={vocabulary} actions={actions} />
          ) : (
            <InspectorEmpty counts={`${visibleRows.length} mathematical items shown. Select one to trace its dependencies, proofs, and formal evidence.`} />
          )}
        </InspectorPanel>
      </div>
      <GraphDialogs dialog={dialog} vocabulary={vocabulary} entities={[...entities.values()]} onClose={() => setDialog(null)} />
    </div>
  );
}

export function OpenSourceButton({ path, range }: { path: string; range: { start: number; end: number } | null }) {
  return (
    <Button size="sm" variant="ghost" icon={FileSearch} onPress={() => void revealSource(path, range)}>
      Open source
    </Button>
  );
}
