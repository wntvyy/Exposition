// Open-editor contract: which resources are open in which editor group, their
// mode and tab state. Features open/close editors through `editors`; the
// workbench shell renders `useEditors` state. Document text never lives here.
import { create } from "zustand";

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

export interface EditorsState {
  groups: Record<EditorGroupId, EditorGroup>;
  activeGroup: EditorGroupId;
}

export interface OpenEditorOptions {
  mode?: DocumentMode;
  group?: EditorGroupId;
  preview?: boolean;
}

/** Resolves true when the editor may close (e.g. after a save/discard prompt). */
export type CloseGuard = () => Promise<boolean>;

const emptyGroup = (id: EditorGroupId): EditorGroup => ({ id, editors: [], activeKey: null });

export const useEditors = create<EditorsState>(() => ({
  groups: { primary: emptyGroup("primary"), secondary: emptyGroup("secondary") },
  activeGroup: "primary",
}));

const guards = new Map<string, CloseGuard>();

function updateGroup(id: EditorGroupId, fn: (group: EditorGroup) => EditorGroup) {
  useEditors.setState((state) => ({ groups: { ...state.groups, [id]: fn(state.groups[id]) } }));
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
    useEditors.setState({ activeGroup: groupId });
  },

  /** Close a resource in every group, honoring its close guard. Resolves false if vetoed. */
  async close(key: string): Promise<boolean> {
    const guard = guards.get(key);
    if (guard && !(await guard())) return false;
    guards.delete(key);
    for (const id of ["primary", "secondary"] as const) {
      updateGroup(id, (group) => {
        const index = group.editors.findIndex((e) => e.resource.key === key);
        if (index < 0) return group;
        const remaining = group.editors.filter((e) => e.resource.key !== key);
        const activeKey =
          group.activeKey === key
            ? (remaining[Math.min(index, remaining.length - 1)]?.resource.key ?? null)
            : group.activeKey;
        return { ...group, editors: remaining, activeKey };
      });
    }
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
    updateGroup(group, (g) => {
      const index = g.editors.findIndex((e) => e.resource.key === key);
      if (index < 0) return g;
      const remaining = g.editors.filter((e) => e.resource.key !== key);
      const activeKey =
        g.activeKey === key ? (remaining[Math.min(index, remaining.length - 1)]?.resource.key ?? null) : g.activeKey;
      return { ...g, editors: remaining, activeKey };
    });
    return true;
  },

  /** Make a group active (keyboard/pointer focus moved into it), even when it is empty. */
  focusGroup(group: EditorGroupId): void {
    if (useEditors.getState().activeGroup !== group) useEditors.setState({ activeGroup: group });
  },

  activate(key: string, group?: EditorGroupId): void {
    const found = group ? { group } : findEditor(key);
    if (!found) return;
    updateGroup(found.group, (g) => ({ ...g, activeKey: key }));
    useEditors.setState({ activeGroup: found.group });
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
