// Runner semantics: statuses, exit codes, dependencies, and scheduling.
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";

import { classify, EXIT, executePlan } from "./runner.mjs";

const sink = () => {
  const chunks = [];
  return { write: (c) => chunks.push(String(c)), text: () => chunks.join("") };
};
const node = (code) => ["node", "-e", code];
const step = (id, command, extra = {}) => ({ id, label: id, kind: "static", weight: "light", command, after: [], reasons: [], ...extra });
const plan = (checks, deferred = [], uncovered = []) => ({ hash: "testplan0000", checks, deferred, uncovered });
const run = (p, opts = {}) => executePlan(p, { out: sink(), ...opts });

const dirs = [];
afterEach(() => {
  while (dirs.length) rmSync(dirs.pop(), { recursive: true, force: true });
});

describe("statuses and exit codes", () => {
  it("passes when every check passes and is complete only without deferred or uncovered items", async () => {
    const ok = await run(plan([step("a", node("")), step("b", node(""))]));
    expect(ok.exitCode).toBe(EXIT.ok);
    expect(ok.results.map((r) => r.status)).toEqual(["passed", "passed"]);
    expect(ok.complete).toBe(true);
    // A deferred required gate keeps the result pending: never a pass.
    const deferred = await run(plan([step("a", node(""))], [{ id: "live:x", label: "x", why: "live" }]));
    expect(deferred.exitCode).toBe(EXIT.pending);
    expect(deferred.complete).toBe(false);
    const uncovered = await run(plan([step("a", node(""))], [], ["docs/x.md"]));
    expect(uncovered.complete).toBe(false);
  });

  it("propagates a failure as exit status 1 and still runs the other checks", async () => {
    const r = await run(plan([step("bad", node("process.exit(4)")), step("good", node(""))]));
    expect(r.exitCode).toBe(EXIT.failed);
    expect(r.results.map((x) => [x.id, x.status, x.reason])).toEqual([
      ["bad", "failed", "exit status 4"],
      ["good", "passed", ""],
    ]);
    expect(r.results[0].tail).toBeDefined();
  });

  it("skips dependents of a failed check", async () => {
    const r = await run(plan([step("build", node("process.exit(1)"), { weight: "heavy" }), step("e2e:x", node(""), { weight: "heavy", kind: "e2e", after: ["build"] })]));
    expect(r.results[1]).toMatchObject({ status: "skipped", reason: "needs build, which failed" });
    expect(r.exitCode).toBe(EXIT.failed);
  });

  it("reports --skip as skipped and the run as incomplete (exit 3)", async () => {
    const r = await run(plan([step("a", node("")), step("e2e:x", node(""), { kind: "e2e", weight: "heavy" })]), { skip: ["e2e"] });
    expect(r.results[1]).toMatchObject({ status: "skipped", reason: "excluded by --skip" });
    expect(r.exitCode).toBe(EXIT.incomplete);
    expect(r.complete).toBe(false);
  });

  it("does not build the app when every scenario that needs it is excluded", async () => {
    const r = await run(
      plan([
        step("build-app", node("process.exit(9)"), { kind: "native-build", weight: "heavy" }),
        step("e2e:x", node(""), { kind: "e2e", weight: "heavy", after: ["build-app"] }),
      ]),
      { skip: ["e2e"] },
    );
    expect(r.results.map((x) => [x.id, x.status, x.reason])).toEqual([
      ["build-app", "skipped", "only needed by checks excluded by --skip"],
      ["e2e:x", "skipped", "excluded by --skip"],
    ]);
    expect(r.exitCode).toBe(EXIT.incomplete);
  });

  it("reports a missing tool or another platform as unavailable", async () => {
    const r = await run(plan([step("tool", ["impact-test-no-such-command-xyz"]), step("win", node(""), { platform: "plan9" })]));
    expect(r.results.map((x) => x.status)).toEqual(["unavailable", "unavailable"]);
    expect(r.results[0].reason).toMatch(/command not found/);
    expect(r.exitCode).toBe(EXIT.incomplete);
  });

  it("treats a scenario that skipped itself as unavailable, and partial skips as a passing note", async () => {
    const whole = await run(plan([step("e2e:a", node('console.log("SKIPPED: Docker is not available")'), { kind: "e2e" })]));
    expect(whole.results[0]).toMatchObject({ status: "unavailable" });
    expect(whole.results[0].reason).toMatch(/Docker is not available/);
    const byName = await run(plan([step("e2e:b", node('console.log("skip review-x (run by name)")'), { kind: "e2e" })]));
    expect(byName.results[0].status).toBe("unavailable");
    const partial = await run(plan([step("e2e:c", node('console.log("SKIPPED Lean step: toolchain missing")'), { kind: "e2e" })]));
    expect(partial.results[0]).toMatchObject({ status: "passed" });
    expect(partial.results[0].reason).toMatch(/skipped steps/);
    expect(classify({ kind: "static", command: ["x"] }, { code: 0, error: null, skips: ["SKIPPED: y"] }).status).toBe("passed");
  });

  it("stops starting checks after a failure with fail-fast", async () => {
    const r = await run(plan([step("a", node("process.exit(1)"), { weight: "heavy" }), step("b", node(""), { weight: "heavy" })]), { failFast: true });
    expect(r.results[1]).toMatchObject({ status: "skipped", reason: "--fail-fast after an earlier failure" });
  });
});

describe("evidence for a snapshot", () => {
  it("records what the results are evidence for", async () => {
    const p = { ...plan([step("a", node(""))]), base: { head: "h".repeat(40), mergeBase: "b".repeat(40) }, inputs: "feedfacefeedface" };
    const r = await run(p, { currentPlanHash: () => "testplan0000" });
    expect(r.exitCode).toBe(EXIT.ok);
    expect(r.stale).toBeNull();
    expect(r.snapshot).toEqual({ head: "h".repeat(40), base: "b".repeat(40), inputs: "feedfacefeedface" });
    expect(r.wallSeconds).toBeGreaterThanOrEqual(0);
  });

  it("a tree that changed during the run makes the result stale, never a pass", async () => {
    const r = await run(plan([step("a", node(""))]), { currentPlanHash: () => "otherplan000" });
    expect(r.results[0].status).toBe("passed");
    expect(r.stale).toEqual({ planned: "testplan0000", now: "otherplan000" });
    expect(r.exitCode).toBe(EXIT.usage);
    expect(r.complete).toBe(false);
    // A failure is still reported as a failure.
    const failed = await run(plan([step("a", node("process.exit(1)"))]), { currentPlanHash: () => "otherplan000" });
    expect(failed.exitCode).toBe(EXIT.failed);
    expect(failed.stale).not.toBeNull();
    // A tree that can no longer be resolved is stale as well.
    const broken = await run(plan([step("a", node(""))]), {
      currentPlanHash: () => {
        throw new Error("no base");
      },
    });
    expect(broken.exitCode).toBe(EXIT.usage);
  });
});

describe("scheduling", () => {
  // Each check appends "<id> start <ms>" and "<id> end <ms>" to a log file.
  const logged = (log, id, ms) =>
    node(`const fs=require("fs");fs.appendFileSync(${JSON.stringify(log)},"${id} start "+Date.now()+"\\n");setTimeout(()=>fs.appendFileSync(${JSON.stringify(log)},"${id} end "+Date.now()+"\\n"),${ms})`);
  const intervals = (log) => {
    const spans = {};
    for (const line of readFileSync(log, "utf8").trim().split("\n")) {
      const [id, what, at] = line.split(" ");
      (spans[id] ??= {})[what] = Number(at);
    }
    return Object.values(spans);
  };
  // The largest number of checks running at the same instant (a sweep over
  // starts and ends; an end at the same millisecond as a start comes first).
  const maxOverlap = (spans) => {
    const events = spans.flatMap((s) => [[s.start, 1], [s.end, -1]]).sort((a, b) => a[0] - b[0] || a[1] - b[1]);
    let active = 0;
    let most = 0;
    for (const [, delta] of events) most = Math.max(most, (active += delta));
    return most;
  };

  it("bounds light concurrency by jobs and never overlaps heavy checks", async () => {
    const dir = mkdtempSync(join(tmpdir(), "xp-impact-run-"));
    dirs.push(dir);
    const lightLog = join(dir, "light.log");
    const heavyLog = join(dir, "heavy.log");
    const checks = [
      ...["l1", "l2", "l3", "l4"].map((id) => step(id, logged(lightLog, id, 400))),
      ...["h1", "h2", "h3"].map((id) => step(id, logged(heavyLog, id, 150), { weight: "heavy" })),
    ];
    const r = await run(plan(checks), { jobs: 2 });
    expect(r.exitCode).toBe(EXIT.ok);
    expect(maxOverlap(intervals(lightLog))).toBe(2);
    expect(maxOverlap(intervals(heavyLog))).toBe(1);
    const lastLight = Math.max(...intervals(lightLog).map((s) => s.end));
    expect(Math.min(...intervals(heavyLog).map((s) => s.start))).toBeGreaterThanOrEqual(lastLight);
  }, 20_000);

  it("runs scenarios up to e2eJobs at once, after the builds, and keeps resource holders apart", async () => {
    const dir = mkdtempSync(join(tmpdir(), "xp-impact-run-"));
    dirs.push(dir);
    const log = join(dir, "e2e.log");
    const scenario = (id, resources = []) => step(`e2e:${id}`, logged(log, id, 300), { kind: "e2e", weight: "heavy", after: ["build-app"], resources });
    const checks = [
      step("build-app", logged(log, "build", 200), { kind: "native-build", weight: "heavy" }),
      scenario("a"),
      scenario("drag", ["desktop"]),
      scenario("b"),
      scenario("clip1", ["clipboard"]),
      scenario("clip2", ["clipboard"]),
      scenario("c"),
    ];
    const r = await run(plan(checks), { e2eJobs: 3 });
    expect(r.exitCode).toBe(EXIT.ok);
    expect(r.settings.e2eJobs).toBe(3);
    expect(r.results.map((x) => x.id)).toEqual(checks.map((c) => c.id));
    const spans = {};
    for (const line of readFileSync(log, "utf8").trim().split("\n")) {
      const [id, what, at] = line.split(" ");
      (spans[id] ??= {})[what] = Number(at);
    }
    const overlap = (x, y) => spans[x].start < spans[y].end && spans[y].start < spans[x].end;
    const ids = ["a", "b", "c", "clip1", "clip2", "drag"];
    // Nothing overlaps the build or the desktop scenario; the two clipboard scenarios never overlap each other.
    for (const id of ids) expect(overlap("build", id)).toBe(false);
    for (const id of ids.filter((i) => i !== "drag")) expect(overlap("drag", id)).toBe(false);
    expect(overlap("clip1", "clip2")).toBe(false);
    // Free scenarios do overlap, never more than three at once.
    expect(overlap("a", "b")).toBe(true);
    const all = ids.map((id) => spans[id]);
    expect(maxOverlap(all)).toBeLessThanOrEqual(3);
  }, 20_000);

  it("runs scenarios one at a time by default", async () => {
    const dir = mkdtempSync(join(tmpdir(), "xp-impact-run-"));
    dirs.push(dir);
    const log = join(dir, "e2e.log");
    const checks = ["a", "b", "c"].map((id) => step(`e2e:${id}`, logged(log, id, 150), { kind: "e2e", weight: "heavy" }));
    const r = await run(plan(checks));
    expect(r.settings.e2eJobs).toBe(1);
    expect(maxOverlap(intervals(log))).toBe(1);
  }, 20_000);

  it("keeps result order equal to plan order", async () => {
    const r = await run(plan([step("slow", node("setTimeout(()=>{},300)")), step("fast", node(""))]), { jobs: 2 });
    expect(r.results.map((x) => x.id)).toEqual(["slow", "fast"]);
    expect(r.plan).toBe("testplan0000");
  });
});
