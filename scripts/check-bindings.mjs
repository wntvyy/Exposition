#!/usr/bin/env node
// Regenerates the TypeScript contract bindings from Rust and fails if they
// differ from what is committed (including stale files for removed types).
import { execFileSync } from "node:child_process";
import { readdirSync, rmSync } from "node:fs";
import { join } from "node:path";

const dir = "src/bindings";
for (const name of readdirSync(dir)) {
  if (name.endsWith(".ts")) rmSync(join(dir, name));
}

execFileSync("cargo", ["test", "--workspace", "--quiet", "export_bindings"], { stdio: "inherit" });

// Compare against the index so staged-but-uncommitted bindings pass, and catch
// new (untracked) or deleted files as well as content changes.
const changed = execFileSync("git", ["diff", "--name-status", "--", dir], { encoding: "utf8" }).trim();
const untracked = execFileSync("git", ["ls-files", "--others", "--exclude-standard", "--", dir], { encoding: "utf8" }).trim();
const status = [changed, untracked].filter(Boolean).join("\n");
if (status) {
  console.error("Generated bindings differ from the committed ones. Commit the regenerated src/bindings:");
  console.error(status);
  process.exit(1);
}
console.log("check:bindings ok");
