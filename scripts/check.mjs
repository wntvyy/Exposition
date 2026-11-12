#!/usr/bin/env node
// Full local check: the same gates CI runs. Usage: `npm run check [-- --skip-rust]`.
import { spawnSync } from "node:child_process";

const skipRust = process.argv.includes("--skip-rust");
const npm = process.platform === "win32" ? "npm.cmd" : "npm";

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
        ["rust tests", "cargo", ["test", "--workspace"]],
        ["bindings", "node", ["scripts/check-bindings.mjs"]],
      ]),
];

const results = [];
for (const [name, cmd, args] of steps) {
  console.log(`\n=== ${name}: ${cmd} ${args.join(" ")}`);
  const started = Date.now();
  // npm.cmd is a batch file; Node requires a shell to run it on Windows.
  const run = spawnSync(cmd, args, { stdio: "inherit", shell: cmd.endsWith(".cmd") });
  results.push({ name, ok: run.status === 0, seconds: ((Date.now() - started) / 1000).toFixed(1) });
}

console.log("\n=== summary");
for (const r of results) console.log(`${r.ok ? "PASS" : "FAIL"}  ${r.name} (${r.seconds}s)`);
process.exit(results.every((r) => r.ok) ? 0 : 1);
