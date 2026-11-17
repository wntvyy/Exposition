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
  const maxOverlap = (spans) => Math.max(...spans.map((s) => spans.filter((o) => o.start < s.end && s.start < o.end).length));

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

  it("keeps result order equal to plan order", async () => {
    const r = await run(plan([step("slow", node("setTimeout(()=>{},300)")), step("fast", node(""))]), { jobs: 2 });
    expect(r.results.map((x) => x.id)).toEqual(["slow", "fast"]);
    expect(r.plan).toBe("testplan0000");
  });
});
