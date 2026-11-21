// Executes a plan from plan.mjs. Light checks run concurrently (bounded by
// `jobs`). Heavy checks never overlap a build: cargo and the app build run one
// at a time, then end-to-end scenarios run up to `e2eJobs` at once (each is an
// isolated app instance). Every check ends as passed, failed, skipped, or
// unavailable; the exit status says whether validation is complete.
import { spawn, spawnSync } from "node:child_process";

/**
 * Exit statuses: 0 every planned check passed and no required gate is pending;
 * 1 a check failed; 2 usage, invalid base, or stale plan (including a working
 * tree that changed while the checks ran); 3 planned checks were
 * skipped or unavailable; 4 pending: planned checks passed but required gates
 * were deferred (live, release, CI, manual) or the plan escalated to full
 * validation without --allow-full. Only 0 is a passing result.
 */
export const EXIT = { ok: 0, failed: 1, usage: 2, incomplete: 3, pending: 4 };

/** A scenario that skipped itself as a whole ("SKIPPED: …" / "skip <name> …"). */
const WHOLE_SKIP = /^\s*(SKIPPED:|skip\s)(.*)$/;
/** A step a scenario skipped while the rest ran. */
const PARTIAL_SKIP = /\b(SKIPPED|NOT VERIFIED)\b/;
const TAIL_LINES = 40;

function commandFor(argv) {
  const [cmd, ...args] = argv;
  if (process.platform === "win32" && (cmd === "npm" || cmd === "npx")) {
    // npm is a batch file on Windows; Node needs a shell to run it. The words
    // come from the plan (fixed script names), never from user input.
    if (args.some((a) => !/^[\w:.\-/=]+$/.test(a))) throw new Error(`refusing to pass ${JSON.stringify(args)} through a shell`);
    return { cmd: [`${cmd}.cmd`, ...args].join(" "), args: [], shell: true };
  }
  return { cmd, args, shell: false };
}

function killTree(child) {
  if (!child.pid) return;
  if (process.platform === "win32") spawnSync("taskkill", ["/pid", String(child.pid), "/T", "/F"], { stdio: "ignore" });
  else child.kill("SIGTERM");
}

/**
 * Runs one check. Output streams through (to `out`) unless `buffer` is set, in
 * which case it is returned and printed by the caller when the check ends.
 */
function runCommand(check, { cwd, env, out, buffer, running }) {
  return new Promise((resolve) => {
    let child;
    try {
      const { cmd, args, shell } = commandFor(check.command);
      child = spawn(cmd, args, { cwd, env: check.env ? { ...env, ...check.env } : env, shell, stdio: ["ignore", "pipe", "pipe"], windowsHide: true });
    } catch (error) {
      resolve({ code: null, error, text: "", tail: [], skips: [] });
      return;
    }
    running.add(child);
    const tail = [];
    const skips = [];
    let text = "";
    const partial = new Map();
    const onData = (stream) => (chunk) => {
      if (buffer) text += chunk;
      else out.write(chunk);
      const lines = ((partial.get(stream) ?? "") + chunk).split(/\r?\n/);
      partial.set(stream, lines.pop());
      for (const line of lines) {
        tail.push(line);
        if (tail.length > TAIL_LINES) tail.shift();
        if (WHOLE_SKIP.test(line) || PARTIAL_SKIP.test(line)) skips.push(line.trim());
      }
    };
    child.stdout.setEncoding("utf8").on("data", onData("out"));
    child.stderr.setEncoding("utf8").on("data", onData("err"));
    child.on("error", (error) => {
      running.delete(child);
      resolve({ code: null, error, text, tail, skips });
    });
    child.on("close", (code) => {
      running.delete(child);
      for (const [, rest] of partial) if (rest && (WHOLE_SKIP.test(rest) || PARTIAL_SKIP.test(rest))) skips.push(rest.trim());
      resolve({ code, error: null, text, tail, skips });
    });
  });
}

/** Maps a finished command to a result status. */
export function classify(check, { code, error, skips }) {
  if (error) {
    return error.code === "ENOENT"
      ? { status: "unavailable", reason: `command not found: ${check.command[0]}` }
      : { status: "failed", reason: String(error.message ?? error) };
  }
  if (code !== 0) return { status: "failed", reason: `exit status ${code}` };
  if (check.kind === "e2e" || check.kind === "live") {
    const whole = skips.find((line) => WHOLE_SKIP.test(line));
    if (whole) return { status: "unavailable", reason: `scenario skipped itself: ${whole}` };
    const steps = skips.filter((line) => PARTIAL_SKIP.test(line));
    if (steps.length) return { status: "passed", reason: `passed with skipped steps: ${steps.slice(0, 3).join(" | ")}` };
  }
  return { status: "passed", reason: "" };
}

/**
 * Executes `plan.checks` in order.
 * @param {object} plan from buildPlan
 * @param {object} opts
 * @param {number} [opts.jobs] concurrent light checks (default 2)
 * @param {number} [opts.e2eJobs] end-to-end scenarios run at once (default 1);
 *   scenarios holding a machine-wide resource are still kept apart
 * @param {() => string} [opts.currentPlanHash] resolves the plan hash of the
 *   tree as it is now; when it differs from the plan after the run, the result
 *   is marked stale (exit status 2)
 * @param {string[]} [opts.skip] check kinds or ids not to run (reported skipped)
 * @param {boolean} [opts.failFast] skip the remaining checks after a failure
 * @param {NodeJS.WritableStream} [opts.out] where check output and progress go
 */
export async function executePlan(
  plan,
  { cwd = process.cwd(), env = process.env, jobs = 2, e2eJobs = 1, skip = [], failFast = false, out = process.stdout, platform = process.platform, currentPlanHash = null } = {},
) {
  const results = new Map();
  const running = new Set();
  const skipSet = new Set(skip);
  let failed = false;
  const onSignal = () => {
    for (const child of running) killTree(child);
    process.exit(130);
  };
  process.once("SIGINT", onSignal);
  process.once("SIGTERM", onSignal);

  const excluded = (check) => skipSet.has(check.kind) || skipSet.has(check.id);
  // A check that exists only for checks the user excluded (the app build for
  // skipped scenarios) is not run either.
  const dependents = (check) => plan.checks.filter((c) => (c.after ?? []).includes(check.id));
  const precheck = (check) => {
    if (excluded(check)) return { status: "skipped", reason: "excluded by --skip" };
    const users = dependents(check);
    if (check.kind === "native-build" && users.length && users.every(excluded)) return { status: "skipped", reason: "only needed by checks excluded by --skip" };
    if (failFast && failed) return { status: "skipped", reason: "--fail-fast after an earlier failure" };
    if (check.platform && check.platform !== platform) return { status: "unavailable", reason: `needs ${check.platform} (this is ${platform})` };
    for (const dep of check.after ?? []) {
      const r = results.get(dep);
      if (r && r.status !== "passed") return { status: "skipped", reason: `needs ${dep}, which ${r.status === "failed" ? "failed" : `was ${r.status}`}` };
    }
    return null;
  };

  const run = async (check, buffer) => {
    const pre = precheck(check);
    if (pre) {
      results.set(check.id, { id: check.id, label: check.label, kind: check.kind, ...pre, seconds: 0 });
      out.write(`\n=== ${check.id}: ${pre.status.toUpperCase()} (${pre.reason})\n`);
      return;
    }
    if (!buffer) out.write(`\n=== ${check.id}: ${check.command.join(" ")}\n`);
    const started = Date.now();
    const outcome = await runCommand(check, { cwd, env, out, buffer, running });
    const seconds = (Date.now() - started) / 1000;
    const verdict = classify(check, outcome);
    if (buffer) out.write(`\n=== ${check.id}: ${check.command.join(" ")}\n${outcome.text}`);
    if (verdict.status === "failed") failed = true;
    results.set(check.id, { id: check.id, label: check.label, kind: check.kind, ...verdict, seconds, tail: verdict.status === "failed" ? outcome.tail : undefined });
    out.write(`=== ${check.id}: ${verdict.status.toUpperCase()} in ${seconds.toFixed(1)}s${verdict.reason ? ` (${verdict.reason})` : ""}\n`);
  };

  // End-to-end scenarios are separate app instances with their own data
  // directory, WebView2 profile, and debugging port, so up to `e2eJobs` of them
  // may run at once. A scenario that holds a machine-wide resource is kept
  // apart: "desktop" (the real pointer, the foreground window, a native
  // dialog) runs with nothing else; any other resource (the OS clipboard)
  // never overlaps another holder of the same resource.
  const e2eWidth = Math.max(1, Math.floor(e2eJobs) || 1);
  const runScenarios = async (batch) => {
    const alone = (c) => (c.resources ?? []).includes("desktop");
    const pending = batch.filter((c) => !alone(c));
    const active = new Map();
    const conflicts = (c) => [...active.keys()].some((o) => (o.resources ?? []).some((r) => (c.resources ?? []).includes(r)));
    while (pending.length || active.size) {
      let started = false;
      while (active.size < e2eWidth) {
        const at = pending.findIndex((c) => !conflicts(c));
        if (at < 0) break;
        const [check] = pending.splice(at, 1);
        const done = run(check, e2eWidth > 1).finally(() => active.delete(check));
        active.set(check, done);
        started = true;
      }
      if (active.size) await Promise.race(active.values());
      else if (!started) break;
    }
    for (const check of batch.filter(alone)) await run(check, false);
  };

  const startedAt = Date.now();
  try {
    const light = plan.checks.filter((c) => c.weight !== "heavy");
    const heavy = plan.checks.filter((c) => c.weight === "heavy");
    const width = Math.max(1, Math.floor(jobs) || 1);
    let next = 0;
    const worker = async () => {
      while (next < light.length) await run(light[next++], width > 1);
    };
    await Promise.all(Array.from({ length: Math.min(width, light.length) }, worker));
    // Builds and cargo never overlap anything. Consecutive scenarios form a batch.
    for (let i = 0; i < heavy.length; ) {
      if (heavy[i].kind !== "e2e") {
        await run(heavy[i++], false);
        continue;
      }
      let end = i;
      while (end < heavy.length && heavy[end].kind === "e2e") end++;
      await runScenarios(heavy.slice(i, end));
      i = end;
    }
  } finally {
    process.removeListener("SIGINT", onSignal);
    process.removeListener("SIGTERM", onSignal);
  }

  const ordered = plan.checks.map((c) => results.get(c.id));
  const count = (s) => ordered.filter((r) => r.status === s).length;
  // The checks read the working tree as they go, not a snapshot. If the tree no
  // longer resolves to the plan that was started, the results describe a mix of
  // states and are not evidence for the current one.
  let stale = null;
  if (currentPlanHash) {
    try {
      const now = currentPlanHash();
      if (now !== plan.hash) stale = { planned: plan.hash, now };
    } catch (error) {
      stale = { planned: plan.hash, now: null, reason: String(error?.message ?? error) };
    }
  }
  const exitCode = count("failed")
    ? EXIT.failed
    : stale
      ? EXIT.usage
      : count("skipped") || count("unavailable")
        ? EXIT.incomplete
        : plan.deferred.length
          ? EXIT.pending
          : EXIT.ok;
  return {
    schema: "exposition-impact-run/1",
    plan: plan.hash,
    // What the results are evidence for, and how they were produced.
    snapshot: { head: plan.base?.head ?? null, base: plan.base?.mergeBase ?? null, inputs: plan.inputs ?? null },
    stale,
    startedAt: new Date(startedAt).toISOString(),
    wallSeconds: Number(((Date.now() - startedAt) / 1000).toFixed(1)),
    settings: { jobs: Math.max(1, Math.floor(jobs) || 1), e2eJobs: e2eWidth, cargoBuildJobs: env.CARGO_BUILD_JOBS ?? null, rustTestThreads: env.RUST_TEST_THREADS ?? null },
    results: ordered,
    totals: { passed: count("passed"), failed: count("failed"), skipped: count("skipped"), unavailable: count("unavailable") },
    deferred: plan.deferred.map(({ id, label, why }) => ({ id, label, why })),
    uncovered: plan.uncovered ?? [],
    // Complete: every planned check passed on an unchanged tree, nothing is
    // deferred, and every changed file is exercised by some check.
    complete: exitCode === EXIT.ok && (plan.uncovered ?? []).length === 0,
    exitCode,
  };
}
