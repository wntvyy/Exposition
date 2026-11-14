// Mathematics subsection of the Graph page: the same canonical entities,
// restricted to mathematics (definitions, assumptions, statements, proofs,
// questions, formal declarations), as a dependency canvas and a verification
// table. Informal proof/review state, formal association, the exact verifier
// verdict, and freshness are separate columns; stale or unknown is never
// shown as success, and a reviewed association is a correspondence, not a
// machine proof of equivalence.
import {
  CircleCheck,
  CircleDashed,
  CircleHelp,
  CircleX,
  FileSearch,
  History,
  LayoutGrid,
  Plus,
  Sigma,
  Table2,
  TriangleAlert,
  Waypoints,
} from "lucide-react";
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
  EmptyState,
  ErrorState,
  LoadingState,
  SearchField,
  SegmentedControl,
  Section,
  StatusText,
  type BadgeTone,
  type DataTableColumn,
  type IconComponent,
  type Status,
} from "../../ui/components";
import { graphApi } from "./api";
import type { CanvasBadge } from "./canvas/types";
import type { CanvasSelection } from "./canvas/GraphCanvas";
import { FiltersButton } from "./Filters";
import { EntityInspector, FormalEvidenceBody, InspectorEmpty, RelationInspector, RelationStatus, type InspectorActions } from "./Inspector";
import { entityDisplay, freshnessView, kindDisplay, VERDICT_VIEW, type VocabularyIndex } from "./model";
import { GraphDialogs, InspectorPanel, useInspectorOpen, type DialogState } from "./ProjectGraph";
import { project } from "./projection";
import { revealSource } from "./reveal";
import { SaveIndicator } from "./SaveIndicator";
import { takeRequest, useGraphNavigation } from "./navigation";
import { coverageSentence } from "./status";
import { canWrite, useGraph } from "./store";
import { SectionToolbar } from "./toolbar";
import { useGraphLoad } from "./useGraphData";
import { useGraphOps } from "./useGraphOps";
import { arrange, ViewCanvas } from "./ViewCanvas";
import { AssociateDeclarationAction } from "./AssociateDeclaration";
import type { ViewStore } from "./viewStore";

type MathMode = "dependencies" | "verification";
type Quick = "all" | "noProof" | "noFormal" | "notCurrent";

/**
 * Informal proof state. A missing proof is a warning, a linked proof a
 * settled fact (neutral), a suggested proof provisional (dashed accent badge).
 */
const INFORMAL: Record<
  MathRow["informal"],
  { label: string; status: Status; tone: BadgeTone; icon: IconComponent; provisional?: boolean } | null
> = {
  notApplicable: null,
  noProof: { label: "No proof linked", status: "warning", tone: "warning", icon: TriangleAlert },
  proofLinked: { label: "Proof linked", status: "idle", tone: "neutral", icon: CircleCheck },
  proofSuggested: { label: "Proof suggested", status: "idle", tone: "accent", icon: CircleDashed, provisional: true },
};

/** Verdict status → node badge tone and icon (the icons StatusText uses for the same status). */
const VERDICT_BADGE: Partial<Record<Status, { tone: BadgeTone; icon: IconComponent }>> = {
  succeeded: { tone: "success", icon: CircleCheck },
  warning: { tone: "warning", icon: TriangleAlert },
  failed: { tone: "danger", icon: CircleX },
  idle: { tone: "neutral", icon: CircleDashed },
};

/** Node badges in the dependency view: verdict, freshness, missing proof — text with icons. */
export function mathBadges(row: MathRow | undefined): CanvasBadge[] {
  if (!row) return [];
  const badges: CanvasBadge[] = [];
  if (row.verdict) {
    const v = VERDICT_VIEW[row.verdict];
    const look = VERDICT_BADGE[v.status] ?? VERDICT_BADGE.idle!;
    badges.push({ label: v.label, tone: look.tone, icon: look.icon, title: v.sentence });
  }
  if (row.freshness && row.freshness.kind !== "current") {
    const f = freshnessView(row.freshness);
    const stale = row.freshness.kind === "stale";
    badges.push({
      label: stale ? "Stale evidence" : "Freshness unknown",
      tone: stale ? "stale" : "neutral",
      icon: stale ? History : CircleHelp,
      dashed: true,
      title: f.detail,
    });
  }
  const informal = INFORMAL[row.informal];
  if (informal && row.informal !== "proofLinked") {
    badges.push({ label: informal.label, tone: informal.tone, icon: informal.icon, dashed: informal.provisional });
  }
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

const capitalize = (text: string) => text.charAt(0).toUpperCase() + text.slice(1);

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
          <span className="xp-graph-table__title">{r.entity.label}</span> <span className="ui-muted">{entityDisplay(vocabulary, r.entity).typeLabel}</span>
        </span>
      ),
    },
    {
      id: "informal",
      title: "Informal proof",
      sortValue: (r) => r.informal,
      render: (r) => {
        const v = INFORMAL[r.informal];
        if (!v) return <span className="ui-muted">Not applicable</span>;
        return v.provisional ? (
          <Badge tone={v.tone} dashed>
            {v.label}
          </Badge>
        ) : (
          <StatusText status={v.status} label={v.label} />
        );
      },
    },
    {
      id: "formal",
      title: "Formal association",
      sortValue: (r) => r.formalAssociation,
      render: (r) => {
        if (r.entity.kind === "formalDeclaration") return <span className="ui-muted">Is a formal declaration</span>;
        const count = r.formalDeclarations.length ? ` (${r.formalDeclarations.length})` : "";
        switch (r.formalAssociation) {
          case "none":
            return <span className="ui-muted">None</span>;
          case "suggested":
            return (
              <Badge tone="accent" dashed>
                Suggested association{count}
              </Badge>
            );
          case "reviewed":
            return <span>Reviewed association{count}</span>;
        }
      },
    },
    {
      id: "verdict",
      title: "Verifier verdict",
      sortValue: (r) => (r.verdict ? VERDICT_VIEW[r.verdict].label : "~"),
      render: (r) =>
        r.verdict ? (
          <StatusText status={VERDICT_VIEW[r.verdict].status} label={VERDICT_VIEW[r.verdict].label} />
        ) : (
          <StatusText status="idle" label="No formal evidence" />
        ),
    },
    {
      id: "freshness",
      title: "Freshness",
      sortValue: (r) => r.freshness?.kind ?? "~",
      render: (r) => {
        if (!r.freshness) return <span className="ui-muted">—</span>;
        const f = freshnessView(r.freshness);
        return <StatusText status={f.status} label={f.label} />;
      },
    },
    {
      id: "source",
      title: "Source",
      sortValue: (r) => r.binding?.path ?? "~",
      render: (r) =>
        r.binding ? (
          <span className="xp-graph-inline">
            <span className="ui-code">{r.binding.path}</span>
            {r.binding.state !== "resolved" && (
              <Badge tone="warning" dashed>
                {capitalize(r.binding.state)}
              </Badge>
            )}
          </span>
        ) : (
          <span className="ui-muted">Not bound</span>
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
  const group = (title: string, items: Entity[]) => (
    <div className="xp-graph-group">
      <h4 className="ui-group-label">
        {title} <span className="xp-graph-count">{items.length}</span>
      </h4>
      {items.length ? (
        <ul className="xp-graph-rows">
          {items.map((e) => (
            <li key={e.id} className="xp-graph-row" data-testid="graph-depends-on" data-entity-id={e.id}>
              <p className="xp-graph-row__line xp-graph-row__main">
                <button type="button" className="xp-link" onClick={() => onSelectEntity(e.id)}>
                  {e.label}
                </button>
                <span className="ui-muted"> ({kindDisplay(vocabulary, e.kind).title})</span>
              </p>
            </li>
          ))}
        </ul>
      ) : (
        <p className="ui-muted">None recorded.</p>
      )}
    </div>
  );
  return (
    <Section title="Depends on" count={loaded.data && !loaded.error ? others.length : undefined}>
      {loaded.loading && !loaded.data ? (
        <p className="ui-muted">Tracing dependencies…</p>
      ) : loaded.error ? (
        <p className="xp-inline-error">{loaded.error}</p>
      ) : others.length === 0 ? (
        <p className="ui-muted">No recorded dependencies or assumptions (through “depends on” and “assumes”).</p>
      ) : (
        <>
          {group("Assumptions", assumptions)}
          {group("Results and definitions", results)}
          {loaded.data?.truncated && <p className="ui-meta">Showing the first {others.length}; the dependency closure is larger.</p>}
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
    <ul className="xp-graph-rows">
      {relations.map((r) => {
        const other = neighbors.get(pick(r));
        return (
          <li key={r.id} className="xp-graph-row" data-testid={testId} data-entity-id={pick(r)}>
            <p className="xp-graph-row__line xp-graph-row__main">
              <button type="button" className="xp-link" onClick={() => onSelectEntity(pick(r))}>
                {other?.label ?? pick(r)}
              </button>{" "}
              <RelationStatus status={r.status} />
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
        <Section title="Proofs targeting this" count={proofs.length}>
          {proofs.length ? relationList(proofs, (r) => r.from, "graph-proof") : <p className="ui-muted">No proof is linked to this statement.</p>}
        </Section>
      )}
      {detail.entity.kind === "proof" && (
        <Section title="Proves" count={targets.length}>
          {targets.length ? relationList(targets, (r) => r.to, "graph-proof-target") : <p className="ui-muted">This proof targets no statement yet.</p>}
        </Section>
      )}
      <Section title="Formal verification">
        <div className="xp-graph-stack" data-testid="graph-formal-section">
          {detail.entity.kind !== "formalDeclaration" &&
            (formalizations.length ? (
              <>
                <p>Associated formal declarations:</p>
                {relationList(formalizations, (r) => r.from, "graph-formalization")}
                <p className="ui-muted" data-testid="graph-correspondence-note">
                  An association, even a reviewed one, records a correspondence; it is not a machine proof that the
                  declaration and this statement are equivalent.
                </p>
              </>
            ) : (
              <p className="ui-muted">No formal declaration is associated with this item.</p>
            ))}
          {detail.entity.kind !== "formalDeclaration" && (
            <AssociateDeclarationAction target={detail.entity} declarations={declarations} canWrite={canWrite} />
          )}
          {row?.verdict ? (
            <p>
              Verifier verdict: <StatusText status={VERDICT_VIEW[row.verdict].status} label={VERDICT_VIEW[row.verdict].label} />{" "}
              <span className="ui-muted">{VERDICT_VIEW[row.verdict].sentence}</span>
            </p>
          ) : (
            <p className="ui-muted">No formal evidence: nothing about this item has been checked by Lean.</p>
          )}
          {row?.freshness && (
            <p>
              Freshness: <StatusText status={freshnessView(row.freshness).status} label={freshnessView(row.freshness).label} />{" "}
              <span className="ui-muted">{freshnessView(row.freshness).detail}</span>
            </p>
          )}
          {row?.evidence?.formal ? (
            <FormalEvidenceBody formal={row.evidence.formal} />
          ) : row?.formalDependencies?.kind === "unavailable" ? (
            <p data-testid="graph-formal-deps-unavailable">Formal dependency edges unavailable: {row.formalDependencies.reason}</p>
          ) : row?.formalDependencies?.kind === "reported" ? (
            <p>Formal dependencies reported by the checker: {row.formalDependencies.names.join(", ") || "none"}.</p>
          ) : (
            <p className="ui-muted" data-testid="graph-formal-deps-unavailable">
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
      <SectionToolbar label="Mathematics tools">
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
      </SectionToolbar>
      <p className="xp-graph-coverage ui-meta" data-testid="graph-math-coverage">
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
                <EmptyState
                  size="page"
                  fill={false}
                  icon={Sigma}
                  title={`No mathematical items${quick !== "all" ? " match" : " yet"}`}
                  description="Definitions, statements, proofs, and formal declarations appear here as they are extracted from sources or created with New."
                />
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
