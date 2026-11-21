#!/usr/bin/env node
// Runs application measurements against the built desktop app, using the
// end-to-end harness (isolated data directory and WebView2 profile per launch).
// Each module in ./app exports `default async function (ctx)` with
//   ctx = { launchApp, fixture(name), workDir, scale, record(measurement), metrics(app), sleep(ms) }
// and calls `ctx.record({ bench, scale, samples_ms, counters })` per measurement.
//
//   node scripts/perf/app-bench.mjs <name>... [--out <file.jsonl>] [--label <text>]
//
// The executable is the debug build (`npm run build:app`) unless
// EXPOSITION_E2E_EXECUTABLE names another one (a release build for numbers
// that represent the shipped app). Scale comes from EXPOSITION_PERF_SCALE
// (small | large | stress; default small). These runs are not part of
// `npm run check` or the impact plans: they are run on purpose, alone, on an
// otherwise idle machine.
import { execFileSync } from "node:child_process";
import { appendFileSync, mkdirSync, mkdtempSync, readdirSync } from "node:fs";
import os from "node:os";
import { dirname, join } from "node:path";
import { pathToFileURL } from "node:url";

import { defaultExecutable, launchApp, REPO_ROOT } from "../e2e/app.mjs";

const argv = process.argv.slice(2);
const opts = { names: [], out: null, label: null };
for (let i = 0; i < argv.length; i++) {
  if (argv[i] === "--out" || argv[i] === "--label") opts[argv[i].slice(2)] = argv[++i];
  else if (argv[i].startsWith("--")) {
    console.error(`app-bench: unknown option ${argv[i]}`);
    process.exit(2);
  } else opts.names.push(argv[i]);
}
const dir = join(import.meta.dirname, "app");
const available = readdirSync(dir).filter((f) => f.endsWith(".mjs")).map((f) => f.replace(/\.mjs$/, ""));
const unknown = opts.names.filter((n) => !available.includes(n));
if (opts.names.length === 0 || unknown.length) {
  console.error(`usage: node scripts/perf/app-bench.mjs <name>... [--out <file.jsonl>] [--label <text>]\navailable: ${available.join(", ")}${unknown.length ? `\nunknown: ${unknown.join(", ")}` : ""}`);
  process.exit(2);
}

const git = (args) => {
  try {
    return execFileSync("git", args, { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] }).trim();
  } catch {
    return null;
  }
};
const executable = defaultExecutable();
const conditions = {
  revision: git(["rev-parse", "--short=10", "HEAD"]),
  dirty: (git(["status", "--porcelain"]) ?? "") !== "",
  executable: executable.replace(REPO_ROOT, "").replaceAll("\\", "/"),
  profile: /[\\/]release[\\/]/.test(executable) ? "release" : "dev",
  scale: process.env.EXPOSITION_PERF_SCALE ?? "small",
  cpu: os.cpus()[0]?.model.trim(),
  ramMiB: Math.round(os.totalmem() / 2 ** 20),
  os: `${os.type()} ${os.release()}`,
  label: opts.label,
};

/** Working sets (MiB) of the app process and of the WebView2 processes under it. */
function processMemory(pid) {
  if (process.platform !== "win32") return null;
  try {
    const script = `$all = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId, Name, WorkingSetSize, PrivatePageCount; $ids = @(${pid}); $grew = $true; while ($grew) { $grew = $false; foreach ($p in $all) { if (($ids -contains $p.ParentProcessId) -and -not ($ids -contains $p.ProcessId)) { $ids += $p.ProcessId; $grew = $true } } }; $all | Where-Object { $ids -contains $_.ProcessId } | ForEach-Object { "$($_.Name)|$($_.WorkingSetSize)|$($_.PrivatePageCount)" }`;
    const out = execFileSync("powershell", ["-NoProfile", "-NonInteractive", "-Command", script], { encoding: "utf8", windowsHide: true });
    let app = 0;
    let webview = 0;
    let privateBytes = 0;
    let count = 0;
    for (const line of out.split(/\r?\n/)) {
      const [name, ws, priv] = line.split("|");
      if (!ws) continue;
      count += 1;
      privateBytes += Number(priv) || 0;
      if (/webview/i.test(name)) webview += Number(ws) || 0;
      else app += Number(ws) || 0;
    }
    const mib = (n) => Math.round(n / 2 ** 20);
    return { appWorkingSetMiB: mib(app), webviewWorkingSetMiB: mib(webview), privateMiB: mib(privateBytes), processes: count };
  } catch {
    return null;
  }
}

/** Page and process metrics of a launched app: JS heap, DOM nodes, listeners, process memory. */
async function metrics(app, { collect = true } = {}) {
  const session = await app.page.context().newCDPSession(app.page);
  try {
    if (collect) await session.send("HeapProfiler.collectGarbage").catch(() => {});
    await session.send("Performance.enable");
    const { metrics: list } = await session.send("Performance.getMetrics");
    const pick = (name) => list.find((m) => m.name === name)?.value ?? null;
    return {
      jsHeapUsedMiB: Number(((pick("JSHeapUsedSize") ?? 0) / 2 ** 20).toFixed(1)),
      domNodes: pick("Nodes"),
      jsEventListeners: pick("JSEventListeners"),
      documents: pick("Documents"),
      layoutCount: pick("LayoutCount"),
      ...(processMemory(app.child.pid) ?? {}),
    };
  } finally {
    await session.detach().catch(() => {});
  }
}

let failed = false;
if (opts.out) mkdirSync(dirname(opts.out), { recursive: true });
for (const name of opts.names) {
  const module = await import(pathToFileURL(join(dir, `${name}.mjs`)).href);
  const ctx = {
    launchApp,
    scale: conditions.scale,
    workDir: mkdtempSync(join(os.tmpdir(), `exposition-perf-${name}-`)),
    fixture: (fixtureName) => join(REPO_ROOT, "tests", "fixtures", fixtureName),
    metrics,
    sleep: (ms) => new Promise((r) => setTimeout(r, ms)),
    record(measurement) {
      const s = Array.isArray(measurement.samples_ms) && measurement.samples_ms.length ? [...measurement.samples_ms].sort((a, b) => a - b) : null;
      const summary = s ? { n: s.length, min: s[0], median: s[Math.floor(s.length / 2)], max: s.at(-1) } : null;
      const full = { scenario: name, at: new Date().toISOString(), ...measurement, summary, conditions };
      if (opts.out) appendFileSync(opts.out, `${JSON.stringify(full)}\n`);
      const time = summary ? ` n=${summary.n} min ${summary.min.toFixed(1)} median ${summary.median.toFixed(1)} max ${summary.max.toFixed(1)} ms` : "";
      console.log(`${measurement.bench}${measurement.scale ? ` ${JSON.stringify(measurement.scale)}` : ""}:${time}${measurement.counters ? ` ${JSON.stringify(measurement.counters)}` : ""}`);
    },
  };
  try {
    await module.default(ctx);
  } catch (error) {
    failed = true;
    console.error(`app-bench: ${name} failed: ${error?.stack ?? error}`);
  }
}
console.error(`app-bench: ${conditions.profile} executable, scale ${conditions.scale}, revision ${conditions.revision}${conditions.dirty ? " dirty" : ""}`);
process.exit(failed ? 1 : 0);
