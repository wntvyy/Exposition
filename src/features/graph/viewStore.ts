// Saved graph views: per-view node positions/sizes, viewport, filters, hidden
// entities, and collapsed groups. Presentation only — a view never changes
// entities or relations. Changes save automatically (debounced) with the
// view's expected revision; a stale save (`graph.staleView`) adopts the newer
// saved view and says so.
import { create, type StoreApi, type UseBoundStore } from "zustand";

import { notify } from "../../app/notifications";
import type { GraphView } from "../../bindings/GraphView";
import type { GraphViewInput } from "../../bindings/GraphViewInput";
import type { ViewScope } from "../../bindings/ViewScope";
import { errorCode, errorText, graphApi } from "./api";
import type { CanvasViewport, Point } from "./canvas/types";
import { defaultFilters, type ClientFilters, type NodeLayout, type SavedView } from "./model";

export type SaveState = "idle" | "pending" | "saving" | "saved" | "error";

export interface ViewStoreState {
  view: SavedView | null;
  loading: boolean;
  loadError: string | null;
  save: SaveState;
  saveError: string | null;
}

export interface ViewStore {
  use: UseBoundStore<StoreApi<ViewStoreState>>;
  /** Load the saved view of this scope (or start an unsaved one). */
  load(): Promise<void>;
  /** Committed node moves (layout only). */
  moveNodes(moves: Map<string, Point>): void;
  /** Place nodes: only those without a position unless `replace` (Arrange). */
  placeNodes(positions: Map<string, Point>, options?: { replace?: boolean }): void;
  setViewport(viewport: CanvasViewport): void;
  setFilters(patch: Partial<ClientFilters>): void;
  hide(ids: string[]): void;
  unhide(ids: string[]): void;
  unhideAll(): void;
  toggleCollapsed(id: string): void;
  /** Save now (e.g. before the window closes). */
  flush(): Promise<void>;
}

export const SAVE_DELAY_MS = 600;

export function emptyView(name: string, scope: ViewScope): SavedView {
  return {
    id: null,
    name,
    scope,
    revision: null,
    positions: {},
    viewport: null,
    filters: { ...defaultFilters },
    hidden: [],
    collapsed: [],
  };
}

export function fromGraphView(view: GraphView): SavedView {
  const positions: Record<string, NodeLayout> = {};
  for (const node of view.nodes) {
    positions[node.entity] = {
      x: node.x,
      y: node.y,
      ...(node.width !== null ? { width: node.width } : {}),
      ...(node.height !== null ? { height: node.height } : {}),
    };
  }
  return {
    id: view.id,
    name: view.name,
    scope: view.scope,
    revision: view.revision,
    positions,
    viewport: view.viewport,
    filters: {
      kinds: view.filters.kinds,
      relationKinds: view.filters.relationKinds,
      text: view.filters.text ?? "",
      showInactive: view.filters.showInactive,
    },
    hidden: view.hidden,
    collapsed: view.collapsed,
  };
}

const clampZoom = (zoom: number) => Math.min(8, Math.max(0.05, zoom));

export function toGraphViewInput(view: SavedView): GraphViewInput {
  return {
    id: view.id,
    name: view.name,
    scope: view.scope,
    nodes: Object.entries(view.positions).map(([entity, p]) => ({
      entity,
      x: p.x,
      y: p.y,
      width: p.width ?? null,
      height: p.height ?? null,
    })),
    viewport: view.viewport
      ? { x: view.viewport.x, y: view.viewport.y, zoom: clampZoom(view.viewport.zoom) }
      : { x: 0, y: 0, zoom: 1 },
    filters: {
      kinds: view.filters.kinds,
      relationKinds: view.filters.relationKinds,
      text: view.filters.text.trim() ? view.filters.text : null,
      showInactive: view.filters.showInactive,
      depth: null,
    },
    hidden: view.hidden,
    collapsed: view.collapsed,
    expectedRevision: view.revision,
  };
}

const sameScope = (a: ViewScope, b: ViewScope) => JSON.stringify(a) === JSON.stringify(b);

export function createViewStore(name: string, scope: ViewScope, options: { saveDelayMs?: number } = {}): ViewStore {
  const delay = options.saveDelayMs ?? SAVE_DELAY_MS;
  const use = create<ViewStoreState>(() => ({ view: null, loading: false, loadError: null, save: "idle", saveError: null }));
  let timer: ReturnType<typeof setTimeout> | null = null;
  let saving: Promise<void> | null = null;
  let dirtyAgain = false;

  const update = (fn: (view: SavedView) => SavedView) => {
    const view = use.getState().view;
    if (!view) return;
    use.setState({ view: fn(view), save: "pending" });
    schedule();
  };

  const schedule = () => {
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => {
      timer = null;
      void save();
    }, delay);
  };

  const adoptLatest = async (id: string) => {
    try {
      const latest = fromGraphView(await graphApi().viewGet(id));
      use.setState({ view: latest, save: "saved" });
    } catch (error) {
      use.setState({ save: "error", saveError: errorText(error) });
    }
    notify({
      id: `graph-view-conflict-${name}`,
      tone: "warning",
      title: `The ${name} view changed elsewhere`,
      body: "Showing its latest saved layout; your last change to this view was not saved.",
    });
  };

  const save = async (): Promise<void> => {
    if (saving) {
      dirtyAgain = true;
      return saving;
    }
    const view = use.getState().view;
    if (!view) return;
    use.setState({ save: "saving", saveError: null });
    saving = (async () => {
      try {
        const saved = await graphApi().viewSave(toGraphViewInput(view));
        // Keep edits made while saving; adopt the id and new revision token.
        use.setState((s) => ({ view: s.view ? { ...s.view, id: saved.id, revision: saved.revision } : fromGraphView(saved) }));
        use.setState({ save: dirtyAgain ? "pending" : "saved" });
      } catch (error) {
        if (errorCode(error) === "graph.staleView" && view.id) {
          dirtyAgain = false;
          await adoptLatest(view.id);
        } else {
          use.setState({ save: "error", saveError: errorText(error) });
        }
      } finally {
        saving = null;
      }
    })();
    await saving;
    if (dirtyAgain) {
      dirtyAgain = false;
      await save();
    }
  };

  const setPositions = (positions: Map<string, Point>, replace: boolean) =>
    update((view) => {
      const next: Record<string, NodeLayout> = { ...view.positions };
      for (const [entity, p] of positions) {
        if (!replace && next[entity]) continue;
        next[entity] = { ...next[entity], x: Math.round(p.x), y: Math.round(p.y) };
      }
      return { ...view, positions: next };
    });

  return {
    use,
    async load() {
      use.setState({ loading: true, loadError: null });
      try {
        const summary = (await graphApi().views()).find((v) => sameScope(v.scope, scope));
        const view = summary ? fromGraphView(await graphApi().viewGet(summary.id)) : emptyView(name, scope);
        use.setState({ view, loading: false, save: "idle" });
      } catch (error) {
        use.setState({ loading: false, loadError: errorText(error) });
      }
    },
    moveNodes(moves) {
      setPositions(moves, true);
    },
    placeNodes(positions, opts = {}) {
      setPositions(positions, Boolean(opts.replace));
    },
    setViewport(viewport) {
      update((view) => ({
        ...view,
        viewport: {
          x: Math.round(viewport.x),
          y: Math.round(viewport.y),
          zoom: Math.round(clampZoom(viewport.zoom) * 1000) / 1000,
        },
      }));
    },
    setFilters(patch) {
      update((view) => ({ ...view, filters: { ...view.filters, ...patch } }));
    },
    hide(ids) {
      update((view) => ({ ...view, hidden: [...new Set([...view.hidden, ...ids])] }));
    },
    unhide(ids) {
      update((view) => ({ ...view, hidden: view.hidden.filter((h) => !ids.includes(h)) }));
    },
    unhideAll() {
      update((view) => ({ ...view, hidden: [] }));
    },
    toggleCollapsed(entity) {
      update((view) => ({
        ...view,
        collapsed: view.collapsed.includes(entity) ? view.collapsed.filter((c) => c !== entity) : [...view.collapsed, entity],
      }));
    },
    async flush() {
      if (timer) {
        clearTimeout(timer);
        timer = null;
        await save();
      } else if (saving) await saving;
      else if (use.getState().save === "error") await save();
    },
  };
}
