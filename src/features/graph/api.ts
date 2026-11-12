// The Graph feature's service boundary. Views call `graphApi()`; the IPC
// implementation (generated bindings → normalized model) is installed by
// `register.tsx`, and tests install an in-memory one. Nothing here decides
// semantics: proposals and applies are validated by the backend.
import type {
  ApplyOutcome,
  EntityDetail,
  GraphOperation,
  GraphStatus,
  ImpactResult,
  MathOverview,
  Proposal,
  SavedView,
  Snapshot,
  SnapshotQuery,
  SourceInspection,
  ViewSaveOutcome,
  Vocabulary,
} from "./model";

export type GraphEvent =
  | { kind: "status"; status: GraphStatus }
  | { kind: "changed"; revision: number; entities: string[]; relations: string[] }
  | { kind: "reconciled"; revision: number };

export interface ViewSummary {
  id: string;
  name: string;
}

export interface GraphApi {
  status(): Promise<GraphStatus>;
  vocabulary(): Promise<Vocabulary>;
  snapshot(query: SnapshotQuery): Promise<Snapshot>;
  entity(id: string): Promise<EntityDetail>;
  inspectAtSource(path: string, offset: number, revision: number | null): Promise<SourceInspection>;
  impact(entities: string[], depth?: number): Promise<ImpactResult>;
  propose(baseRevision: number, operations: GraphOperation[]): Promise<Proposal>;
  apply(proposal: Proposal, expectedRevision: number): Promise<ApplyOutcome>;
  views(): Promise<ViewSummary[]>;
  /** A saved view, or null when none is saved under this id yet. */
  viewGet(id: string): Promise<SavedView | null>;
  viewSave(view: SavedView): Promise<ViewSaveOutcome>;
  mathOverview(): Promise<MathOverview>;
  /** Subscribe to `graph:event` / `graph:resync`; returns an unsubscribe function. */
  subscribe(onEvent: (event: GraphEvent) => void, onResync: () => void): () => void;
}

/** Error raised when the graph service cannot answer (shown, never hidden). */
export class GraphUnavailableError extends Error {
  readonly code: string;
  constructor(message: string, code = "graph.unavailable") {
    super(message);
    this.code = code;
  }
}

const missing = (): Promise<never> =>
  Promise.reject(new GraphUnavailableError("The project graph service is not available in this build."));

/** Used until an implementation is installed: every call reports the service unavailable. */
export const unavailableGraphApi: GraphApi = {
  status: missing,
  vocabulary: missing,
  snapshot: missing,
  entity: missing,
  inspectAtSource: missing,
  impact: missing,
  propose: missing,
  apply: missing,
  views: missing,
  viewGet: missing,
  viewSave: missing,
  mathOverview: missing,
  subscribe: () => () => {},
};

let current: GraphApi = unavailableGraphApi;

export function graphApi(): GraphApi {
  return current;
}

/** Install the implementation (IPC at startup; in-memory in tests). Returns the previous one. */
export function setGraphApi(api: GraphApi): GraphApi {
  const previous = current;
  current = api;
  return previous;
}

export function errorText(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === "object" && error !== null && "message" in error) return String((error as { message: unknown }).message);
  return String(error);
}
