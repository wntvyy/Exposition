// Comparison base, change collection, and index on disposable git repositories.
import { execFileSync } from "node:child_process";
import { afterEach, describe, expect, it } from "vitest";

import { buildIndex, classifyPackageJson, collectChanges, ImpactError, resolveBase } from "./repo.mjs";
import { tempRepo } from "./testkit.mjs";

const repos = [];
const make = (...args) => {
  const r = tempRepo(...args);
  repos.push(r);
  return r;
};
afterEach(() => {
  while (repos.length) repos.pop().cleanup();
});
const noCargo = () => [];
const env = {};

describe("comparison base", () => {
  it("uses the merge base with main, so commits on main after branching are not changes", () => {
    const r = make({ "a.txt": "a\n" });
    const branchPoint = r.git("rev-parse", "HEAD");
    r.git("checkout", "-q", "-b", "feature");
    r.write("b.txt", "b\n");
    r.git("add", "-A");
    r.git("commit", "-q", "-m", "feature");
    r.git("checkout", "-q", "main");
    r.write("later-on-main.txt", "x\n");
    r.git("add", "-A");
    r.git("commit", "-q", "-m", "main moves on");
    r.git("checkout", "-q", "feature");
    const base = resolveBase({ cwd: r.dir, env });
    expect(base).toMatchObject({ ok: true, ref: "main", source: "default", mergeBase: branchPoint });
    expect(collectChanges({ cwd: r.dir, mergeBase: base.mergeBase })).toEqual([{ status: "A", path: "b.txt" }]);
  });

  it("prefers origin/main and honors --base and EXPOSITION_IMPACT_BASE", () => {
    const r = make({ "a.txt": "a\n" });
    const first = r.git("rev-parse", "HEAD");
    r.git("update-ref", "refs/remotes/origin/main", first);
    r.write("a.txt", "a2\n");
    r.git("commit", "-qam", "second");
    const second = r.git("rev-parse", "HEAD");
    expect(resolveBase({ cwd: r.dir, env }).ref).toBe("origin/main");
    expect(resolveBase({ cwd: r.dir, env }).mergeBase).toBe(first);
    expect(resolveBase({ cwd: r.dir, base: "HEAD", env }).mergeBase).toBe(second);
    expect(resolveBase({ cwd: r.dir, env: { EXPOSITION_IMPACT_BASE: "HEAD" } })).toMatchObject({ source: "EXPOSITION_IMPACT_BASE", mergeBase: second });
    expect(resolveBase({ cwd: r.dir, env: { EXPOSITION_IMPACT_BASE: "" } })).toMatchObject({ source: "default", ref: "origin/main" });
  });

  it("rejects an explicit base that does not exist or is not a ref", () => {
    const r = make({ "a.txt": "a\n" });
    expect(() => resolveBase({ cwd: r.dir, base: "no-such-branch", env })).toThrow(ImpactError);
    expect(() => resolveBase({ cwd: r.dir, base: "--output=x", env })).toThrow(/invalid base ref/);
    expect(() => resolveBase({ cwd: r.dir, env: { EXPOSITION_IMPACT_BASE: "gone" } })).toThrow(/EXPOSITION_IMPACT_BASE/);
  });

  it("reports a missing default base instead of guessing", () => {
    const r = make({ "a.txt": "a\n" }, { branch: "trunk" });
    const base = resolveBase({ cwd: r.dir, env });
    expect(base.ok).toBe(false);
    expect(base.reason).toMatch(/no comparison base/);
  });

  it("reports a shallow clone whose history lacks the merge base", () => {
    const origin = make({ "a.txt": "a\n" });
    origin.git("checkout", "-q", "-b", "feature");
    for (const n of [1, 2, 3]) {
      origin.write("a.txt", `feature ${n}\n`);
      origin.git("commit", "-qam", `feature ${n}`);
    }
    origin.git("checkout", "-q", "main");
    origin.write("m.txt", "main\n");
    origin.git("add", "-A");
    origin.git("commit", "-qm", "main 2");
    const clone = make();
    clone.cleanup();
    execFileSync("git", ["clone", "-q", "--depth", "1", "--branch", "feature", `file://${origin.dir.replaceAll("\\", "/")}`, clone.dir], { stdio: "ignore" });
    execFileSync("git", ["fetch", "-q", "--depth", "1", "origin", "main:refs/remotes/origin/main"], { cwd: clone.dir, stdio: "ignore" });
    const base = resolveBase({ cwd: clone.dir, env });
    expect(base.ok).toBe(false);
    expect(base.shallow).toBe(true);
    expect(base.reason).toMatch(/shallow/);
  });
});

describe("change collection", () => {
  it("includes committed, staged, unstaged, and untracked changes, but not ignored files", () => {
    const r = make({ ".gitignore": "ignored.txt\n", "a.txt": "a\n", "b.txt": "b\n", "c.txt": "c\n" });
    const base = r.git("rev-parse", "HEAD");
    r.write("a.txt", "a2\n");
    r.git("commit", "-qam", "committed");
    r.write("b.txt", "b2\n");
    r.git("add", "b.txt");
    r.write("c.txt", "c2\n");
    r.write("d.txt", "untracked\n");
    r.write("ignored.txt", "ignored\n");
    expect(collectChanges({ cwd: r.dir, mergeBase: base })).toEqual([
      { status: "M", path: "a.txt" },
      { status: "M", path: "b.txt" },
      { status: "M", path: "c.txt" },
      { status: "?", path: "d.txt" },
    ]);
  });

  it("reports renames with both paths, and committed and unstaged deletions", () => {
    const r = make({ "src/old.ts": "export const value = 'a fairly long line so rename detection is certain';\n", "gone.txt": "x\n", "later.txt": "y\n" });
    const base = r.git("rev-parse", "HEAD");
    r.git("mv", "src/old.ts", "src/new.ts");
    r.git("rm", "-q", "gone.txt");
    r.git("commit", "-qm", "rename and delete");
    r.remove("later.txt");
    expect(collectChanges({ cwd: r.dir, mergeBase: base })).toEqual([
      { status: "D", path: "gone.txt" },
      { status: "D", path: "later.txt" },
      { status: "R", path: "src/new.ts", from: "src/old.ts" },
    ]);
  });

  it("handles paths with spaces and unicode", () => {
    const r = make({ "docs/a b.md": "x\n" });
    const base = r.git("rev-parse", "HEAD");
    r.write("docs/a b.md", "y\n");
    r.write("docs/ü.md", "z\n");
    expect(collectChanges({ cwd: r.dir, mergeBase: base }).map((c) => c.path)).toEqual(["docs/a b.md", "docs/ü.md"]);
  });
});

describe("index", () => {
  it("resolves relative imports, keeps importers of deleted modules, and reads path references", () => {
    const r = make({
      "src/ui/index.ts": 'export { Button } from "./Button";\n',
      "src/ui/Button.tsx": 'import "./button.css";\nexport const Button = 1;\n',
      "src/ui/button.css": '@import "./tokens.css";\n',
      "src/ui/tokens.css": ":root {}\n",
      "src/ui/old.ts": "export const old = 1;\n",
      "src/app/App.tsx": 'import { Button } from "../ui";\nimport { old } from "../ui/old";\nimport React from "react";\n',
      "src/ui/tokens.test.ts": 'const css = readFileSync("src/ui/tokens.css", "utf8");\n',
      "scripts/e2e/scenarios/lean-check.mjs": 'const dir = fixture("lean-basic");\n',
      "tests/fixtures/lean-basic/Defs.lean": "def x := 1\n",
      "workers/kernel-host/host.py": "print(1)\n",
      "crates/exposition-research/src/helper.rs": 'const SRC: &str = include_str!("../../../workers/kernel-host/host.py");\n#[cfg(test)]\nmod tests { const T: &str = include_str!("../../../tests/fixtures/lean-basic/Defs.lean"); }\n',
    });
    const base = r.git("rev-parse", "HEAD");
    r.git("rm", "-q", "src/ui/old.ts");
    const changes = collectChanges({ cwd: r.dir, mergeBase: base });
    const index = buildIndex({ cwd: r.dir, changes, mergeBase: base, cargoMetadata: noCargo });
    expect(index.imports["src/ui/index.ts"]).toEqual(["src/ui/Button.tsx"]);
    expect(index.imports["src/ui/Button.tsx"]).toEqual(["src/ui/button.css"]);
    expect(index.imports["src/ui/button.css"]).toEqual(["src/ui/tokens.css"]);
    expect(index.imports["src/app/App.tsx"]).toEqual(["src/ui/index.ts", "src/ui/old.ts"]);
    expect(index.files).not.toContain("src/ui/old.ts");
    expect(index.refs["src/ui/tokens.test.ts"]).toEqual([{ kind: "path", target: "src/ui/tokens.css", compiled: false }]);
    expect(index.refs["scripts/e2e/scenarios/lean-check.mjs"]).toEqual([{ kind: "fixture", target: "lean-basic", compiled: false }]);
    expect(index.refs["crates/exposition-research/src/helper.rs"]).toEqual([
      { kind: "path", target: "tests/fixtures/lean-basic/Defs.lean", compiled: false },
      { kind: "path", target: "workers/kernel-host/host.py", compiled: true },
    ]);
  });

  it("treats include_str! after an out-of-line `mod tests;` as compiled code (review finding)", () => {
    const r = make({
      "tests/fixtures/f1/x.txt": "x\n",
      "crates/a/src/lib.rs": '#[cfg(test)]\nmod tests;\npub const T: &str = include_str!("../../../tests/fixtures/f1/x.txt");\n',
    });
    const index = buildIndex({ cwd: r.dir, changes: [], mergeBase: r.git("rev-parse", "HEAD"), cargoMetadata: noCargo });
    expect(index.refs["crates/a/src/lib.rs"]).toContainEqual({ kind: "path", target: "tests/fixtures/f1/x.txt", compiled: true });
  });

  it("resolves paths split into join() segments (review finding)", () => {
    const r = make({
      "scripts/e2e/os-drag.ps1": "#\n",
      "src-tauri/capabilities/default.json": "{}\n",
      "scripts/e2e/scenarios/qol-files.mjs": 'const ps = join(REPO_ROOT, "scripts", "e2e", "os-drag.ps1");\nconst cap = join(tauriDir, "capabilities", "default.json");\nconst other = join(tmp, "a", "b.txt");\n',
    });
    const index = buildIndex({ cwd: r.dir, changes: [], mergeBase: r.git("rev-parse", "HEAD"), cargoMetadata: noCargo });
    expect(index.refs["scripts/e2e/scenarios/qol-files.mjs"]).toEqual([
      { kind: "path", target: "scripts/e2e/os-drag.ps1", compiled: false },
      { kind: "path", target: "src-tauri/capabilities/default.json", compiled: false },
    ]);
  });

  it("counts a bare root file name only when anchored to the repository root", () => {
    const r = make({
      "package.json": "{}\n",
      "scripts/a.test.mjs": 'const pkg = readFileSync(join(ROOT, "package.json"), "utf8");\n',
      "scripts/b.test.mjs": 'const cfg = readFileSync("package.json", "utf8");\n',
      "scripts/c.test.mjs": 'const other = join(tempDir, "package.json");\n',
      "crates/x/src/lib.rs": 'fn f(dir: &Path) { dir.join("package.json"); }\n',
    });
    const index = buildIndex({ cwd: r.dir, changes: [], mergeBase: r.git("rev-parse", "HEAD"), cargoMetadata: noCargo });
    expect(index.refs["scripts/a.test.mjs"]).toEqual([{ kind: "path", target: "package.json", compiled: false }]);
    expect(index.refs["scripts/b.test.mjs"]).toEqual([{ kind: "path", target: "package.json", compiled: false }]);
    expect(index.refs["scripts/c.test.mjs"]).toBeUndefined();
    expect(index.refs["crates/x/src/lib.rs"]).toBeUndefined();
  });

  it("flags changed Rust files with contract types, and files that lose them", () => {
    const r = make({
      "crates/c/src/types.rs": "#[derive(Serialize, TS)]\n#[ts(export)]\npub struct A;\n#[derive(Serialize, TS)]\n#[ts(export)]\npub struct B;\n",
      "crates/c/src/plain.rs": "pub fn f() {}\n",
    });
    const base = r.git("rev-parse", "HEAD");
    r.write("crates/c/src/types.rs", "#[derive(Serialize, TS)]\n#[ts(export)]\npub struct A;\n");
    r.write("crates/c/src/plain.rs", "pub fn g() {}\n");
    const changes = collectChanges({ cwd: r.dir, mergeBase: base });
    const index = buildIndex({ cwd: r.dir, changes, mergeBase: base, cargoMetadata: noCargo });
    expect(index.tsExports).toEqual(["crates/c/src/types.rs"]);
    expect(index.tsRemoved).toEqual(["crates/c/src/types.rs"]);
  });

  it("lists scenarios and reads package.json changes", () => {
    const r = make({ "package.json": '{"name":"x","scripts":{"test":"vitest"}}\n', "scripts/e2e/scenarios/smoke.mjs": "export default () => {};\n" });
    const base = r.git("rev-parse", "HEAD");
    r.write("package.json", '{"name":"x","scripts":{"test":"vitest","check:plan":"node p.mjs"}}\n');
    const changes = collectChanges({ cwd: r.dir, mergeBase: base });
    const index = buildIndex({ cwd: r.dir, changes, mergeBase: base, cargoMetadata: noCargo });
    expect(index.scenarios).toEqual(["smoke"]);
    expect(index.packageJson.kind).toBe("scripts-added");
  });
});

describe("package.json classification", () => {
  const pkg = (extra) => JSON.stringify({ name: "x", dependencies: { a: "1" }, scripts: { test: "vitest" }, ...extra });
  it("distinguishes added scripts from behavior changes", () => {
    expect(classifyPackageJson(pkg(), pkg({ scripts: { test: "vitest", new: "x" } })).kind).toBe("scripts-added");
    expect(classifyPackageJson(pkg(), pkg({ scripts: { test: "vitest --changed" } })).kind).toBe("changed");
    expect(classifyPackageJson(pkg(), pkg({ scripts: {} })).kind).toBe("changed");
    expect(classifyPackageJson(pkg(), pkg({ dependencies: { a: "2" } })).kind).toBe("changed");
    expect(classifyPackageJson(null, pkg()).kind).toBe("changed");
    expect(classifyPackageJson(pkg(), pkg()).kind).toBe("unchanged");
    // npm runs these implicitly around existing commands (review finding).
    expect(classifyPackageJson(pkg(), pkg({ scripts: { test: "vitest", pretest: "exit 1" } })).kind).toBe("changed");
    expect(classifyPackageJson(pkg(), pkg({ scripts: { test: "vitest", postinstall: "x" } })).kind).toBe("changed");
    expect(classifyPackageJson(pkg(), pkg({ scripts: { test: "vitest", prepare: "x" } })).kind).toBe("changed");
  });
});
