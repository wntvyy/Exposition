// The Graph feature's service boundary. Views call `graphApi()`; by default it
// is the typed IPC (`src/lib/graphIpc.ts`), and tests install an in-memory
// implementation. Nothing here decides semantics: proposals and applies are
// validated by the backend.
import type { ApplyChangeRequest } from "../../bindings/ApplyChangeRequest";
import type { ApplyOutcome } from "../../bindings/ApplyOutcome";
import type { ChangeProposal } from "../../bindings/ChangeProposal";
import type { ChangeRequest } from "../../bindings/ChangeRequest";
import type { EntityDetail } from "../../bindings/EntityDetail";
import type { GraphEvent } from "../../bindings/GraphEvent";
import type { GraphQuery } from "../../bindings/GraphQuery";
import type { GraphSnapshot } from "../../bindings/GraphSnapshot";
import type { GraphStatus } from "../../bindings/GraphStatus";
import type { GraphView } from "../../bindings/GraphView";
import type { GraphViewInput } from "../../bindings/GraphViewInput";
import type { GraphViewSummary } from "../../bindings/GraphViewSummary";
import type { ImpactReport } from "../../bindings/ImpactReport";
import type { ImpactRequest } from "../../bindings/ImpactRequest";
import type { InspectAtSourceRequest } from "../../bindings/InspectAtSourceRequest";
import type { MathOverview } from "../../bindings/MathOverview";
import type { RecoveryOption } from "../../bindings/RecoveryOption";
import type { SourceInspection } from "../../bindings/SourceInspection";
import { graphCommands, graphEvents } from "../../lib/graphIpc";
import { indexVocabulary, type VocabularyIndex } from "./model";

export interface GraphApi {
  status(): Promise<GraphStatus>;
  vocabulary(): Promise<VocabularyIndex>;
  snapshot(query: GraphQuery): Promise<GraphSnapshot>;
  entity(id: string): Promise<EntityDetail>;
  inspectAtSource(request: InspectAtSourceRequest): Promise<SourceInspection>;
  impact(request: ImpactRequest): Promise<ImpactReport>;
  propose(request: ChangeRequest): Promise<ChangeProposal>;
  apply(request: ApplyChangeRequest): Promise<ApplyOutcome>;
  views(): Promise<GraphViewSummary[]>;
  viewGet(id: string): Promise<GraphView>;
  /** Rejects with code `graph.staleView` when `expectedRevision` no longer matches. */
  viewSave(view: GraphViewInput): Promise<GraphView>;
  mathOverview(): Promise<MathOverview>;
  recover(option: RecoveryOption): Promise<GraphStatus>;
  /** Subscribe to `graph:event` / `graph:resync`; returns an unsubscribe function. */
  subscribe(onEvent: (event: GraphEvent) => void, onResync: () => void): () => void;
}

export const ipcGraphApi: GraphApi = {
  status: () => graphCommands.status(),
  vocabulary: async () => indexVocabulary(await graphCommands.vocabulary()),
  snapshot: (query) => graphCommands.snapshot(query),
  entity: (id) => graphCommands.entity(id),
  inspectAtSource: (request) => graphCommands.inspectAtSource(request),
  impact: (request) => graphCommands.impact(request),
  propose: (request) => graphCommands.proposeChange(request),
  apply: (request) => graphCommands.applyChange(request),
  views: () => graphCommands.views(),
  viewGet: (id) => graphCommands.viewGet(id),
  viewSave: (view) => graphCommands.viewSave(view),
  mathOverview: () => graphCommands.mathOverview(),
  recover: (option) => graphCommands.recover(option),
  subscribe(onEvent, onResync) {
    let alive = true;
    const offs: (() => void)[] = [];
    const keep = (pending: Promise<() => void>) =>
      pending.then(
        (off) => (alive ? offs.push(off) : off()),
        () => {}, // Not running in the desktop shell (unit tests).
      );
    void keep(graphEvents.graph(onEvent));
    void keep(graphEvents.graphResync(onResync));
    return () => {
      alive = false;
      for (const off of offs.splice(0)) off();
    };
  },
};

let current: GraphApi = ipcGraphApi;

export function graphApi(): GraphApi {
  return current;
}

/** Install an implementation (tests). Returns the previous one. */
export function setGraphApi(api: GraphApi): GraphApi {
  const previous = current;
  current = api;
  return previous;
}

/** Message of a command rejection (`{ code, message }`), an Error, or anything else. */
export function errorText(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === "object" && error !== null && "message" in error) return String((error as { message: unknown }).message);
  return String(error);
}

export function errorCode(error: unknown): string | null {
  if (typeof error === "object" && error !== null && "code" in error) return String((error as { code: unknown }).code);
  return null;
}
