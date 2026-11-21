// The run history is read in pages. Further pages are added to the runs
// listed (never replacing them), a refresh reads every page loaded so far
// again, events keep merging into the list whatever its length, and a run the
// loaded pages do not list is read by id instead of being taken as missing.
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { ResearchEvent } from "../../bindings/ResearchEvent";
import type { RunSummary } from "../../bindings/RunSummary";

const PAGE = 500;

const run = (n: number, patch: Partial<RunSummary> = {}): RunSummary => ({
  id: `run-${String(n).padStart(6, "0")}`,
  sessionId: "session-0",
  owner: { kind: "user" },
  status: { kind: "succeeded" },
  queued: false,
  origin: { kind: "input" },
  codePreview: `print(${n})`,
  executionCount: 1,
  startedAtMs: 1_000 + n,
  finishedAtMs: 1_001 + n,
  outputs: [],
  error: null,
  ...patch,
});

/** A stand-in for the service: stored runs newest first, pages continued by position. */
const server = vi.hoisted(() => ({
  stored: [] as RunSummary[],
  unreadable: 0,
  /** Continuations refused once with `research.historyChanged`. */
  refuse: new Set<string>(),
  calls: [] as (string | null)[],
  /** `research_run` fails with this. */
  runFailure: null as { code: string; message: string } | null,
  handler: null as ((event: unknown) => void) | null,
  opListener: null as ((result: unknown) => void) | null,
}));

const ipc = vi.hoisted(() => ({
  commands: {
    researchWorkspace: vi.fn(async () => ({ root: "C:/project", trusted: true })),
    researchSessions: vi.fn(async () => []),
    researchRunOutput: vi.fn(async (runId: string) => ({
      runId,
      entries: [{ seq: 1, item: { kind: "stream", name: "stdout", text: "out" } }],
      firstRetainedSeq: 1,
      lastSeq: 1,
      truncated: false,
    })),
    researchRuns: vi.fn(async (request?: { after?: string; limit?: number }) => {
      const after = request?.after ?? null;
      server.calls.push(after);
      if (after !== null && server.refuse.delete(after)) {
        throw { code: "research.historyChanged", message: "The run history changed since this part of it was listed." };
      }
      // A continuation names the last run of the page before.
      const start = after === null ? 0 : server.stored.findIndex((r) => r.id < after);
      const from = start < 0 ? server.stored.length : start;
      const runs = server.stored.slice(from, from + 500);
      const more = from + runs.length < server.stored.length;
      return {
        runs,
        next: more ? runs[runs.length - 1]!.id : null,
        hasMore: more,
        total: server.stored.length + server.unreadable,
        unreadable: server.unreadable,
      };
    }),
    researchRun: vi.fn(async (runId: string) => {
      if (server.runFailure) throw server.runFailure;
      const found = server.stored.find((r) => r.id === runId);
      if (!found) throw { code: "research.unknownRun", message: `unknown run ${runId}` };
      return { found };
    }),
  },
}));

vi.mock("../../lib/ipc", () => ({
  commands: ipc.commands,
  events: {
    research: vi.fn(async (handler: (event: unknown) => void) => {
      server.handler = handler;
      return () => {};
    }),
    researchResync: vi.fn(async () => () => {}),
    trustChanged: vi.fn(async () => () => {}),
  },
  errorMessage: (e: unknown) => (typeof e === "object" && e !== null && "message" in e ? String(e.message) : String(e)),
  isCommandError: (e: unknown) => typeof e === "object" && e !== null && "code" in e,
}));
vi.mock("./ipc", () => ({ setupCommands: { selection: vi.fn(async () => null) } }));
vi.mock("../../app/notifications", () => ({ notify: vi.fn() }));
vi.mock("../../app/registry", () => ({ registry: { runCommand: vi.fn() } }));
vi.mock("../jobs/trust", () => ({ requestWorkspaceTrust: vi.fn(async () => true) }));
vi.mock("../editor", () => ({ activeHandle: () => null, handleForPath: () => null }));
vi.mock("../operations/client", () => ({
  onProjectOp: vi.fn((listener: (result: unknown) => void) => {
    server.opListener = listener;
    return () => {};
  }),
}));
// A manifest is turned into a history entry by the model; here the stand-in
// hands the entry over as it is.
vi.mock("./model", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./model")>()),
  summaryOfManifest: (manifest: unknown) => (manifest as { found: RunSummary }).found,
}));

const store = await import("./store");
const { useResearch, refreshRuns, loadMoreRuns, lookupRun, retryLookup, selectRun, findRun, flushEvents, startResearchTracking } = store;

const ids = () => useResearch.getState().runs.map((r) => r.id);
const emit = (event: ResearchEvent) => {
  server.handler!(event);
  flushEvents();
};
const settle = async () => {
  for (let i = 0; i < 20; i += 1) await Promise.resolve();
  await refreshRuns();
};

/** `n` stored runs, newest (highest number) first. */
function storeRuns(n: number): void {
  server.stored = Array.from({ length: n }, (_, i) => run(n - i));
}

// One subscription for the file (the store subscribes once).
storeRuns(0);
startResearchTracking();
await settle();

beforeEach(async () => {
  server.unreadable = 0;
  server.refuse.clear();
  server.runFailure = null;
  // Another project: the list, the pages loaded, and the selection start over.
  ipc.commands.researchWorkspace.mockResolvedValueOnce({ root: `C:/other-${Math.random()}`, trusted: true });
  storeRuns(0);
  await store.refreshAll();
  server.calls.length = 0;
});

describe("paged run history", () => {
  it("lists the first page and says how many stored runs are on later pages", async () => {
    storeRuns(1_230);
    await refreshRuns();
    const state = useResearch.getState();
    expect(state.runs).toHaveLength(PAGE);
    expect(state.runs[0]!.id).toBe(run(1_230).id);
    expect(state.runsNext).not.toBeNull();
    expect(state.runsRemaining).toBe(730);
    expect(server.calls).toEqual([null]);
  });

  it("adds each further page to the runs listed until the history ends", async () => {
    storeRuns(1_230);
    await refreshRuns();
    await loadMoreRuns();
    expect(useResearch.getState().runs).toHaveLength(1_000);
    expect(useResearch.getState().runsRemaining).toBe(230);
    expect(useResearch.getState().runsLoadingMore).toBe(false);
    await loadMoreRuns();
    const state = useResearch.getState();
    expect(state.runs).toHaveLength(1_230);
    expect(state.runsNext).toBeNull();
    expect(state.runsRemaining).toBe(0);
    expect(new Set(ids()).size).toBe(1_230);
    expect(ids()).toEqual(server.stored.map((r) => r.id));
    // Nothing more to read.
    const calls = server.calls.length;
    await loadMoreRuns();
    expect(server.calls).toHaveLength(calls);
  });

  it("keeps the selection and the loaded pages when a refresh reads the history again", async () => {
    storeRuns(1_230);
    await refreshRuns();
    await loadMoreRuns();
    await loadMoreRuns();
    const oldest = run(1).id;
    useResearch.setState({ selectedRun: oldest });
    server.calls.length = 0;
    await refreshRuns();
    expect(server.calls).toHaveLength(3);
    expect(useResearch.getState().runs).toHaveLength(1_230);
    expect(useResearch.getState().selectedRun).toBe(oldest);
    expect(findRun(useResearch.getState(), oldest)?.id).toBe(oldest);
  });

  it("removes a run deleted from a later page and keeps the others", async () => {
    storeRuns(1_230);
    await refreshRuns();
    await loadMoreRuns();
    await loadMoreRuns();
    const deleted = run(3).id;
    const kept = run(2).id;
    useResearch.setState({ selectedRun: deleted });
    server.stored = server.stored.filter((r) => r.id !== deleted);
    server.opListener!({ outcomes: [{ target: { kind: "researchRun", run: deleted } }] });
    await settle();
    expect(ids()).not.toContain(deleted);
    expect(ids()).toContain(kept);
    expect(useResearch.getState().runs).toHaveLength(1_229);
    expect(useResearch.getState().selectedRun).toBeNull();
    // Restored: listed again where it was, with every page still loaded.
    server.stored = Array.from({ length: 1_230 }, (_, i) => run(1_230 - i));
    server.opListener!({ outcomes: [{ target: { kind: "recoverable", id: `researchRun:${deleted}` } }] });
    await settle();
    expect(ids()).toEqual(server.stored.map((r) => r.id));
  });

  it("merges a run that starts and finishes while later pages are loaded", async () => {
    storeRuns(1_230);
    await refreshRuns();
    await loadMoreRuns();
    const selected = run(400).id;
    useResearch.setState({ selectedRun: selected });
    const started = run(5_000, { owner: { kind: "agent", taskId: "task-1" }, status: { kind: "running" }, queued: true, finishedAtMs: null });
    emit({ kind: "runQueued", run: started });
    emit({ kind: "runStarted", runId: started.id, sessionId: started.sessionId, executionCount: 7 });
    emit({ kind: "runFinished", run: { ...started, status: { kind: "succeeded" }, queued: false, finishedAtMs: 9_000 } });
    const state = useResearch.getState();
    expect(state.runs).toHaveLength(1_001);
    expect(state.runs[0]).toMatchObject({ id: started.id, status: { kind: "succeeded" }, finishedAtMs: 9_000 });
    expect(state.runs.at(-1)!.id).toBe(run(231).id);
    expect(state.selectedRun).toBe(selected);
    // The stored runs still to come are the same ones.
    expect(state.runsRemaining).toBe(230);
    // A run of a later page that changes is changed in place.
    const late = run(300);
    emit({ kind: "runFinished", run: { ...late, status: { kind: "failed", message: "late" } } });
    expect(useResearch.getState().runs).toHaveLength(1_001);
    expect(findRun(useResearch.getState(), late.id)?.status.kind).toBe("failed");
    // The next page follows on from the stored runs, not from the new run.
    server.stored.unshift({ ...started, status: { kind: "succeeded" } });
    await loadMoreRuns();
    expect(useResearch.getState().runs).toHaveLength(1_231);
    expect(new Set(ids()).size).toBe(1_231);
    expect(useResearch.getState().selectedRun).toBe(selected);
  });

  it("reads the history again from its start when the continuation is refused", async () => {
    storeRuns(1_230);
    await refreshRuns();
    const next = useResearch.getState().runsNext!;
    server.refuse.add(next);
    server.calls.length = 0;
    await loadMoreRuns();
    // The refused request, then both pages from the start.
    expect(server.calls).toEqual([next, null, next]);
    const state = useResearch.getState();
    expect(state.error).toBeNull();
    expect(state.runs).toHaveLength(1_000);
    expect(new Set(ids()).size).toBe(1_000);
    expect(state.runsRemaining).toBe(230);
    expect(state.runsLoadingMore).toBe(false);
  });

  it("counts records that cannot be read apart from the runs still to come", async () => {
    storeRuns(700);
    server.unreadable = 4;
    await refreshRuns();
    expect(useResearch.getState().runsRemaining).toBe(200);
    expect(useResearch.getState().runsUnreadable).toBe(4);
    await loadMoreRuns();
    expect(useResearch.getState().runsRemaining).toBe(0);
    expect(useResearch.getState().runs).toHaveLength(700);
  });

  it("reads a run by id when the loaded pages do not list it", async () => {
    storeRuns(1_230);
    await refreshRuns();
    const oldest = run(1).id;
    expect(findRun(useResearch.getState(), oldest)).toBeUndefined();
    await lookupRun(oldest);
    expect(findRun(useResearch.getState(), oldest)?.id).toBe(oldest);
    expect(ipc.commands.researchRun).toHaveBeenCalledWith(oldest);
    // Only the service's "no such run" records a run as missing.
    await lookupRun("run-missing");
    expect(useResearch.getState().lookups["run-missing"]).toBeNull();
    // A listed run is not read again.
    ipc.commands.researchRun.mockClear();
    await lookupRun(run(1_230).id);
    await lookupRun(oldest);
    expect(ipc.commands.researchRun).not.toHaveBeenCalled();
    // Deleted through a project operation: asked for again next time.
    server.opListener!({ outcomes: [{ target: { kind: "researchRun", run: oldest } }] });
    expect(oldest in useResearch.getState().lookups).toBe(false);
    await settle();
  });

  it("reports a run that could not be read and reads it again on request", async () => {
    storeRuns(1_230);
    await refreshRuns();
    const oldest = run(1).id;
    server.runFailure = { code: "research.io", message: "run manifest is invalid: EOF" };
    await lookupRun(oldest);
    let state = useResearch.getState();
    // Not "no such run" and not left as being read: the failure is kept.
    expect(oldest in state.lookups).toBe(false);
    expect(state.lookupErrors[oldest]).toBe("run manifest is invalid: EOF");
    // The record is readable again.
    server.runFailure = null;
    await retryLookup(oldest);
    state = useResearch.getState();
    expect(state.lookupErrors).toEqual({});
    expect(findRun(state, oldest)?.id).toBe(oldest);
  });

  it("asks again for a run recorded as missing once something is restored", async () => {
    storeRuns(1_230);
    await refreshRuns();
    await lookupRun("run-gone");
    server.runFailure = { code: "research.io", message: "damaged" };
    await lookupRun(run(2).id);
    expect(useResearch.getState().lookups["run-gone"]).toBeNull();
    server.opListener!({ outcomes: [{ target: { kind: "recoverable", id: "researchRun:run-gone" } }] });
    expect(useResearch.getState().lookups).toEqual({});
    expect(useResearch.getState().lookupErrors).toEqual({});
    server.runFailure = null;
    await settle();
  });

  it("holds the outputs and the summaries of runs read by id within their bounds", async () => {
    storeRuns(1_230);
    await refreshRuns();
    const { KEPT_FINISHED_OUTPUTS } = await import("./model");
    // Old runs (not on the loaded page) are looked at one after another.
    for (let n = 1; n <= 80; n += 1) {
      await lookupRun(run(n).id);
      await selectRun(run(n).id);
    }
    const state = useResearch.getState();
    expect(Object.keys(state.outputs).length).toBeLessThanOrEqual(KEPT_FINISHED_OUTPUTS + 1);
    expect(state.outputs[run(80).id]).toBeDefined();
    expect(Object.keys(state.lookups).length).toBeLessThanOrEqual(store.KEPT_LOOKUPS);
    expect(findRun(state, run(80).id)?.id).toBe(run(80).id);
    // A forgotten one is read again when asked for.
    expect(findRun(state, run(1).id)).toBeUndefined();
    await lookupRun(run(1).id);
    expect(findRun(useResearch.getState(), run(1).id)?.id).toBe(run(1).id);
  });
});
