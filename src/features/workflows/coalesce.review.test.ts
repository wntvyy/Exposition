// Independent review of `coalesced`: whatever the reads in flight do, a read
// starts after the last call, so the view ends on the last event's state.
import { describe, expect, it } from "vitest";

import { coalesced } from "./coalesce";

/** Let the promise reactions of a settled read run (no timers involved). */
const settle = async () => {
  for (let i = 0; i < 12; i++) await Promise.resolve();
};

describe("review: coalesced", () => {
  it("always reads after the last call, for random interleavings of calls, slow reads, and failing reads", async () => {
    let seed = 12345;
    const random = (n: number) => {
      seed = (seed * 1103515245 + 12345) & 0x7fffffff;
      return seed % n;
    };
    for (let round = 0; round < 200; round++) {
      let calls = 0;
      /** The number of calls made when each read started. */
      const startedAt: number[] = [];
      const pending: { resolve: () => void; reject: (e: Error) => void }[] = [];
      const run = coalesced(
        () =>
          new Promise<void>((resolve, reject) => {
            startedAt.push(calls);
            pending.push({ resolve, reject });
          }),
      );
      const steps = 5 + random(40);
      for (let step = 0; step < steps; step++) {
        const action = random(4);
        if (action < 2) {
          calls += 1;
          run();
        } else if (pending.length > 0) {
          const read = pending.shift()!;
          if (action === 2) read.resolve();
          else read.reject(new Error("store busy"));
          await settle();
        }
        // Never more than one read in flight.
        expect(pending.length).toBeLessThanOrEqual(1);
      }
      // Let everything in flight finish (the last read may fail too).
      for (let guard = 0; pending.length > 0 && guard < 10; guard++) {
        const read = pending.shift()!;
        if (random(2)) read.resolve();
        else read.reject(new Error("store busy"));
        await settle();
      }
      expect(pending).toHaveLength(0);
      if (calls > 0) expect(startedAt.at(-1)).toBe(calls);
      // At most one read per call.
      expect(startedAt.length).toBeLessThanOrEqual(calls);
    }
  });

  it("starts the trailing read after an unmounted view's read settles, and not before", async () => {
    // The view's listener is removed on unmount; a call already counted still
    // gets its trailing read (the view's own ticket discards a stale result).
    const reads: string[] = [];
    let release!: () => void;
    const run = coalesced(() => {
      reads.push("start");
      return new Promise<void>((r) => (release = r));
    });
    run();
    run();
    expect(reads).toHaveLength(1);
    release();
    await settle();
    expect(reads).toHaveLength(2);
    release();
    await settle();
    expect(reads).toHaveLength(2);
  });

  // Not reachable from the two call sites today (both pass an async
  // function, which cannot throw before returning its promise).
  it("review: a task that throws synchronously does not stop later reads", () => {
    let n = 0;
    const run = coalesced(() => {
      n += 1;
      if (n === 1) throw new Error("thrown before a promise exists");
      return Promise.resolve();
    });
    try {
      run();
    } catch {
      // The first call surfaces the error to its caller.
    }
    run();
    expect(n).toBe(2);
  });
});
