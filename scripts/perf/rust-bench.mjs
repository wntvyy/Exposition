#!/usr/bin/env node
// Runs the ignored scale benchmarks of one or more crates and collects their
// results. A benchmark is an integration test in `crates/<crate>/tests/perf_*.rs`
// marked `#[ignore = "perf"]` that prints one line per measurement:
//
//   PERF {"bench":"graph.reconcile_one_file","scale":{"files":10000},"samples_ms":[4.1,4.0],"counters":{"statements":12}}
//
// `samples_ms` are repeated timings of the same operation after warm-up;
// `counters` are deterministic counts (statements, rows, bytes, copies) that
// do not depend on the build profile or the machine.
//
//   node scripts/perf/rust-bench.mjs <crate>... [--release] [--filter <text>] [--out <file.jsonl>] [--label <text>]
//
// Benchmarks run one at a time (--test-threads=1). Timings from the default
// (debug) profile compare only with other debug timings; use --release for
// numbers that represent the shipped app. Scale is chosen by the benchmark
// from EXPOSITION_PERF_SCALE (small | large | stress; default small).
import { spawn, execFileSync } from "node:child_process";
import { appendFileSync, mkdirSync, readdirSync } from "node:fs";
import os from "node:os";
import { dirname, join } from "node:path";

const argv = process.argv.slice(2);
const opts = { crates: [], release: false, filter: null, out: null, label: null };
for (let i = 0; i < argv.length; i++) {
  const a = argv[i];
  if (a === "--release") opts.release = true;
  else if (a === "--filter" || a === "--out" || a === "--label") opts[a.slice(2)] = argv[++i];
  else if (a.startsWith("--")) {
    console.error(`rust-bench: unknown option ${a}`);
    process.exit(2);
  } else opts.crates.push(a);
}
if (opts.crates.length === 0) {
  console.error("usage: node scripts/perf/rust-bench.mjs <crate>... [--release] [--filter <text>] [--out <file.jsonl>] [--label <text>]");
  process.exit(2);
}

const git = (args) => {
  try {
    return execFileSync("git", args, { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] }).trim();
  } catch {
    return null;
  }
};
const conditions = {
  revision: git(["rev-parse", "--short=10", "HEAD"]),
  dirty: (git(["status", "--porcelain"]) ?? "") !== "",
  profile: opts.release ? "release" : "dev",
  scale: process.env.EXPOSITION_PERF_SCALE ?? "small",
  cpu: os.cpus()[0]?.model.trim(),
  ramMiB: Math.round(os.totalmem() / 2 ** 20),
  os: `${os.type()} ${os.release()}`,
  label: opts.label,
};

function benchTargets(crate) {
  const dir = crate === "exposition" ? "src-tauri" : join("crates", crate);
  try {
    return readdirSync(join(dir, "tests")).filter((f) => /^perf_.*\.rs$/.test(f)).map((f) => f.replace(/\.rs$/, ""));
  } catch {
    return [];
  }
}

function run(crate, target) {
  return new Promise((resolve) => {
    // The app crate is always selected too: it keeps the dependency features of
    // the workspace build, so a benchmark does not rebuild the dependency tree.
    const args = ["test", "-p", crate, ...(crate === "exposition" ? [] : ["-p", "exposition"]), "--test", target, ...(opts.release ? ["--release"] : []), "--", "--ignored", "--nocapture", "--test-threads=1", ...(opts.filter ? [opts.filter] : [])];
    console.error(`rust-bench: cargo ${args.join(" ")}`);
    const child = spawn("cargo", args, { stdio: ["ignore", "pipe", "inherit"] });
    let rest = "";
    const records = [];
    child.stdout.setEncoding("utf8").on("data", (chunk) => {
      const lines = (rest + chunk).split(/\r?\n/);
      rest = lines.pop();
      for (const line of lines) {
        const at = line.indexOf("PERF {");
        if (at < 0) {
          if (/^test .* (FAILED|ok)$|^test result/.test(line)) console.error(line);
          continue;
        }
        try {
          records.push(JSON.parse(line.slice(at + 5)));
        } catch {
          console.error(`rust-bench: unparseable PERF line: ${line}`);
        }
      }
    });
    child.on("close", (code) => resolve({ code, records }));
  });
}

const stats = (samples) => {
  const s = [...samples].sort((a, b) => a - b);
  return { n: s.length, min: s[0], median: s[Math.floor(s.length / 2)], max: s.at(-1) };
};

let failed = false;
let total = 0;
if (opts.out) mkdirSync(dirname(opts.out), { recursive: true });
for (const crate of opts.crates) {
  const targets = benchTargets(crate);
  if (targets.length === 0) {
    console.error(`rust-bench: ${crate} has no tests/perf_*.rs`);
    failed = true;
    continue;
  }
  for (const target of targets) {
    const { code, records } = await run(crate, target);
    if (code !== 0) failed = true;
    for (const record of records) {
      total += 1;
      const summary = Array.isArray(record.samples_ms) && record.samples_ms.length ? stats(record.samples_ms) : null;
      const full = { crate, target, at: new Date().toISOString(), ...record, summary, conditions };
      if (opts.out) appendFileSync(opts.out, `${JSON.stringify(full)}\n`);
      const scale = record.scale ? ` ${JSON.stringify(record.scale)}` : "";
      const time = summary ? ` n=${summary.n} min ${summary.min.toFixed(2)} median ${summary.median.toFixed(2)} max ${summary.max.toFixed(2)} ms` : "";
      const counters = record.counters ? ` ${JSON.stringify(record.counters)}` : "";
      console.log(`${record.bench}${scale}:${time}${counters}`);
    }
  }
}
console.error(`rust-bench: ${total} measurements (${conditions.profile} profile, scale ${conditions.scale}, revision ${conditions.revision}${conditions.dirty ? " dirty" : ""})`);
process.exit(failed ? 1 : 0);
