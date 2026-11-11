// Workbench layout state: region visibility, sizes, active views, and the
// editor split. Persisted to localStorage (guarded; corrupt or missing data
// falls back to defaults). Narrow windows auto-collapse the auxiliary panel,
// then the sidebar, without changing the user's saved preference.
import { create } from "zustand";

import { parseJson, readStored, writeStored } from "../ui/storage";

export type Region = "sidebar" | "panel" | "auxiliary";

export interface RegionLayout {
  visible: boolean;
  /** Pixels: width for sidebar/auxiliary, height for the panel. */
  size: number;
  /** View id shown in the region; `null` means the first registered view. */
  activeView: string | null;
}

export interface PersistedLayout {
  sidebar: RegionLayout;
  panel: RegionLayout;
  auxiliary: RegionLayout;
  /** Width of the primary editor group, in percent, when two groups are shown. */
  editorSplit: number;
}

export const LAYOUT_STORAGE_KEY = "exposition.workbench.layout";
const LAYOUT_VERSION = 1;

/** Below this window width the auxiliary panel auto-collapses. */
export const AUXILIARY_COLLAPSE_WIDTH = 1100;
/** Below this window width the sidebar auto-collapses. */
export const SIDEBAR_COLLAPSE_WIDTH = 860;

export const SIZE_LIMITS: Record<Region, { min: number; max: number; initial: number }> = {
  sidebar: { min: 170, max: 600, initial: 260 },
  panel: { min: 100, max: 1200, initial: 240 },
  auxiliary: { min: 220, max: 640, initial: 320 },
};
const SPLIT_LIMITS = { min: 20, max: 80, initial: 50 };

export const defaultLayout: PersistedLayout = {
  sidebar: { visible: true, size: SIZE_LIMITS.sidebar.initial, activeView: null },
  panel: { visible: true, size: SIZE_LIMITS.panel.initial, activeView: null },
  auxiliary: { visible: true, size: SIZE_LIMITS.auxiliary.initial, activeView: null },
  editorSplit: SPLIT_LIMITS.initial,
};

const clamp = (value: number, min: number, max: number) => Math.min(max, Math.max(min, value));

function parseRegion(value: unknown, region: Region): RegionLayout {
  const fallback = defaultLayout[region];
  if (typeof value !== "object" || value === null) return { ...fallback };
  const v = value as Record<string, unknown>;
  const limits = SIZE_LIMITS[region];
  return {
    visible: typeof v.visible === "boolean" ? v.visible : fallback.visible,
    size: typeof v.size === "number" && Number.isFinite(v.size) ? clamp(v.size, limits.min, limits.max) : fallback.size,
    activeView: typeof v.activeView === "string" ? v.activeView : fallback.activeView,
  };
}

export function parseLayout(raw: string | null): PersistedLayout {
  const value = parseJson(raw);
  if (typeof value !== "object" || value === null || (value as { version?: unknown }).version !== LAYOUT_VERSION) {
    return structuredClone(defaultLayout);
  }
  const v = value as Record<string, unknown>;
  return {
    sidebar: parseRegion(v.sidebar, "sidebar"),
    panel: parseRegion(v.panel, "panel"),
    auxiliary: parseRegion(v.auxiliary, "auxiliary"),
    editorSplit:
      typeof v.editorSplit === "number" && Number.isFinite(v.editorSplit)
        ? clamp(v.editorSplit, SPLIT_LIMITS.min, SPLIT_LIMITS.max)
        : SPLIT_LIMITS.initial,
  };
}

export function serializeLayout(state: PersistedLayout): string {
  const { sidebar, panel, auxiliary, editorSplit } = state;
  return JSON.stringify({ version: LAYOUT_VERSION, sidebar, panel, auxiliary, editorSplit });
}

export type Visibility = Record<Region, boolean>;

/** Visible regions for a window width. `forced` marks regions the user opened while auto-collapsed. */
export function effectiveVisibility(
  preferred: Visibility,
  viewportWidth: number,
  forced: Partial<Visibility>,
): Visibility {
  const auxCollapsed = viewportWidth < AUXILIARY_COLLAPSE_WIDTH;
  const sidebarCollapsed = viewportWidth < SIDEBAR_COLLAPSE_WIDTH;
  return {
    sidebar: preferred.sidebar && (!sidebarCollapsed || Boolean(forced.sidebar)),
    panel: preferred.panel,
    auxiliary: preferred.auxiliary && (!auxCollapsed || Boolean(forced.auxiliary)),
  };
}

export interface LayoutState extends PersistedLayout {
  viewportWidth: number;
  /** Regions opened explicitly while the window is narrow; cleared when it widens. */
  forced: Partial<Visibility>;
}

function initialState(): LayoutState {
  return {
    ...parseLayout(readStored(LAYOUT_STORAGE_KEY)),
    viewportWidth: typeof window === "undefined" ? 1440 : window.innerWidth,
    forced: {},
  };
}

export const useLayout = create<LayoutState>(initialState);

function persist() {
  writeStored(LAYOUT_STORAGE_KEY, serializeLayout(useLayout.getState()));
}

function patchRegion(region: Region, patch: Partial<RegionLayout>) {
  useLayout.setState((state) => ({ [region]: { ...state[region], ...patch } }));
  persist();
}

function visibilityOf(state: LayoutState): Visibility {
  return effectiveVisibility(
    { sidebar: state.sidebar.visible, panel: state.panel.visible, auxiliary: state.auxiliary.visible },
    state.viewportWidth,
    state.forced,
  );
}

function autoCollapsed(region: Region, width: number): boolean {
  if (region === "sidebar") return width < SIDEBAR_COLLAPSE_WIDTH;
  if (region === "auxiliary") return width < AUXILIARY_COLLAPSE_WIDTH;
  return false;
}

export const layout = {
  isVisible(region: Region): boolean {
    return visibilityOf(useLayout.getState())[region];
  },

  /**
   * Show or hide a region. In a window narrow enough to auto-collapse it,
   * showing force-opens it (and saves "visible"), while hiding only dismisses
   * it, keeping the saved preference for when the window widens again.
   */
  setVisible(region: Region, visible: boolean): void {
    const state = useLayout.getState();
    if (autoCollapsed(region, state.viewportWidth)) {
      useLayout.setState({ forced: { ...state.forced, [region]: visible } });
      if (visible) patchRegion(region, { visible });
      return;
    }
    const forced = { ...state.forced };
    delete forced[region];
    useLayout.setState({ forced });
    patchRegion(region, { visible });
  },

  toggle(region: Region): void {
    layout.setVisible(region, !layout.isVisible(region));
  },

  setSize(region: Region, size: number): void {
    const { min, max } = SIZE_LIMITS[region];
    const next = clamp(Math.round(size), min, max);
    if (useLayout.getState()[region].size !== next) patchRegion(region, { size: next });
  },

  setActiveView(region: Region, viewId: string): void {
    if (useLayout.getState()[region].activeView !== viewId) patchRegion(region, { activeView: viewId });
  },

  /** Activate a view and make its region visible. */
  showView(region: Region, viewId: string): void {
    layout.setActiveView(region, viewId);
    if (!layout.isVisible(region)) layout.setVisible(region, true);
  },

  setEditorSplit(percent: number): void {
    const next = clamp(percent, SPLIT_LIMITS.min, SPLIT_LIMITS.max);
    if (Math.abs(useLayout.getState().editorSplit - next) > 0.01) {
      useLayout.setState({ editorSplit: next });
      persist();
    }
  },

  setViewportWidth(width: number): void {
    const state = useLayout.getState();
    if (state.viewportWidth === width) return;
    // Forced-open regions return to automatic behavior once the window is wide enough.
    const forced = { ...state.forced };
    for (const region of ["sidebar", "auxiliary"] as const) {
      if (!autoCollapsed(region, width)) delete forced[region];
    }
    useLayout.setState({ viewportWidth: width, forced });
  },
};

/** React hook: effective region visibility. */
export function useVisibility(): Visibility {
  const sidebar = useLayout((s) => visibilityOf(s).sidebar);
  const panel = useLayout((s) => visibilityOf(s).panel);
  const auxiliary = useLayout((s) => visibilityOf(s).auxiliary);
  return { sidebar, panel, auxiliary };
}

/** Test helper: reset the store from (cleared) storage. */
export function resetLayoutForTests(): void {
  useLayout.setState(initialState(), true);
}
