// Open-editor contract: which resources are open in which editor group, their
// mode and tab state. Features open/close editors through `editors`; the
// workbench shell renders `useEditors` state. Document text never lives here.
import { create } from "zustand";

import { EDITOR_PAGE, pages } from "./pages";
import type { DocumentMode, EditorResource } from "./registry";

export type EditorGroupId = "primary" | "secondary";

export interface OpenEditor {
  resource: EditorResource;
  mode: DocumentMode;
  /** Unsaved changes indicator, set by the editor that owns the resource. */
  dirty: boolean;
  /** A preview tab is replaced by the next preview open in the same group. */
  preview: boolean;
}

export interface EditorGroup {
  id: EditorGroupId;
  editors: OpenEditor[];
  activeKey: string | null;
}

/**
 * Invariants (kept by every change made through `editors`):
 * - The right group shows editors only while the left one does: when the left
 *   group's last editor closes, the right group takes its place.
 * - The active group holds an editor whenever any group does; an empty right
 *   group (not shown) is never active.
 * - A group's `activeKey` names one of its editors (null only when empty).
 */
export interface EditorsState {
  groups: Record<EditorGroupId, EditorGroup>;
  activeGroup: EditorGroupId;
  /**
   * Bumped when a group closes (its last editor closed) and another group is
   * left with editors: the workbench moves keyboard focus into the active group.
   */
  focusRequest: number;
}

export interface OpenEditorOptions {
  mode?: DocumentMode;
  group?: EditorGroupId;
  preview?: boolean;
  /**
   * Show the Editor page (default true): opening a tab from the Research,
   * Agents, or Graph page reveals it. Pass false to open it in the background.
   */
  reveal?: boolean;
}

/** Resolves true when the editor may close (e.g. after a save/discard prompt). */
export type CloseGuard = () => Promise<boolean>;

const emptyGroup = (id: EditorGroupId): EditorGroup => ({ id, editors: [], activeKey: null });

export const useEditors = create<EditorsState>(() => ({
  groups: { primary: emptyGroup("primary"), secondary: emptyGroup("secondary") },
  activeGroup: "primary",
  focusRequest: 0,
}));

const guards = new Map<string, CloseGuard>();

function withValidActiveKey(group: EditorGroup): EditorGroup {
  if (group.activeKey !== null && group.editors.some((e) => e.resource.key === group.activeKey)) return group;
  const activeKey = group.editors[0]?.resource.key ?? null;
  return activeKey === group.activeKey ? group : { ...group, activeKey };
}

/** Restore the `EditorsState` invariants after a change from `prev` to `next`. */
export function normalizeEditors(next: EditorsState, prev: EditorsState): EditorsState {
  let primary = withValidActiveKey(next.groups.primary);
  let secondary = withValidActiveKey(next.groups.secondary);
  let activeGroup = next.activeGroup;
  if (primary.editors.length === 0 && secondary.editors.length > 0) {
    // The left group closed: the right group takes its place (VS Code closes an empty group).
    primary = { ...secondary, id: "primary" };
    secondary = emptyGroup("secondary");
    activeGroup = "primary";
  }
  if (activeGroup === "secondary" && secondary.editors.length === 0) activeGroup = "primary";
  const closedGroup = (["primary", "secondary"] as const).some(
    (id) => prev.groups[id].editors.length > 0 && next.groups[id].editors.length === 0,
  );
  const survivor = activeGroup === "primary" ? primary : secondary;
  const focusRequest = closedGroup && survivor.editors.length > 0 ? prev.focusRequest + 1 : next.focusRequest;
  const groups =
    primary === next.groups.primary && secondary === next.groups.secondary ? next.groups : { primary, secondary };
  return { ...next, groups, activeGroup, focusRequest };
}

/** Apply a change to the open-editor state, keeping its invariants. */
function change(fn: (state: EditorsState) => Partial<EditorsState>) {
  useEditors.setState((state) => normalizeEditors({ ...state, ...fn(state) }, state));
}

function updateGroup(id: EditorGroupId, fn: (group: EditorGroup) => EditorGroup) {
  change((state) => ({ groups: { ...state.groups, [id]: fn(state.groups[id]) } }));
}

/** The group without `key`; when it was the active tab, its neighbor becomes active. */
function withoutEditor(group: EditorGroup, key: string): EditorGroup {
  const index = group.editors.findIndex((e) => e.resource.key === key);
  if (index < 0) return group;
  const remaining = group.editors.filter((e) => e.resource.key !== key);
  const activeKey =
    group.activeKey === key ? (remaining[Math.min(index, remaining.length - 1)]?.resource.key ?? null) : group.activeKey;
  return { ...group, editors: remaining, activeKey };
}

function findEditor(key: string): { group: EditorGroupId; editor: OpenEditor } | undefined {
  const { groups, activeGroup } = useEditors.getState();
  const order: EditorGroupId[] = activeGroup === "primary" ? ["primary", "secondary"] : ["secondary", "primary"];
  for (const id of order) {
    const editor = groups[id].editors.find((e) => e.resource.key === key);
    if (editor) return { group: id, editor };
  }
  return undefined;
}

function patchEditor(key: string, patch: Partial<OpenEditor>) {
  for (const id of ["primary", "secondary"] as const) {
    updateGroup(id, (group) => ({
      ...group,
      editors: group.editors.map((e) => (e.resource.key === key ? { ...e, ...patch } : e)),
    }));
  }
}

export const editors = {
  /** Open (or focus) a resource. Re-opening an open resource activates it and applies `mode`. */
  open(resource: EditorResource, options: OpenEditorOptions = {}): void {
    const groupId = options.group ?? useEditors.getState().activeGroup;
    const existing = useEditors.getState().groups[groupId].editors.find((e) => e.resource.key === resource.key);
    if (existing) {
      updateGroup(groupId, (group) => ({
        ...group,
        activeKey: resource.key,
        editors: group.editors.map((e) =>
          e.resource.key === resource.key
            ? { ...e, mode: options.mode ?? e.mode, preview: e.preview && (options.preview ?? true) }
            : e,
        ),
      }));
    } else {
      const entry: OpenEditor = {
        resource,
        mode: options.mode ?? "source",
        dirty: false,
        preview: options.preview ?? false,
      };
      updateGroup(groupId, (group) => {
        const kept = entry.preview ? group.editors.filter((e) => !(e.preview && !e.dirty)) : group.editors;
        return { ...group, editors: [...kept, entry], activeKey: resource.key };
      });
    }
    change(() => ({ activeGroup: groupId }));
    if (options.reveal !== false) pages.show(EDITOR_PAGE);
  },

  /** Close a resource in every group, honoring its close guard. Resolves false if vetoed. */
  async close(key: string): Promise<boolean> {
    const guard = guards.get(key);
    if (guard && !(await guard())) return false;
    guards.delete(key);
    // Both groups in one change: closing the left group's last editor moves the
    // right group into its place.
    change((state) => ({
      groups: {
        primary: withoutEditor(state.groups.primary, key),
        secondary: withoutEditor(state.groups.secondary, key),
      },
    }));
    return true;
  },

  /**
   * Close a resource in one group. When another group still shows it, only this
   * tab is removed (no guard); otherwise this behaves like `close`.
   */
  async closeInGroup(key: string, group: EditorGroupId): Promise<boolean> {
    const other: EditorGroupId = group === "primary" ? "secondary" : "primary";
    const { groups } = useEditors.getState();
    if (!groups[other].editors.some((e) => e.resource.key === key)) return editors.close(key);
    updateGroup(group, (g) => withoutEditor(g, key));
    return true;
  },

  /**
   * Close every editor in a group, each honoring its close guard; stops at the
   * first veto. The neighbor group becomes active (see `EditorsState`).
   */
  async closeGroup(group: EditorGroupId): Promise<boolean> {
    for (const editor of [...useEditors.getState().groups[group].editors]) {
      if (!(await editors.closeInGroup(editor.resource.key, group))) return false;
    }
    return true;
  },

  /**
   * Make a group active (keyboard/pointer focus moved into it). An empty group
   * becomes active only when no group has editors (then the left one is).
   */
  focusGroup(group: EditorGroupId): void {
    if (useEditors.getState().activeGroup !== group) change(() => ({ activeGroup: group }));
  },

  activate(key: string, group?: EditorGroupId): void {
    const found = group ? { group } : findEditor(key);
    if (!found) return;
    change((state) => ({
      groups: { ...state.groups, [found.group]: { ...state.groups[found.group], activeKey: key } },
      activeGroup: found.group,
    }));
  },

  setMode(key: string, mode: DocumentMode): void {
    patchEditor(key, { mode });
  },

  setDirty(key: string, dirty: boolean): void {
    patchEditor(key, dirty ? { dirty, preview: false } : { dirty });
  },

  setTitle(key: string, title: string): void {
    for (const id of ["primary", "secondary"] as const) {
      updateGroup(id, (group) => ({
        ...group,
        editors: group.editors.map((e) => (e.resource.key === key ? { ...e, resource: { ...e.resource, title } } : e)),
      }));
    }
  },

  /** Register a guard consulted before the resource's editor closes. */
  registerCloseGuard(key: string, guard: CloseGuard): () => void {
    guards.set(key, guard);
    return () => {
      if (guards.get(key) === guard) guards.delete(key);
    };
  },

  /** The active editor of the active group. */
  active(): OpenEditor | undefined {
    const { groups, activeGroup } = useEditors.getState();
    const group = groups[activeGroup];
    return group.editors.find((e) => e.resource.key === group.activeKey);
  },

  all(): OpenEditor[] {
    const { groups } = useEditors.getState();
    return [...groups.primary.editors, ...groups.secondary.editors];
  },
};

/** React hook: the active editor of the active group. */
export function useActiveEditor(): OpenEditor | undefined {
  return useEditors((state) => {
    const group = state.groups[state.activeGroup];
    return group.editors.find((e) => e.resource.key === group.activeKey);
  });
}
