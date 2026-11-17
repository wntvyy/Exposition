#!/usr/bin/env node
// Scoped bindings check for impact runs: regenerates the contract bindings of
// the named crates (`cargo test -p <crate> export_bindings`) and fails if the
// result differs from what is staged or committed in src/bindings — the same
// comparison as scripts/check-bindings.mjs, without deleting other crates'
// files first. Stale files of removed types are caught only by the full gate
// (`npm run check:bindings` / `npm run check`), which the planner selects when
// a changed file loses exported types.
// Usage: node scripts/impact/bindings-drift.mjs -p <crate> [-p <crate>...]
import { execFileSync } from "node:child_process";

const args = process.argv.slice(2);
const crates = [];
for (let i = 0; i < args.length; i++) {
  if (args[i] === "-p" && args[i + 1]) crates.push(args[++i]);
  else {
    console.error(`usage: bindings-drift.mjs -p <crate> [-p <crate>...] (unexpected ${args[i]})`);
    process.exit(2);
  }
}
if (crates.length === 0) {
  console.error("bindings-drift.mjs: name at least one crate with -p");
  process.exit(2);
}

const dir = "src/bindings";
try {
  execFileSync("cargo", ["test", ...crates.flatMap((c) => ["-p", c]), "--quiet", "export_bindings"], { stdio: "inherit" });
} catch (error) {
  console.error("Regenerating bindings failed.");
  process.exit(typeof error.status === "number" && error.status !== 0 ? error.status : 1);
}
const changed = execFileSync("git", ["diff", "--name-status", "--", dir], { encoding: "utf8" }).trim();
const untracked = execFileSync("git", ["ls-files", "--others", "--exclude-standard", "--", dir], { encoding: "utf8" }).trim();
const status = [changed, untracked].filter(Boolean).join("\n");
if (status) {
  console.error("Generated bindings differ from the staged/committed ones. Stage or commit the regenerated src/bindings:");
  console.error(status);
  process.exit(1);
}
console.log(`bindings drift ok (${crates.join(", ")})`);
