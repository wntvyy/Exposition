// Saved workflows changed: a draft was created, saved, or imported here, or
// something else (an agent task) saved one, which arrives as a graph change.
// Views that list saved workflows or show their revisions (the Workflows
// list, the composite steps of an open parent) re-read them. Deletions and
// restores arrive as project operations (`onProjectOp`).
import { graphEvents } from "../../lib/graphIpc";

type Listener = () => void;

const listeners = new Set<Listener>();
/** Several signals in a burst (a local save and its graph change) refresh once. */
const SETTLE_MS = 150;
let timer: ReturnType<typeof setTimeout> | null = null;
let listening = false;

function schedule(): void {
  if (timer !== null) clearTimeout(timer);
  timer = setTimeout(() => {
    timer = null;
    for (const listener of [...listeners]) listener();
  }, SETTLE_MS);
}

/** A workflow was saved or imported through this window. */
export function workflowsChanged(): void {
  schedule();
}

function ensureListening(): void {
  if (listening) return;
  listening = true;
  try {
    // Workflow saves are graph transactions; file extraction never saves one.
    void graphEvents
      .graph((event) => {
        if (event.kind === "changed" && event.origin.kind !== "extracted") schedule();
      })
      .catch(() => {
        // Not running inside the desktop shell (unit tests).
      });
    void graphEvents.graphResync(() => schedule()).catch(() => {});
  } catch {
    // Not running inside the desktop shell (unit tests).
  }
}

/** Subscribe to saved-workflow changes; returns an unsubscribe function. */
export function onWorkflowsChanged(listener: Listener): () => void {
  ensureListening();
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
