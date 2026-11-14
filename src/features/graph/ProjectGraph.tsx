// Project subsection of the Graph page: the project's saved view as a canvas
// or as tables, with search, filters, local neighborhood focus, typed
// relationship authoring, and the inspector.
import { Eye, FolderPlus, LayoutGrid, Plus, Rows3, Waypoints, X } from "lucide-react";
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";

import { useLayout } from "../../app/layout";
import type { Entity } from "../../bindings/Entity";
import type { GraphQuery } from "../../bindings/GraphQuery";
import type { GraphSnapshot } from "../../bindings/GraphSnapshot";
import type { Relation } from "../../bindings/Relation";
import {
  Banner,
  Button,
  ErrorState,
  IconButton,
  LoadingState,
  Menu,
  MenuItem,
  MenuTrigger,
  SearchField,
  SegmentedControl,
  Select,
  Tab,
  TabList,
  TabPanel,
  Tabs,
} from "../../ui/components";
import { readStored, writeStored } from "../../ui/storage";
import { graphApi } from "./api";
import type { CanvasSelection } from "./canvas/GraphCanvas";
import { AddToCollectionDialog, CreateEntityDialog, RelateDialog } from "./changes";
import { FiltersButton } from "./Filters";
import { EntityInspector, InspectorEmpty, RelationInspector, type InspectorActions } from "./Inspector";
import type { EntityKind } from "../../bindings/EntityKind";
import type { ViewFocus, VocabularyIndex } from "./model";
import { project as projectView } from "./projection";
import { canWrite, useGraph } from "./store";
import { EntityTable, RelationTable } from "./Tables";
import { useGraphLoad } from "./useGraphData";
import { useGraphOps } from "./useGraphOps";
import { arrange, ViewCanvas } from "./ViewCanvas";
import { SaveIndicator } from "./SaveIndicator";
import { takeRequest, useGraphNavigation } from "./navigation";
import type { ViewStore } from "./viewStore";

export type DialogState =
  | { kind: "relate"; from: Entity; to: Entity | null }
  | { kind: "create"; entityKind?: EntityKind }
  | { kind: "collection"; entity: Entity }
  | null;

export const DEFAULT_LIMIT = 300;
const MODE_KEY = "exposition.graph.mode";
const NARROW_WIDTH = 1100;

export function useDisplayMode(key: string) {
  const [mode, setMode] = useState<"canvas" | "table">(() => (readStored(`${MODE_KEY}.${key}`) === "table" ? "table" : "canvas"));
  return [
    mode,
    (next: "canvas" | "table") => {
      setMode(next);
      writeStored(`${MODE_KEY}.${key}`, next);
    },
  ] as const;
}

/** The inspector column: docked when the window is wide, an overlay drawer when narrow. */
export function InspectorPanel({ open, onClose, children }: { open: boolean; onClose: () => void; children: ReactNode }) {
  const narrow = useLayout((s) => s.viewportWidth < NARROW_WIDTH);
  if (!open) return null;
  return (
    <aside className="xp-graph-inspector" data-region="graph-inspector" data-narrow={narrow || undefined} aria-label="Inspector">
      <header className="xp-graph-inspector__bar">
        <h2 className="xp-graph-inspector__bar-title">Inspector</h2>
        <IconButton icon={X} size="sm" aria-label="Close inspector" onPress={onClose} />
      </header>
      <div className="xp-graph-inspector__scroll">{children}</div>
    </aside>
  );
}

export function useInspectorOpen() {
  const narrow = useLayout((s) => s.viewportWidth < NARROW_WIDTH);
  const [open, setOpen] = useState(!narrow);
  useEffect(() => setOpen(!narrow), [narrow]);
  return [open, setOpen] as const;
}

export function GraphDialogs({
  dialog,
  vocabulary,
  entities,
  onClose,
}: {
  dialog: DialogState;
  vocabulary: VocabularyIndex | null;
  entities: Entity[];
  onClose: () => void;
}) {
  if (!dialog) return null;
  if (dialog.kind === "relate") {
    return <RelateDialog vocabulary={vocabulary} from={dialog.from} to={dialog.to} candidates={entities} onClose={onClose} />;
  }
  if (dialog.kind === "create") return <CreateEntityDialog vocabulary={vocabulary} kind={dialog.entityKind} onClose={onClose} />;
  return (
    <AddToCollectionDialog
      vocabulary={vocabulary}
      entity={dialog.entity}
      collections={entities.filter((e) => e.kind === "collection" || e.kind === "researchSpace")}
      onClose={onClose}
    />
  );
}

export function ProjectGraph({ store }: { store: ViewStore }) {
  const vocabulary = useGraph((s) => s.vocabulary);
  const status = useGraph((s) => s.status);
  const writable = canWrite(status);
  const view = store.use((s) => s.view);
  const viewError = store.use((s) => s.loadError);
  const [mode, setMode] = useDisplayMode("project");
  const [selection, setSelection] = useState<CanvasSelection>({ nodes: [], edge: null });
  const [focus, setFocus] = useState<ViewFocus | null>(null);
  const [limit, setLimit] = useState(DEFAULT_LIMIT);
  const [searchAll, setSearchAll] = useState(false);
  const [dialog, setDialog] = useState<DialogState>(null);
  const [fitRequest, setFitRequest] = useState<{ nonce: number; nodes?: string[] }>({ nonce: 0 });
  const [inspectorOpen, setInspectorOpen] = useInspectorOpen();
  const [tableTab, setTableTab] = useState<"entities" | "relations">("entities");

  const filters = view?.filters;
  const query: GraphQuery | null = filters
    ? {
        kinds: filters.kinds.length ? filters.kinds : null,
        text: searchAll && filters.text.trim() ? filters.text.trim() : null,
        around: focus ? { entity: focus.entity, depth: focus.depth, relationKinds: null, direction: focus.direction } : null,
        includeInactive: filters.showInactive,
        limit,
      }
    : null;
  const snapshot = useGraphLoad<GraphSnapshot>(query ? JSON.stringify(query) : null, () => graphApi().snapshot(query!));
  const data = snapshot.data;
  const entities = useMemo(() => new Map((data?.entities ?? []).map((e) => [e.id, e])), [data]);
  const relations = useMemo(() => new Map<string, Relation>((data?.relations ?? []).map((r) => [r.id, r])), [data]);
  const projection = useMemo(
    () => (data && view ? projectView(data, vocabulary, view) : null),
    [data, view, vocabulary],
  );

  // Drop a selection whose item is gone (removed relationship, filtered out).
  useEffect(() => {
    if (!projection) return;
    const nodes = selection.nodes.filter((id) => projection.nodes.some((n) => n.id === id));
    const edge = selection.edge && projection.edges.some((e) => e.id === selection.edge) ? selection.edge : null;
    if (nodes.length !== selection.nodes.length || edge !== selection.edge) setSelection({ nodes, edge });
  }, [projection, selection]);

  const select = (next: CanvasSelection) => {
    setSelection(next);
    if (next.nodes.length || next.edge) setInspectorOpen(true);
  };

  // "Open in Graph" from elsewhere: select the entity; when the current view
  // does not include it, show its neighborhood instead.
  const request = useGraphNavigation((s) => s.request);
  const handled = useRef(0);
  useEffect(() => {
    const pending = takeRequest("project", handled.current);
    if (!pending || !projection) return;
    handled.current = pending.nonce;
    if (!projection.nodes.some((n) => n.id === pending.entity)) {
      setFocus({ entity: pending.entity, depth: 1, direction: "both" });
      if (view?.hidden.includes(pending.entity)) store.unhide([pending.entity]);
    }
    select({ nodes: [pending.entity], edge: null });
    setFitRequest((f) => ({ nonce: f.nonce + 1, nodes: [pending.entity] }));
  }, [request, projection]);

  // Copy / Paste / Duplicate / Delete for the focused selection (P1), with a context menu.
  const ops = useGraphOps({
    id: "graph-project",
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

  const actions: InspectorActions = {
    canWrite: writable,
    onSelectEntity: (id) => select({ nodes: [id], edge: null }),
    onFocus: (id) => setFocus({ entity: id, depth: 2, direction: "both" }),
    onRelate: (entity) => setDialog({ kind: "relate", from: entity, to: null }),
    onAddToCollection: (entity) => setDialog({ kind: "collection", entity }),
    onHide: (id) => {
      store.hide([id]);
      setSelection({ nodes: [], edge: null });
    },
    onUnhide: (id) => store.unhide([id]),
    isHidden: (id) => view?.hidden.includes(id) ?? false,
  };

  if (viewError) return <ErrorState title="Could not load the saved view" description={viewError} actions={<Button onPress={() => void store.load()}>Retry</Button>} />;
  if (!view || (!data && snapshot.loading)) return <LoadingState title="Loading the project graph…" />;
  if (!data || !projection) {
    return (
      <ErrorState
        title="Could not load the project graph"
        description={snapshot.error ?? undefined}
        actions={<Button onPress={snapshot.reload}>Retry</Button>}
      />
    );
  }

  const focusEntity = focus ? entities.get(focus.entity) : undefined;
  const selectedEntity = selection.nodes.length === 1 ? selection.nodes[0]! : null;
  const selectedRelation = selection.edge ? relations.get(selection.edge) : undefined;
  const counts = `${projection.nodes.length} items and ${projection.edges.length} relationships shown.`;

  return (
    <div className="xp-graph-section" data-testid="graph-project">
      <div className="xp-graph-toolbar" data-region="graph-toolbar" role="toolbar" aria-label="Project graph tools">
        <SegmentedControl<"canvas" | "table">
          aria-label="Display"
          value={mode}
          onChange={setMode}
          options={[
            { id: "canvas", label: "Canvas", icon: LayoutGrid },
            { id: "table", label: "Table", icon: Rows3 },
          ]}
        />
        <SearchField
          aria-label="Search the graph"
          placeholder="Search items"
          className="xp-graph-toolbar__search"
          value={filters!.text}
          onChange={(text) => store.setFilters({ text })}
        />
        <FiltersButton vocabulary={vocabulary} filters={filters!} onChange={(patch) => store.setFilters(patch)} />
        <MenuTrigger>
          <Button size="sm" variant="ghost" icon={Plus} isDisabled={!writable}>
            New
          </Button>
          <Menu
            aria-label="New"
            onAction={(key) => setDialog(key === "collection" ? { kind: "create", entityKind: "collection" } : { kind: "create" })}
          >
            <MenuItem id="collection" icon={FolderPlus}>
              New collection…
            </MenuItem>
            <MenuItem id="entity" icon={Plus} description="A claim, theorem, question, reference…">
              New item…
            </MenuItem>
          </Menu>
        </MenuTrigger>
        <Button size="sm" variant="ghost" icon={Waypoints} isDisabled={mode !== "canvas" || projection.nodes.length === 0} onPress={() => arrange(store, projection, () => setFitRequest((f) => ({ nonce: f.nonce + 1 })))}>
          Arrange
        </Button>
        {view.hidden.length > 0 && (
          <Button size="sm" variant="ghost" icon={Eye} onPress={() => store.unhideAll()}>
            Show {view.hidden.length} hidden
          </Button>
        )}
        <span className="xp-graph-toolbar__spacer" />
        <SaveIndicator store={store} />
        {!inspectorOpen && (
          <Button size="sm" variant="ghost" onPress={() => setInspectorOpen(true)}>
            Inspector
          </Button>
        )}
      </div>

      {focus && (
        <Banner
          tone="info"
          title={`Showing ${focus.depth} ${focus.depth === 1 ? "step" : "steps"} around ${focusEntity?.label ?? "the selected item"}`}
          actions={
            <>
              <Select<number>
                aria-label="Depth"
                options={[1, 2, 3, 4].map((d) => ({ id: d, label: `${d} ${d === 1 ? "step" : "steps"}` }))}
                value={focus.depth}
                onChange={(depth) => setFocus({ ...focus, depth })}
              />
              <Select<ViewFocus["direction"]>
                aria-label="Direction"
                options={[
                  { id: "both", label: "Both directions" },
                  { id: "outgoing", label: "What it relates to" },
                  { id: "incoming", label: "What relates to it" },
                ]}
                value={focus.direction}
                onChange={(direction) => setFocus({ ...focus, direction })}
              />
              <Button size="sm" onPress={() => setFocus(null)}>
                Show whole project
              </Button>
            </>
          }
        />
      )}
      {data.truncated && (
        <Banner
          tone="info"
          title={`Showing ${data.entities.length} of ${data.total} items`}
          actions={
            <>
              {filters!.text.trim() && !searchAll && (
                <Button size="sm" onPress={() => setSearchAll(true)}>
                  Search all {data.total}
                </Button>
              )}
              {limit < 1000 && (
                <Button size="sm" onPress={() => setLimit(1000)}>
                  Show up to 1000
                </Button>
              )}
            </>
          }
        >
          Large projects start with a subset. Search, filter by kind, or show the neighborhood of an item to see others.
        </Banner>
      )}
      {status?.state.kind === "temporaryReadOnly" && (
        <Banner tone="warning" title="Temporary read-only graph">
          {status.state.reason} Changes and saved layouts cannot be kept for this folder.
        </Banner>
      )}

      <div className="xp-graph-body">
        <div className="xp-graph-main" data-region="graph-main" aria-label="Project graph" ref={ops.ref} onContextMenu={ops.onContextMenu}>
          {mode === "canvas" ? (
            <ViewCanvas
              aria-label="Project graph canvas"
              data-testid="graph-canvas"
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
              empty={<EmptyCanvas filtered={projection.filteredOut + projection.hidden} total={data.entities.length} />}
            />
          ) : (
            <Tabs selectedKey={tableTab} onSelectionChange={(k) => setTableTab(k as "entities" | "relations")} className="xp-graph-tables">
              <TabList aria-label="Tables">
                <Tab id="entities">Items ({projection.nodes.length})</Tab>
                <Tab id="relations">Relationships ({projection.edges.length})</Tab>
              </TabList>
              <TabPanel id="entities" className="xp-graph-tables__panel">
                <EntityTable
                  projection={projection}
                  entities={entities}
                  selected={selectedEntity}
                  onSelect={(id) => select({ nodes: id ? [id] : [], edge: null })}
                />
              </TabPanel>
              <TabPanel id="relations" className="xp-graph-tables__panel">
                <RelationTable
                  projection={projection}
                  entities={entities}
                  relations={relations}
                  vocabulary={vocabulary}
                  selected={selection.edge}
                  onSelect={(id) => select({ nodes: [], edge: id })}
                />
              </TabPanel>
            </Tabs>
          )}
          {ops.menu}
        </div>
        <InspectorPanel open={inspectorOpen} onClose={() => setInspectorOpen(false)}>
          {selectedEntity ? (
            <EntityInspector key={selectedEntity} entityId={selectedEntity} vocabulary={vocabulary} actions={actions} />
          ) : selectedRelation ? (
            <RelationInspector relation={selectedRelation} entities={entities} vocabulary={vocabulary} actions={actions} />
          ) : selection.edge ? (
            <InspectorEmpty counts="This relationship stands for several relationships folded into a collapsed group; expand the group to inspect them." />
          ) : selection.nodes.length > 1 ? (
            <InspectorEmpty counts={`${selection.nodes.length} items selected. Select one to inspect it.`} />
          ) : (
            <InspectorEmpty counts={counts} />
          )}
        </InspectorPanel>
      </div>
      <GraphDialogs dialog={dialog} vocabulary={vocabulary} entities={data.entities} onClose={() => setDialog(null)} />
    </div>
  );
}

function EmptyCanvas({ filtered, total }: { filtered: number; total: number }) {
  return (
    <div className="xp-graph-empty" data-testid="graph-empty">
      <p className="xp-graph-empty__title">{total === 0 ? "The project graph has no items yet" : "No items match this view"}</p>
      <p className="xp-muted">
        {total === 0
          ? "Resources appear as the project is indexed. You can also create claims, theorems, questions, and collections with New."
          : `${filtered} items are hidden or filtered out. Change the filters, clear the search, or show hidden items.`}
      </p>
    </div>
  );
}
