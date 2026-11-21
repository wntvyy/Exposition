// Job tracking: the list is re-read on lifecycle events only (never per
// output event), while the selected job's output is still pulled by sequence
// number for every output event.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { JobEvent } from "../../bindings/JobEvent";

const ipc = vi.hoisted(() => {
  const state = { handler: null as ((event: unknown) => void) | null, resync: null as (() => void) | null };
  return {
    state,
    commands: {
      jobList: vi.fn(async () => []),
      jobOutput: vi.fn(async () => ({ chunks: [], hasMore: false, truncated: false, complete: false })),
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
vi.mock("../../app/notifications", () => ({ notify: vi.fn() }));

const { startJobsTracking, useJobs } = await import("./store");

const emit = (event: JobEvent) => ipc.state.handler!(event);
const output = (id: string): JobEvent => ({ kind: "output", id, stream: "stdout", text: "" });
const finished = (id: string): JobEvent => ({
  kind: "finished",
  result: { id, outcome: { kind: "exited", code: 0 }, durationMs: 1, outputTruncated: false },
});

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("job tracking", () => {
  it("re-reads the list on lifecycle events and resync, not on output events", async () => {
    startJobsTracking();
    await vi.advanceTimersByTimeAsync(0);
    expect(ipc.commands.jobList).toHaveBeenCalledTimes(1);

    // A long output stream from two jobs: no list read at all.
    for (let i = 0; i < 500; i++) {
      emit(output(i % 2 ? "job-a" : "job-b"));
      await vi.advanceTimersByTimeAsync(20);
    }
    expect(ipc.commands.jobList).toHaveBeenCalledTimes(1);
    expect(ipc.commands.jobOutput).not.toHaveBeenCalled();

    // The selected job's output is pulled for each announcement (coalesced while in flight).
    useJobs.setState({ selected: "job-a" });
    emit(output("job-a"));
    emit(output("job-b"));
    await vi.advanceTimersByTimeAsync(100);
    expect(ipc.commands.jobOutput).toHaveBeenCalledTimes(1);
    expect(ipc.commands.jobOutput).toHaveBeenCalledWith("job-a", null, 128 * 1024);
    expect(ipc.commands.jobList).toHaveBeenCalledTimes(1);

    // Lifecycle events in a burst refresh once; the final output is pulled.
    emit({ kind: "queued", id: "job-c", label: "c" });
    emit({ kind: "started", id: "job-c", pid: 1 });
    emit({ kind: "cancelRequested", id: "job-c" });
    emit(finished("job-a"));
    await vi.advanceTimersByTimeAsync(100);
    expect(ipc.commands.jobList).toHaveBeenCalledTimes(2);
    expect(ipc.commands.jobOutput).toHaveBeenCalledTimes(2);

    ipc.state.resync!();
    await vi.advanceTimersByTimeAsync(100);
    expect(ipc.commands.jobList).toHaveBeenCalledTimes(3);
  });
});
