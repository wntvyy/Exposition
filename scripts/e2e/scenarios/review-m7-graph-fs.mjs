// R-m7 independent review: graph behavior against the real file system in
// the real app (debug build): a metadata folder spelled with another case,
// research-space adoption after a preset, and a project created inside an
// open project. Prints `ok:` / `DEFECT:` / `info:` lines and fails if any
// DEFECT was reproduced.
//
// Runs only when named: `npm run e2e -- review-m7-graph-fs`.
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { invoke, waitFor } from "../workspace-helpers.mjs";

const NAME = "review-m7-graph-fs";

function recorder(artifactsDir) {
  const results = [];
  const log = (status, id, detail) => {
    results.push({ status, id, detail });
    const tag = status === "DEFECT" ? "DEFECT:" : status === "ok" ? "ok:" : "info:";
    console.log(`${tag} ${id}${detail ? ` — ${detail}` : ""}`);
  };
  return {
    info: (id, detail = "") => log("info", id, detail),
    expect(id, ok, detail = "") {
      log(ok ? "ok" : "DEFECT", id, detail);
      return ok;
    },
    async finish() {
      await writeFile(join(artifactsDir, "findings.json"), JSON.stringify(results, null, 2));
      const defects = results.filter((r) => r.status === "DEFECT");
      if (defects.length) throw new Error(`${defects.length} defect(s) reproduced: ${defects.map((d) => d.id).join("; ")}`);
    },
  };
}

async function write(root, rel, body) {
  const full = join(root, ...rel.split("/"));
  await mkdir(join(full, ".."), { recursive: true });
  await writeFile(full, body);
}

const short = (s, n = 200) => (typeof s === "string" ? s : JSON.stringify(s) ?? "undefined").slice(0, n);

export default async function reviewM7GraphFs({ launchApp, artifactsDir }) {
  if (!process.argv.slice(2).includes(NAME)) {
    console.log(`skip ${NAME} (run by name)`);
    return;
  }
  const r = recorder(artifactsDir);
  const base = await mkdtemp(join(tmpdir(), "xp-rm7fs-"));
  const dataDir = join(base, "data");
  await mkdir(dataDir, { recursive: true });
  const app = await launchApp({ dataDir });
  const { page } = app;
  const call = (cmd, args = {}) => invoke(page, cmd, args);
  const status = () => call("graph_status");
  const synchronized = (message, timeout = 60_000) =>
    waitFor(
      async () => {
        const s = await status();
        return s.state.kind === "attached" && s.freshness.state === "synchronized" && s.freshness.pending === 0 ? s : null;
      },
      { timeout, interval: 100, message },
    );
  const resources = async () =>
    (await call("graph_snapshot", { query: { kinds: ["resource"], text: null, around: null, includeInactive: false, limit: 2000 } })).entities;
  try {
    // ---------------------------------------------------- 0. control (lowercase)
    {
      const plain = join(base, "plain");
      await write(plain, "main.typ", "= Paper\n");
      await call("workspace_open", { path: plain });
      await synchronized("control project synchronized");
      await write(plain, ".exposition/runs/run-1.json", '{"record":{}}');
      await write(plain, ".exposition/artifacts/run-1/output-0.png", "png");
      await call("graph_reconcile", { force: true });
      await page.waitForTimeout(20000);
      const s = await status();
      r.info("0 control: lowercase .exposition after the same steps and 20 s", `${s.revision}:${s.freshness.state}:${s.freshness.pending}`);
      const keys = (await resources()).map((e) => e.key);
      r.expect("0 control: synchronized, no run records admitted", s.freshness.state === "synchronized" && !keys.some((k) => k.startsWith(".exposition/runs")), keys.join(", "));
    }
    // ---------------------------------------------------- 1. `.Exposition`
    const cased = join(base, "cased");
    await write(cased, "main.typ", "= Paper\n");
    await mkdir(join(cased, ".Exposition"), { recursive: true });
    await call("workspace_open", { path: cased });
    const first = await synchronized("cased project synchronized");
    await write(cased, ".exposition/runs/run-1.json", '{"record":{}}');
    await write(cased, ".exposition/artifacts/run-1/output-0.png", "png");
    await call("graph_reconcile", { force: true });
    const keys = (await resources()).map((e) => e.key);
    const leaked = keys.filter((k) => /^\.exposition\/(runs|artifacts)\//i.test(k));
    r.expect("1 run records/artifacts under `.Exposition` are not resources", leaked.length === 0, leaked.join(", "));
    // Does the graph settle, or do its own writes keep it busy?
    const samples = [];
    const t0 = Date.now();
    while (Date.now() - t0 < 20000) {
      const s = await status();
      samples.push(`${s.revision}:${s.freshness.state}:${s.freshness.pending}`);
      await page.waitForTimeout(250);
    }
    const revs = new Set(samples.map((x) => x.split(":")[0]));
    r.info("1 status samples over 20 s", `${samples[0]} … ${samples.at(-1)} (${revs.size} distinct revisions; states ${[...new Set(samples.map((x) => x.split(":")[1]))].join(",")})`);
    const last = await status();
    r.expect("1 the graph reaches synchronized (agent effects are not paused forever)", last.freshness.state === "synchronized", `${last.freshness.state} pending=${last.freshness.pending}`);
    r.expect("1 the graph settles (its own writes do not trigger reconciliation)", revs.size <= 2, `revisions seen: ${[...revs].join(",")}; first ${first.revision}`);

    // ---------------------------------------- 2. preset research space adoption
    const comp = join(base, "computational");
    const bp = await call("project_preset_blueprint", {
      request: { preset: "computational", title: "Review computational", format: null, runtime: "python", leanToolchain: null, includeWorkflow: false, purpose: null },
    });
    r.info("2 preset research space", short(bp.research));
    const created = await call("project_create", { request: { destination: comp, blueprint: JSON.stringify(bp), acceptTrustedLocation: false } });
    r.info("2 created", short(created.root));
    await synchronized("computational project synchronized");
    const before = await call("research_space");
    const enabled = await call("research_space_enable");
    const spaces = (await call("graph_snapshot", { query: { kinds: ["researchSpace"], text: null, around: null, includeInactive: true, limit: 20 } })).entities;
    r.expect(
      "2 Enable Research adopts the preset's research space (one space)",
      spaces.length === 1 && before?.spaceId === enabled.spaceId && enabled.spaceId === bp.research?.id,
      `before=${before?.spaceId} enabled=${enabled.spaceId} blueprint=${bp.research?.id} spaces=${spaces.length} keys=${spaces.map((s) => s.key).join(",")}`,
    );
    const ops = await call("graph_operations");
    r.expect("2 creation left no unresolved operation", ops.length === 0, short(ops));
    const trust = await call("trust_status", { root: comp }).catch((e) => ({ error: e.message }));
    r.expect("2 the new project is not trusted", trust && trust.trusted === false, short(trust));
    const sessions = await call("research_sessions");
    r.expect("2 no research session was started", sessions.length === 0, short(sessions));

    // ---------------------------------------- 3. a project inside an open project
    const outer = join(base, "outer");
    await write(outer, "paper.typ", "= Outer\n");
    await call("workspace_open", { path: outer });
    await synchronized("outer synchronized");
    await mkdir(join(outer, "examples"), { recursive: true });
    const inner = join(outer, "examples", "inner");
    const check = await call("project_check_destination", { destination: inner });
    r.info("3 destination inside an open project", short(check));
    if (check.ok) {
      const innerBp = await call("project_preset_blueprint", {
        request: { preset: "computational", title: "Inner", format: null, runtime: "python", leanToolchain: null, includeWorkflow: false, purpose: null },
      });
      await call("project_create", { request: { destination: inner, blueprint: JSON.stringify(innerBp), acceptTrustedLocation: false } });
      await synchronized("inner synchronized");
      await write(inner, ".exposition/runs/run-2.json", '{"record":{}}');
      await write(inner, ".exposition/artifacts/run-2/output-0.csv", "a,b\n1,2\n");
      await call("workspace_open", { path: outer });
      await synchronized("outer again");
      await call("graph_reconcile", { force: true });
      const outerKeys = (await resources()).map((e) => e.key);
      const nestedMeta = outerKeys.filter((k) => /examples\/inner\/\.exposition\/(runs|artifacts|graph)/.test(k));
      r.expect("3 the outer project does not index the inner project's run records/artifacts", nestedMeta.length === 0, nestedMeta.join(", "));
      r.info("3 inner project files indexed by the outer project", outerKeys.filter((k) => k.startsWith("examples/inner/")).join(", "));
    }
    r.expect("x graph db exists for cased project", existsSync(join(cased, ".exposition", "graph.sqlite3")));
  } finally {
    try {
      await page.screenshot({ path: join(artifactsDir, "final.png") });
    } catch {}
    await app.close();
  }
  await r.finish();
}
