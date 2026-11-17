// The CLI end to end on a disposable repository: preview and execution resolve
// the same plan, failures reach the exit status, and bad input is refused.
import { execFileSync, spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { afterEach, describe, expect, it } from "vitest";

import { tempRepo } from "./testkit.mjs";

const ROOT = resolve(import.meta.dirname, "../..");
const CLI = join(ROOT, "scripts/impact.mjs");

const repos = [];
afterEach(() => {
  while (repos.length) repos.pop().cleanup();
});

/** A repository on a feature branch with one documentation change. */
function docsRepo() {
  const r = tempRepo({ "docs/guide.md": "one\n", ".gitignore": readFileSync(join(ROOT, ".gitignore"), "utf8") });
  repos.push(r);
  mkdirSync(join(r.dir, "scripts"), { recursive: true });
  copyFileSync(join(ROOT, "scripts/check-private-paths.mjs"), join(r.dir, "scripts/check-private-paths.mjs"));
  r.git("add", "-A");
  r.git("commit", "-qm", "tooling");
  r.git("checkout", "-q", "-b", "feature");
  r.write("docs/guide.md", "two\n");
  return r;
}

const cli = (cwd, ...args) => {
  const env = { ...process.env };
  delete env.EXPOSITION_IMPACT_BASE;
  const r = spawnSync(process.execPath, [CLI, ...args], { cwd, encoding: "utf8", env });
  return { status: r.status, stdout: r.stdout, stderr: r.stderr };
};

describe("impact CLI", () => {
  it("previews and executes the same plan, and reports what remains unverified", () => {
    const r = docsRepo();
    const preview = cli(r.dir, "plan", "--json");
    expect(preview.status).toBe(0);
    const plan = JSON.parse(preview.stdout);
    expect(plan).toMatchObject({ schema: "exposition-impact/1", mode: "affected", level: "local", uncovered: ["docs/guide.md"] });
    expect(plan.base).toMatchObject({ ok: true, ref: "main" });
    expect(plan.changes).toEqual([{ status: "M", path: "docs/guide.md" }]);
    expect(plan.checks.map((c) => c.id)).toEqual(["private-paths"]);

    const executed = cli(r.dir, "run", "--json", "--expect-plan", plan.hash);
    expect(executed.status).toBe(0);
    const result = JSON.parse(executed.stdout);
    expect(result.plan).toBe(plan.hash);
    expect(result.results.map((x) => [x.id, x.status])).toEqual([["private-paths", "passed"]]);
    expect(result.complete).toBe(false);
    expect(result.uncovered).toEqual(["docs/guide.md"]);
    expect(executed.stderr).toMatch(new RegExp(`Impact plan ${plan.hash}`));
  }, 30_000);

  it("refuses to run a plan that differs from the previewed one", () => {
    const r = docsRepo();
    const { hash } = JSON.parse(cli(r.dir, "plan", "--json").stdout);
    r.write("docs/other.md", "new\n");
    const executed = cli(r.dir, "run", "--expect-plan", hash);
    expect(executed.status).toBe(2);
    expect(executed.stderr).toMatch(/not the expected/);
  }, 30_000);

  it("propagates a failing check as exit status 1", () => {
    const r = docsRepo();
    // Ignore rules that stop covering CLAUDE.md: the private-path guard fails.
    r.write(".gitignore", readFileSync(join(r.dir, ".gitignore"), "utf8").replace(/^.*CLAUDE.*$/gm, ""));
    const executed = cli(r.dir, "run", "--json");
    expect(executed.status).toBe(1);
    const result = JSON.parse(executed.stdout);
    expect(result.results.find((x) => x.id === "private-paths").status).toBe("failed");
  }, 30_000);

  it("exits 3 when a planned check is skipped", () => {
    const r = docsRepo();
    const executed = cli(r.dir, "run", "--skip", "static");
    expect(executed.status).toBe(3);
    expect(executed.stdout).toMatch(/INCOMPLETE/);
  }, 30_000);

  it("refuses an invalid comparison base and unknown options with exit status 2", () => {
    const r = docsRepo();
    const badBase = cli(r.dir, "plan", "--base", "no-such-ref");
    expect(badBase.status).toBe(2);
    expect(badBase.stderr).toMatch(/does not name a commit/);
    expect(cli(r.dir, "plan", "--nope").status).toBe(2);
    expect(cli(r.dir, "explode").status).toBe(2);
    expect(cli(r.dir, "run", "--jobs", "0").status).toBe(2);
  }, 30_000);

  it("broadens to full validation without a usable base, and plans full validation on request", () => {
    const r = docsRepo();
    r.git("branch", "-q", "-m", "main", "trunk"); // no main, no origin/main
    const plan = JSON.parse(cli(r.dir, "plan", "--json").stdout);
    expect(plan.level).toBe("full");
    expect(plan.escalations[0].reason).toMatch(/no comparison base/);
    expect(plan.checks[0].id).toBe("check-all");
    const full = JSON.parse(cli(r.dir, "plan", "--full", "--json").stdout);
    expect(full.mode).toBe("full");
    expect(full.deferred.map((d) => d.id)).toContain("release");
  }, 30_000);

  it("prints a readable plan with reasons", () => {
    const r = docsRepo();
    const text = cli(r.dir, "plan").stdout;
    expect(text).toMatch(/Impact plan [0-9a-f]{12} \(affected; level local\)/);
    expect(text).toMatch(/Base: main \(default\) merge base/);
    expect(text).toMatch(/because baseline/);
    expect(text).toMatch(/Uncovered: no automated check exercises docs\/guide\.md/);
  }, 30_000);
});

describe("bindings drift helper", () => {
  it("refuses to run without crates or with unknown arguments", () => {
    const helper = join(ROOT, "scripts/impact/bindings-drift.mjs");
    const none = spawnSync(process.execPath, [helper], { encoding: "utf8" });
    expect(none.status).toBe(2);
    expect(none.stderr).toMatch(/at least one crate/);
    const bad = spawnSync(process.execPath, [helper, "--workspace"], { encoding: "utf8" });
    expect(bad.status).toBe(2);
  });
});

describe("npm entry points", () => {
  it("map to the impact CLI", () => {
    const { scripts } = JSON.parse(readFileSync(join(ROOT, "package.json"), "utf8"));
    expect(scripts["check:plan"]).toBe("node scripts/impact.mjs plan");
    expect(scripts["check:affected"]).toBe("node scripts/impact.mjs run");
    expect(scripts["check:full"]).toBe("node scripts/impact.mjs run --full");
    expect(scripts.check).toBe("node scripts/check.mjs");
    execFileSync(process.execPath, ["--check", CLI]);
  });
});
