// Independent review of "a jobs resync pulls the selected job's output as
// well as the list": the resync with nothing selected, with a finished job
// whose output is complete, with the selection changing while the pull is in
// flight, and with a pull already in flight when the resync arrives.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const ipc = vi.hoisted(() => {
  const state = {
    resync: null as (() => void) | null,
    /** The service's log per job. */
    logs: {} as Record<string, { seq: number; stream: "stdout" | "stderr"; text: string }[]>,
    done: new Set<string>(),
    /** Resolvers of output requests held open by a test. */
    held: [] as (() => void)[],
    hold: false,
  };
  return {
    state,
    commands: {
      jobList: vi.fn(async () => []),
      // Two chunks a page, so a long log needs several requests.
      // The page is what the service has when asked; a held request answers it later.
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
      jobs: vi.fn(async () => () => {}),
      jobsResync: vi.fn(async (handler: () => void) => {
        state.resync = handler;
        return () => {};
      }),
    },
  };
});
vi.mock("../../lib/ipc", () => ({ commands: ipc.commands, events: ipc.events, errorMessage: (e: unknown) => String(e) }));
vi.mock("../../app/notifications", () => ({ notify: vi.fn() }));

const { startJobsTracking, selectJob, useJobs } = await import("./store");

/** The service appends to its log; the announcement is among the dropped events. */
function write(id: string, text: string): void {
  const log = (ipc.state.logs[id] ??= []);
  log.push({ seq: log.length + 1, stream: "stdout", text });
}
const shown = (id: string): string => (useJobs.getState().outputs[id]?.chunks ?? []).map((c) => c.text).join("");
const logged = (id: string): string => (ipc.state.logs[id] ?? []).map((c) => c.text).join("");
const settle = () => vi.advanceTimersByTimeAsync(200);
const release = async () => {
  for (let round = 0; round < 8; round++) {
    for (const resolve of ipc.state.held.splice(0)) resolve();
    await settle();
  }
};

beforeEach(async () => {
  vi.useFakeTimers();
  startJobsTracking();
  await settle();
  ipc.commands.jobOutput.mockClear();
  ipc.commands.jobList.mockClear();
});

afterEach(() => {
  ipc.state.hold = false;
  for (const resolve of ipc.state.held.splice(0)) resolve();
  selectJob(null);
  vi.useRealTimers();
});

describe("review: a resync pulls the selected job's output", () => {
  it("with nothing selected reads the list and no output", async () => {
    write("job-n", "unseen\n");
    ipc.state.resync!();
    await settle();
    expect(ipc.commands.jobList).toHaveBeenCalledTimes(1);
    expect(ipc.commands.jobOutput).not.toHaveBeenCalled();
    expect(useJobs.getState().outputs["job-n"]).toBeUndefined();
  });

  it("with a finished job whose output is complete asks once and changes nothing", async () => {
    for (let i = 0; i < 5; i++) write("job-f", `line ${i}\n`);
    ipc.state.done.add("job-f");
    selectJob("job-f");
    await settle();
    const before = useJobs.getState().outputs["job-f"]!;
    expect(before.complete).toBe(true);
    ipc.commands.jobOutput.mockClear();
    ipc.state.resync!();
    await settle();
    expect(ipc.commands.jobOutput).toHaveBeenCalledTimes(1);
    expect(ipc.commands.jobOutput).toHaveBeenCalledWith("job-f", before.lastSeq, expect.any(Number));
    const after = useJobs.getState().outputs["job-f"]!;
    expect(after.chunks).toEqual(before.chunks);
    expect(after.complete).toBe(true);
    expect(shown("job-f")).toBe(logged("job-f"));
  });

  it("brings the dropped last output and exit of the selected job, over several pages", async () => {
    selectJob("job-l");
    await settle();
    // The last output and the exit are written; their events are dropped.
    for (let i = 0; i < 7; i++) write("job-l", `last ${i}\n`);
    ipc.state.done.add("job-l");
    ipc.state.resync!();
    await settle();
    expect(shown("job-l")).toBe(logged("job-l"));
    expect(useJobs.getState().outputs["job-l"]!.complete).toBe(true);
  });

  it("follows a selection that changes while the resync's pull is in flight", async () => {
    selectJob("job-x");
    await settle();
    write("job-x", "x1\n");
    write("job-y", "y1\n");
    write("job-y", "y2\n");
    ipc.state.done.add("job-y");
    ipc.state.hold = true;
    ipc.state.resync!();
    await settle();
    // The user selects job-y while the resync's pull of job-x is held.
    selectJob("job-y");
    await settle();
    write("job-x", "x2\n");
    ipc.state.hold = false;
    await release();
    expect(shown("job-y")).toBe(logged("job-y"));
    expect(useJobs.getState().outputs["job-y"]!.complete).toBe(true);
    // job-x keeps only what it was sent (nothing of job-y), in order.
    expect(logged("job-x").startsWith(shown("job-x"))).toBe(true);
    expect(shown("job-x")).not.toContain("y");
    // Selected again, it is brought up to date.
    selectJob("job-x");
    await settle();
    expect(shown("job-x")).toBe(logged("job-x"));
  });

  it("with a pull of the selected job already in flight still ends complete", async () => {
    selectJob("job-p");
    await settle();
    ipc.state.hold = true;
    write("job-p", "p1\n");
    selectJob("job-p");
    await settle();
    // The resync arrives while that pull is held; then the job writes its end.
    ipc.state.resync!();
    write("job-p", "p2\n");
    write("job-p", "p3\n");
    ipc.state.done.add("job-p");
    ipc.state.hold = false;
    await release();
    expect(shown("job-p")).toBe(logged("job-p"));
    expect(useJobs.getState().outputs["job-p"]!.complete).toBe(true);
  });
});
