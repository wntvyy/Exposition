import { describe, expect, it } from "vitest";

import { waitForAsync } from "./wait.mjs";

/** Minimal stand-in for a Playwright page: `evaluate` runs the function locally. */
const fakePage = { evaluate: async (fn, arg) => fn(arg) };

describe("waitForAsync", () => {
  it("awaits async predicates and keeps polling while they resolve falsy", async () => {
    let calls = 0;
    const value = await waitForAsync(
      fakePage,
      async () => {
        calls += 1;
        await new Promise((r) => setTimeout(r, 5));
        return calls >= 3 ? "ready" : false;
      },
      undefined,
      { interval: 1, timeout: 2000 },
    );
    expect(value).toBe("ready");
    expect(calls).toBe(3);
  });

  it("times out on a predicate that never becomes truthy", async () => {
    await expect(
      waitForAsync(fakePage, async () => false, undefined, { interval: 1, timeout: 30, message: "never" }),
    ).rejects.toThrow(/timed out after 30 ms waiting for never/);
  });

  it("retries through evaluation errors and reports the last one", async () => {
    await expect(
      waitForAsync(
        fakePage,
        async () => {
          throw new Error("boom");
        },
        undefined,
        { interval: 1, timeout: 20 },
      ),
    ).rejects.toThrow(/last error: boom/);
  });
});

describe("e2e scenarios", () => {
  it("never pass async predicates to page.waitForFunction (they are not awaited)", async () => {
    const { readdir, readFile } = await import("node:fs/promises");
    const { join } = await import("node:path");
    const roots = [import.meta.dirname, join(import.meta.dirname, "scenarios")];
    const offenders = [];
    for (const dir of roots) {
      for (const name of await readdir(dir)) {
        if (!name.endsWith(".mjs") || name.endsWith(".test.mjs")) continue;
        const text = await readFile(join(dir, name), "utf8");
        if (/waitForFunction\(\s*async\b/.test(text)) offenders.push(name);
      }
    }
    expect(offenders).toEqual([]);
  });
});
