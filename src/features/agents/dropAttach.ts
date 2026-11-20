// Files dragged from the operating system onto the Agent chat are attached
// to that conversation's draft, like files chosen with Attach files….
//
// The desktop shell delivers OS drags as native events (the webview's HTML5
// drop has no paths); the one listener and the routing live in
// `workspace/dropTarget.ts`; this module registers a zone for it. The chat is
// an eligible target wherever a composer is shown: the element under the
// pointer is inside a composer, or inside its chat region (`data-agent-chat`:
// the transcript and the composer of a task, or the new-task view). While a
// drag is over it, that composer is highlighted; the highlight goes when the
// drag leaves, is canceled, is dropped, or the composer goes away.
//
// A drop is bound to the draft of the composer it was dropped on (and to the
// project that was open), so files read after a task or project switch still
// land in that draft, or are discarded when it no longer exists. Dropping
// attaches only: nothing is sent, imported into the project, or started.
import { create } from "zustand";

import { registerNativeDropZone, type NativeDropZone } from "../workspace/nativeDropZones";
import { drafts, NEW_TASK_DRAFT } from "./drafts";
import { useAgents } from "./store";

/** The composer (its zone id) a drag is over, if any. */
export const useAgentDrop = create<{ zone: string | null }>(() => ({ zone: null }));

/** The composer a point belongs to: the one under it, or the one of the chat region under it. */
export function composerAt(element: Element | null): { zone: string; draftKey: string } | null {
  if (!element) return null;
  const composer =
    element.closest("[data-agent-drop-zone]") ?? element.closest("[data-agent-chat]")?.querySelector("[data-agent-drop-zone]") ?? null;
  const zone = composer?.getAttribute("data-agent-drop-zone");
  const draftKey = composer?.getAttribute("data-draft-key");
  return zone && draftKey ? { zone, draftKey } : null;
}

function setZone(zone: string | null): void {
  if (useAgentDrop.getState().zone !== zone) useAgentDrop.setState({ zone });
}

/**
 * Whether a draft still exists as it did when files were dropped on it: the
 * new-task draft belongs to the project that was open; a task's draft to a
 * task that is still listed.
 */
function bound(draftKey: string): () => boolean {
  if (draftKey === NEW_TASK_DRAFT) {
    const root = useAgents.getState().workspace?.root ?? null;
    return () => (useAgents.getState().workspace?.root ?? null) === root;
  }
  return () => useAgents.getState().tasks.some((task) => task.id === draftKey);
}

export const agentDrop: NativeDropZone & { leave(zone: string): void } = {
  hover(element) {
    const target = composerAt(element);
    setZone(target?.zone ?? null);
    return target !== null;
  },

  drop(element, paths) {
    const target = composerAt(element);
    setZone(null);
    if (!target) return false;
    // Claimed even when the drop carries no file: it was meant for the chat.
    if (paths.length) void drafts.dropFiles(target.draftKey, paths, bound(target.draftKey));
    return true;
  },

  clear() {
    setZone(null);
  },

  /** A composer went away or shows another conversation: its highlight goes with it. */
  leave(zone) {
    if (useAgentDrop.getState().zone === zone) setZone(null);
  },
};

let started = false;

/** Take OS file drops on the chat (once). */
export function startAgentDrop(): void {
  if (started) return;
  started = true;
  registerNativeDropZone(agentDrop);
}
