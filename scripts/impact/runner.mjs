// Executes a plan from plan.mjs. Light checks run concurrently (bounded by
// `jobs`); heavy checks (cargo, app build, e2e) run one at a time so builds and
// app instances never overlap. Every check ends as passed, failed, skipped, or
// unavailable; the exit status says whether validation is complete.
import { spawn, spawnSync } from "node:child_process";

/** Exit statuses of `impact run`. */
export const EXIT = { ok: 0, failed: 1, usage: 2, incomplete: 3 };

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
 * @param {string[]} [opts.skip] check kinds or ids not to run (reported skipped)
 * @param {boolean} [opts.failFast] skip the remaining checks after a failure
 * @param {NodeJS.WritableStream} [opts.out] where check output and progress go
 */
export async function executePlan(plan, { cwd = process.cwd(), env = process.env, jobs = 2, skip = [], failFast = false, out = process.stdout, platform = process.platform } = {}) {
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

  try {
    const light = plan.checks.filter((c) => c.weight !== "heavy");
    const heavy = plan.checks.filter((c) => c.weight === "heavy");
    const width = Math.max(1, Math.floor(jobs) || 1);
    let next = 0;
    const worker = async () => {
      while (next < light.length) await run(light[next++], width > 1);
    };
    await Promise.all(Array.from({ length: Math.min(width, light.length) }, worker));
    for (const check of heavy) await run(check, false);
  } finally {
    process.removeListener("SIGINT", onSignal);
    process.removeListener("SIGTERM", onSignal);
  }

  const ordered = plan.checks.map((c) => results.get(c.id));
  const count = (s) => ordered.filter((r) => r.status === s).length;
  const exitCode = count("failed") ? EXIT.failed : count("skipped") || count("unavailable") ? EXIT.incomplete : EXIT.ok;
  return {
    schema: "exposition-impact-run/1",
    plan: plan.hash,
    results: ordered,
    totals: { passed: count("passed"), failed: count("failed"), skipped: count("skipped"), unavailable: count("unavailable") },
    deferred: plan.deferred.map(({ id, label, why }) => ({ id, label, why })),
    uncovered: plan.uncovered ?? [],
    // Complete: every planned check passed, nothing is deferred, and every
    // changed file is exercised by some check.
    complete: exitCode === EXIT.ok && plan.deferred.length === 0 && (plan.uncovered ?? []).length === 0,
    exitCode,
  };
}
