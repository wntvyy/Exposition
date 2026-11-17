// Planner behavior on synthetic repositories: which checks each kind of change
// selects, how it escalates, and that plans are deterministic.
import { describe, expect, it } from "vitest";

import { INVARIANT_SCENARIOS, SCENARIOS } from "./manifest.mjs";
import { buildPlan, globToRegExp, isRustTestOnly } from "./plan.mjs";

const ALL_SCENARIOS = [...new Set([...Object.values(SCENARIOS).flat(), ...INVARIANT_SCENARIOS, "live-agents", "live-graph", "live-restricted", "live-agentctx", "live-a1-loop", "live-workflow", "live-workflow-audit", "live-m9-agents", "live-review-m8-agent", "mathlib", "mathlib-setup"])].sort();

const CRATES = [
  { name: "exposition-core", dir: "crates/exposition-core", deps: [] },
  { name: "exposition-workspace", dir: "crates/exposition-workspace", deps: ["exposition-core"] },
  { name: "exposition-page", dir: "crates/exposition-page", deps: ["exposition-core", "exposition-workspace"] },
  { name: "exposition-tex", dir: "crates/exposition-tex", deps: ["exposition-core", "exposition-page"] },
  { name: "exposition-markdown", dir: "crates/exposition-markdown", deps: ["exposition-core", "exposition-page", "exposition-tex"] }, // tex: dev-dependency
  { name: "exposition-graph", dir: "crates/exposition-graph", deps: ["exposition-core", "exposition-workspace"] },
  { name: "exposition-lean", dir: "crates/exposition-lean", deps: ["exposition-core", "exposition-graph"] },
  { name: "exposition-agents", dir: "crates/exposition-agents", deps: ["exposition-graph", "exposition-lean"] },
  { name: "exposition", dir: "src-tauri", deps: ["exposition-agents", "exposition-graph", "exposition-lean", "exposition-markdown", "exposition-page", "exposition-tex", "exposition-workspace", "exposition-core"] },
];

function index(overrides = {}) {
  const files = [
    "src/ui/components/Button.tsx",
    "src/ui/components/index.ts",
    "src/ui/components/Button.test.tsx",
    "src/features/outline/store.ts",
    "src/features/outline/store.test.ts",
    "src/features/outline/Outline.tsx",
    "src/features/outline/outline.css",
    "src/features/graph/Inspector.tsx",
    "src/features/graph/Inspector.test.tsx",
    "src/app/Workbench.tsx",
    "src/app/Workbench.test.tsx",
    "scripts/e2e/workspace-helpers.mjs",
    "scripts/e2e/scenarios/workspace-files.mjs",
    "scripts/e2e/scenarios/qol-files.mjs",
    "scripts/e2e/scenarios/live-agents.mjs",
    "crates/exposition-graph/src/lib.rs",
    "crates/exposition-graph/src/extract.rs",
    "crates/exposition-graph/tests/hostile.rs",
    "crates/exposition-agents/src/prompt.rs",
    "crates/exposition-tex/src/lib.rs",
    "crates/exposition-core/src/lib.rs",
    "tests/fixtures/lean-basic/Defs.lean",
    "tests/fixtures/orphan/data.txt",
    "docs/functionality.md",
    ...ALL_SCENARIOS.map((s) => `scripts/e2e/scenarios/${s}.mjs`),
  ];
  return {
    files: [...new Set(files)].sort(),
    imports: {
      "src/ui/components/index.ts": ["src/ui/components/Button.tsx"],
      "src/ui/components/Button.test.tsx": ["src/ui/components/Button.tsx"],
      "src/features/outline/Outline.tsx": ["src/features/outline/outline.css", "src/features/outline/store.ts", "src/ui/components/index.ts"],
      "src/features/outline/store.test.ts": ["src/features/outline/store.ts"],
      "src/features/graph/Inspector.tsx": ["src/ui/components/index.ts"],
      "src/features/graph/Inspector.test.tsx": ["src/features/graph/Inspector.tsx"],
      "src/app/Workbench.tsx": ["src/features/outline/Outline.tsx", "src/features/graph/Inspector.tsx"],
      "src/app/Workbench.test.tsx": ["src/app/Workbench.tsx"],
      "scripts/e2e/scenarios/workspace-files.mjs": ["scripts/e2e/workspace-helpers.mjs"],
      "scripts/e2e/scenarios/qol-files.mjs": ["scripts/e2e/workspace-helpers.mjs"],
    },
    refs: {
      "crates/exposition-graph/src/extract.rs": [{ kind: "path", target: "tests/fixtures/lean-basic/Defs.lean", compiled: false }],
      "crates/exposition-agents/src/prompt.rs": [{ kind: "path", target: "src/features/graph/Inspector.tsx", compiled: false }],
      "scripts/e2e/scenarios/lean-check.mjs": [{ kind: "fixture", target: "lean-basic" }],
    },
    crates: CRATES,
    tsExports: [],
    tsRemoved: [],
    packageJson: { kind: "unchanged", detail: "unchanged" },
    scenarios: ALL_SCENARIOS,
    ...overrides,
  };
}

const BASE = { ok: true, ref: "origin/main", mergeBase: "a".repeat(40), head: "b".repeat(40), source: "default" };
const plan = (changes, { idx = index(), base = BASE, options } = {}) => buildPlan({ mode: "affected", base, changes, index: idx, options });
const ids = (p) => p.checks.map((c) => c.id);
const check = (p, id) => p.checks.find((c) => c.id === id);
const e2e = (p) => ids(p).filter((id) => id.startsWith("e2e:")).map((id) => id.slice(4));
const M = (path) => ({ status: "M", path });

describe("globs", () => {
  it("matches segments, deep paths, and alternatives", () => {
    expect(globToRegExp("src/features/graph/**").test("src/features/graph/a/b.tsx")).toBe(true);
    expect(globToRegExp("crates/*/Cargo.toml").test("crates/x/Cargo.toml")).toBe(true);
    expect(globToRegExp("crates/*/Cargo.toml").test("crates/x/y/Cargo.toml")).toBe(false);
    expect(globToRegExp("src/**/*.test.{ts,tsx}").test("src/a.test.tsx")).toBe(true);
    expect(globToRegExp("src/**/*.test.{ts,tsx}").test("src/a/b.test.ts")).toBe(true);
    expect(globToRegExp("src-tauri/src/workflow_*.rs").test("src-tauri/src/workflow_runs.rs")).toBe(true);
  });

  it("recognizes Rust sources only tests use", () => {
    expect(isRustTestOnly("crates/exposition-graph/tests/hostile.rs")).toBe(true);
    expect(isRustTestOnly("crates/exposition-workspace/src/service/files_tests.rs")).toBe(true);
    expect(isRustTestOnly("src-tauri/src/project_ops/tests.rs")).toBe(true);
    expect(isRustTestOnly("crates/exposition-graph/src/lib.rs")).toBe(false);
  });
});

describe("local changes", () => {
  it("a feature module selects its tests, transitive consumers, and the area's scenarios", () => {
    const p = plan([M("src/features/outline/store.ts")]);
    expect(p.level).toBe("local");
    expect(check(p, "frontend-tests").command.slice(3)).toEqual(["src/app/Workbench.test.tsx", "src/features/outline/store.test.ts"]);
    expect(ids(p)).toEqual(expect.arrayContaining(["private-paths", "typecheck", "frontend-build", "build-app"]));
    expect(e2e(p)).toEqual(expect.arrayContaining(SCENARIOS.editor));
    expect(ids(p).some((id) => id.startsWith("rust"))).toBe(false);
    expect(check(p, "e2e:editor-find-outline").after).toEqual(["build-app"]);
  });

  it("a shared UI component is broad: the whole frontend suite and the invariant scenarios", () => {
    const p = plan([M("src/ui/components/Button.tsx")]);
    expect(p.level).toBe("broad");
    expect(check(p, "frontend-tests").command).toEqual(["npm", "test"]);
    expect(e2e(p)).toEqual(expect.arrayContaining([...INVARIANT_SCENARIOS, "editor-find-outline", "graph-canvas"]));
  });

  it("a test-only change runs that test and no scenario", () => {
    const p = plan([M("src/features/outline/store.test.ts")]);
    expect(check(p, "frontend-tests").command.slice(3)).toEqual(["src/features/outline/store.test.ts"]);
    expect(e2e(p)).toEqual([]);
    expect(ids(p)).not.toContain("build-app");
  });

  it("a stylesheet-only change runs the area's visual scenarios", () => {
    const p = plan([M("src/features/outline/outline.css")]);
    expect(e2e(p).sort()).toEqual(["editor-find-outline", "qol-media"]);
    expect(p.notes.join("\n")).toMatch(/ui-reference/);
  });

  it("a scenario helper runs the scenarios that import it", () => {
    const p = plan([M("scripts/e2e/workspace-helpers.mjs")]);
    expect(e2e(p)).toEqual(["qol-files", "workspace-files"]);
  });

  it("documentation needs no automated check and is reported as uncovered", () => {
    const p = plan([M("docs/functionality.md")]);
    expect(ids(p)).toEqual(["private-paths"]);
    expect(p.uncovered).toEqual(["docs/functionality.md"]);
  });
});

describe("Rust consumers", () => {
  it("a crate change tests the crate and every transitive dependent", () => {
    const p = plan([M("crates/exposition-tex/src/lib.rs")]);
    // markdown uses tex only as a dev-dependency: its tests are consumers too.
    expect(check(p, "rust-tests").command).toEqual(["cargo", "test", "-p", "exposition", "-p", "exposition-markdown", "-p", "exposition-tex", "--no-fail-fast"]);
    expect(check(p, "rust-lint").command.slice(0, 8)).toEqual(["cargo", "clippy", "-p", "exposition", "-p", "exposition-markdown", "-p", "exposition-tex"]);
    expect(ids(p)).toContain("rust-format");
  });

  it("a crate's integration test changes only that crate (plus the app crate as feature anchor)", () => {
    const p = plan([M("crates/exposition-graph/tests/hostile.rs")]);
    expect(check(p, "rust-tests").command).toEqual(["cargo", "test", "-p", "exposition", "-p", "exposition-graph", "--no-fail-fast"]);
    expect(check(p, "rust-tests").reasons.join("\n")).toMatch(/dependency features match the workspace build/);
    expect(e2e(p)).toEqual([]);
  });

  it("a frontend file a Rust test reads selects that crate's tests", () => {
    const p = plan([M("src/features/graph/Inspector.tsx")]);
    expect(check(p, "rust-tests").command).toEqual(["cargo", "test", "-p", "exposition", "-p", "exposition-agents", "--no-fail-fast"]);
  });

  it("a fixture selects the tests and scenarios that name it", () => {
    const p = plan([M("tests/fixtures/lean-basic/Defs.lean")]);
    expect(check(p, "rust-tests").command).toEqual(["cargo", "test", "-p", "exposition", "-p", "exposition-graph", "--no-fail-fast"]);
    expect(e2e(p)).toEqual(["lean-check"]);
    expect(p.level).toBe("local");
  });

  it("the shared core crate tests the whole workspace with the CI gate commands", () => {
    const p = plan([M("crates/exposition-core/src/lib.rs")]);
    expect(p.level).toBe("broad");
    expect(check(p, "rust-tests").command).toEqual(["cargo", "test", "--workspace", "--no-fail-fast"]);
    expect(check(p, "bindings").command).toEqual(["node", "scripts/check-bindings.mjs"]);
  });

  it("contract types use the scoped drift check, or the full gate when types are removed", () => {
    const changed = plan([M("crates/exposition-tex/src/lib.rs")], { idx: index({ tsExports: ["crates/exposition-tex/src/lib.rs"] }) });
    expect(check(changed, "bindings").command).toEqual(["node", "scripts/impact/bindings-drift.mjs", "-p", "exposition", "-p", "exposition-tex"]);
    const removed = plan([M("crates/exposition-tex/src/lib.rs")], {
      idx: index({ tsExports: ["crates/exposition-tex/src/lib.rs"], tsRemoved: ["crates/exposition-tex/src/lib.rs"] }),
    });
    expect(check(removed, "bindings").command).toEqual(["node", "scripts/check-bindings.mjs"]);
  });
});

describe("escalation", () => {
  it("graph changes are broad: dependents, all frontend tests, and invariant scenarios", () => {
    const p = plan([M("crates/exposition-graph/src/lib.rs")]);
    expect(p.level).toBe("broad");
    expect(p.escalations[0].reason).toMatch(/area graph/);
    expect(check(p, "frontend-tests").command).toEqual(["npm", "test"]);
    expect(check(p, "rust-tests").command).toEqual(["cargo", "test", "-p", "exposition", "-p", "exposition-agents", "-p", "exposition-graph", "-p", "exposition-lean", "--no-fail-fast"]);
    expect(e2e(p)).toEqual(expect.arrayContaining([...INVARIANT_SCENARIOS, ...SCENARIOS.graph]));
    expect(p.deferred.map((d) => d.id)).toContain("live:live-graph");
  });

  it("an unmapped path escalates to full validation and says why", () => {
    const p = plan([{ status: "?", path: "newdir/thing.txt" }]);
    expect(p.level).toBe("full");
    expect(p.escalations.map((e) => e.reason).join("\n")).toMatch(/newdir\/thing\.txt matches no area/);
    expect(ids(p)).toContain("check-all");
    expect(ids(p)).not.toContain("private-paths"); // subsumed by npm run check
    expect(e2e(p).length).toBeGreaterThan(40);
  });

  it("a fixture nothing names is uncertain and escalates", () => {
    const p = plan([M("tests/fixtures/orphan/data.txt")]);
    expect(p.level).toBe("broad");
    expect(p.escalations[0].reason).toMatch(/consumers are unknown/);
  });

  it("a manifest naming a missing scenario is an incomplete mapping", () => {
    const idx = index({ scenarios: ALL_SCENARIOS.filter((s) => s !== "editor-find-outline") });
    const p = plan([M("src/features/outline/store.ts")], { idx });
    expect(p.level).toBe("full");
    expect(p.escalations.some((e) => /editor-find-outline/.test(e.reason))).toBe(true);
  });

  it("an unavailable crate graph escalates Rust changes", () => {
    const p = plan([M("crates/exposition-tex/src/lib.rs")], { idx: index({ crates: null }) });
    expect(p.level).toBe("full");
  });

  it("an unusable comparison base escalates to full validation", () => {
    const p = plan([], { base: { ok: false, reason: "no merge base with origin/main: the clone is shallow" } });
    expect(p.level).toBe("full");
    expect(p.escalations[0].reason).toMatch(/shallow/);
  });

  it("package.json: added scripts are local, anything else is full", () => {
    const added = plan([M("package.json")], { idx: index({ packageJson: { kind: "scripts-added", detail: "only script entries added: x" } }) });
    expect(added.level).toBe("local");
    const deps = plan([M("package.json")], { idx: index({ packageJson: { kind: "changed", detail: "fields other than scripts changed" } }) });
    expect(deps.level).toBe("full");
  });

  it("the CI gate runner runs npm run check, which subsumes the individual gates", () => {
    const p = plan([M("scripts/check.mjs"), M("src/features/outline/store.ts")]);
    expect(ids(p)).toContain("check-all");
    expect(ids(p).filter((id) => ["private-paths", "typecheck", "frontend-tests", "rust-tests"].includes(id))).toEqual([]);
  });
});

describe("renames, deletions, and no changes", () => {
  it("a rename affects the consumers of both the old and the new path", () => {
    const p = plan([{ status: "R", from: "src/features/outline/store.ts", path: "src/features/settings/store.ts" }]);
    expect(check(p, "frontend-tests").command.slice(3)).toEqual(["src/app/Workbench.test.tsx", "src/features/outline/store.test.ts"]);
    expect(e2e(p)).toEqual(expect.arrayContaining([...SCENARIOS.editor, ...SCENARIOS.settings]));
  });

  it("a deleted module still selects the tests that import it", () => {
    const idx = index();
    idx.files = idx.files.filter((f) => f !== "src/features/outline/store.ts");
    const p = plan([{ status: "D", path: "src/features/outline/store.ts" }], { idx });
    expect(check(p, "frontend-tests").command.slice(3)).toContain("src/features/outline/store.test.ts");
    expect(p.uncovered).toEqual([]);
  });

  it("a deleted file nothing visibly uses is uncovered, not silently complete (review finding)", () => {
    const p = plan([{ status: "D", path: "scripts/e2e/os-drag.ps1" }]);
    expect(p.uncovered).toEqual(["scripts/e2e/os-drag.ps1"]);
  });

  it("broad plans report uncovered files too (review finding)", () => {
    const p = plan([M("crates/exposition-graph/src/lib.rs"), M("docs/functionality.md")]);
    expect(p.level).toBe("broad");
    expect(p.uncovered).toEqual(["docs/functionality.md"]);
  });

  it("no changes run only the baseline and say so", () => {
    const p = plan([]);
    expect(ids(p)).toEqual(["private-paths"]);
    expect(p.notes[0]).toMatch(/No changes/);
  });
});

describe("deferred qualification", () => {
  it("live scenarios are deferred unless --live is given", () => {
    const deferred = plan([M("crates/exposition-agents/src/prompt.rs")]);
    expect(deferred.deferred.map((d) => d.id)).toEqual(expect.arrayContaining(["live:live-agents", "live:live-a1-loop"]));
    expect(ids(deferred).some((id) => id.startsWith("live:"))).toBe(false);
    const live = plan([M("crates/exposition-agents/src/prompt.rs")], { options: { live: true } });
    expect(ids(live)).toContain("live:live-agents");
    expect(check(live, "live:live-agents").after).toEqual(["build-app"]);
    expect(live.hash).not.toBe(deferred.hash);
  });

  it("a changed live scenario is never run implicitly", () => {
    const p = plan([M("scripts/e2e/scenarios/live-agents.mjs")]);
    expect(ids(p)).toEqual(["private-paths"]);
    expect(p.deferred.map((d) => d.id)).toEqual(["live:live-agents"]);
  });

  it("any live-* scenario is live, even one the manifest does not list yet (review finding)", () => {
    const idx = index({ files: [...index().files, "scripts/e2e/scenarios/live-new.mjs"].sort(), scenarios: [...ALL_SCENARIOS, "live-new"].sort() });
    const p = plan([{ status: "?", path: "scripts/e2e/scenarios/live-new.mjs" }], { idx });
    expect(ids(p)).toEqual(["private-paths"]);
    expect(p.deferred.map((d) => d.id)).toEqual(["live:live-new"]);
    const full = buildPlan({ mode: "full", base: BASE, changes: [], index: idx });
    expect(ids(full)).not.toContain("e2e:live-new");
    expect(full.deferred.map((d) => d.id)).toContain("live:live-new");
  });

  it("--release adds the release build and the release smoke run (review finding)", () => {
    const deferred = plan([M("src-tauri/tauri.conf.json")]);
    expect(deferred.deferred.map((d) => d.id)).toContain("release");
    const p = plan([M("src-tauri/tauri.conf.json")], { options: { release: true } });
    expect(p.deferred.map((d) => d.id)).not.toContain("release");
    expect(check(p, "release-build").command).toEqual(["npm", "run", "build:release"]);
    expect(check(p, "release-smoke")).toMatchObject({
      command: ["node", "scripts/e2e/run.mjs", "smoke", "ui-shell", "pages-review"],
      env: { EXPOSITION_E2E_EXECUTABLE: "target/release/exposition.exe" },
      after: ["release-build"],
    });
  });

  it("full validation lists live, manual, and release qualification as deferred", () => {
    const p = buildPlan({ mode: "full", base: BASE, changes: [], index: index() });
    const deferred = p.deferred.map((d) => d.id);
    expect(deferred).toEqual(expect.arrayContaining(["live:live-agents", "manual:mathlib", "release"]));
    expect(e2e(p)).not.toContain("live-agents");
    expect(e2e(p)).not.toContain("mathlib");
  });
});

describe("determinism", () => {
  it("the same inputs give the same plan and hash regardless of change order", () => {
    const changes = [M("src/features/outline/store.ts"), M("crates/exposition-tex/src/lib.rs"), { status: "?", path: "scripts/e2e/workspace-helpers.mjs" }];
    const a = plan(changes);
    const b = plan([...changes].reverse());
    expect(JSON.stringify(b)).toBe(JSON.stringify(a));
    expect(a.hash).toMatch(/^[0-9a-f]{12}$/);
  });

  it("checks come in a fixed order: static, frontend, rust, build, e2e", () => {
    const p = plan([M("src/features/outline/store.ts"), M("crates/exposition-tex/src/lib.rs")]);
    const order = ids(p);
    expect(order.indexOf("private-paths")).toBeLessThan(order.indexOf("typecheck"));
    expect(order.indexOf("frontend-build")).toBeLessThan(order.indexOf("rust-format"));
    expect(order.indexOf("rust-tests")).toBeLessThan(order.indexOf("build-app"));
    expect(order.indexOf("build-app")).toBeLessThan(order.findIndex((id) => id.startsWith("e2e:")));
  });
});
