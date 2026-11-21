// Builds a deterministic validation plan from a change set and a repository
// index. Pure: the same inputs always give the same plan (and plan hash), so a
// preview and an execution of the same tree run exactly the same checks.
// Inputs come from repo.mjs; policy comes from manifest.mjs.
import { createHash } from "node:crypto";

import {
  AREAS,
  DEFERRED,
  FIXTURE_DIR,
  INVARIANT_SCENARIOS,
  LEVELS,
  scenarioResources,
  SCENARIO_DIR,
  SCENARIO_POLICY,
  VITEST_INCLUDE,
} from "./manifest.mjs";

export const PLAN_SCHEMA = "exposition-impact/1";

/**
 * The gates of `npm run check` (scripts/check.mjs), with the same labels and
 * commands. manifest.test.mjs keeps the two lists identical.
 */
export const GATES = {
  "private-paths": { label: "private paths", kind: "static", weight: "light", command: ["node", "scripts/check-private-paths.mjs"] },
  permissions: { label: "permission files", kind: "static", weight: "light", command: ["node", "scripts/check-permissions.mjs"] },
  typecheck: { label: "typecheck", kind: "frontend", weight: "medium", command: ["npm", "run", "typecheck"] },
  "frontend-tests": { label: "frontend tests", kind: "frontend", weight: "medium", command: ["npm", "test"] },
  "frontend-build": { label: "frontend build", kind: "frontend", weight: "medium", command: ["npm", "run", "build:web"] },
  "rust-format": { label: "rust format", kind: "rust", weight: "light", command: ["cargo", "fmt", "--all", "--", "--check"] },
  "rust-lint": {
    label: "rust lint",
    kind: "rust",
    weight: "heavy",
    command: ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"],
  },
  "rust-tests": { label: "rust tests", kind: "rust", weight: "heavy", command: ["cargo", "test", "--workspace", "--no-fail-fast"] },
  bindings: { label: "bindings", kind: "rust", weight: "heavy", command: ["node", "scripts/check-bindings.mjs"] },
};

/** Above this many selected test files, the whole frontend suite runs instead. */
export const VITEST_FILE_LIMIT = 40;
const VITEST_CLI = ["node", "node_modules/vitest/vitest.mjs", "run"];
const MAX_REASONS = 6;

export const KINDS = ["static", "frontend", "rust", "gates", "native-build", "e2e", "live", "release"];

// ---------------------------------------------------------------- globs

const globCache = new Map();
/** `*` matches within a path segment, `**` across segments, `{a,b}` either. */
export function globToRegExp(glob) {
  let cached = globCache.get(glob);
  if (cached) return cached;
  let re = "";
  for (let i = 0; i < glob.length; i++) {
    const c = glob[i];
    if (c === "*" && glob[i + 1] === "*") {
      if (glob[i + 2] === "/") {
        re += "(?:.*/)?";
        i += 2;
      } else {
        re += ".*";
        i += 1;
      }
    } else if (c === "*") re += "[^/]*";
    else if (c === "?") re += "[^/]";
    else if (c === "{") re += "(?:";
    else if (c === "}") re += ")";
    else if (c === "," && glob.lastIndexOf("{", i) > glob.lastIndexOf("}", i)) re += "|";
    else re += c.replace(/[.+^$()|[\]\\]/g, "\\$&");
  }
  cached = new RegExp(`^${re}$`);
  globCache.set(glob, cached);
  return cached;
}

export const matchesGlob = (path, glob) => globToRegExp(glob).test(path);

export function areasFor(path, areas = AREAS) {
  return areas.filter((area) => area.paths.some((glob) => matchesGlob(path, glob)));
}

export const isVitestFile = (path) => VITEST_INCLUDE.some((glob) => matchesGlob(path, glob));
export const scenarioName = (path) =>
  path.startsWith(`${SCENARIO_DIR}/`) && path.endsWith(".mjs") && !path.slice(SCENARIO_DIR.length + 1).includes("/")
    ? path.slice(SCENARIO_DIR.length + 1, -".mjs".length)
    : null;

/** A Rust source that only tests use: changing it cannot affect dependents. */
export function isRustTestOnly(path) {
  return /(^|\/)(tests|benches|examples)\//.test(path) || /(^|\/)(tests|[\w-]+_tests)\.rs$/.test(path);
}

const levelRank = (level) => LEVELS.indexOf(level);

// ---------------------------------------------------------------- planning

class PlanBuilder {
  constructor() {
    this.checks = new Map();
    this.deferred = new Map();
    this.escalations = [];
    this.notes = new Set();
    this.level = "none";
  }

  escalate(level, reason) {
    if (levelRank(level) > levelRank(this.level)) this.level = level;
    if (levelRank(level) >= levelRank("broad")) this.escalations.push({ level, reason });
  }

  add(id, spec, reason) {
    let check = this.checks.get(id);
    if (!check) {
      check = { id, after: [], ...spec, reasons: new Set() };
      this.checks.set(id, check);
    }
    if (reason) check.reasons.add(reason);
    return check;
  }

  defer(id, spec, reason) {
    let item = this.deferred.get(id);
    if (!item) {
      item = { id, ...spec, reasons: new Set() };
      this.deferred.set(id, item);
    }
    if (reason) item.reasons.add(reason);
  }
}

function gate(builder, id, reason) {
  return builder.add(id, { ...GATES[id] }, reason);
}

/**
 * Why a scenario never runs implicitly, or undefined. Every `live-*` scenario
 * is live (the harness gives those names the real credential), listed or not.
 */
export function scenarioPolicy(name) {
  return SCENARIO_POLICY[name] ?? (name.startsWith("live-") ? "live" : undefined);
}

function deferScenario(builder, name, reason) {
  const policy = scenarioPolicy(name);
  if (policy === "live") {
    builder.defer(
      `live:${name}`,
      {
        label: `live scenario ${name}`,
        kind: "live",
        command: ["node", "scripts/e2e/run.mjs", name],
        flag: "--live",
        why: "Calls the real model provider with the user's saved key; runs only with explicit authorization.",
      },
      reason,
    );
  } else {
    builder.defer(`manual:${name}`, { label: `scenario ${name}`, kind: "manual", command: ["node", "scripts/e2e/run.mjs", name], why: policy }, reason);
  }
}

function deferItem(builder, key, reason) {
  const spec = DEFERRED[key];
  builder.defer(key, { label: spec.label, kind: key === "release" ? "release" : "manual", command: spec.command, flag: spec.flag, why: spec.reason }, reason);
}

/** Reverse edges of a file graph: target -> [files that use it]. */
function reverse(edges) {
  const rev = new Map();
  for (const [from, targets] of Object.entries(edges)) {
    for (const to of targets) {
      if (!rev.has(to)) rev.set(to, new Set());
      rev.get(to).add(from);
    }
  }
  return rev;
}

/**
 * Files that use `start` directly or transitively (excluding `start`). A file is
 * also used through any folder that contains it (a test may name a folder).
 */
function consumersOf(start, rev) {
  const seen = new Set();
  const stack = [start];
  for (let dir = start.lastIndexOf("/"); dir > 0; dir = start.lastIndexOf("/", dir - 1)) stack.push(start.slice(0, dir));
  while (stack.length) {
    const file = stack.pop();
    for (const user of rev.get(file) ?? []) {
      if (!seen.has(user) && user !== start) {
        seen.add(user);
        stack.push(user);
      }
    }
  }
  return seen;
}

/** Import edges plus path references from scripts and tests (not Rust). */
function usageEdges(index) {
  const edges = {};
  for (const [file, targets] of Object.entries(index.imports)) edges[file] = [...targets];
  for (const [file, list] of Object.entries(index.refs)) {
    if (file.endsWith(".rs")) continue;
    for (const ref of list) if (ref.kind === "path") (edges[file] ??= []).push(ref.target);
  }
  return edges;
}

function crateOf(path, crates) {
  let best = null;
  for (const crate of crates) {
    if ((path === crate.dir || path.startsWith(`${crate.dir}/`)) && (!best || crate.dir.length > best.dir.length)) best = crate;
  }
  return best;
}

function dependentsOf(name, crates) {
  const users = new Map(crates.map((c) => [c.name, []]));
  for (const crate of crates) for (const dep of crate.deps) users.get(dep)?.push(crate.name);
  const seen = new Set([name]);
  const stack = [name];
  while (stack.length) for (const user of users.get(stack.pop()) ?? []) if (!seen.has(user)) seen.add(user), stack.push(user);
  return seen;
}

function fixtureNameOf(path) {
  const prefix = `${FIXTURE_DIR}/`;
  if (!path.startsWith(prefix)) return null;
  const name = path.slice(prefix.length).split("/")[0];
  return name || null;
}

/** The text-reference consumers of a path: [{file, compiled}]. */
function referencesTo(path, refs) {
  const fixture = fixtureNameOf(path);
  const users = [];
  for (const [file, list] of Object.entries(refs)) {
    const hits = list.filter((ref) =>
      ref.kind === "fixture" ? fixture !== null && ref.target === fixture : ref.target === path || path.startsWith(`${ref.target}/`),
    );
    if (hits.length) users.push({ file, compiled: hits.some((ref) => ref.compiled) });
  }
  return users;
}

const sortById = (a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0);

function finish(builder, { mode, base, changes, options, env, inputs }) {
  // `npm run check` runs every gate over the whole workspace: it subsumes them.
  if (builder.checks.has("check-all")) for (const id of Object.keys(GATES)) builder.checks.delete(id);
  const order = ["private-paths", "permissions", "typecheck", "frontend-tests", "frontend-build", "check-all", "rust-format", "rust-lint", "rust-tests", "bindings", "release-build", "release-smoke", "build-app"];
  const rank = (id) => {
    const i = order.indexOf(id);
    if (i >= 0) return i;
    if (id.startsWith("e2e:")) return order.length;
    if (id.startsWith("live:")) return order.length + 1;
    return order.length + 2;
  };
  const checks = [...builder.checks.values()]
    .map((check) => ({ ...check, reasons: summarize(check.reasons) }))
    .sort((a, b) => rank(a.id) - rank(b.id) || sortById(a, b));
  const deferred = [...builder.deferred.values()].map((item) => ({ ...item, reasons: summarize(item.reasons) })).sort(sortById);
  const notes = [...builder.notes].sort();
  if (env?.e2eExecutable && checks.some((c) => c.kind === "e2e")) {
    notes.push(`EXPOSITION_E2E_EXECUTABLE is set: scenarios run against ${env.e2eExecutable}, not the debug build made by build-app.`);
  }
  const plan = {
    schema: PLAN_SCHEMA,
    mode,
    base,
    inputs: inputs ?? null,
    changes,
    level: builder.level,
    escalations: builder.escalations,
    uncovered: builder.uncovered ?? [],
    checks,
    deferred,
    notes,
    options: { live: Boolean(options.live), release: Boolean(options.release) },
  };
  plan.hash = planHash(plan);
  return plan;
}

function summarize(reasons) {
  const list = [...reasons].sort();
  return list.length > MAX_REASONS ? [...list.slice(0, MAX_REASONS), `… and ${list.length - MAX_REASONS} more`] : list;
}

export function planHash(plan) {
  const essence = {
    schema: plan.schema,
    mode: plan.mode,
    base: plan.base ? { ref: plan.base.ref ?? null, mergeBase: plan.base.mergeBase ?? null } : null,
    inputs: plan.inputs, // content of the changed files: a later edit makes a preview stale
    changes: plan.changes,
    checks: plan.checks.map(({ id, command, after, env }) => ({ id, command, after, env: env ?? null })),
    deferred: plan.deferred.map(({ id }) => id),
    uncovered: plan.uncovered,
    notes: plan.notes, // includes the e2e executable when EXPOSITION_E2E_EXECUTABLE is set
  };
  return createHash("sha256").update(JSON.stringify(essence)).digest("hex").slice(0, 12);
}

function addE2e(builder, names, reason, scenarios) {
  for (const name of names) {
    if (scenarioPolicy(name)) {
      deferScenario(builder, name, reason);
      continue;
    }
    if (!scenarios.includes(name)) {
      builder.escalate("full", `the manifest names scenario "${name}", which does not exist (incomplete mapping)`);
      continue;
    }
    builder.add(
      `e2e:${name}`,
      { label: `e2e ${name}`, kind: "e2e", weight: "heavy", platform: "win32", command: ["node", "scripts/e2e/run.mjs", name], after: ["build-app"], resources: scenarioResources(name) },
      reason,
    );
  }
}

function addCheckAll(builder, reason) {
  builder.add("check-all", { label: "npm run check (all CI gates)", kind: "gates", weight: "heavy", command: ["npm", "run", "check"] }, reason);
}

function addFullChecks(builder, scenarios, options) {
  addCheckAll(builder, "full validation");
  addE2e(builder, scenarios.filter((s) => !scenarioPolicy(s)), "full validation", scenarios);
  for (const name of scenarios) if (scenarioPolicy(name)) deferScenario(builder, name, "full validation");
  deferItem(builder, "release", "full validation");
  addNativeBuild(builder);
  applyOptions(builder, options);
}

function addNativeBuild(builder) {
  if ([...builder.checks.keys()].some((id) => id.startsWith("e2e:") || id.startsWith("live:"))) {
    builder.add("build-app", { label: "debug app build", kind: "native-build", weight: "heavy", command: ["npm", "run", "build:app"] }, "end-to-end scenarios run against the built app");
  }
}

/** --live and --release turn deferred items into checks. */
function applyOptions(builder, options) {
  for (const item of [...builder.deferred.values()]) {
    if (options.live && item.kind === "live") {
      builder.deferred.delete(item.id);
      builder.add(item.id, { label: item.label, kind: "live", weight: "heavy", platform: "win32", command: item.command, after: ["build-app"] }, "--live (authorized)");
      for (const r of item.reasons) builder.checks.get(item.id).reasons.add(r);
    }
    if (options.release && item.kind === "release") {
      builder.deferred.delete(item.id);
      builder.add("release-build", { label: "release build", kind: "release", weight: "heavy", command: item.command }, "--release");
      // Never key-touching scenarios against a release build (it ignores the
      // development credential namespace).
      builder.add(
        "release-smoke",
        {
          label: "release smoke (smoke, ui-shell, pages-review)",
          kind: "release",
          weight: "heavy",
          platform: "win32",
          command: ["node", "scripts/e2e/run.mjs", "smoke", "ui-shell", "pages-review"],
          env: { EXPOSITION_E2E_EXECUTABLE: "target/release/exposition.exe" },
          after: ["release-build"],
        },
        "--release: the release executable must start and render",
      );
    }
  }
  addNativeBuild(builder);
}

/**
 * Builds the plan.
 * @param {object} input
 * @param {"affected"|"full"} input.mode
 * @param {{ok: boolean, reason?: string}} input.base resolved comparison base
 * @param {{status: string, path: string, from?: string}[]} input.changes
 * @param {object} input.index repository index (repo.mjs buildIndex)
 * @param {{live?: boolean, release?: boolean}} [input.options]
 * @param {string|null} [input.inputs] digest of the inputs (repo.mjs inputDigest)
 */
export function buildPlan({ mode, base, changes: unsorted, index, options = {}, env = {}, inputs = null }) {
  const changes = [...unsorted].sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : (a.from ?? "") < (b.from ?? "") ? -1 : 1));
  const builder = new PlanBuilder();
  const scenarios = index.scenarios;
  gate(builder, "private-paths", "baseline: every change (guards private files from entering the repository)");

  if (mode === "full") {
    builder.escalate("full", "full validation requested");
    addFullChecks(builder, scenarios, options);
    return finish(builder, { mode, base, changes, options, env, inputs });
  }
  if (!base.ok) {
    builder.escalate("full", base.reason);
    addFullChecks(builder, scenarios, options);
    return finish(builder, { mode, base, changes, options, env, inputs });
  }
  if (changes.length === 0) {
    builder.notes.add(`No changes between the merge base and the working tree: only the baseline runs.`);
    builder.level = "none";
    return finish(builder, { mode, base, changes, options, env, inputs });
  }

  const rev = reverse(usageEdges(index));
  const crates = index.crates;
  const present = new Set(index.files);
  const tsExports = new Set(index.tsExports ?? []);
  const tsRemoved = new Set(index.tsRemoved ?? []);
  const rustCrates = new Map(); // crate name -> Set(reason)
  const vitestFiles = new Map(); // test file -> Set(reason)
  const touched = new Set(); // paths, both sides of renames
  const covered = new Set(); // paths some automated check exercises
  for (const change of changes) {
    touched.add(change.path);
    if (change.from) touched.add(change.from);
    // A removed path counts as covered only through what still uses it (found
    // below) or its area's checks: some readers are invisible to the analysis.
  }
  const paths = [...touched].sort();

  const addRustCrate = (name, reason) => {
    if (!rustCrates.has(name)) rustCrates.set(name, new Set());
    rustCrates.get(name).add(reason);
  };
  const addRustConsumers = (name, reason, withDependents) => {
    for (const crate of withDependents ? dependentsOf(name, crates) : [name]) {
      addRustCrate(crate, crate === name ? reason : `${reason} (depends on ${name})`);
    }
  };

  // Crates that depend on a changed crate are used through their own areas'
  // scenarios, which the changed file's area does not list: the dependent
  // crate's Rust tests alone do not show that its features still work.
  const addDependentScenarios = (changed, why) => {
    for (const name of dependentsOf(changed, crates)) {
      if (name === changed) continue;
      const dependent = crates.find((c) => c.name === name);
      for (const area of AREAS) {
        if (!area.dependents?.length || !area.paths.includes(`${dependent.dir}/**`)) continue;
        addE2e(builder, area.dependents, `${name} depends on ${changed} (${why})`, scenarios);
      }
    }
  };

  // Area policy: levels, scenarios, extra checks, deferred items.
  const areaPaths = new Map();
  for (const path of paths) {
    const matched = areasFor(path);
    if (matched.length === 0) {
      builder.escalate("full", `${path} matches no area in scripts/impact/manifest.mjs (unknown impact)`);
      continue;
    }
    for (const area of matched) {
      if (!areaPaths.has(area.id)) areaPaths.set(area.id, { area, paths: [] });
      areaPaths.get(area.id).paths.push(path);
    }
  }
  for (const { area, paths: hits } of [...areaPaths.values()].sort((a, b) => (a.area.id < b.area.id ? -1 : 1))) {
    const reason = `${hits[0]}${hits.length > 1 ? ` (+${hits.length - 1})` : ""} → area ${area.id}`;
    // Tests and scenarios change no product behavior: they never escalate.
    const behavior = hits.filter((p) => !isVitestFile(p) && !isRustTestOnly(p) && !scenarioName(p));
    const stylesOnly = behavior.length > 0 && behavior.every((p) => p.endsWith(".css"));
    if (area.note) builder.notes.add(`${area.id}: ${area.note}`);
    if (behavior.length === 0) {
      builder.escalate("local", reason);
      continue;
    }
    if (area.id === "package-manifest") {
      const pkg = index.packageJson ?? { kind: "changed", detail: "package.json changed" };
      builder.notes.add(`package.json: ${pkg.detail}`);
      builder.escalate(pkg.kind === "scripts-added" || pkg.kind === "unchanged" ? "local" : "full", `package.json: ${pkg.detail}`);
    } else {
      builder.escalate(area.level, area.level === "local" ? reason : `${reason}: ${area.note ?? area.id}`);
    }
    const scenarioList = stylesOnly && area.visual ? area.visual : area.e2e;
    if (scenarioList.length || (area.checks ?? []).length) for (const p of behavior) covered.add(p);
    addE2e(builder, scenarioList, reason, scenarios);
    for (const name of area.live ?? []) deferScenario(builder, name, reason);
    for (const key of area.deferred ?? []) {
      if (key === "mathlib") for (const name of ["mathlib", "mathlib-setup"]) deferScenario(builder, name, reason);
      else deferItem(builder, key, reason);
    }
    if (stylesOnly) builder.notes.add("Styling change: compare the refreshed ui-reference screens with the previous ones (run e2e with EXPOSITION_DEV_HOME set).");
    for (const id of area.checks ?? []) {
      if (id === "check-all") addCheckAll(builder, reason);
      else if (id === "bindings-full") gate(builder, "bindings", reason);
      else gate(builder, id, reason);
    }
  }

  // Frontend and script consumers through imports and path references.
  for (const path of paths) {
    const direct = new Set([path]);
    const readers = referencesTo(path, index.refs);
    for (const { file, compiled } of readers) {
      if (!file.endsWith(".rs")) {
        direct.add(file);
        continue;
      }
      const crate = crates ? crateOf(file, crates) : null;
      if (crate) {
        addRustConsumers(crate.name, `${file} reads ${path}`, compiled && !isRustTestOnly(file));
        if (compiled && !isRustTestOnly(file)) addDependentScenarios(crate.name, `${file} embeds ${path}`);
        covered.add(path);
      } else builder.escalate("full", `${file} reads ${path} but its crate is unknown (crate graph unavailable)`);
    }
    const reached = new Set(direct);
    for (const file of direct) for (const user of consumersOf(file, rev)) reached.add(user);
    for (const file of [...reached].sort()) {
      if (isVitestFile(file) && present.has(file)) {
        if (!vitestFiles.has(file)) vitestFiles.set(file, new Set());
        vitestFiles.get(file).add(file === path ? "changed test" : `uses ${path}`);
        covered.add(path);
      }
      const scenario = scenarioName(file);
      if (scenario && present.has(file)) {
        addE2e(builder, [scenario], file === path ? "changed scenario" : `uses ${path}`, scenarios);
        covered.add(path);
      }
    }
    const fixture = fixtureNameOf(path);
    if (fixture && readers.length === 0) {
      builder.escalate("broad", `no test or scenario names fixture "${fixture}" (${path}); its consumers are unknown`);
    }
  }

  // Rust crates owning changed files, plus dependents for non-test changes.
  const rustTouched = paths.some((p) => p.endsWith(".rs"));
  for (const path of paths) {
    const inCrateDir = /^(crates\/[^/]+|src-tauri)\//.test(path);
    if (!inCrateDir) continue;
    // Merged into the configuration by `tauri build --config` only; no crate compiles it.
    if (path === "src-tauri/tauri.release.conf.json") continue;
    if (path.startsWith("src-tauri/permissions/") || path.startsWith("src-tauri/capabilities/") || path === "src-tauri/tauri.conf.json" || path.startsWith("src-tauri/icons/")) {
      // Compiled into the app crate by tauri-build / generate_context!.
      if (crates) addRustCrate("exposition", `${path} is compiled into the app crate`);
      continue;
    }
    if (!crates) {
      builder.escalate("full", `the Rust crate graph is unavailable (cargo metadata failed), so ${path} has unknown consumers`);
      continue;
    }
    const crate = crateOf(path, crates);
    if (!crate) {
      builder.escalate("full", `${path} is under a crate directory that cargo metadata does not list`);
      continue;
    }
    addRustConsumers(crate.name, `${path} changed`, !isRustTestOnly(path));
    covered.add(path);
    if (!isRustTestOnly(path)) addDependentScenarios(crate.name, `${path} changed`);
  }

  if (builder.level === "full") {
    addFullChecks(builder, scenarios, options);
    return finish(builder, { mode, base, changes, options, env, inputs });
  }
  // Every plan below full says which changed files no selected check exercises.
  builder.uncovered = paths.filter((p) => !covered.has(p));
  for (const p of builder.uncovered) builder.notes.add(`Uncovered: no automated check exercises ${p}; verify it by hand, or add a test or a mapping.`);

  // Broad: every frontend consumer and the cross-cutting invariant scenarios.
  if (builder.level === "broad") {
    gate(builder, "typecheck", "broad change: all frontend consumers");
    gate(builder, "frontend-tests", "broad change: all frontend consumers");
    gate(builder, "frontend-build", "broad change: all frontend consumers");
    addE2e(builder, INVARIANT_SCENARIOS, "broad change: cross-cutting invariants", scenarios);
  }

  const frontendTouched = paths.some((p) => /^src\//.test(p) || p === "index.html");
  if (frontendTouched) {
    gate(builder, "typecheck", "frontend source changed");
    gate(builder, "frontend-build", "frontend source changed");
  }
  if (!builder.checks.has("frontend-tests") && vitestFiles.size > 0) {
    const files = [...vitestFiles.keys()].sort();
    if (files.length > VITEST_FILE_LIMIT) {
      gate(builder, "frontend-tests", `${files.length} affected test files (over ${VITEST_FILE_LIMIT}: whole suite)`);
    } else {
      const check = builder.add("frontend-tests", { label: `frontend tests (${files.length} files)`, kind: "frontend", weight: "medium", command: [...VITEST_CLI, ...files] });
      for (const file of files) for (const r of vitestFiles.get(file)) check.reasons.add(`${file}: ${r}`);
    }
  }
  if (frontendTouched && vitestFiles.size === 0 && !builder.checks.has("frontend-tests")) {
    builder.notes.add("No frontend unit test imports the changed frontend files; typecheck, build, and end-to-end scenarios cover them.");
  }

  if (rustTouched) gate(builder, "rust-format", "Rust source changed");
  if (rustCrates.size > 0 && !builder.checks.has("check-all")) {
    const all = crates.map((c) => c.name).sort();
    const names = [...rustCrates.keys()].sort();
    const reasons = names.flatMap((n) => [...rustCrates.get(n)].map((r) => `${n}: ${r}`));
    if (names.length === all.length) {
      for (const id of ["rust-lint", "rust-tests"]) {
        const check = gate(builder, id);
        for (const r of reasons) check.reasons.add(r);
      }
    } else {
      // Always select the app crate (it depends on every workspace crate):
      // a `-p` subset without it resolves different dependency features and
      // rebuilds the third-party dependencies (measured: 141 crates, 18 min at
      // one build job), while with it the warm workspace build is reused.
      const anchor = crates.find((c) => c.dir === "src-tauri")?.name;
      if (anchor && !names.includes(anchor)) {
        names.push(anchor);
        names.sort();
        reasons.push(`${anchor}: selected so dependency features match the workspace build (avoids rebuilding third-party crates)`);
      }
      const packages = names.flatMap((n) => ["-p", n]);
      const lint = builder.add("rust-lint", {
        label: `rust lint (${names.length} crates)`,
        kind: "rust",
        weight: "heavy",
        command: ["cargo", "clippy", ...packages, "--all-targets", "--", "-D", "warnings"],
      });
      const tests = builder.add("rust-tests", {
        label: `rust tests (${names.length} crates)`,
        kind: "rust",
        weight: "heavy",
        command: ["cargo", "test", ...packages, "--no-fail-fast"],
      });
      for (const r of reasons) lint.reasons.add(r), tests.reasons.add(r);
    }
    const exporting = paths.filter((p) => tsExports.has(p));
    const removed = paths.filter((p) => tsRemoved.has(p));
    if (builder.checks.has("bindings")) {
      // Already the full gate (shared contracts changed).
    } else if (removed.length) {
      gate(builder, "bindings", `${removed[0]} removes or renames exported contract types (stale binding files need the full gate)`);
    } else if (exporting.length && names.length === all.length) {
      gate(builder, "bindings", `${exporting[0]} defines exported contract types`);
    } else if (exporting.length) {
      const anchor = crates.find((c) => c.dir === "src-tauri")?.name;
      const owners = [...new Set([...exporting.map((p) => crateOf(p, crates)?.name), anchor].filter(Boolean))].sort();
      builder.add(
        "bindings",
        {
          label: `bindings drift (${owners.join(", ")})`,
          kind: "rust",
          weight: "heavy",
          command: ["node", "scripts/impact/bindings-drift.mjs", ...owners.flatMap((n) => ["-p", n])],
        },
        `${exporting[0]} defines exported contract types`,
      );
      builder.notes.add("Bindings drift regenerates the changed crates' contract types; stale files of removed types are caught only by the full bindings gate.");
    }
  }

  applyOptions(builder, options);
  return finish(builder, { mode, base, changes, options, env, inputs });
}
