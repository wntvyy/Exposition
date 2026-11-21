#!/usr/bin/env node
// Times a command over repeated runs and records the conditions it ran under,
// so a build/test measurement can be repeated and compared like with like.
//
//   node scripts/perf/measure.mjs --label <name> [--runs <n>] [--out <file.jsonl>]
//        [--note <text>] [--before <shell command>] -- <command> [args...]
//
// One JSON line per run is appended to --out (default: printed only):
//   label, run, command, seconds, exit, memory (see below), and the conditions:
//   git revision and dirty state, CPU, RAM, OS, Node, CARGO_BUILD_JOBS,
//   RUST_TEST_THREADS, and the free memory when the run started.
// --before runs before each run and is not timed (e.g. touching a file to
// measure an incremental rebuild).
//
// Memory: every 2 s the working sets of build and app processes are summed by
// process name (cargo, rustc, link, node, the app, WebView2, ...). The baseline
// sum before the command starts is subtracted. It is a sampled estimate of the
// extra resident memory, not an exact per-tree peak, and it includes unrelated
// processes with those names that start during the run. Windows only; on other
// platforms only the lowest free memory is recorded.
import { execFileSync, spawn, spawnSync } from "node:child_process";
import { appendFileSync, mkdirSync } from "node:fs";
import os from "node:os";
import { dirname } from "node:path";

const WATCHED = new Set(
  [
    "cargo", "rustc", "link", "rust-lld", "lld-link", "clippy-driver", "rustfmt", "rustdoc", "node", "esbuild", "tsc", "tsgo",
    "exposition", "msedgewebview2", "lean", "lake", "leanc", "python", "python3", "Rscript", "julia", "typst", "cargo-tauri", "tauri",
  ].map((n) => n.toLowerCase()),
);

function parseArgs(argv) {
  const split = argv.indexOf("--");
  if (split < 0 || split === argv.length - 1) throw new Error("usage: measure.mjs --label <name> [--runs n] [--out file] [--note text] [--before cmd] -- <command...>");
  const opts = { runs: 1, command: argv.slice(split + 1) };
  const flags = argv.slice(0, split);
  for (let i = 0; i < flags.length; i += 2) {
    const key = flags[i].replace(/^--/, "");
    if (!["label", "runs", "out", "note", "before"].includes(key) || flags[i + 1] === undefined) throw new Error(`unknown or incomplete option ${flags[i]}`);
    opts[key] = flags[i + 1];
  }
  opts.runs = Number(opts.runs);
  if (!opts.label || !Number.isInteger(opts.runs) || opts.runs < 1) throw new Error("--label and a positive --runs are required");
  return opts;
}

function git(args) {
  try {
    return execFileSync("git", args, { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] }).trim();
  } catch {
    return null;
  }
}

/** Summed working set (MiB) and count of the watched process names. */
function sampleWatched() {
  if (process.platform !== "win32") return null;
  const r = spawnSync("tasklist", ["/fo", "csv", "/nh"], { encoding: "utf8", windowsHide: true });
  if (r.status !== 0) return null;
  let kib = 0;
  let count = 0;
  for (const line of r.stdout.split(/\r?\n/)) {
    const cells = line.split('","');
    if (cells.length < 5) continue;
    const name = cells[0].replace(/^"/, "").replace(/\.exe$/i, "").toLowerCase();
    if (!WATCHED.has(name)) continue;
    kib += Number(cells[4].replace(/[^\d]/g, "")) || 0;
    count += 1;
  }
  return { mib: Math.round(kib / 1024), count };
}

function runOnce(command) {
  return new Promise((resolve) => {
    const baseline = sampleWatched();
    const freeAtStart = os.freemem();
    let minFree = freeAtStart;
    let peak = baseline ? { ...baseline } : null;
    const timer = setInterval(() => {
      minFree = Math.min(minFree, os.freemem());
      const s = sampleWatched();
      if (s && peak) {
        if (s.mib > peak.mib) peak.mib = s.mib;
        if (s.count > peak.count) peak.count = s.count;
      }
    }, 2000);
    const [cmd, ...args] = command;
    const isBatch = process.platform === "win32" && (cmd === "npm" || cmd === "npx");
    const started = performance.now();
    const child = isBatch
      ? spawn([`${cmd}.cmd`, ...args].join(" "), [], { stdio: "inherit", shell: true })
      : spawn(cmd, args, { stdio: "inherit" });
    const done = (exit) => {
      clearInterval(timer);
      resolve({
        seconds: Number(((performance.now() - started) / 1000).toFixed(2)),
        exit,
        memory: {
          freeAtStartMiB: Math.round(freeAtStart / 2 ** 20),
          minFreeMiB: Math.round(minFree / 2 ** 20),
          watchedBaselineMiB: baseline?.mib ?? null,
          watchedPeakExtraMiB: peak && baseline ? Math.max(0, peak.mib - baseline.mib) : null,
          watchedPeakProcesses: peak?.count ?? null,
        },
      });
    };
    child.on("error", () => done(null));
    child.on("close", (code) => done(code));
  });
}

const opts = parseArgs(process.argv.slice(2));
const conditions = {
  revision: git(["rev-parse", "--short=10", "HEAD"]),
  dirty: (git(["status", "--porcelain"]) ?? "") !== "",
  cpu: os.cpus()[0]?.model.trim(),
  logicalCpus: os.cpus().length,
  ramMiB: Math.round(os.totalmem() / 2 ** 20),
  os: `${os.type()} ${os.release()}`,
  node: process.version,
  cargoBuildJobs: process.env.CARGO_BUILD_JOBS ?? null,
  rustTestThreads: process.env.RUST_TEST_THREADS ?? null,
};
if (opts.out) mkdirSync(dirname(opts.out), { recursive: true });
const all = [];
for (let run = 1; run <= opts.runs; run++) {
  if (opts.before) {
    const pre = spawnSync(opts.before, { shell: true, stdio: "inherit" });
    if (pre.status !== 0) {
      console.error(`measure: --before failed with status ${pre.status}`);
      process.exit(2);
    }
  }
  const result = await runOnce(opts.command);
  const record = { label: opts.label, run, of: opts.runs, at: new Date().toISOString(), command: opts.command.join(" "), note: opts.note ?? null, ...result, conditions };
  all.push(record);
  if (opts.out) appendFileSync(opts.out, `${JSON.stringify(record)}\n`);
  console.error(`measure: ${opts.label} run ${run}/${opts.runs}: ${result.seconds}s exit ${result.exit} peak +${result.memory.watchedPeakExtraMiB ?? "?"} MiB, min free ${result.memory.minFreeMiB} MiB`);
}
const secs = all.map((r) => r.seconds).sort((a, b) => a - b);
console.error(`measure: ${opts.label}: n=${secs.length} min ${secs[0]}s median ${secs[Math.floor(secs.length / 2)]}s max ${secs.at(-1)}s`);
process.exit(all.every((r) => r.exit === 0) ? 0 : 1);
