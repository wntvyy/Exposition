// The manifest against the real repository: complete coverage of tracked files
// and scenarios, gates identical to `npm run check`, and representative plans
// computed on the real import graph and crate graph (read-only).
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { beforeAll, describe, expect, it } from "vitest";

import { AREAS, DEFERRED, INVARIANT_SCENARIOS, SCENARIO_POLICY, SCENARIO_RESOURCES, SCENARIOS, VITEST_INCLUDE } from "./manifest.mjs";
import { areasFor, buildPlan, GATES, scenarioPolicy } from "./plan.mjs";
import { buildIndex, changeFacts } from "./repo.mjs";

const ROOT = resolve(import.meta.dirname, "../..");
const tracked = execFileSync("git", ["ls-files", "-z"], { cwd: ROOT, encoding: "utf8" }).split("\0").filter(Boolean);
const scenarioFiles = tracked.filter((f) => /^scripts\/e2e\/scenarios\/[^/]+\.mjs$/.test(f)).map((f) => f.slice("scripts/e2e/scenarios/".length, -".mjs".length));

describe("manifest completeness", () => {
  it("maps every tracked file to an area", () => {
    expect(tracked.filter((f) => areasFor(f).length === 0)).toEqual([]);
  });

  it("names every scenario, and only scenarios that exist", () => {
    const named = new Set([...AREAS.flatMap((a) => [...(a.e2e ?? []), ...(a.visual ?? []), ...(a.live ?? [])]), ...INVARIANT_SCENARIOS, ...Object.keys(SCENARIO_POLICY)]);
    expect(scenarioFiles.filter((s) => !named.has(s))).toEqual([]);
    expect([...named].filter((s) => !scenarioFiles.includes(s))).toEqual([]);
    expect(scenarioFiles.filter((s) => s.startsWith("live-") && SCENARIO_POLICY[s] !== "live")).toEqual([]);
  });

  it("uses known check and deferred names", () => {
    for (const area of AREAS) {
      for (const id of area.checks ?? []) expect(["check-all", "bindings-full", ...Object.keys(GATES)]).toContain(id);
      for (const key of area.deferred ?? []) expect(Object.keys(DEFERRED)).toContain(key);
    }
  });

  it("mirrors the gates of npm run check exactly", () => {
    const text = readFileSync(join(ROOT, "scripts/check.mjs"), "utf8");
    const gates = [...text.matchAll(/\["([^"]+)",\s*("node"|"cargo"|npm),\s*\[([^\]]*)\]\]/g)].map(([, label, cmd, args]) => ({
      label,
      command: [cmd === "npm" ? "npm" : JSON.parse(cmd), ...JSON.parse(`[${args}]`)],
    }));
    expect(gates.length).toBe(9);
    expect(Object.values(GATES).map(({ label, command }) => ({ label, command }))).toEqual(gates);
  });

  it("uses the vitest include patterns of vite.config.ts", () => {
    const text = readFileSync(join(ROOT, "vite.config.ts"), "utf8");
    const include = JSON.parse(/include:\s*(\[[^\]]*\])/.exec(text)[1]);
    expect(VITEST_INCLUDE).toEqual(include);
  });
});

// Planning on the real repository reads git; allow for a loaded machine.
describe("representative plans on the real repository", { timeout: 30_000 }, () => {
  let index;
  const base = { ok: true, ref: "origin/main", mergeBase: "HEAD", head: "HEAD", source: "test" };
  const planFor = (...paths) => {
    const changes = paths.map((path) => ({ status: "M", path }));
    return buildPlan({ mode: "affected", base, changes, index: { ...index, ...changeFacts({ cwd: ROOT, changes, mergeBase: "HEAD" }) } });
  };
  const command = (p, id) => p.checks.find((c) => c.id === id)?.command ?? [];
  const crates = (p) => command(p, "rust-tests").filter((_, i, a) => a[i - 1] === "-p");
  beforeAll(() => {
    index = buildIndex({ cwd: ROOT, changes: [], mergeBase: "HEAD" });
    expect(index.crates?.length).toBeGreaterThan(10);
  }, 30_000);

  it("a frontend feature module selects its own tests and the area scenarios", () => {
    const p = planFor("src/features/outline/store.ts");
    expect(p.level).toBe("local");
    expect(command(p, "frontend-tests")).toContain("src/features/outline/store.test.ts");
    expect(p.checks.map((c) => c.id)).toEqual(expect.arrayContaining(["typecheck", "frontend-build", "build-app", "e2e:editor-find-outline"]));
    expect(crates(p)).toEqual([]);
  });

  it("the graph crate tests every dependent crate and escalates to broad", () => {
    const p = planFor("crates/exposition-graph/src/lib.rs");
    expect(p.level).toBe("broad");
    expect(crates(p)).toEqual(["exposition", "exposition-agents", "exposition-graph", "exposition-lean", "exposition-research", "exposition-workflow"]);
    expect(command(p, "frontend-tests")).toEqual(["npm", "test"]);
  });

  it("a fixture reaches the Rust tests that include or load it and the scenarios that open it", () => {
    const p = planFor("tests/fixtures/lean-basic/LeanBasic/Defs.lean");
    expect(crates(p)).toEqual(expect.arrayContaining(["exposition-graph", "exposition-lean", "exposition-agents", "exposition-workflow"]));
    expect(p.checks.map((c) => c.id)).toEqual(expect.arrayContaining(["e2e:lean-check", "e2e:lean-evidence"]));
  });

  it("a frontend file that a Rust test reads selects that crate", () => {
    const p = planFor("src/features/editor/register.tsx");
    expect(crates(p)).toEqual(["exposition", "exposition-agents"]);
  });

  it("a lower crate's change runs the scenarios of the areas whose crates depend on it", () => {
    const ids = (p) => p.checks.map((c) => c.id);
    // Typesetting is used by the agents crate (an agent builds a document): the agent scenarios run.
    const typesetting = planFor("crates/exposition-typesetting/src/service.rs");
    expect(typesetting.level).toBe("local");
    expect(ids(typesetting)).toEqual(expect.arrayContaining(["e2e:page-typst", "e2e:agents-harness", "e2e:agent-save"]));
    // Lean is used by agents and workflows.
    const lean = planFor("crates/exposition-lean/src/lib.rs");
    expect(ids(lean)).toEqual(expect.arrayContaining(["e2e:lean-check", "e2e:agents-harness", "e2e:workflow-formal", "e2e:workflow-run"]));
    // Research is used by agents and workflows; agents by workflows.
    expect(ids(planFor("crates/exposition-research/src/lib.rs"))).toEqual(expect.arrayContaining(["e2e:agents-harness", "e2e:workflow-run"]));
    expect(ids(planFor("crates/exposition-agents/src/lib.rs"))).toEqual(expect.arrayContaining(["e2e:workflow-run", "e2e:review-m8-agent"]));
    // The embedded kernel host is research code: research's dependents run too.
    expect(ids(planFor("workers/kernel-host/exposition_kernel_host.py"))).toEqual(expect.arrayContaining(["e2e:agents-harness", "e2e:workflow-run"]));
    // A test-only change does not reach other crates.
    const testOnly = planFor("crates/exposition-lean/tests/lean_server.rs");
    expect(ids(testOnly)).not.toContain("e2e:agents-harness");
    // Every named scenario exists and is not deferred.
    for (const area of AREAS) for (const name of area.dependents ?? []) expect(scenarioFiles).toContain(name);
  });

  it("the kernel host compiled into the research crate tests research and its dependents", () => {
    const p = planFor("workers/kernel-host/exposition_kernel_host.py");
    expect(crates(p)).toEqual(expect.arrayContaining(["exposition-research", "exposition-agents", "exposition-workflow", "exposition"]));
    expect(p.checks.map((c) => c.id)).toContain("e2e:research-python");
  });

  it("cross-language test reads: a Rust source read by a frontend test, a stylesheet read by a test", () => {
    expect(command(planFor("crates/exposition-graph/src/workflow.rs"), "frontend-tests")).toEqual(["npm", "test"]); // broad (graph)
    const css = planFor("src/ui/tokens.css"); // shared tokens: broad; tokens.test.ts reads the file
    expect(index.refs["src/ui/tokens.test.ts"]).toContainEqual({ kind: "path", target: "src/ui/tokens.css", compiled: false });
    expect(command(css, "frontend-tests")).toEqual(["npm", "test"]);
    expect(css.checks.map((c) => c.id)).toEqual(expect.arrayContaining(["e2e:ui-shell", "e2e:pages-review", "e2e:graph-canvas"]));
  });

  it("files scenarios reach through join() segments or variables are covered (review findings)", () => {
    expect(planFor("scripts/e2e/os-drag.ps1").checks.map((c) => c.id)).toContain("e2e:qol-files");
    expect(planFor("src-tauri/src/agents.rs").checks.map((c) => c.id)).toContain("e2e:review-sec-acl");
    expect(planFor(".gitattributes").level).toBe("full");
    expect(planFor("src/ui/components/Badge.tsx").level).toBe("broad");
  });

  it("a stylesheet-only change to the Markdown component runs the agent response scenarios (QC mapping gap)", () => {
    const ids = planFor("src/ui/components/Markdown.css").checks.map((c) => c.id);
    expect(ids).toEqual(expect.arrayContaining(["e2e:a1-responses", "e2e:v2-tour-agents", "e2e:ui-shell"]));
  });

  it("the native-dialog and OS-drag helpers select every scenario that uses them (QC mapping gap)", () => {
    const importers = (helper) =>
      scenarioFiles.filter((s) => readFileSync(join(ROOT, "scripts/e2e/scenarios", `${s}.mjs`), "utf8").includes(`from "../${helper}"`)).sort();
    // The named lists are exactly the importing scenarios, so a new user of a helper cannot be forgotten.
    expect([...SCENARIOS.nativeDialog].sort()).toEqual(importers("native-dialog.mjs"));
    expect([...SCENARIOS.osDrag].sort()).toEqual(importers("os-drag.mjs"));
    // Scenarios that hold the desktop or the clipboard are never run next to a conflicting one.
    expect([...SCENARIO_RESOURCES.desktop].sort()).toEqual([...new Set([...importers("native-dialog.mjs"), ...importers("os-drag.mjs")])].sort());
    const mentionsClipboard = scenarioFiles
      .filter((s) => !scenarioPolicy(s) && /clipboard|Control(OrMeta)?\+[CVX]\b/i.test(readFileSync(join(ROOT, "scripts/e2e/scenarios", `${s}.mjs`), "utf8")))
      .sort();
    expect([...SCENARIO_RESOURCES.clipboard].sort()).toEqual(mentionsClipboard);
    expect(planFor("scripts/e2e/os-drag.mjs").checks.find((c) => c.id === "e2e:qol-files").resources).toEqual(["desktop"]);
    expect(planFor("scripts/e2e/scenarios/p1-files.mjs").checks.find((c) => c.id === "e2e:p1-files").resources).toEqual(["clipboard"]);
    expect(planFor("scripts/e2e/scenarios/smoke.mjs").checks.find((c) => c.id === "e2e:smoke").resources).toEqual([]);
    for (const file of ["scripts/e2e/native-dialog.ps1", "scripts/e2e/native-dialog.mjs"]) {
      const p = planFor(file);
      expect(p.checks.map((c) => c.id)).toEqual(expect.arrayContaining(SCENARIOS.nativeDialog.map((s) => `e2e:${s}`)));
      expect(p.uncovered).toEqual([]);
    }
    for (const file of ["scripts/e2e/os-drag.ps1", "scripts/e2e/os-drag.mjs"]) {
      const p = planFor(file);
      expect(p.checks.map((c) => c.id)).toEqual(expect.arrayContaining(SCENARIOS.osDrag.map((s) => `e2e:${s}`)));
      expect(p.uncovered).toEqual([]);
    }
  });

  it("the shared formatter selects its importers' tests and the scenarios that assert formatted text", () => {
    const p = planFor("src/lib/format.ts");
    expect(p.level).toBe("local");
    // Many features format through it: its importers' tests are selected, or the whole suite when they are too many.
    const tests = command(p, "frontend-tests");
    expect(tests.join(" ") === "npm test" || ["src/lib/format.test.ts", "src/features/jobs/model.test.ts"].every((f) => tests.includes(f))).toBe(true);
    expect(p.checks.map((c) => c.id)).toEqual(expect.arrayContaining(["e2e:qol-media", "e2e:pdf-viewer", "e2e:agents-composer-input"]));
  });

  it("permissions are a security boundary", () => {
    const p = planFor("src-tauri/capabilities/default.json");
    expect(p.level).toBe("broad");
    expect(p.checks.map((c) => c.id)).toEqual(expect.arrayContaining(["permissions", "e2e:review-sec-acl", "e2e:review-sec-ipc"]));
    expect(crates(p)).toEqual(["exposition"]);
  });

  it("the selector's own code selects its own tests", () => {
    const p = planFor("scripts/impact/plan.mjs");
    expect(command(p, "frontend-tests")).toEqual(expect.arrayContaining(["scripts/impact/plan.test.mjs", "scripts/impact/cli.test.mjs", "scripts/impact/manifest.test.mjs"]));
    expect(p.uncovered).toEqual([]);
  });
});
