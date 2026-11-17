#!/usr/bin/env node
// Change-impact validation: chooses the checks a change requires and runs them.
//
//   npm run check:plan      [-- options]   preview the plan (runs nothing)
//   npm run check:affected  [-- options]   run the plan for the current changes
//   npm run check:full      [-- options]   run full validation
//
// Options:
//   --base <ref>         compare against the merge base of HEAD and <ref>
//                        (default: origin/main, else main; env EXPOSITION_IMPACT_BASE)
//   --full               plan full validation regardless of the changes
//   --json               plan: print the plan as JSON; run: print the result as
//                        JSON on stdout (progress and check output go to stderr)
//   --json-out <file>    run: also write the result JSON to <file>
//   --expect-plan <hash> run: refuse unless the resolved plan has this hash
//                        (the hash a preview printed)
//   --jobs <n>           concurrent light checks (default 2; heavy checks never overlap)
//   --skip <kinds|ids>   comma list of check kinds (static, frontend, rust, gates,
//                        native-build, e2e, live, release) or ids not to run;
//                        they are reported as skipped and the run is incomplete
//   --fail-fast          stop starting checks after the first failure
//   --live               run deferred live-provider scenarios (spends API credit:
//                        only with the user's authorization)
//   --release            run the deferred release build and release smoke
//   --allow-full         run: execute the plan even when the change escalated to
//                        full validation (otherwise the run stops as pending)
//
// Exit status: 0 every planned check passed and nothing required is pending;
// 1 a check failed; 2 usage error, invalid comparison base, or a plan that no
// longer matches --expect-plan; 3 nothing failed but planned checks were
// skipped or unavailable; 4 pending: required gates were deferred (live,
// release, CI, manual) or full validation was needed and not allowed. Deferred
// items never run implicitly. Uncovered files keep `complete` false.
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { buildPlan } from "./impact/plan.mjs";
import { buildIndex, collectChanges, ImpactError, inputDigest, resolveBase } from "./impact/repo.mjs";
import { EXIT, executePlan } from "./impact/runner.mjs";

const VALUE_FLAGS = new Set(["--base", "--json-out", "--expect-plan", "--jobs", "--skip"]);
const BOOLEAN_FLAGS = new Set(["--full", "--json", "--fail-fast", "--live", "--release", "--allow-full"]);

export function parseArgs(argv) {
  const [command, ...rest] = argv;
  if (command !== "plan" && command !== "run") throw new ImpactError(`expected "plan" or "run", got ${JSON.stringify(command ?? "")}`);
  const opts = { command };
  for (let i = 0; i < rest.length; i++) {
    const arg = rest[i];
    const [flag, inline] = arg.includes("=") ? [arg.slice(0, arg.indexOf("=")), arg.slice(arg.indexOf("=") + 1)] : [arg, undefined];
    if (BOOLEAN_FLAGS.has(flag) && inline === undefined) opts[flag.slice(2)] = true;
    else if (VALUE_FLAGS.has(flag)) {
      const value = inline ?? rest[++i];
      if (value === undefined || value === "") throw new ImpactError(`${flag} needs a value`);
      opts[flag.slice(2)] = value;
    } else throw new ImpactError(`unknown option ${arg}`);
  }
  if (opts.jobs !== undefined && !/^[1-9]\d*$/.test(opts.jobs)) throw new ImpactError("--jobs takes a positive integer");
  opts.skip = opts.skip ? opts.skip.split(",").map((s) => s.trim()).filter(Boolean) : [];
  return opts;
}

/** Resolves the plan exactly as both `plan` and `run` do. */
export function resolvePlan({ cwd, full = false, base, live = false, release = false, env = process.env }) {
  const resolved = resolveBase({ cwd, base, env });
  const changes = resolved.ok ? collectChanges({ cwd, mergeBase: resolved.mergeBase }) : [];
  const index = buildIndex({ cwd, changes, mergeBase: resolved.ok ? resolved.mergeBase : "HEAD" });
  return buildPlan({
    mode: full ? "full" : "affected",
    base: resolved,
    changes,
    index,
    options: { live, release },
    env: { e2eExecutable: env.EXPOSITION_E2E_EXECUTABLE || null },
    inputs: inputDigest({ cwd, base: resolved, changes }),
  });
}

const short = (sha) => (sha ? sha.slice(0, 10) : "none");

export function formatPlan(plan) {
  const lines = [];
  lines.push(`Impact plan ${plan.hash} (${plan.mode}; level ${plan.level})`);
  const b = plan.base;
  if (b?.ok) lines.push(`Base: ${b.ref} (${b.source}) merge base ${short(b.mergeBase)}; HEAD ${short(b.head)}; includes committed, staged, unstaged, and untracked changes`);
  else if (b) lines.push(`Base: unavailable — ${b.reason}`);
  if (plan.mode === "affected") {
    lines.push(`Changes (${plan.changes.length}):`);
    for (const c of plan.changes.slice(0, 40)) lines.push(`  ${c.status} ${c.from ? `${c.from} -> ` : ""}${c.path}`);
    if (plan.changes.length > 40) lines.push(`  … and ${plan.changes.length - 40} more (use --json for all)`);
  }
  if (plan.escalations.length) {
    lines.push("Escalations:");
    for (const e of plan.escalations) lines.push(`  [${e.level}] ${e.reason}`);
  }
  lines.push(`Checks (${plan.checks.length}):`);
  for (const c of plan.checks) {
    lines.push(`  ${c.id}  —  ${c.command.join(" ")}`);
    for (const r of c.reasons.slice(0, 3)) lines.push(`      because ${r}`);
    if (c.reasons.length > 3) lines.push(`      … ${c.reasons.length - 3} more reasons (--json)`);
  }
  if (plan.deferred.length) {
    lines.push(`Deferred — required for qualification, not run by this command (${plan.deferred.length}):`);
    for (const d of plan.deferred) lines.push(`  ${d.id}  —  ${d.why}${d.flag ? ` [${d.flag}]` : ""}`);
  }
  if (plan.notes.length) {
    lines.push("Notes:");
    for (const n of plan.notes) lines.push(`  - ${n}`);
  }
  return lines.join("\n");
}

export function formatResult(result) {
  const lines = ["", `=== summary (plan ${result.plan})`];
  for (const r of result.results) {
    lines.push(`${r.status.toUpperCase().padEnd(11)} ${r.id}${r.seconds ? ` (${r.seconds.toFixed(1)}s)` : ""}${r.reason ? ` — ${r.reason}` : ""}`);
  }
  for (const d of result.deferred) lines.push(`DEFERRED    ${d.id} — ${d.why}`);
  for (const p of result.uncovered) lines.push(`UNCOVERED   ${p} — no automated check exercises it`);
  const t = result.totals;
  const verdict =
    result.exitCode === EXIT.failed
      ? "FAILED"
      : result.exitCode === EXIT.incomplete
        ? "INCOMPLETE: planned checks were skipped or unavailable"
        : result.exitCode === EXIT.pending
          ? "PENDING: planned checks passed; the deferred gates above are still required"
          : result.uncovered.length
            ? "PASSED (planned checks); the uncovered files above have no automated check"
            : "PASSED: every required check ran and passed";
  lines.push(`${t.passed} passed, ${t.failed} failed, ${t.skipped} skipped, ${t.unavailable} unavailable, ${result.deferred.length} deferred — ${verdict}`);
  return lines.join("\n");
}

async function main() {
  let opts;
  try {
    opts = parseArgs(process.argv.slice(2));
  } catch (error) {
    console.error(`impact: ${error.message}\nusage: node scripts/impact.mjs <plan|run> [--full] [--base <ref>] [--json] [--jobs <n>] [--skip <kinds>] [--live] [--release] [--allow-full] [--expect-plan <hash>] [--json-out <file>] [--fail-fast]`);
    return EXIT.usage;
  }
  const started = performance.now();
  let plan;
  try {
    plan = resolvePlan({ cwd: process.cwd(), full: opts.full, base: opts.base, live: opts.live, release: opts.release });
  } catch (error) {
    if (error instanceof ImpactError) {
      console.error(`impact: ${error.message}`);
      return EXIT.usage;
    }
    throw error;
  }
  const planSeconds = (performance.now() - started) / 1000;

  if (opts.command === "plan") {
    if (opts.json) process.stdout.write(`${JSON.stringify(plan, null, 2)}\n`);
    else console.log(`${formatPlan(plan)}\n(planned in ${planSeconds.toFixed(2)}s; run it with: npm run check:${plan.mode === "full" ? "full" : "affected"})`);
    return EXIT.ok;
  }

  const log = opts.json ? process.stderr : process.stdout;
  log.write(`${formatPlan(plan)}\n(planned in ${planSeconds.toFixed(2)}s)\n`);
  if (opts["expect-plan"] && opts["expect-plan"] !== plan.hash) {
    console.error(`impact: the resolved plan is ${plan.hash}, not the expected ${opts["expect-plan"]}; the changes, their content, or the options differ from the preview. Preview again.`);
    return EXIT.usage;
  }
  if (plan.mode === "affected" && plan.level === "full" && !opts["allow-full"]) {
    // Escalation is never silent: running full validation is an explicit choice.
    log.write(
      `\n=== PENDING: this change escalated to full validation (see Escalations above). Nothing was run.\n` +
        `Run it with: npm run check:affected -- --allow-full   (or npm run check:full)\n`,
    );
    return EXIT.pending;
  }
  const result = await executePlan(plan, { jobs: opts.jobs ? Number(opts.jobs) : 2, skip: opts.skip, failFast: opts["fail-fast"], out: log });
  log.write(`${formatResult(result)}\n`);
  if (opts["json-out"]) writeFileSync(opts["json-out"], `${JSON.stringify(result, null, 2)}\n`);
  if (opts.json) process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
  return result.exitCode;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  process.exitCode = await main();
}
