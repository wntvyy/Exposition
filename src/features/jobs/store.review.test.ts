// Independent review of job tracking after output events stopped carrying
// their text and stopped re-reading the list: the selected job's output is
// complete and current when the panel opens mid-run, when the selection
// changes during a burst, when the last output precedes the exit, and after
// a resync; and the list still shows every refusal.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { JobEvent } from "../../bindings/JobEvent";

interface Chunk {
  seq: number;
  stream: "stdout" | "stderr";
  text: string;
}

const ipc = vi.hoisted(() => {
  const state = {
    handler: null as ((event: unknown) => void) | null,
    resync: null as (() => void) | null,
    /** The service's log per job. */
    logs: {} as Record<string, { seq: number; stream: "stdout" | "stderr"; text: string }[]>,
    done: new Set<string>(),
    jobs: [] as unknown[],
    /** Resolvers of output requests held open by a test. */
    held: [] as (() => void)[],
    hold: false,
  };
  return {
    state,
    notify: vi.fn(),
    commands: {
      jobList: vi.fn(async () => state.jobs),
      // Two chunks a page, so a long log needs several requests.
      jobOutput: vi.fn(async (id: string, after: number | null) => {
        const log = state.logs[id] ?? [];
        const rest = log.filter((c) => c.seq > (after ?? 0));
        const page = { chunks: rest.slice(0, 2), hasMore: rest.length > 2, truncated: false, complete: state.done.has(id) };
        if (state.hold) await new Promise<void>((resolve) => state.held.push(resolve));
        return page;
      }),
      jobCancel: vi.fn(async () => true),
    },
    events: {
      jobs: vi.fn(async (handler: (event: unknown) => void) => {
        state.handler = handler;
        return () => {};
      }),
      jobsResync: vi.fn(async (handler: () => void) => {
        state.resync = handler;
        return () => {};
      }),
    },
  };
});
vi.mock("../../lib/ipc", () => ({ commands: ipc.commands, events: ipc.events, errorMessage: (e: unknown) => String(e) }));
vi.mock("../../app/notifications", () => ({ notify: ipc.notify }));

const { startJobsTracking, selectJob, useJobs } = await import("./store");

const emit = (event: JobEvent) => ipc.state.handler!(event);
/** The service appends to its log, then announces it (the event carries no text). */
function write(id: string, text: string, announce = true): void {
  const log = (ipc.state.logs[id] ??= []);
  log.push({ seq: log.length + 1, stream: "stdout", text });
  if (announce) emit({ kind: "output", id, stream: "stdout", text: "" });
}
function finish(id: string): void {
  ipc.state.done.add(id);
  emit({ kind: "finished", result: { id, outcome: { kind: "exited", code: 0 }, durationMs: 1, outputTruncated: false } });
}
const shown = (id: string): string => (useJobs.getState().outputs[id]?.chunks ?? []).map((c: Chunk) => c.text).join("");
const logged = (id: string): string => (ipc.state.logs[id] ?? []).map((c) => c.text).join("");
const settle = () => vi.advanceTimersByTimeAsync(200);

beforeEach(() => {
  vi.useFakeTimers();
  startJobsTracking();
});

afterEach(() => {
  ipc.state.hold = false;
  for (const release of ipc.state.held.splice(0)) release();
  vi.useRealTimers();
});

describe("review: job output without event text", () => {
  it("shows the whole log when the job is selected mid-run, and keeps up to the end", async () => {
    await settle();
    // Output before anything is selected: announced, never pulled.
    for (let i = 0; i < 7; i++) write("job-a", `early ${i}\n`);
    await settle();
    expect(ipc.commands.jobOutput).not.toHaveBeenCalled();
    // The panel opens and the job is selected mid-run: everything so far, over several pages.
    selectJob("job-a");
    await settle();
    expect(shown("job-a")).toBe(logged("job-a"));
    // A burst while a pull is in flight: nothing is skipped.
    ipc.state.hold = true;
    write("job-a", "burst 0\n");
    for (let i = 1; i < 9; i++) write("job-a", `burst ${i}\n`);
    ipc.state.hold = false;
    for (const release of ipc.state.held.splice(0)) release();
    await settle();
    expect(shown("job-a")).toBe(logged("job-a"));
    // The last output is written but its announcement is lost; the exit event alone brings it.
    write("job-a", "last words\n", false);
    finish("job-a");
    await settle();
    expect(shown("job-a")).toBe(logged("job-a"));
    expect(useJobs.getState().outputs["job-a"]!.complete).toBe(true);
  });

  it("follows the selection when it changes during a burst", async () => {
    await settle();
    selectJob("job-b");
    await settle();
    ipc.state.hold = true;
    write("job-b", "b1\n");
    write("job-c", "c1\n");
    // The selection moves while job-b's pull is in flight and both jobs keep writing.
    selectJob("job-c");
    write("job-b", "b2\n");
    write("job-c", "c2\n");
    write("job-c", "c3\n");
    ipc.state.hold = false;
    for (let round = 0; round < 6; round++) {
      for (const release of ipc.state.held.splice(0)) release();
      await settle();
    }
    expect(shown("job-c")).toBe(logged("job-c"));
    // Coming back to job-b shows all of it, including what was written while it was not selected.
    write("job-b", "b3\n");
    selectJob("job-b");
    await settle();
    expect(shown("job-b")).toBe(logged("job-b"));
  });

  it("reports a refusal that arrives as a lifecycle event between output bursts", async () => {
    await settle();
    ipc.notify.mockClear();
    for (let i = 0; i < 50; i++) write("job-d", `noise ${i}\n`);
    ipc.state.jobs = [
      {
        info: { id: "job-r", label: "Refused", program: "x", args: [], policy: { kind: "native" }, startedAtMs: null, result: null },
        kind: "batch",
        state: "finished",
        workingDirectory: "C:/p",
        refusal: { code: "untrustedWorkspace", message: "Not trusted.", workspaceRoot: "C:/p" },
        pid: null,
      },
    ];
    emit({
      kind: "finished",
      result: { id: "job-r", outcome: { kind: "failedToStart", reason: "Not trusted." }, durationMs: 0, outputTruncated: false },
    });
    for (let i = 0; i < 50; i++) write("job-d", `more ${i}\n`);
    await settle();
    expect(ipc.notify).toHaveBeenCalledTimes(1);
    expect(useJobs.getState().jobs).toHaveLength(1);
  });

  // After the webview fell behind, events were dropped: if they were the
  // selected job's last output and its exit, nothing else would pull it.
  it("a resync pulls the selected job's output as well as the list", async () => {
    await settle();
    selectJob("job-e");
    await settle();
    write("job-e", "lost announcement\n", false);
    ipc.state.done.add("job-e");
    ipc.state.resync!();
    await settle();
    expect(shown("job-e")).toBe(logged("job-e"));
    expect(useJobs.getState().outputs["job-e"]!.complete).toBe(true);
  });
});
