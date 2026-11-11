#!/usr/bin/env node
// Fails when private planning/agent documents or local credentials are tracked
// or staged, or when the ignore rules stop covering them. Run before commits and
// in CI (`npm run check:private`).
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

/** Path patterns that must never enter the repository (POSIX-style paths). */
export const PRIVATE_PATTERNS = [
  /(^|\/)CLAUDE(\.local)?\.md$/i,
  /(^|\/)AGENTS(\.local)?\.md$/i,
  /(^|\/)\.claude\//,
  /(^|\/)\.agents\//,
  /(^|\/)\.exposition-dev\//,
  /(^|\/)\.env$/,
  /(^|\/)\.env\.(?!example$)[^/]+$/,
];

/** Representative paths the ignore rules must cover. */
export const MUST_BE_IGNORED = [
  "CLAUDE.md",
  "CLAUDE.local.md",
  "AGENTS.md",
  "docs/CLAUDE.md",
  ".claude/settings.local.json",
  ".agents/notes.md",
  ".exposition-dev/MASTER.md",
  ".exposition-dev/UI_STANDARD.md",
  ".env",
  ".env.local",
];

export function findPrivatePaths(paths) {
  return paths.filter((p) => PRIVATE_PATTERNS.some((re) => re.test(p.replaceAll("\\", "/"))));
}

function git(args) {
  return execFileSync("git", args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
}

function main() {
  const indexed = git(["ls-files", "--cached", "-z"]).split("\0").filter(Boolean);
  const leaked = findPrivatePaths(indexed);

  const notIgnored = [];
  for (const path of MUST_BE_IGNORED) {
    try {
      git(["check-ignore", "-q", "--no-index", path]);
    } catch {
      notIgnored.push(path);
    }
  }

  if (leaked.length || notIgnored.length) {
    if (leaked.length) {
      console.error("Private paths are tracked or staged (remove them from the index with `git rm --cached`):");
      for (const p of leaked) console.error(`  ${p}`);
    }
    if (notIgnored.length) {
      console.error("Ignore rules no longer cover these private paths:");
      for (const p of notIgnored) console.error(`  ${p}`);
    }
    process.exit(1);
  }
  console.log(`check:private ok (${indexed.length} indexed paths, ${MUST_BE_IGNORED.length} ignore rules verified)`);
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  main();
}
