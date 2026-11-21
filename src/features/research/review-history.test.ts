// Independent review of the paged run history in the store: races between
// reading pages, refreshes, events and a change of project, and the runs read
// by id. `it.skip` cases marked "review: defect" fail against the code under
// review; every other case passes.
import { renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { ResearchEvent } from "../../bindings/ResearchEvent";
import type { RunSummary } from "../../bindings/RunSummary";

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
  /** Continuations refused once. */
  refuseOnce: new Set<string>(),
  /** Every continuation is refused. */
  refuseAll: false,
  calls: [] as (string | null)[],
  /** Answers wait for this (the answer itself is made when the request arrives). */
  gate: null as Promise<void> | null,
  /** Called when a request arrived, before its answer is released. */
  onRequest: null as ((after: string | null) => void) | null,
  runError: null as { code: string; message: string } | null,
  handler: null as ((event: unknown) => void) | null,
  opListener: null as ((result: unknown) => void) | null,
}));

const ipc = vi.hoisted(() => ({
  commands: {
    researchWorkspace: vi.fn(async () => ({ root: "C:/project", trusted: true })),
    researchSessions: vi.fn(async () => []),
    researchRuns: vi.fn(async (request?: { after?: string; limit?: number }) => {
      const after = request?.after ?? null;
      server.calls.push(after);
      const refused = after !== null && (server.refuseAll || server.refuseOnce.delete(after));
      const start = after === null ? 0 : server.stored.findIndex((r) => r.id < after);
      const from = start < 0 ? server.stored.length : start;
      const runs = server.stored.slice(from, from + 500);
      const more = from + runs.length < server.stored.length;
      const answer = { runs, next: more ? runs[runs.length - 1]!.id : null, hasMore: more, total: server.stored.length, unreadable: 0 };
      server.onRequest?.(after);
      if (server.gate) await server.gate;
      if (refused) throw { code: "research.historyChanged", message: "The run history changed since this part of it was listed." };
      return answer;
    }),
    researchRun: vi.fn(async (runId: string) => {
      if (server.runError) throw server.runError;
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
vi.mock("./model", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./model")>()),
  summaryOfManifest: (manifest: unknown) => (manifest as { found: RunSummary }).found,
}));

const store = await import("./store");
const model = await import("./model");
const { useResearch, refreshRuns, loadMoreRuns, lookupRun, findRun, flushEvents, startResearchTracking, useRun } = store;

const ids = () => useResearch.getState().runs.map((r) => r.id);
const emit = (event: ResearchEvent) => {
  server.handler!(event);
  flushEvents();
};
const ticks = async (n = 30) => {
  for (let i = 0; i < n; i += 1) await Promise.resolve();
};
const settle = async () => {
  await ticks();
  await refreshRuns();
};

function storeRuns(n: number, base = 0): void {
  server.stored = Array.from({ length: n }, (_, i) => run(base + n - i));
}

function gate(): () => void {
  let open!: () => void;
  server.gate = new Promise<void>((resolve) => {
    open = () => {
      server.gate = null;
      resolve();
    };
  });
  return open;
}

storeRuns(0);
startResearchTracking();
await settle();

beforeEach(async () => {
  server.refuseOnce.clear();
  server.refuseAll = false;
  server.gate = null;
  server.onRequest = null;
  server.runError = null;
  ipc.commands.researchWorkspace.mockResolvedValueOnce({ root: `C:/other-${Math.random()}`, trusted: true });
  storeRuns(0);
  await store.refreshAll();
  useResearch.setState({ error: null });
  server.calls.length = 0;
});

describe("review: paged run history under races", () => {
  it("adds two pages once each when Show more is pressed twice at once", async () => {
    storeRuns(1_730);
    await refreshRuns();
    const [a, b] = [loadMoreRuns(), loadMoreRuns()];
    await Promise.all([a, b]);
    const state = useResearch.getState();
    expect(state.runs).toHaveLength(1_500);
    expect(new Set(ids()).size).toBe(1_500);
    expect(ids()).toEqual(server.stored.slice(0, 1_500).map((r) => r.id));
    expect(state.runsRemaining).toBe(230);
    expect(state.runsLoadingMore).toBe(false);
    // Each continuation was asked for once.
    expect(server.calls.filter((c) => c !== null)).toHaveLength(2);
    expect(new Set(server.calls).size).toBe(server.calls.length);
  });

  it("keeps the selection and adds no row twice when a refresh and Show more interleave", async () => {
    storeRuns(1_730);
    await refreshRuns();
    await loadMoreRuns();
    const selected = run(900).id;
    useResearch.setState({ selectedRun: selected });
    const all = [refreshRuns(), loadMoreRuns(), refreshRuns(), loadMoreRuns(), refreshRuns()];
    await Promise.all(all);
    const state = useResearch.getState();
    expect(state.runs).toHaveLength(1_730);
    expect(new Set(ids()).size).toBe(1_730);
    expect(ids()).toEqual(server.stored.map((r) => r.id));
    expect(state.runsNext).toBeNull();
    expect(state.runsRemaining).toBe(0);
    expect(state.selectedRun).toBe(selected);
    expect(state.error).toBeNull();
  });

  it("reads the history again from its start when a continuation is refused in the middle of a refresh", async () => {
    storeRuns(1_230);
    await refreshRuns();
    await loadMoreRuns();
    await loadMoreRuns();
    // The second continuation is refused once, during the refresh after a delete.
    const second = server.stored[999]!.id;
    server.refuseOnce.add(second);
    const deleted = run(5).id;
    server.stored = server.stored.filter((r) => r.id !== deleted);
    server.calls.length = 0;
    server.opListener!({ outcomes: [{ target: { kind: "researchRun", run: deleted } }] });
    await settle();
    const state = useResearch.getState();
    expect(state.error).toBeNull();
    expect(ids()).toEqual(server.stored.map((r) => r.id));
    expect(state.runs).toHaveLength(1_229);
    // Never a list patched together from two listings: the read started over.
    expect(server.calls.slice(0, 6)).toEqual([null, server.stored[499]!.id, second, null, server.stored[499]!.id, second]);
  });

  it("keeps the list and reports the failure when the continuation is refused every time", async () => {
    storeRuns(1_730);
    await refreshRuns();
    await loadMoreRuns();
    const before = ids();
    server.refuseAll = true;
    server.calls.length = 0;
    await loadMoreRuns();
    let state = useResearch.getState();
    expect(state.error).not.toBeNull();
    expect(ids()).toEqual(before);
    expect(state.runsLoadingMore).toBe(false);
    expect(state.runsNext).not.toBeNull();
    // Bounded: the refused request and three readings from the start.
    expect(server.calls.length).toBeLessThanOrEqual(1 + 3 * 3);
    // A refresh that is refused every time keeps the list too.
    useResearch.setState({ error: null });
    await refreshRuns();
    state = useResearch.getState();
    expect(state.error).not.toBeNull();
    expect(ids()).toEqual(before);
    // The service answers again: the next page is added, nothing was skipped.
    server.refuseAll = false;
    useResearch.setState({ error: null });
    await loadMoreRuns();
    expect(ids()).toEqual(server.stored.slice(0, 1_500).map((r) => r.id));
    await refreshRuns();
    expect(ids()).toEqual(server.stored.slice(0, 1_500).map((r) => r.id));
  });

  it("does not add a page of the project that was open before to the project opened meanwhile", async () => {
    storeRuns(1_230);
    await refreshRuns();
    const open = gate();
    const more = loadMoreRuns();
    await ticks();
    expect(useResearch.getState().runsLoadingMore).toBe(true);
    // Another project is opened while the page is on its way.
    storeRuns(40, 50_000);
    const theirs = server.stored.map((r) => r.id);
    ipc.commands.researchWorkspace.mockResolvedValueOnce({ root: "C:/another-project", trusted: true });
    const switching = store.refreshAll();
    await ticks();
    open();
    await Promise.all([more, switching]);
    await settle();
    const state = useResearch.getState();
    expect(state.workspace?.root).toBe("C:/another-project");
    expect(ids()).toEqual(theirs);
    expect(state.runsNext).toBeNull();
    expect(state.runsRemaining).toBe(0);
    expect(state.runsLoadingMore).toBe(false);
    expect(state.error).toBeNull();
  });

  it("does not add a refresh of the project that was open before to the project opened meanwhile", async () => {
    storeRuns(1_230);
    await refreshRuns();
    await loadMoreRuns();
    const open = gate();
    const refresh = refreshRuns();
    await ticks();
    storeRuns(7, 80_000);
    const theirs = server.stored.map((r) => r.id);
    ipc.commands.researchWorkspace.mockResolvedValueOnce({ root: "C:/third-project", trusted: true });
    const switching = store.refreshAll();
    await ticks();
    open();
    await Promise.all([refresh, switching]);
    await settle();
    expect(ids()).toEqual(theirs);
    // The new project is read to the depth of one page again.
    storeRuns(1_230, 80_000);
    server.calls.length = 0;
    await refreshRuns();
    expect(server.calls).toEqual([null]);
    expect(useResearch.getState().runs).toHaveLength(500);
  });

  it("keeps what events said about runs that changed while several pages were read again", async () => {
    storeRuns(1_230);
    await refreshRuns();
    await loadMoreRuns();
    await loadMoreRuns();
    const late = run(3);
    const started = run(9_000, { status: { kind: "running" }, queued: true, finishedAtMs: null });
    // While the second page of the refresh is on its way: a stored run of the
    // last page changes and a run starts.
    let fired = false;
    server.onRequest = (after) => {
      if (after === null || fired) return;
      fired = true;
      server.handler!({ kind: "runFinished", run: { ...late, status: { kind: "failed", message: "changed meanwhile" } } });
      server.handler!({ kind: "runQueued", run: started });
    };
    await refreshRuns();
    flushEvents();
    server.onRequest = null;
    const state = useResearch.getState();
    expect(fired).toBe(true);
    expect(findRun(state, late.id)?.status.kind).toBe("failed");
    expect(state.runs[0]!.id).toBe(started.id);
    expect(state.runs).toHaveLength(1_231);
    expect(new Set(ids()).size).toBe(1_231);
    emit({ kind: "runFinished", run: { ...started, status: { kind: "succeeded" }, queued: false, finishedAtMs: 12_000 } });
    expect(useResearch.getState().runs[0]!.status.kind).toBe("succeeded");
  });

  it("reads a run by id once however many views ask for it, and not for another project", async () => {
    storeRuns(1_230);
    await refreshRuns();
    const oldest = run(1).id;
    ipc.commands.researchRun.mockClear();
    await Promise.all([lookupRun(oldest), lookupRun(oldest), lookupRun(oldest)]);
    expect(ipc.commands.researchRun).toHaveBeenCalledTimes(1);
    expect(findRun(useResearch.getState(), oldest)?.id).toBe(oldest);
    // Another project: what was read by id is forgotten.
    ipc.commands.researchWorkspace.mockResolvedValueOnce({ root: "C:/fourth-project", trusted: true });
    storeRuns(3, 70_000);
    await store.refreshAll();
    expect(useResearch.getState().lookups).toEqual({});
    expect(findRun(useResearch.getState(), oldest)).toBeUndefined();
  });
});

describe("review: runs read by id", () => {
  // The inspector of a result shows "Run … is not in the run history of this
  // project." for a `null` lookup. A run deleted (recoverably) while it is on
  // a page not loaded, looked at, and then restored (Undo, Storage & History)
  // keeps that answer: a restore reports the target `recoverable`, which
  // does not clear the remembered "no such run".
  it.skip("review: defect — a run looked up while deleted is still reported missing after it is restored", async () => {
    storeRuns(1_230);
    await refreshRuns();
    const oldest = run(1);
    // Deleted from Storage & History.
    server.stored = server.stored.filter((r) => r.id !== oldest.id);
    server.opListener!({ outcomes: [{ target: { kind: "researchRun", run: oldest.id } }] });
    await settle();
    const view = renderHook(() => useRun(oldest.id));
    await waitFor(() => expect(view.result.current).toBeNull());
    // Restored.
    server.stored = [...server.stored, oldest];
    server.opListener!({ outcomes: [{ target: { kind: "recoverable", id: `researchRun:${oldest.id}` } }] });
    await settle();
    await waitFor(() => expect(view.result.current?.id).toBe(oldest.id));
    view.unmount();
  });

  // `lookupRun` records an answer only for a run that was read or that the
  // service says does not exist. Any other failure (a damaged record:
  // `research.io`) records nothing, nothing asks again, and the inspector
  // shows "Reading run …" for good, with no error.
  it.skip("review: defect — a run whose record cannot be read is shown as being read for ever", async () => {
    storeRuns(1_230);
    await refreshRuns();
    const damaged = "run-damaged";
    server.runError = { code: "research.io", message: "run manifest run-damaged is invalid: EOF" };
    const view = renderHook(() => useRun(damaged));
    await ticks(200);
    expect(ipc.commands.researchRun).toHaveBeenCalledWith(damaged);
    // Not "being read" any more: either an answer or a reported failure.
    const state = useResearch.getState();
    expect(view.result.current !== undefined || state.error !== null).toBe(true);
    view.unmount();
  });

  // Selecting a run read by id loads its output into `outputs` (up to 10,000
  // entries). `pruneOutputs` releases only outputs of runs in the loaded
  // list, so these copies are never released while the project stays open.
  it.skip("review: defect — the outputs of runs read by id are never released", () => {
    const listed = [run(1), run(2)];
    const outputs = Object.fromEntries(
      Array.from({ length: 40 }, (_, i) => [run(10_000 + i).id, { ...model.EMPTY_RUN_OUTPUT, loaded: true }]),
    );
    const selected = run(10_039).id;
    const pruned = model.pruneOutputs(outputs, listed, selected);
    expect(Object.keys(pruned).length).toBeLessThanOrEqual(model.KEPT_FINISHED_OUTPUTS + 1);
  });
});
