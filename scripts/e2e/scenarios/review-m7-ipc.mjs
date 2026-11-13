// R-m7 independent review: the M7 IPC surface (graph_*, research_*, lean_*
// evidence/association, project_*, workflow_*) as a compromised or buggy
// webview script sees it, against the real app (debug build) with isolated
// data dirs. Prints `ok:` / `DEFECT:` / `info:` lines, writes findings.json,
// and fails if any DEFECT was reproduced.
//
// Runs only when named: `npm run e2e -- review-m7-ipc`.
//
// Sections:
//   A. graph_* malformed/hostile arguments, service-owned evidence forgery,
//      observed relations, saved views with hostile numbers.
//   B. graph_recover with an option the service did not offer, for a
//      project whose `.exposition` is a junction to another folder.
//   C. research_* names, imports, retain/state/experiment forgeries.
//   D. lean_* evidence/association refusals.
//   E. project_* destinations and hostile blueprints.
//   F. workflow_* hostile imports and forged evidence ports.
import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, readdir, readFile, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";

import { invoke, waitFor } from "../workspace-helpers.mjs";

const NAME = "review-m7-ipc";

function recorder(artifactsDir) {
  const results = [];
  const log = (status, id, detail) => {
    results.push({ status, id, detail });
    const tag = status === "DEFECT" ? "DEFECT:" : status === "ok" ? "ok:" : "info:";
    console.log(`${tag} ${id}${detail ? ` — ${detail}` : ""}`);
  };
  return {
    ok: (id, detail = "") => log("ok", id, detail),
    defect: (id, detail = "") => log("DEFECT", id, detail),
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

function call(page, command, args = {}) {
  return page.evaluate(
    ([c, a]) =>
      window.__TAURI_INTERNALS__.invoke(c, a).then(
        (ok) => ({ ok }),
        (error) => ({ error: error ?? "rejected" }),
      ),
    [command, args],
  );
}

const short = (s, n = 180) => (typeof s === "string" ? s : JSON.stringify(s) ?? "undefined").slice(0, n);
const junction = (link, target) => execFileSync("cmd", ["/c", "mklink", "/J", link, target], { stdio: "ignore" });

async function write(root, rel, body) {
  const full = join(root, ...rel.split("/"));
  await mkdir(join(full, ".."), { recursive: true });
  await writeFile(full, body);
}

const PAPER = `= Paper\n\n#lemma[Squares grow.] <lem:sq>\n\nSee @lem:sq.\n`;

export default async function reviewM7Ipc({ launchApp, artifactsDir }) {
  if (!process.argv.slice(2).includes(NAME)) {
    console.log(`skip ${NAME} (run by name)`);
    return;
  }
  const r = recorder(artifactsDir);
  const base = await mkdtemp(join(tmpdir(), "xp-rm7-"));
  const dataDir = join(base, "data");
  await mkdir(dataDir, { recursive: true });
  const root = join(base, "project");
  await write(root, "main.typ", PAPER);
  await write(root, "notes.md", "# Notes\n");
  await write(root, "Proofs/Main.lean", "theorem t : 1 = 1 := rfl\n");
  const outside = join(base, "outside");
  await write(outside, "secret.txt", "outside\n");

  const app = await launchApp({ dataDir, env: { EXPOSITION_CREDENTIAL_NAMESPACE: `review-m7-${Date.now()}` } });
  const { page } = app;
  const refused = (res) => !!res.error && !res.ok;
  const status = async () => (await call(page, "graph_status")).ok;
  const synchronized = (message) =>
    waitFor(
      async () => {
        const s = await status();
        return s?.state.kind === "attached" && s.freshness.state === "synchronized" && s.freshness.pending === 0 ? s : null;
      },
      { timeout: 60_000, interval: 100, message },
    );
  try {
    await invoke(page, "workspace_open", { path: root });
    let s = await synchronized("project graph synchronized");
    const snap = (await call(page, "graph_snapshot", { query: { kinds: null, text: null, around: null, includeInactive: false, limit: 500 } })).ok;
    const resource = snap.entities.find((e) => e.kind === "resource" && e.key === "main.typ");
    const project = snap.entities.find((e) => e.kind === "project");
    r.info("graph", `revision ${s.revision}, ${snap.entities.length} entities`);

    // ---------------------------------------------------------------- A
    for (const [id, bad] of [
      ["traversal id", "../../x"],
      ["empty id", ""],
      ["long id", `ent-${"a".repeat(5000)}`],
      ["sql id", "ent-1' OR '1'='1"],
    ]) {
      const res = await call(page, "graph_entity", { entity: bad });
      r.expect(`A graph_entity refuses ${id}`, refused(res), short(res.error));
    }
    for (const [id, path] of [
      ["traversal", "../outside/secret.txt"],
      ["absolute", "C:\\Windows\\win.ini"],
      ["unc", "\\\\localhost\\C$\\Windows\\win.ini"],
      ["graph storage", ".exposition/graph.sqlite3"],
    ]) {
      const res = await call(page, "graph_inspect_at_source", { request: { path, offset: null, revision: null } });
      const ok = refused(res) || (res.ok && res.ok.entities.length === 0 && !res.ok.resource);
      r.expect(`A graph_inspect_at_source ${id} reveals nothing`, ok, short(res.error ?? res.ok));
    }
    {
      const res = await call(page, "graph_snapshot", { query: { kinds: null, text: "x".repeat(201), around: null, includeInactive: false, limit: 10 } });
      r.expect("A graph_snapshot refuses a 201-char search", refused(res), short(res.error));
      const big = await call(page, "graph_snapshot", { query: { kinds: null, text: null, around: null, includeInactive: true, limit: 4294967295 } });
      r.expect("A graph_snapshot clamps a huge limit", !!big.ok && big.ok.entities.length <= 2000, `${big.ok?.entities.length}`);
      const extra = await call(page, "graph_snapshot", { query: { kinds: null, text: null, around: null, includeInactive: false, limit: 10, sql: "DROP TABLE entities" } });
      r.expect("A graph_snapshot refuses unknown fields", refused(extra), short(extra.error));
    }
    const propose = async (operations, extra = {}) =>
      call(page, "graph_propose_change", { request: { baseRevision: (await status()).revision, operations, summary: null, ...extra } });
    const lemma = { op: "createEntity", tempId: "l", kind: "lemma", roles: [], label: "Review lemma", key: null, attributes: {}, binding: null };
    const cases = [
      ["resource by hand", [{ ...lemma, kind: "resource", roles: ["manuscript"], key: "main.typ" }]],
      ["formal declaration by hand", [{ ...lemma, kind: "formalDeclaration", key: "t" }]],
      ["formalCheck evidence without a record", [lemma, { op: "addRelation", kind: "evidenceFor", from: { kind: "id", id: resource.id }, to: { kind: "temp", tempId: "l" }, evidenceKind: "formalCheck", evidence: [], note: null }]],
      ["computational evidence citing a forged id", [lemma, { op: "addRelation", kind: "evidenceFor", from: { kind: "id", id: resource.id }, to: { kind: "temp", tempId: "l" }, evidenceKind: "computational", evidence: ["evd-00000000000000000000000000000000"], note: null }]],
      ["observed-only relation (imports)", [{ op: "addRelation", kind: "imports", from: { kind: "id", id: resource.id }, to: { kind: "id", id: project.id }, evidenceKind: null, evidence: [], note: null }]],
      ["tombstoning by hand", [{ op: "setLifecycle", entity: resource.id, expectedRevision: resource.revision, lifecycle: "tombstoned" }]],
      ["archiving the project", [{ op: "setLifecycle", entity: project.id, expectedRevision: project.revision, lifecycle: "archived" }]],
      ["renaming a file entity", [{ op: "updateEntity", entity: resource.id, expectedRevision: resource.revision, label: "other.typ", attributes: null }]],
      ["control characters in a label", [{ ...lemma, label: "a\u0007b" }]],
      ["a 100 KB label", [{ ...lemma, label: "x".repeat(100_000) }]],
      ["600 operations", Array.from({ length: 600 }, (_, i) => ({ ...lemma, tempId: `t${i}` }))],
      ["binding outside the project", [{ ...lemma, binding: { path: "../outside/secret.txt", selector: { kind: "wholeFile" }, range: null } }]],
    ];
    for (const [id, ops] of cases) {
      const res = await propose(ops);
      const ok = refused(res) || (res.ok && !res.ok.valid);
      r.expect(`A propose refuses ${id}`, ok, short(res.error ?? res.ok?.diagnostics));
    }
    {
      const res = await propose([lemma], { origin: { kind: "service", service: "lean" } });
      r.expect("A propose refuses a payload-chosen origin", refused(res), short(res.error));
      const unknown = await call(page, "graph_apply_change", { request: { proposal: "prp-00000000000000000000000000000000", expectedRevision: 0 } });
      r.expect("A apply refuses an unknown proposal", refused(unknown), short(unknown.error));
    }
    // Views: presentation only; hostile numbers refused; saving never changes the graph.
    {
      const before = (await status()).revision;
      const view = (over = {}) => ({
        id: null,
        name: "Review view",
        scope: { kind: "project" },
        nodes: [{ entity: resource.id, x: 10, y: 20, width: 200, height: 80 }],
        viewport: { x: 0, y: 0, zoom: 1 },
        filters: { kinds: [], relationKinds: [], text: null, showInactive: false, depth: null },
        hidden: [],
        collapsed: [],
        expectedRevision: null,
        ...over,
      });
      const saved = await call(page, "graph_view_save", { view: view() });
      r.expect("A a valid view saves", !!saved.ok, short(saved.error));
      for (const [id, v] of [
        ["zoom 0", view({ viewport: { x: 0, y: 0, zoom: 0 } })],
        ["zoom 1e9", view({ viewport: { x: 0, y: 0, zoom: 1e9 } })],
        ["x 1e300", view({ nodes: [{ entity: resource.id, x: 1e300, y: 0, width: null, height: null }] })],
        ["negative width", view({ nodes: [{ entity: resource.id, x: 0, y: 0, width: -5, height: 10 }] })],
        ["NaN (serialized null)", view({ viewport: { x: NaN, y: 0, zoom: 1 } })],
        ["5001 nodes", view({ nodes: Array.from({ length: 5001 }, () => ({ entity: resource.id, x: 0, y: 0, width: null, height: null })) })],
        ["control char name", view({ name: "v\u0000" })],
        ["stale revision", view({ id: saved.ok?.id ?? null, expectedRevision: 99 })],
      ]) {
        const res = await call(page, "graph_view_save", { view: v });
        r.expect(`A view refuses ${id}`, refused(res), short(res.error ?? res.ok));
      }
      const after = (await status()).revision;
      r.expect("A saving views never changes the graph revision", after === before, `${before} → ${after}`);
    }
    {
      const res = await call(page, "graph_recover", { option: { kind: "startNew" } });
      r.expect("A graph_recover refuses when nothing needs recovery", refused(res), short(res.error));
    }
    {
      // The graph's own storage is never listed in the explorer (workspace-files
      // asserts this; it failed intermittently in the full run).
      const backup = await call(page, "graph_backup");
      r.info("A manual backup", short(backup.ok?.name ?? backup.error));
      const listed = (await call(page, "workspace_list_dir", { path: ".exposition" })).ok ?? [];
      const names = listed.map((e) => e.name);
      r.expect(
        "A the explorer does not list the graph's storage (.exposition/graph-backups)",
        !names.some((n) => n.startsWith("graph.sqlite3") || n === "graph-backups" || n.startsWith("graph.damaged")),
        names.join(", "),
      );
    }

    // ---------------------------------------------------------------- C
    const enabled = await call(page, "research_space_enable");
    r.expect("C research enable", !!enabled.ok, short(enabled.error));
    const again = await call(page, "research_space_enable");
    r.expect("C enabling twice keeps one space", again.ok?.spaceId === enabled.ok?.spaceId, `${enabled.ok?.spaceId} / ${again.ok?.spaceId}`);
    const sessions = await call(page, "research_sessions");
    r.expect("C enabling Research launched no session", Array.isArray(sessions.ok) && sessions.ok.length === 0, short(sessions.ok ?? sessions.error));
    for (const name of ["../x.py", "a/b.py", "a\\b.py", "CON.py", ".hidden.py", "x.exe", "x.py:ads", "x.", "x.unknownext", "", "y".repeat(200) + ".py"]) {
      const res = await call(page, "research_space_create_file", { request: { group: "sources", name } });
      r.expect(`C create_file refuses ${JSON.stringify(name).slice(0, 40)}`, refused(res), short(res.error));
    }
    const graphDb = join(root, ".exposition", "graph.sqlite3");
    for (const [id, source] of [
      ["relative", "notes.md"],
      ["missing", join(base, "missing.csv")],
      ["a folder", outside],
      ["project metadata", graphDb],
      ["traversal spelling", join(root, "..", "project", ".exposition", "graph.sqlite3")],
    ]) {
      const res = await call(page, "research_space_import", { request: { group: "data", source, mode: "copy" } });
      r.expect(`C import refuses ${id}`, refused(res), short(res.error));
    }
    for (const [id, args] of [
      ["retain forged ids", ["research_retain", { request: { runId: "../../x", artifactId: "a", state: "active", experiment: null } }]],
      ["retain unknown run", ["research_retain", { request: { runId: "run-000000000000", artifactId: "a", state: "active", experiment: null } }]],
      ["state of a manuscript", ["research_result_state", { request: { entity: resource.id, state: "discarded" } }]],
      ["experiment with an outside procedure", ["research_experiment_save", { input: { entity: null, expectedRevision: null, title: "E", objective: "o", procedure: "../outside/secret.txt", inputs: [], outcomes: [] } }]],
      ["experiment outcome = project", ["research_experiment_save", { input: { entity: null, expectedRevision: null, title: "E", objective: "o", procedure: null, inputs: [], outcomes: [project.id] } }]],
      ["approve unknown promotion", ["research_promotion_approve", { promotion: "../../x" }]],
    ]) {
      const res = await call(page, args[0], args[1]);
      r.expect(`C ${id} refused`, refused(res), short(res.error ?? res.ok));
    }
    {
      const res = await call(page, "research_experiment_save", {
        input: { entity: null, expectedRevision: null, title: "E", objective: "o", procedure: ".exposition/runs/run-1.json", inputs: [".exposition/graph.sqlite3"], outcomes: [] },
      });
      r.expect("C experiment refuses service-owned/graph-storage inputs", refused(res), short(res.error ?? res.ok));
    }

    // ---------------------------------------------------------------- D
    {
      const res = await call(page, "lean_record_evidence", { document: "doc-999999", declarations: ["t"] });
      r.expect("D lean_record_evidence refuses an unknown document", refused(res), short(res.error));
      const many = await call(page, "lean_record_evidence", { document: "doc-1", declarations: Array.from({ length: 201 }, (_, i) => `d${i}`) });
      r.expect("D lean_record_evidence refuses 201 names", refused(many), short(many.error));
      const lemmaRes = await propose([lemma]);
      const applied = await call(page, "graph_apply_change", { request: { proposal: lemmaRes.ok.id, expectedRevision: (await status()).revision } });
      const lemmaId = applied.ok?.created?.[0]?.id;
      const notDecl = await call(page, "lean_associate_declaration", { request: { declaration: { kind: "entity", entity: lemmaId }, target: lemmaId, note: null } });
      r.expect("D association needs a formal declaration", refused(notDecl), short(notDecl.error));
      await synchronized("Lean file indexed");
      const decls = (await call(page, "graph_snapshot", { query: { kinds: ["formalDeclaration"], text: null, around: null, includeInactive: false, limit: 10 } })).ok.entities;
      if (decls.length) {
        const toResource = await call(page, "lean_associate_declaration", { request: { declaration: { kind: "entity", entity: decls[0].id }, target: resource.id, note: null } });
        r.expect("D a declaration cannot formalize a file", refused(toResource), short(toResource.error));
        const longNote = await call(page, "lean_associate_declaration", { request: { declaration: { kind: "entity", entity: decls[0].id }, target: lemmaId, note: "n".repeat(600) } });
        r.expect("D a 600-char note is refused", refused(longNote), short(longNote.error));
        const md = await call(page, "graph_math_overview");
        const row = md.ok?.rows.find((x) => x.entity.id === decls[0].id);
        r.expect("D an unaudited declaration has no verdict", row && row.verdict === null, short(row?.verdict));
      } else {
        r.info("D no formal declaration indexed", "skipped association checks");
      }
    }

    // ---------------------------------------------------------------- E
    {
      const bpRes = await call(page, "project_preset_blueprint", { request: { preset: "paper", title: "Review", format: "typst", runtime: null, leanToolchain: null, includeWorkflow: false, purpose: null } });
      const firstPreset = bpRes.ok;
      r.expect("E the paper preset blueprint is available", !!firstPreset, short(bpRes.error));
      const dests = [
        ["empty", ""],
        ["relative", "relative\\proj"],
        ["drive root", "C:\\"],
        ["home", homedir()],
        ["data folder (new)", join(dataDir, "proj")],
        ["existing project", root],
        ["non-empty", outside],
        ["trailing-dot name", join(base, "proj.")],
        ["device name", join(base, "CON")],
        ["verbatim data folder", `\\\\?\\${join(dataDir, "proj2")}`],
        ["admin share of data folder", `\\\\localhost\\${dataDir[0]}$${join(dataDir, "proj3").slice(2)}`],
      ];
      for (const [id, destination] of dests) {
        const res = await call(page, "project_check_destination", { destination });
        r.expect(`E destination refused: ${id}`, !!res.ok && res.ok.ok === false, short(res.ok?.message ?? res.error));
      }
      // A junction to the data folder as the parent of a new project.
      const j = join(base, "jdata");
      junction(j, dataDir);
      const viaJ = await call(page, "project_check_destination", { destination: join(j, "proj4") });
      r.expect("E destination refused: new folder under a junction to the data folder", !!viaJ.ok && viaJ.ok.ok === false, short(viaJ.ok?.message ?? viaJ.error));
      const hostile = [
        ["trust field", { ...firstPreset, trust: true }],
        ["hook file", { ...firstPreset, files: [...firstPreset.files, { path: "sub/.latexmkrc", content: "system('calc')" }] }],
        ["executable", { ...firstPreset, files: [...firstPreset.files, { path: "tools/run.ps1", content: "calc" }] }],
        ["traversal", { ...firstPreset, files: [...firstPreset.files, { path: "../escape.txt", content: "x" }] }],
        ["git hook", { ...firstPreset, files: [...firstPreset.files, { path: ".git/hooks/post-checkout", content: "x" }] }],
        ["case-variant metadata", { ...firstPreset, files: [...firstPreset.files, { path: ".EXPOSITION/runs/run-1.json", content: "{}" }] }],
        ["case-variant graph", { ...firstPreset, files: [...firstPreset.files, { path: ".Exposition/graph.sqlite3", content: "x" }] }],
        ["duplicate path by case", { ...firstPreset, files: [...firstPreset.files, { path: "Notes.md", content: "a" }, { path: "notes.md", content: "b" }] }],
        ["oversized", { ...firstPreset, files: [...firstPreset.files, { path: "big.txt", content: "x".repeat(2 * 1024 * 1024) }] }],
      ];
      for (const [id, blueprint] of hostile) {
        const destination = join(base, `bp-${id.replace(/\W+/g, "-")}`);
        const res = await call(page, "project_create", { request: { destination, blueprint: JSON.stringify(blueprint), acceptTrustedLocation: false } });
        const nothing = !existsSync(destination);
        r.expect(`E blueprint refused (${id}); nothing written`, refused(res) && nothing, `${short(res.error)} written=${!nothing}`);
      }
      const resolve = await call(page, "project_creation_resolve", { operation: "op-00000000000000000000000000000000", action: "finish" });
      r.expect("E project_creation_resolve refuses an unknown operation", refused(resolve), short(resolve.error));
    }

    // ---------------------------------------------------------------- F
    {
      await invoke(page, "workspace_open", { path: root });
      await synchronized("back in the review project");
      const presets = (await call(page, "workflow_presets")).ok ?? [];
      r.info("F workflow presets", presets.map((p) => p.id).join(", "));
      const draft = (await call(page, "workflow_from_preset", { request: { preset: presets[0]?.id ?? "numerical-experiment", script: null, data: null, manuscript: null, leanFile: null } })).ok;
      for (const [id, text] of [
        ["run flag", JSON.stringify({ ...draft, run: true })],
        ["trusted flag", JSON.stringify({ ...draft, trusted: true })],
        ["approval", JSON.stringify({ ...draft, approvals: [{ tool: "run_command" }] })],
        ["20 MB", "x".repeat(20 * 1024 * 1024)],
        ["not JSON", "{"],
      ]) {
        const res = await call(page, "workflow_import", { text });
        r.expect(`F workflow_import refuses ${id}`, refused(res), short(res.error ?? res.ok?.validation));
      }
      {
        // An unregistered operation: a draft may be incomplete, but it must
        // carry an error diagnostic and never be runnable.
        const res = await call(page, "workflow_import", { text: JSON.stringify({ ...draft, steps: [{ ...(draft?.steps?.[0] ?? {}), operation: { id: "shell.exec", version: 1 } }] }) });
        const v = res.ok?.validation;
        const ok = refused(res) || (v && v.runnable === false && v.diagnostics.some((d) => d.severity === "error"));
        r.expect("F an imported draft naming an unregistered operation is refused or saved with an error, never runnable", ok, short(res.error ?? v?.diagnostics?.filter((d) => d.severity === "error").map((d) => d.code)));
      }
      const imported = await call(page, "workflow_import", { text: JSON.stringify(draft) });
      r.expect("F a valid import is saved and nothing runs on import (M8: runnable only reports readiness)", !!imported.ok && ((await call(page, "workflow_runs", { workflow: null })).ok ?? []).length === 0, short(imported.error ?? imported.ok?.validation.runnableReason));
      const list = (await call(page, "workflow_list")).ok ?? [];
      r.expect("F imported drafts are listed as imported, and none was run", list.some((w) => w.imported) && ((await call(page, "workflow_runs", { workflow: null })).ok ?? []).length === 0, short(list.map((w) => [w.title, w.runnable, w.imported])));
      const ops = (await call(page, "workflow_operations")).ok ?? [];
      r.info("F operations", ops.map((o) => o.id ?? o.operation).join(", "));
    }

    // ---------------------------------------------------------------- B
    {
      // Another folder holding a real graph database, and a project whose
      // `.exposition` is a junction to it.
      const other = join(base, "other-project");
      await write(other, "main.typ", "= Other\n");
      await invoke(page, "workspace_open", { path: other });
      await waitFor(async () => (await status())?.state.kind === "attached", { timeout: 30_000, message: "other graph" });
      const otherDb = join(other, ".exposition", "graph.sqlite3");
      const linked = join(base, "linked");
      await write(linked, "main.typ", "= Linked\n");
      junction(join(linked, ".exposition"), join(other, ".exposition"));
      await invoke(page, "workspace_open", { path: linked });
      const st = await waitFor(async () => {
        const x = await status();
        return x && x.state.kind !== "detached" ? x : null;
      }, { timeout: 30_000, message: "linked status" });
      r.info("B linked project graph state", short(st.state));
      const offered = st.state.recovery?.map((o) => o.kind) ?? [];
      const before = await readdir(join(other, ".exposition"));
      const res = await call(page, "graph_recover", { option: { kind: "startNew" } });
      const after = await readdir(join(other, ".exposition"));
      const moved = after.filter((n) => n.startsWith("graph.damaged-"));
      r.expect(
        `B graph_recover(startNew) not offered (${offered.join(",")}) must not touch another folder through the link`,
        moved.length === 0 && existsSync(otherDb),
        `${short(res.error ?? res.ok?.state)}; other folder before=${before.join(",")} after=${after.join(",")}`,
      );
    }
  } finally {
    try {
      await page.screenshot({ path: join(artifactsDir, "final.png") });
    } catch {}
    await app.close();
  }
  await r.finish();
}
