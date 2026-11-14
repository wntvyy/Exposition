// Focus-scoped operation commands (P1 spec §3, decision 8).
//
// Surfaces (the Files and Research explorers, graph canvases/tables, the
// Workflows list and canvas, run and agent task lists) register their root
// element and what Copy/Paste/Duplicate/Delete/Delete Permanently mean for
// their current selection. The generic commands below are bound through the
// existing registry/dispatcher and act ONLY on the surface that holds focus:
// - never while focus is in Monaco, a text field, a rename field, a terminal,
//   a dialog, or a menu (keybinding scope "workbench" + an explicit check),
// - never on selected text (Copy leaves a text selection to the browser),
// - never for an auto-repeated destructive key (held Delete),
// - never while IME composition is active (the dispatcher ignores it),
// - from the command palette, for the surface that had focus before it opened.
import { useEffect, useRef, type RefObject } from "react";

import { detectPlatform, isEditableTarget } from "../../app/keybindings";
import type { CommandContribution } from "../../app/registry";
import { commandPalette } from "../../app/shell/CommandPalette";
import { operationInFlight, opHistory, redoLast, undoLast } from "./client";

export interface SurfaceAction {
  enabled: boolean;
  /** Why it is unavailable (shown in menus). */
  reason?: string;
  /** Menu label override (e.g. "Paste into figures/", "Delete Source File"). */
  label?: string;
  /** Menu description (e.g. recovery: "Moves to the Recycle Bin"). */
  description?: string;
  run: () => void | Promise<void>;
}

export type SurfaceActionId = "copy" | "paste" | "duplicate" | "delete" | "deletePermanently";

export type SurfaceActions = Partial<Record<SurfaceActionId, SurfaceAction>>;

export interface OpSurface {
  id: string;
  /** Root element: focus inside it makes this surface the target of the commands. */
  element: () => HTMLElement | null;
  /** What each command does for the current selection (absent: not supported here). */
  actions: () => SurfaceActions;
  /** Ctrl/Cmd+Z / Shift+Z undo and redo project operations here (default true). */
  history?: boolean;
  /**
   * Whether the focused element stands for the selection (e.g. a canvas node
   * or a table row, not a toolbar button inside the same region). Default: any.
   */
  accepts?: (element: Element) => boolean;
}

const surfaces = new Set<OpSurface>();
let lastSurface: OpSurface | null = null;
let paletteSurface: OpSurface | null = null;
let paletteClosedAt = 0;
let tracking = false;

/** Overlays (dialogs, menus, popovers, the palette) keep the surface that opened them. */
const OVERLAY = ".ui-modal-overlay, .ui-popover, [role='dialog'], [role='alertdialog'], [role='menu'], [role='listbox']";

function inOverlay(element: Element): boolean {
  return element.closest(OVERLAY) !== null;
}

/** The innermost registered surface containing `node`. */
export function surfaceFor(node: EventTarget | null): OpSurface | null {
  if (!(node instanceof Node)) return null;
  let best: OpSurface | null = null;
  let bestElement: HTMLElement | null = null;
  for (const surface of surfaces) {
    const element = surface.element();
    if (!element || !element.contains(node)) continue;
    if (!bestElement || bestElement.contains(element)) {
      best = surface;
      bestElement = element;
    }
  }
  return best;
}

/** The surface whose selection the focused `element` stands for. */
function focusedSurface(element: Element): OpSurface | null {
  const surface = surfaceFor(element);
  return surface && (!surface.accepts || surface.accepts(element)) ? surface : null;
}

function connected(surface: OpSurface | null): OpSurface | null {
  return surface && surfaces.has(surface) && surface.element()?.isConnected ? surface : null;
}

/**
 * The surface the operation commands act on now: the one holding keyboard
 * focus on a non-editable element; while the command palette is open (or has
 * just closed and focus is returning), the one that had focus before it.
 */
export function activeSurface(): OpSurface | null {
  const element = typeof document === "undefined" ? null : document.activeElement;
  if (element && element !== document.body && !inOverlay(element)) {
    // Strict: the focused element itself must be inside a surface (and not edit text).
    if (isEditableTarget(element)) return null;
    return focusedSurface(element);
  }
  // Focus is in the palette (or returning from it to the body): the surface it was opened from.
  if (commandPalette.isOpen() || Date.now() - paletteClosedAt < 1500) return connected(paletteSurface);
  return null;
}

/** Start following focus (idempotent). */
export function startSurfaceTracking(): void {
  if (tracking || typeof document === "undefined") return;
  tracking = true;
  document.addEventListener(
    "focusin",
    (event) => {
      const target = event.target;
      const surface = target instanceof Element ? focusedSurface(target) : null;
      if (surface) lastSurface = surface;
      else if (!(target instanceof Element) || !inOverlay(target)) lastSurface = null;
    },
    true,
  );
  commandPalette.subscribe((open) => {
    if (open) paletteSurface = connected(lastSurface) ?? surfaceFor(document.activeElement);
    else paletteClosedAt = Date.now();
  });
}

/** Register a surface. Returns the disposer. */
export function registerSurface(surface: OpSurface): () => void {
  startSurfaceTracking();
  surfaces.add(surface);
  return () => {
    surfaces.delete(surface);
    if (lastSurface === surface) lastSurface = null;
    if (paletteSurface === surface) paletteSurface = null;
  };
}

/**
 * Register a surface for a component's root element. `actions` is read at
 * command time, so it always sees the latest selection.
 */
export function useOpSurface(
  id: string,
  ref: RefObject<HTMLElement | null>,
  actions: () => SurfaceActions,
  options: { history?: boolean; accepts?: (element: Element) => boolean } = {},
): void {
  const latest = useRef(actions);
  latest.current = actions;
  const { history = true, accepts } = options;
  useEffect(
    () => registerSurface({ id, element: () => ref.current, actions: () => latest.current(), history, accepts }),
    [id, ref, history, accepts],
  );
}

/** Canvas nodes/edges and table rows/cells stand for a graph-like selection; toolbars and tabs do not. */
export function acceptsItems(element: Element): boolean {
  // A canvas wrapper itself (focused after a background click, for Paste) also counts.
  if (element.matches("[data-op-canvas]")) return true;
  return element.closest(".react-flow__node, .react-flow__edge, [role='row'], [role='gridcell'], [role='rowheader'], [data-op-item]") !== null;
}

// ----- the commands -------------------------------------------------------------------

const mac = () => detectPlatform() === "mac";

/** Default bindings (spec §3); macOS equivalents where they differ. */
export const OP_KEYBINDINGS = {
  copy: "Mod+C",
  paste: "Mod+V",
  duplicate: "Mod+D",
  delete: () => (mac() ? "Mod+Backspace" : "Delete"),
  deletePermanently: () => (mac() ? "Mod+Alt+Backspace" : "Shift+Delete"),
  undo: "Mod+Z",
  redo: "Mod+Shift+Z",
} as const;

export function keybindingFor(id: SurfaceActionId | "undo" | "redo"): string {
  const binding = OP_KEYBINDINGS[id];
  return typeof binding === "function" ? binding() : binding;
}

export const OP_COMMAND_IDS: Record<SurfaceActionId | "undo" | "redo", string> = {
  copy: "operations.copy",
  paste: "operations.paste",
  duplicate: "operations.duplicate",
  delete: "operations.delete",
  deletePermanently: "operations.deletePermanently",
  undo: "operations.undo",
  redo: "operations.redo",
};

/** Text is selected inside the active surface (e.g. a log or a detail line): Copy copies that text. */
function hasTextSelection(): boolean {
  const selection = typeof window === "undefined" ? null : window.getSelection();
  if (!selection || selection.isCollapsed || selection.rangeCount === 0) return false;
  const element = activeSurface()?.element();
  const anchor = selection.anchorNode;
  return Boolean(element && anchor && element.contains(anchor));
}

function actionOf(id: SurfaceActionId): SurfaceAction | null {
  const surface = activeSurface();
  if (!surface) return null;
  const action = surface.actions()[id];
  return action?.enabled ? action : null;
}

const TITLES: Record<SurfaceActionId, string> = {
  copy: "Copy Selected Items",
  paste: "Paste Items",
  duplicate: "Duplicate Selected Items",
  delete: "Delete Selected Items",
  deletePermanently: "Delete Selected Items Permanently",
};

const DESTRUCTIVE = new Set<SurfaceActionId>(["delete", "deletePermanently"]);

export function operationCommands(): CommandContribution[] {
  const surfaceCommands = (Object.keys(TITLES) as SurfaceActionId[]).map(
    (id): CommandContribution => ({
      id: OP_COMMAND_IDS[id],
      title: TITLES[id],
      category: "Edit",
      keybinding: keybindingFor(id),
      // Never in text fields, Monaco, or the page editor: they keep their own keys.
      keybindingScope: "workbench",
      ignoreRepeat: DESTRUCTIVE.has(id) || id === "duplicate" || id === "paste",
      enabled: () => {
        if (id === "copy" && hasTextSelection()) return false;
        if (DESTRUCTIVE.has(id) && operationInFlight()) return false;
        return actionOf(id) !== null;
      },
      run: async () => {
        const action = actionOf(id);
        if (action) await action.run();
      },
    }),
  );
  const historyEnabled = () => {
    const surface = activeSurface();
    return surface !== null && surface.history !== false && !operationInFlight();
  };
  return [
    ...surfaceCommands,
    {
      id: OP_COMMAND_IDS.undo,
      title: "Undo Last Deletion",
      category: "Edit",
      keybinding: keybindingFor("undo"),
      keybindingScope: "workbench",
      ignoreRepeat: true,
      enabled: () => historyEnabled() && opHistory.canUndo(),
      run: async () => {
        await undoLast();
      },
    },
    {
      id: OP_COMMAND_IDS.redo,
      title: "Redo Last Deletion",
      category: "Edit",
      keybinding: keybindingFor("redo"),
      keybindingScope: "workbench",
      ignoreRepeat: true,
      enabled: () => historyEnabled() && opHistory.canRedo(),
      run: async () => {
        await redoLast();
      },
    },
  ];
}

/** Test helper: forget every surface and focus memory. */
export function resetSurfacesForTests(): void {
  surfaces.clear();
  lastSurface = null;
  paletteSurface = null;
  paletteClosedAt = 0;
}
