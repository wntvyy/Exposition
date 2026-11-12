// Saved graph views: per-view node positions/sizes, viewport, filters, hidden
// entities, collapsed groups, and local focus. Presentation only — a view
// never changes entities or relations. Changes save automatically (debounced)
// with the view's expected revision; a conflict adopts the newer saved view
// and says so.
import { create, type StoreApi, type UseBoundStore } from "zustand";

import { notify } from "../../app/notifications";
import { errorText, graphApi } from "./api";
import type { CanvasViewport, Point } from "./canvas/types";
import { defaultFilters, type NodeLayout, type SavedView, type ViewFilters, type ViewFocus } from "./model";

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
  load(): Promise<void>;
  /** Committed node moves (layout only). */
  moveNodes(moves: Map<string, Point>): void;
  /** Place nodes without saved positions (initial placement or Arrange). */
  placeNodes(positions: Map<string, Point>, options?: { replace?: boolean }): void;
  setViewport(viewport: CanvasViewport): void;
  setFilters(patch: Partial<ViewFilters>): void;
  hide(ids: string[]): void;
  unhide(ids: string[]): void;
  unhideAll(): void;
  toggleCollapsed(id: string): void;
  setFocus(focus: ViewFocus | null): void;
  /** Save now (e.g. before the window closes). */
  flush(): Promise<void>;
}

export const SAVE_DELAY_MS = 600;

export function emptyView(id: string, name: string): SavedView {
  return {
    id,
    name,
    revision: 0,
    positions: {},
    viewport: null,
    filters: { ...defaultFilters },
    hidden: [],
    collapsed: [],
    focus: null,
  };
}

export function createViewStore(id: string, name: string, options: { saveDelayMs?: number } = {}): ViewStore {
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
        const outcome = await graphApi().viewSave(view);
        if (outcome.kind === "saved") {
          // Keep edits made while saving; adopt only the new revision token.
          use.setState((s) => ({ view: s.view ? { ...s.view, revision: outcome.view.revision } : outcome.view }));
          use.setState({ save: dirtyAgain ? "pending" : "saved" });
        } else {
          use.setState({ view: outcome.current, save: "saved" });
          notify({
            id: `graph-view-conflict-${id}`,
            tone: "warning",
            title: `The ${name} view changed elsewhere`,
            body: "Showing its latest saved layout; your last change to this view was not saved.",
          });
        }
      } catch (error) {
        use.setState({ save: "error", saveError: errorText(error) });
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
        const saved = await graphApi().viewGet(id);
        use.setState({ view: saved ?? emptyView(id, name), loading: false, save: "idle" });
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
        viewport: { x: Math.round(viewport.x), y: Math.round(viewport.y), zoom: Math.round(viewport.zoom * 1000) / 1000 },
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
    setFocus(focus) {
      update((view) => ({ ...view, focus }));
    },
    async flush() {
      if (timer) {
        clearTimeout(timer);
        timer = null;
        await save();
      } else if (saving) await saving;
    },
  };
}
