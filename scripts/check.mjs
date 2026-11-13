#!/usr/bin/env node
// Full local check: the same gates CI runs. Usage: `npm run check [-- --skip-rust]`.
import { spawn } from "node:child_process";

const skipRust = process.argv.includes("--skip-rust");
const npm = process.platform === "win32" ? "npm.cmd" : "npm";
// On GitHub Actions a failing gate is also reported as an error annotation
// carrying the end of its output, so the failure shows on the run page and in
// the check-run annotations without opening the full log.
const annotate = process.env.GITHUB_ACTIONS === "true";
const TAIL_LINES = 60;
// Failure lines kept from anywhere in a gate's output (a failed Rust test or
// panic, a compiler or type error, a failed vitest file), beyond the tail.
const NOTABLE = /\.\.\. FAILED$|panicked at|^error(\[\w+\])?:|error TS\d+|^\s*(FAIL|×)\s/;
// A Rust panic's message follows its "panicked at" line; keep up to this many.
const PANIC_MESSAGE_LINES = 4;
const NOTABLE_LINES = 80;
const MAX_ANNOTATION = 12_000;
const ANSI = new RegExp(String.raw`\u001b\[[0-9;]*[A-Za-z]`, "g");

const steps = [
  ["private paths", "node", ["scripts/check-private-paths.mjs"]],
  ["permission files", "node", ["scripts/check-permissions.mjs"]],
  ["typecheck", npm, ["run", "typecheck"]],
  ["frontend tests", npm, ["test"]],
  ["frontend build", npm, ["run", "build:web"]],
  ...(skipRust
    ? []
    : [
        ["rust format", "cargo", ["fmt", "--all", "--", "--check"]],
        ["rust lint", "cargo", ["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]],
        // Run every test binary even after one fails, so a run reports all failures.
        ["rust tests", "cargo", ["test", "--workspace", "--no-fail-fast"]],
        ["bindings", "node", ["scripts/check-bindings.mjs"]],
      ]),
];

/** Runs one gate, streaming its output; when annotating, keeps the last lines. */
function run(cmd, args) {
  return new Promise((resolve) => {
    // npm.cmd is a batch file; Node requires a shell to run it on Windows.
    const child = spawn(cmd, args, {
      stdio: annotate ? ["inherit", "pipe", "pipe"] : "inherit",
      shell: cmd.endsWith(".cmd"),
    });
    let tail = [];
    const notable = [];
    const partial = new Map();
    // Per stream: how many lines of a panic message are still to be kept.
    const following = new Map();
    const take = (lines, stream) => {
      for (const raw of lines) {
        const line = raw.replace(ANSI, "");
        const follow = following.get(stream) ?? 0;
        if (follow > 0 && line.trim() && !line.startsWith("note: ")) {
          if (notable.length < NOTABLE_LINES) notable.push(`    ${line}`);
          following.set(stream, follow - 1);
        } else {
          following.set(stream, 0);
        }
        if (NOTABLE.test(line) && notable.length < NOTABLE_LINES && !notable.includes(line)) {
          notable.push(line);
          if (line.includes("panicked at")) following.set(stream, PANIC_MESSAGE_LINES);
        }
        tail.push(line);
      }
      if (tail.length > TAIL_LINES) tail = tail.slice(-TAIL_LINES);
    };
    const keep = (stream, out) => {
      stream.setEncoding("utf8");
      stream.on("data", (chunk) => {
        out.write(chunk);
        const lines = ((partial.get(stream) ?? "") + chunk).split(/\r?\n/);
        partial.set(stream, lines.pop());
        take(lines, stream);
      });
    };
    if (child.stdout) keep(child.stdout, process.stdout);
    if (child.stderr) keep(child.stderr, process.stderr);
    child.on("error", (error) => resolve({ status: null, notable, tail: [String(error)] }));
    child.on("close", (status) => {
      for (const [stream, rest] of partial) if (rest) take([rest], stream);
      resolve({ status, notable, tail });
    });
  });
}

// Workflow-command escaping (https://github.com/actions/toolkit, core/command.ts).
const escapeData = (s) => s.replaceAll("%", "%25").replaceAll("\r", "%0D").replaceAll("\n", "%0A");
const escapeProperty = (s) => escapeData(s).replaceAll(":", "%3A").replaceAll(",", "%2C");

const results = [];
for (const [name, cmd, args] of steps) {
  console.log(`\n=== ${name}: ${cmd} ${args.join(" ")}`);
  const started = Date.now();
  const { status, notable, tail } = await run(cmd, args);
  const ok = status === 0;
  results.push({ name, ok, seconds: ((Date.now() - started) / 1000).toFixed(1) });
  if (!ok && annotate) {
    const parts = notable.length ? [...notable, "--- last lines ---", ...tail] : tail;
    const text = parts.join("\n").slice(-MAX_ANNOTATION);
    console.log(`::error title=${escapeProperty(`npm run check: ${name} failed`)}::${escapeData(text)}`);
  }
}

console.log("\n=== summary");
for (const r of results) console.log(`${r.ok ? "PASS" : "FAIL"}  ${r.name} (${r.seconds}s)`);
process.exit(results.every((r) => r.ok) ? 0 : 1);
