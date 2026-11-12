// Graph service state shared by the Graph page, Mathematics, and the Editor's
// dependency inspector: service status, vocabulary, and the latest known graph
// revision. Started on first use only, so the app never touches the graph
// while no graph view is open. Correctness never depends on receiving every
// event: a `graph:resync` (or a workspace change) re-reads everything.
import { create } from "zustand";

import { errorText, graphApi, type GraphEvent } from "./api";
import type { GraphStatus, Vocabulary } from "./model";

export interface GraphState {
  started: boolean;
  status: GraphStatus | null;
  statusError: string | null;
  vocabulary: Vocabulary | null;
  vocabularyError: string | null;
  /** Latest known graph revision (status, change, and reconcile events). */
  revision: number;
  /** Bumped when everything must be re-read (resync, folder change). */
  epoch: number;
}

const initial = (): GraphState => ({
  started: false,
  status: null,
  statusError: null,
  vocabulary: null,
  vocabularyError: null,
  revision: 0,
  epoch: 0,
});

export const useGraph = create<GraphState>(initial);

let stopEvents: (() => void) | null = null;

function bumpRevision(revision: number) {
  useGraph.setState((s) => (revision > s.revision ? { revision } : s));
}

function onEvent(event: GraphEvent) {
  switch (event.kind) {
    case "status":
      useGraph.setState({ status: event.status, statusError: null });
      bumpRevision(event.status.revision);
      break;
    case "changed":
    case "reconciled":
      bumpRevision(event.revision);
      break;
  }
}

async function loadStatus() {
  try {
    const status = await graphApi().status();
    useGraph.setState({ status, statusError: null });
    bumpRevision(status.revision);
  } catch (error) {
    useGraph.setState({ status: null, statusError: errorText(error) });
  }
}

async function loadVocabulary() {
  try {
    useGraph.setState({ vocabulary: await graphApi().vocabulary(), vocabularyError: null });
  } catch (error) {
    useGraph.setState({ vocabularyError: errorText(error) });
  }
}

export const graph = {
  /** Subscribe to graph events and load status + vocabulary (idempotent). */
  start(): void {
    if (useGraph.getState().started) return;
    useGraph.setState({ started: true });
    stopEvents = graphApi().subscribe(onEvent, () => void graph.resync());
    void loadStatus();
    void loadVocabulary();
  },

  /** Re-read status and vocabulary and ask every view to re-read its data. */
  async resync(): Promise<void> {
    useGraph.setState((s) => ({ epoch: s.epoch + 1, revision: 0 }));
    await Promise.all([loadStatus(), loadVocabulary()]);
  },

  /** The open folder changed: re-read everything if the graph is in use. */
  workspaceChanged(): void {
    if (useGraph.getState().started) void graph.resync();
  },

  /** Record a revision the caller learned (e.g. from an applied change). */
  noteRevision(revision: number): void {
    bumpRevision(revision);
  },
};

/** Test helper: stop listening and reset state. */
export function resetGraphForTests(): void {
  stopEvents?.();
  stopEvents = null;
  useGraph.setState(initial(), true);
}

/** Is the graph usable for reads (attached or temporary read-only)? */
export function canRead(status: GraphStatus | null): boolean {
  return status !== null && (status.availability.kind === "attached" || status.availability.kind === "temporaryReadOnly");
}

/** May graph changes be proposed/applied? */
export function canWrite(status: GraphStatus | null): boolean {
  return status !== null && status.availability.kind === "attached";
}
