// Reads the repository for the impact planner: the comparison base, the change
// set (committed branch changes plus staged, unstaged, and untracked files),
// and an index of how files use each other.
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join, posix } from "node:path";

import { FIXTURE_DIR, SCENARIO_DIR } from "./manifest.mjs";

/** An error the CLI reports with exit status 2 (bad usage or comparison base). */
export class ImpactError extends Error {}

/** Default comparison bases, first that resolves wins. */
export const DEFAULT_BASES = ["origin/main", "main"];

function git(cwd, args, options = {}) {
  return execFileSync("git", args, {
    cwd,
    encoding: options.encoding ?? "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    maxBuffer: 256 * 1024 * 1024,
  });
}

function tryGit(cwd, args) {
  try {
    return git(cwd, args).trim();
  } catch {
    return null;
  }
}

/**
 * Resolves the comparison base: the merge base of HEAD and the base ref, so the
 * change set is everything this branch and working tree add on top of it.
 * - An explicit base (`--base` or EXPOSITION_IMPACT_BASE) that does not resolve
 *   is an error: the caller asked for something specific.
 * - When no default base resolves, or history is too shallow for a merge base,
 *   the result is `ok: false` and the planner broadens to full validation.
 */
export function resolveBase({ cwd, base, env = process.env }) {
  const explicit = base ?? (env.EXPOSITION_IMPACT_BASE || null);
  const source = base ? "--base" : env.EXPOSITION_IMPACT_BASE ? "EXPOSITION_IMPACT_BASE" : "default";
  const head = tryGit(cwd, ["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]);
  const shallow = tryGit(cwd, ["rev-parse", "--is-shallow-repository"]) === "true";
  if (explicit !== null) {
    if (!/^[\w./@{}~^:-]+$/.test(explicit) || explicit.startsWith("-")) throw new ImpactError(`invalid base ref: ${JSON.stringify(explicit)}`);
    const commit = tryGit(cwd, ["rev-parse", "--verify", "--quiet", `${explicit}^{commit}`]);
    if (!commit) throw new ImpactError(`base ref ${JSON.stringify(explicit)} (${source}) does not name a commit in this repository`);
    return withMergeBase({ cwd, ref: explicit, commit, head, shallow, source });
  }
  for (const ref of DEFAULT_BASES) {
    const commit = tryGit(cwd, ["rev-parse", "--verify", "--quiet", `${ref}^{commit}`]);
    if (commit) return withMergeBase({ cwd, ref, commit, head, shallow, source });
  }
  return { ok: false, ref: null, head, shallow, source, reason: `no comparison base: none of ${DEFAULT_BASES.join(", ")} exists (pass --base <ref>)` };
}

function withMergeBase({ cwd, ref, commit, head, shallow, source }) {
  if (!head) return { ok: false, ref, commit, head: null, shallow, source, reason: "HEAD has no commit yet: the change set cannot be computed" };
  const mergeBase = tryGit(cwd, ["merge-base", "HEAD", commit]);
  if (!mergeBase) {
    const why = shallow ? "the clone is shallow and the merge base is not in its history (fetch more history)" : "HEAD and the base have no common history";
    return { ok: false, ref, commit, head, shallow, source, reason: `no merge base with ${ref}: ${why}` };
  }
  return { ok: true, ref, commit, head, mergeBase, shallow, source };
}

/**
 * Changes from the merge base to the working tree: committed branch changes
 * and staged and unstaged edits (one `git diff <merge-base>`), with renames
 * detected, plus untracked files that are not ignored. Sorted by path.
 */
export function collectChanges({ cwd, mergeBase }) {
  const raw = git(cwd, ["diff", "--name-status", "-z", "--find-renames", "--no-ext-diff", "--no-textconv", mergeBase, "--"]);
  const parts = raw.split("\0");
  const changes = [];
  for (let i = 0; i < parts.length - 1; ) {
    const code = parts[i++];
    if (!code) continue;
    const letter = code[0];
    if (letter === "R" || letter === "C") {
      const from = parts[i++];
      const path = parts[i++];
      changes.push(letter === "R" ? { status: "R", path, from } : { status: "A", path });
    } else {
      changes.push({ status: letter === "T" ? "M" : letter, path: parts[i++] });
    }
  }
  const untracked = git(cwd, ["ls-files", "--others", "--exclude-standard", "-z"]).split("\0").filter(Boolean);
  for (const path of untracked) changes.push({ status: "?", path });
  return changes.sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
}

// ---------------------------------------------------------------- index

const CODE_EXT = /\.(ts|tsx|mjs|js|cjs)$/;
const IMPORT_PATTERNS = [
  /\bfrom\s*["']([^"']+)["']/g,
  /\bimport\s*["']([^"']+)["']/g,
  /\bimport\(\s*["']([^"']+)["']\s*\)/g,
  /\bvi\.(?:mock|doMock|importActual)\(\s*["']([^"']+)["']/g,
  /@import\s+(?:url\()?["']([^"']+)["']/g,
];
const RESOLVE_SUFFIXES = ["", ".ts", ".tsx", ".mjs", ".js", ".json", "/index.ts", "/index.tsx", "/index.mjs", "/index.js"];
const PATH_LITERAL = /["'`]((?:\.{1,2}\/)*(?:src|crates|scripts|tests|workers|docs|src-tauri|assets)\/[\w@.+\-/]*[\w])["'`]/g;
const WORD_LITERAL = /["'`]([\w][\w.-]*)["'`]/g; // fixture names and root files ("package.json")
const SEGMENTS = /["'`][\w.@-]+["'`](?:\s*,\s*["'`][\w.@-]+["'`])+/g;
const TS_EXPORT =/#\[ts\(export|derive\([^)]*\bTS\b/g;

/**
 * Files whose string literals can name other files at test time. The impact
 * selector's own modules are excluded: they name paths as policy, not to use
 * them (its tests still count).
 */
const isReferenceSource = (path) =>
  /^src\/.*\.test\.(ts|tsx)$/.test(path) ||
  (/^scripts\/.*\.(mjs|js)$/.test(path) && !/^scripts\/impact(\.mjs$|\/(?!.*\.test\.mjs$))/.test(path)) ||
  /^(crates\/[^/]+|src-tauri)\/.*\.rs$/.test(path);

function resolveImport(from, spec, known) {
  if (!spec.startsWith(".")) return null;
  const clean = spec.replace(/[?#].*$/, "");
  const base = posix.normalize(posix.join(posix.dirname(from), clean));
  for (const suffix of RESOLVE_SUFFIXES) if (known.has(base + suffix)) return base + suffix;
  return null;
}

/** A repository path a chain of join() segments names: exact, or a unique suffix. */
function resolveChain(chain, known, knownDirs) {
  if (known.has(chain) || knownDirs.has(chain)) return chain;
  if (!chain.includes(".")) return null; // folder suffixes are too ambiguous
  let found = null;
  for (const path of known) {
    if (path.endsWith(`/${chain}`)) {
      if (found) return null;
      found = path;
    }
  }
  return found;
}

/** True when `index` is on a line comment (`//`, `///`, `//!`) or a `*` doc-block line. */
function isCommentAt(text, index) {
  const start = text.lastIndexOf("\n", index) + 1;
  const prefix = text.slice(start, index);
  return /^\s*(\/\/|\*|\/\*)/.test(prefix) || /\s\/\/[^"'`]*$/.test(prefix);
}

/**
 * A bare name such as "package.json" is a repository file only when its line
 * anchors it to the repository root, or reads it relative to the working
 * directory (`readFileSync("vite.config.ts")`); elsewhere it is usually a file
 * in some temporary folder.
 */
function namesRootFile(text, index) {
  const start = text.lastIndexOf("\n", index) + 1;
  const end = text.indexOf("\n", index);
  const line = text.slice(start, end < 0 ? text.length : end);
  return /\b(ROOT|REPO_ROOT|CARGO_MANIFEST_DIR)\b/.test(line) || /readFileSync\(\s*$/.test(text.slice(start, index));
}

/**
 * Builds the index the planner reads.
 * - files: current files (tracked and untracked, not ignored)
 * - imports: file -> files it imports (relative specifiers; deleted targets kept)
 * - refs: file -> [{kind: "path"|"fixture", target, compiled}] paths a test or
 *   source names as a string (readFileSync, include_str!, fixture("name"));
 *   `compiled` marks include_str!/include_bytes! outside #[cfg(test)]
 * - crates: workspace crates with their workspace dependencies (incl. dev)
 * - tsExports / tsRemoved: changed Rust files defining ts-rs contract types /
 *   with fewer exported types than at the merge base
 * - packageJson: how package.json changed
 * - scenarios: e2e scenario names
 */
export function buildIndex({ cwd, changes, mergeBase, cargoMetadata = readCargoMetadata }) {
  const listed = git(cwd, ["ls-files", "--cached", "--others", "--exclude-standard", "-z"]).split("\0").filter(Boolean);
  const files = [...new Set(listed)].filter((p) => existsSync(join(cwd, p))).sort();
  const deleted = changes.flatMap((c) => (c.status === "D" ? [c.path] : c.status === "R" ? [c.from] : []));
  const known = new Set([...files, ...deleted]);
  const knownDirs = new Set();
  for (const file of known) for (let d = posix.dirname(file); d !== "."; d = posix.dirname(d)) knownDirs.add(d);
  const read = (p) => {
    try {
      return readFileSync(join(cwd, p), "utf8");
    } catch {
      return null;
    }
  };

  const fixtures = new Set(files.filter((f) => f.startsWith(`${FIXTURE_DIR}/`)).map((f) => f.split("/")[2]));
  const imports = {};
  const refs = {};
  for (const file of files) {
    const isCode = /^(src|scripts)\//.test(file) && (CODE_EXT.test(file) || file.endsWith(".css"));
    const isRef = isReferenceSource(file);
    if (!isCode && !isRef) continue;
    const text = read(file);
    if (text === null) continue;
    if (isCode) {
      const targets = new Set();
      for (const pattern of IMPORT_PATTERNS) {
        for (const [, spec] of text.matchAll(pattern)) {
          const target = resolveImport(file, spec, known);
          if (target && target !== file) targets.add(target);
        }
      }
      if (targets.size) imports[file] = [...targets].sort();
    }
    if (isRef) {
      const list = new Map();
      // Where the file's inline test module starts (`#[cfg(test)] mod tests {`);
      // an out-of-line `mod tests;` declaration ends nothing.
      const testFrom = file.endsWith(".rs") ? text.search(/#\[cfg\(test\)\]\s*(pub(\([^)]*\))?\s*)?mod\s+\w+\s*\{/) : -1;
      const crateDir = file.startsWith("src-tauri/") ? "src-tauri" : file.split("/").slice(0, 2).join("/");
      for (const match of text.matchAll(PATH_LITERAL)) {
        const literal = match[1];
        if (isCommentAt(text, match.index)) continue; // a mention, not a read
        const candidates = [
          posix.normalize(posix.join(posix.dirname(file), literal)),
          posix.normalize(posix.join(crateDir, literal)),
          literal.replace(/^(\.{1,2}\/)+/, ""),
        ];
        const target = candidates.find((c) => !c.startsWith("..") && (known.has(c) || knownDirs.has(c)));
        // The fixture root is named by tests that pick fixtures by name.
        if (!target || target === FIXTURE_DIR || target === file) continue;
        const before = text.slice(Math.max(0, match.index - 40), match.index);
        const compiled = /include_(str|bytes)!\(\s*$/.test(before) && (testFrom < 0 || match.index < testFrom);
        const prev = list.get(`path:${target}`);
        list.set(`path:${target}`, { kind: "path", target, compiled: Boolean(prev?.compiled || compiled) });
      }
      // Paths split into join() segments: join(ROOT, "scripts", "e2e", "os-drag.ps1"),
      // or join(dir, "capabilities", "default.json") naming a unique known suffix.
      for (const match of text.matchAll(SEGMENTS)) {
        if (isCommentAt(text, match.index)) continue;
        const chain = [...match[0].matchAll(/["'`]([\w.@-]+)["'`]/g)].map((m) => m[1]).join("/");
        const target = resolveChain(chain, known, knownDirs);
        if (target && target !== file && target !== FIXTURE_DIR && !list.has(`path:${target}`)) {
          list.set(`path:${target}`, { kind: "path", target, compiled: false });
        }
      }
      for (const match of text.matchAll(WORD_LITERAL)) {
        const word = match[1];
        if (fixtures.has(word)) list.set(`fixture:${word}`, { kind: "fixture", target: word, compiled: false });
        else if (word.includes(".") && known.has(word) && word !== file && !list.has(`path:${word}`) && namesRootFile(text, match.index)) {
          list.set(`path:${word}`, { kind: "path", target: word, compiled: false });
        }
      }
      if (list.size) refs[file] = [...list.values()].sort((a, b) => (a.kind + a.target < b.kind + b.target ? -1 : 1));
    }
  }

  const scenarios = files
    .filter((f) => f.startsWith(`${SCENARIO_DIR}/`) && f.endsWith(".mjs") && !f.slice(SCENARIO_DIR.length + 1).includes("/"))
    .map((f) => f.slice(SCENARIO_DIR.length + 1, -4))
    .sort();

  return { files, imports, refs, crates: cargoMetadata(cwd), scenarios, ...changeFacts({ cwd, changes, mergeBase }) };
}

/**
 * Facts about the changed files themselves: Rust files defining exported
 * contract types (now or at the merge base) and those that lose some, and how
 * package.json changed.
 */
export function changeFacts({ cwd, changes, mergeBase }) {
  const read = (p) => {
    try {
      return readFileSync(join(cwd, p), "utf8");
    } catch {
      return null;
    }
  };
  const changedRust = new Set(changes.flatMap((c) => [c.path, c.from]).filter((p) => p && p.endsWith(".rs")));
  const tsExports = [];
  const tsRemoved = [];
  const count = (t) => (t ? (t.match(TS_EXPORT) ?? []).length : 0);
  for (const path of [...changedRust].sort()) {
    const now = read(path);
    const before = safeShow(cwd, mergeBase, path);
    if (count(now) > 0 || count(before) > 0) tsExports.push(path);
    if (count(before) > count(now)) tsRemoved.push(path);
  }
  let packageJson = { kind: "unchanged", detail: "unchanged" };
  if (changes.some((c) => c.path === "package.json" || c.from === "package.json")) {
    packageJson = classifyPackageJson(safeShow(cwd, mergeBase, "package.json"), read("package.json"));
  }
  return { tsExports, tsRemoved, packageJson };
}

function safeShow(cwd, rev, path) {
  try {
    return git(cwd, ["show", `${rev}:${path}`]);
  } catch {
    return null;
  }
}

/**
 * package.json changes that only add script entries cannot change existing
 * behavior; anything else (dependencies, overrides, an existing script) can.
 */
export function classifyPackageJson(beforeText, afterText) {
  let before;
  let after;
  try {
    before = JSON.parse(beforeText);
    after = JSON.parse(afterText);
  } catch {
    before = null;
  }
  const isObject = (v) => v !== null && typeof v === "object" && !Array.isArray(v);
  if (!isObject(before) || !isObject(after)) return { kind: "changed", detail: "package.json added, removed, or not parseable" };
  const { scripts: beforeScripts = {}, ...beforeRest } = before;
  const { scripts: afterScripts = {}, ...afterRest } = after;
  if (JSON.stringify(beforeRest) !== JSON.stringify(afterRest)) return { kind: "changed", detail: "fields other than scripts changed (dependencies or configuration)" };
  const changed = Object.keys(beforeScripts).filter((k) => afterScripts[k] !== beforeScripts[k]);
  if (changed.length) return { kind: "changed", detail: `existing scripts changed or removed: ${changed.sort().join(", ")}` };
  const added = Object.keys(afterScripts).filter((k) => !(k in beforeScripts));
  // npm runs pre<x>/post<x> and lifecycle scripts implicitly around other commands.
  const implicit = added.filter((k) => /^(pre|post)/.test(k) || ["install", "prepare", "prepack", "publish", "dependencies"].includes(k));
  if (implicit.length) return { kind: "changed", detail: `added scripts that npm runs implicitly: ${implicit.sort().join(", ")}` };
  return added.length ? { kind: "scripts-added", detail: `only script entries added: ${added.sort().join(", ")}` } : { kind: "unchanged", detail: "no effective change" };
}

/** Workspace crates and their workspace dependencies, or null when unavailable. */
export function readCargoMetadata(cwd) {
  if (!existsSync(join(cwd, "Cargo.toml"))) return [];
  let meta;
  try {
    meta = JSON.parse(execFileSync("cargo", ["metadata", "--no-deps", "--format-version", "1", "--offline"], { cwd, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], maxBuffer: 64 * 1024 * 1024 }));
  } catch {
    return null;
  }
  const root = meta.workspace_root.replaceAll("\\", "/");
  const names = new Set(meta.packages.map((p) => p.name));
  return meta.packages
    .map((p) => ({
      name: p.name,
      dir: posix.relative(root, dirname(p.manifest_path).replaceAll("\\", "/")),
      deps: [...new Set(p.dependencies.filter((d) => d.path && names.has(d.name)).map((d) => d.name))].sort(),
    }))
    .sort((a, b) => (a.name < b.name ? -1 : 1));
}
