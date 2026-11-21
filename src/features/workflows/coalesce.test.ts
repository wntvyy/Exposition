import { describe, expect, it } from "vitest";

import { coalesced } from "./coalesce";

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((r) => (resolve = r));
  return { promise, resolve };
}

const settle = () => new Promise((r) => setTimeout(r, 0));

describe("coalesced", () => {
  it("runs at once, then once more for any number of calls made while it runs", async () => {
    const gates = [deferred(), deferred(), deferred()];
    const starts: number[] = [];
    let calls = 0;
    const run = coalesced(() => {
      starts.push(calls);
      return gates[starts.length - 1]!.promise;
    });
    // 60 events (a 20-instance forEach: intent, dispatched, completion each).
    for (calls = 1; calls <= 60; calls++) run();
    calls = 60;
    expect(starts).toEqual([1]);
    gates[0]!.resolve();
    await settle();
    // One trailing read, started after the last call.
    expect(starts).toEqual([1, 60]);
    gates[1]!.resolve();
    await settle();
    expect(starts).toHaveLength(2);
    // Idle again: the next event reads immediately.
    run();
    expect(starts).toHaveLength(3);
  });

  it("keeps working after a failed read", async () => {
    let n = 0;
    const run = coalesced(async () => {
      n += 1;
      if (n === 1) throw new Error("store busy");
    });
    run();
    run();
    await settle();
    expect(n).toBe(2);
  });
});
