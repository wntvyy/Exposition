// The project graph in the real app with no graph UI open (M7 acceptance 1,
// 2, 3, 9 and the hostile-database part of 7): opening a manuscript project
// and a research-only project indexes meaningful resources with stable ids
// (caches, logs, runs, artifacts, planning files are not entities); curated
// relations and collections survive reopening; edits through Monaco and the
// exact-page editor are overlaid as unsaved and followed by undo/save;
// external rename/move/delete, a branch-switch-like mass change, and a forced
// reindex keep identities or mark them explicitly; references are not
// dependencies; impact explains itself; coverage is visible; a hostile graph
// database is refused and restorable from a consistent backup. Latencies are
// measured on a generated 3,000-file project and written to timings.json.
import { execFileSync } from "node:child_process";
import { DatabaseSync } from "node:sqlite";
import { cp, mkdir, mkdtemp, readFile, rename, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";

import { focusEditor, invoke, openFromTree, openInEditor, waitFor } from "../workspace-helpers.mjs";
import { waitForAsync } from "../wait.mjs";

const THEORY = String.raw`\documentclass{article}
\usepackage{amsthm}
\newtheorem{theorem}{Theorem}
\newtheorem{lemma}{Lemma}
\newcommand{\keylabel}{\label{lem:hidden}}
\begin{document}
\section{Results}
\begin{lemma}\label{lem:key}
Every triangular number is a sum of consecutive integers.
\end{lemma}
\begin{proof}
Immediate.
\end{proof}
\begin{theorem}[Closed form]\label{thm:main}
By Lemma~\ref{lem:key}, $\sum_{k=1}^n k = n(n+1)/2$, see \cite{gauss1801}.
\end{theorem}
\input{chapters/discussion}
\end{document}
`;
const DISCUSSION = String.raw`\section{Discussion}
\begin{corollary}\label{cor:growth}
Triangular numbers grow quadratically, as Theorem~\ref{thm:main} shows.
\end{corollary}
`;
const BIB = "@book{gauss1801, title = {Disquisitiones Arithmeticae}, author = {Gauss}, year = 1801}\n";

async function write(root, rel, body) {
  await mkdir(dirname(join(root, rel)), { recursive: true });
  await writeFile(join(root, rel), body);
}

/** A generated project of about `n` files (same shape as the Rust perf test). */
async function generate(root, n) {
  const tex = Math.floor((n * 12) / 100);
  let main = "\\documentclass{article}\n\\begin{document}\n";
  for (let i = 0; i < tex; i++) main += `\\input{chapters/c${i % 20}/ch${i}}\n`;
  await write(root, "main.tex", main + "\\end{document}\n");
  const jobs = [];
  for (let i = 0; i < tex; i++) {
    const prev = i > 0 ? `Lemma~\\ref{lem:${i - 1}}` : "nothing";
    jobs.push(
      write(
        root,
        `chapters/c${i % 20}/ch${i}.tex`,
        `\\section{Chapter ${i}}\n\\begin{lemma}\\label{lem:${i}}\nStatement ${i} uses ${prev}.\n\\end{lemma}\n\\begin{theorem}\\label{thm:${i}}\nSee Lemma~\\ref{lem:${i}}.\n\\end{theorem}\n`,
      ),
    );
  }
  const md = Math.floor((n * 25) / 100);
  for (let i = 0; i < md; i++) jobs.push(write(root, `notes/d${i % 30}/m${i}.md`, `# Note ${i}\nSee [next](m${i + 1}.md).\n`));
  const py = n - tex - md;
  for (let i = 0; i < py; i++) jobs.push(write(root, `src/pkg${i % 40}/m${i}.py`, `import numpy\nX = ${i}\n`));
  for (let i = 0; i < 200; i++) jobs.push(write(root, `node_modules/p/i${i}.js`, "x"));
  await Promise.all(jobs);
}

function workingSetMb(pid) {
  try {
    const out = execFileSync(
      "powershell",
      ["-NoProfile", "-Command", `(Get-Process -Id ${pid}).WorkingSet64`],
      { encoding: "utf8" },
    );
    return Math.round(Number(out.trim()) / 1048576);
  } catch {
    return null;
  }
}

const median = (xs) => {
  const s = [...xs].sort((a, b) => a - b);
  return s.length ? s[Math.floor(s.length / 2)] : null;
};
const p95 = (xs) => {
  const s = [...xs].sort((a, b) => a - b);
  return s.length ? s[Math.min(s.length - 1, Math.floor(s.length * 0.95))] : null;
};

export default async function graphCore({ launchApp, artifactsDir, assert, fixture }) {
  const base = await mkdtemp(join(tmpdir(), "xp-graph-"));
  const dataDir = join(base, "data");
  await mkdir(dataDir, { recursive: true });
  const paper = join(base, "paper");
  await cp(fixture("m6-paper"), paper, { recursive: true });
  await write(paper, "theory.tex", THEORY);
  await write(paper, "chapters/discussion.tex", DISCUSSION);
  await write(paper, "refs.bib", BIB);
  await write(paper, "notes.typ", "= Notes <sec:notes>\n#lemma[A Typst lemma.] <lem:typst>\nSee @lem:typst and @gauss1801.\n");
  for (const [rel, body] of [
    [".git/HEAD", "ref: refs/heads/main\n"],
    ["node_modules/pkg/index.js", "x"],
    ["theory.aux", "aux"],
    ["theory.log", "log"],
    [".exposition/runs/run-1.json", "{}"],
    [".exposition/artifacts/run-1/output-0.png", "png"],
    ["CLAUDE.md", "planning notes"],
    ["analysis/__pycache__/fit.cpython-314.pyc", "pyc"],
  ]) {
    await write(paper, rel, body);
  }
  const research = join(base, "research");
  await write(research, ".exposition/research/rs-1/sources/fit.py", "from helpers import smooth\nimport numpy\nprint(smooth([1, 2]))\n");
  await write(research, ".exposition/research/rs-1/sources/helpers.py", "def smooth(xs):\n    return xs\n");
  await write(research, ".exposition/research/rs-1/data/obs.csv", "x,y\n1,2\n");
  await write(research, "notes.md", "# Findings\n\nSee ![plot](figures/plot.png).\n");
  await write(research, "figures/plot.png", "png");
  await write(research, ".venv/lib/site.py", "x");
  const big = join(base, "big");
  await generate(big, 3000);

  const timings = {};
  const app = await launchApp({ dataDir, env: { EXPOSITION_GRAPH_TRACE: "1" } });
  const { page } = app;
  const shot = (name) => page.screenshot({ path: join(artifactsDir, `${name}.png`) });
  const call = (cmd, args = {}) => invoke(page, cmd, args);
  const status = () => call("graph_status");
  const synchronized = async (message, timeout = 60_000) =>
    waitFor(
      async () => {
        const s = await status();
        return s.state.kind === "attached" && s.freshness.state === "synchronized" && s.freshness.pending === 0 ? s : null;
      },
      { timeout, interval: 50, message },
    );
  const entities = async (kinds, includeInactive = false) =>
    (await call("graph_snapshot", { query: { kinds, includeInactive, limit: 2000 } })).entities;
  const byKey = async (kind, key, includeInactive = false) =>
    (await entities([kind], includeInactive)).find((e) => e.key === key);
  const detail = (id) => call("graph_entity", { entity: id });
  const change = async (operations) => {
    const s = await status();
    const proposal = await call("graph_propose_change", { request: { baseRevision: s.revision, operations } });
    assert.ok(proposal.valid, JSON.stringify(proposal.diagnostics));
    const outcome = await call("graph_apply_change", {
      request: { proposal: proposal.id, expectedRevision: s.revision },
    });
    assert.equal(outcome.kind, "applied", JSON.stringify(outcome));
    return outcome;
  };
  const rel = (kind, from, to) => ({ op: "addRelation", kind, from: { kind: "id", id: from }, to: { kind: "id", id: to }, evidence: [] });
  const applyEdit = async (docId, offset, text) => {
    const state = await call("document_state", { document: docId });
    const started = performance.now();
    const outcome = await call("document_apply", {
      request: {
        id: `t-graph-${Date.now()}-${Math.random()}`,
        origin: { kind: "source" },
        edits: [{ document: docId, baseRevision: state.info.revision, patches: [{ range: { start: offset, end: offset }, text }] }],
        undo: { kind: "newGroup" },
        conflict: "reject",
        label: null,
        selection: null,
      },
    });
    return { ms: performance.now() - started, revision: outcome.applied[0].revision };
  };

  try {
    // ---- 1. A manuscript project, opened with no graph UI -------------------
    let started = Date.now();
    await call("workspace_open", { path: paper });
    let s = await synchronized("manuscript project indexed");
    timings.manuscriptOpenToSynchronizedMs = Date.now() - started;
    const resources = await entities(["resource"]);
    const keys = resources.map((r) => r.key);
    for (const expected of ["main.typ", "theory.tex", "chapters/discussion.tex", "refs.bib", "Paper/Gauss.lean", "analysis/fit.py", "notes.typ"]) {
      assert.ok(keys.includes(expected), `${expected} is a resource: ${keys.join(", ")}`);
    }
    for (const excluded of ["theory.aux", "theory.log", ".git/HEAD", "node_modules/pkg/index.js", ".exposition/runs/run-1.json", ".exposition/artifacts/run-1/output-0.png", "CLAUDE.md"]) {
      assert.ok(!keys.includes(excluded), `${excluded} must not be an entity`);
    }
    assert.ok(!keys.some((k) => k.includes("__pycache__")), "caches are not entities");
    const lemma = await byKey("lemma", "lem:key");
    const theorem = await byKey("theorem", "thm:main");
    const corollary = await byKey("corollary", "cor:growth");
    const proof = (await entities(["proof"]))[0];
    const gauss = await byKey("reference", "gauss1801");
    const formal = await byKey("formalDeclaration", "Paper.two_mul_sumTo");
    assert.ok(lemma && theorem && corollary && proof && gauss && formal, "declarations extracted");
    assert.equal(formal.attributes["lean.module"], "Paper.Gauss");
    const theoryResource = resources.find((r) => r.key === "theory.tex");

    // References are observations, never dependencies; the proof convention.
    const thmDetail = await detail(theorem.id);
    const refToLemma = thmDetail.outgoing.find((r) => r.to === lemma.id);
    assert.equal(refToLemma?.kind, "references", "\\ref is a reference");
    assert.equal(refToLemma.status, "observed");
    assert.ok(!thmDetail.outgoing.some((r) => r.kind === "dependsOn"), "no dependency is inferred from a reference");
    assert.ok(thmDetail.outgoing.some((r) => r.kind === "references" && r.to === gauss.id), "\\cite → bibliography entry");
    const proofDetail = await detail(proof.id);
    assert.ok(proofDetail.outgoing.some((r) => r.kind === "proofOf" && r.to === lemma.id), "proof following the lemma");
    const corDetail = await detail(corollary.id);
    assert.ok(corDetail.outgoing.some((r) => r.kind === "references" && r.to === theorem.id), "cross-file \\ref");
    const theoryDetail = await detail(theoryResource.id);
    assert.ok(
      theoryDetail.outgoing.some((r) => r.kind === "imports"),
      "\\input → imports",
    );
    // Coverage is visible: the macro hiding a \label makes theory.tex partial.
    assert.equal(theoryDetail.coverage.state, "partial", JSON.stringify(theoryDetail.coverage));
    assert.match(theoryDetail.coverage.detail, /keylabel|macro/i);
    assert.ok(s.coverage.partial >= 1 && s.coverage.resources >= 7, JSON.stringify(s.coverage));

    // Curated records: a collection and an accepted dependency.
    const created = await change([
      { op: "createEntity", tempId: "c", kind: "collection", roles: [], label: "Core results", attributes: {} },
    ]);
    const collection = created.created[0].id;
    await change([rel("memberOf", theorem.id, collection), rel("memberOf", lemma.id, collection), rel("dependsOn", theorem.id, lemma.id)]);
    const impact = await call("graph_impact", { request: { entities: [lemma.id] } });
    const thmItem = impact.items.find((i) => i.entity.id === theorem.id);
    assert.equal(thmItem?.action, "needsReview", JSON.stringify(impact.items.map((i) => [i.entity.label, i.action, i.reason])));
    assert.match(thmItem.reason, /depends on/);
    const corItem = impact.items.find((i) => i.entity.id === corollary.id);
    assert.equal(corItem?.action, "citationReview", "a citing entity needs a citation check, not a proof review");
    assert.ok(impact.unknownCoverage.includes("theory.tex"), "impact through a partially indexed file is unknown");
    timings.impactItems = impact.items.length;
    await shot("01-indexed");

    // ---- 2. Monaco: typing is overlaid as unsaved; undo drops it; save reconciles
    const theoryDoc = await openInEditor(page, "theory.tex");
    const theoryText = (await call("document_state", { document: theoryDoc })).text;
    const keyLatencies = [];
    await focusEditor(page, "theory.tex", "end");
    for (const ch of "% typed") {
      const before = (await call("document_state", { document: theoryDoc })).info.revision;
      const t0 = performance.now();
      await page.keyboard.type(ch);
      await waitForAsync(
        page,
        async ([doc, rev]) =>
          (await window.__TAURI_INTERNALS__.invoke("document_state", { document: doc })).info.revision > rev,
        [theoryDoc, before],
        { timeout: 5000 },
      );
      keyLatencies.push(performance.now() - t0);
    }
    timings.monacoKeystrokeToAppliedMs = { median: median(keyLatencies), p95: p95(keyLatencies), samples: keyLatencies.length };
    // A new statement inserted through the document service (the Monaco path).
    const insertAt = theoryText.indexOf("\\input{chapters/discussion}");
    const edit = await applyEdit(
      theoryDoc,
      insertAt,
      "\\begin{lemma}\\label{lem:unsaved}\nA new lemma, cf.~\\ref{thm:main}.\n\\end{lemma}\n",
    );
    const overlay = await waitFor(
      async () => {
        const i = await call("graph_inspect_at_source", {
          request: { path: "theory.tex", offset: insertAt + 30, revision: edit.revision },
        });
        return i.unsaved ? i : null;
      },
      { timeout: 10_000, message: "unsaved overlay" },
    );
    const provisional = overlay.entities.find((e) => e.entity.key === "lem:unsaved");
    assert.ok(provisional, "the unsaved lemma is visible in inspection");
    assert.equal(provisional.entity.attributes["overlay.provisional"], "true");
    assert.equal(provisional.binding.version.saved, false, "labelled unsaved, never saved state");
    assert.ok(provisional.relations.some((r) => r.kind === "references" && r.to === theorem.id));
    assert.equal(await byKey("lemma", "lem:unsaved"), undefined, "unsaved text is not written to the graph");
    const thmSaved = await call("graph_inspect_at_source", { request: { path: "theory.tex", offset: theoryText.indexOf("By Lemma"), revision: null } });
    assert.equal(thmSaved.unsaved, false);
    assert.ok(thmSaved.entities.some((e) => e.entity.id === theorem.id && e.binding.state === "resolved"));
    // Undo both edits through the editor: back at the saved bytes → no overlay.
    await focusEditor(page, "theory.tex", "end");
    for (let i = 0; i < 12; i++) {
      const st = await call("document_state", { document: theoryDoc });
      if (st.text === theoryText) break;
      await page.keyboard.press("Control+Z");
      await page.waitForTimeout(60);
    }
    assert.equal((await call("document_state", { document: theoryDoc })).text, theoryText, "undo restored the text");
    await waitFor(async () => (await status()).freshness.overlay.length === 0, { timeout: 10_000, message: "overlay dropped after undo" });
    // Redo the inserted lemma via the service and save: it becomes an entity.
    // (before the theorem, so the theorem's binding must follow its moved text)
    const again = await applyEdit(
      theoryDoc,
      theoryText.indexOf("\\begin{theorem}"),
      "\\begin{lemma}\\label{lem:saved}\nSaved lemma.\n\\end{lemma}\n",
    );
    assert.ok(again.revision > 0);
    started = Date.now();
    await call("document_save", { document: theoryDoc, overwrite: false });
    const savedLemma = await waitFor(() => byKey("lemma", "lem:saved"), { timeout: 15_000, message: "saved lemma reconciled" });
    timings.saveToReconciledMs = Date.now() - started;
    assert.ok(savedLemma);
    assert.equal((await byKey("theorem", "thm:main")).id, theorem.id, "the theorem kept its identity through edits");
    const shifted = (await detail(theorem.id)).bindings[0];
    assert.equal(shifted.state, "resolved");
    assert.ok(shifted.range.start > theoryText.indexOf("\\begin{theorem}"), "binding follows the moved text");
    await shot("02-monaco");

    // ---- 3. Exact-page editor: type in main.typ, undo ---------------------------
    await openFromTree(page, "main.typ");
    const modeButton = (label) => page.locator(".ui-segmented__item", { hasText: label }).first();
    await modeButton("Page").waitFor({ timeout: 10_000 });
    await modeButton("Page").click();
    await page.locator('[data-testid="page-editor"]').waitFor({ timeout: 15_000 });
    const mainDoc = (await call("document_list")).find((d) => d.path === "main.typ").id;
    const mainText = (await call("document_state", { document: mainDoc })).text;
    await waitForAsync(
      page,
      () => {
        const el = document.querySelector('[data-testid="page-editor"]');
        return el?.dataset.stale === "false" && !!el.dataset.build && !!document.querySelector('[data-testid="page"] img')?.complete;
      },
      null,
      { timeout: 30_000 },
    );
    const editor = page.locator('[data-testid="page-editor"]');
    const at = mainText.indexOf("dominates") + 3;
    const g = await call("page_caret", { build: await editor.getAttribute("data-build"), path: "main.typ", offset: at, affinity: "downstream" });
    const pageEl = page.locator(`[data-testid="page"][data-page="${g.page}"]`);
    await pageEl.evaluate((el, top) => {
      const scroller = el.closest('[data-testid="page-scroller"]');
      const scale = el.getBoundingClientRect().width / Number(el.dataset.widthPt);
      const view = scroller.getBoundingClientRect();
      scroller.scrollTop += el.getBoundingClientRect().top + top * scale - (view.top + view.height / 2);
    }, g.top);
    const box = await pageEl.boundingBox();
    const scale = box.width / Number(await pageEl.getAttribute("data-width-pt"));
    await page.mouse.click(box.x + (g.x + 0.6) * scale, box.y + ((g.top + g.bottom) / 2) * scale);
    await page.waitForFunction(
      (o) => document.querySelector('[data-testid="page-caret"]')?.getAttribute("data-offset") === String(o),
      at,
      { timeout: 10_000 },
    );
    await page.keyboard.type("XY");
    const typed = await waitFor(async () => {
      const st = await call("document_state", { document: mainDoc });
      return st.text.includes("domXYinates") ? st : null;
    }, { message: "page edit applied" });
    await waitFor(
      async () => (await status()).freshness.overlay.some((o) => o.path === "main.typ" && o.revision === typed.info.revision),
      { timeout: 10_000, message: "page edit overlaid" },
    );
    const mainResource = await byKey("resource", "main.typ");
    const mainInspect = await call("graph_inspect_at_source", { request: { path: "main.typ", offset: null, revision: typed.info.revision } });
    assert.equal(mainInspect.unsaved, true);
    assert.equal(mainInspect.resource.id, mainResource.id);
    await page.locator('[data-testid="page-input"]').focus();
    await page.keyboard.press("Control+Z");
    await waitFor(async () => (await call("document_state", { document: mainDoc })).text === mainText, { message: "page undo" });
    await waitFor(async () => !(await status()).freshness.overlay.some((o) => o.path === "main.typ"), {
      timeout: 10_000,
      message: "page overlay dropped after undo",
    });
    await shot("03-page");

    // ---- 4. External rename, move, delete -----------------------------------------
    const discussion = await byKey("resource", "chapters/discussion.tex");
    await rename(join(paper, "chapters"), join(paper, "parts"));
    const moved = await waitFor(() => byKey("resource", "parts/discussion.tex"), { timeout: 15_000, message: "folder rename" });
    assert.equal(moved.id, discussion.id, "a renamed folder's file keeps its identity");
    assert.equal((await byKey("corollary", "cor:growth")).id, corollary.id, "declarations keep identity across the move");
    await synchronized("after the folder rename");
    // theory.tex's \input{chapters/discussion} now points nowhere: explicitly unresolved.
    const theoryAfter = await detail(theoryResource.id);
    assert.ok(theoryAfter.coverage.unresolved >= 1, JSON.stringify(theoryAfter.coverage));
    assert.match(theoryAfter.coverage.detail, /chapters\/discussion/);
    const fit = await byKey("resource", "analysis/fit.py");
    await rename(join(paper, "analysis", "fit.py"), join(paper, "analysis", "fit_model.py"));
    const renamedFit = await waitFor(() => byKey("resource", "analysis/fit_model.py"), { timeout: 15_000, message: "file rename" });
    assert.equal(renamedFit.id, fit.id, "rename = same id");
    await rm(join(paper, "refs.bib"));
    await waitFor(async () => (await detail(gauss.id)).entity.lifecycle === "tombstoned", { timeout: 15_000, message: "deleted bibliography" });
    await synchronized("after the delete");
    const thmAfterDelete = await detail(theorem.id);
    assert.ok(!thmAfterDelete.outgoing.some((r) => r.kind === "references" && r.to === gauss.id), "observed citation of a deleted entry removed");
    assert.ok(thmAfterDelete.outgoing.some((r) => r.kind === "dependsOn" && r.to === lemma.id), "curated dependency kept");

    // ---- 5. Branch-switch-like mass change, then a forced resync ------------------
    const massStarted = Date.now();
    await Promise.all(
      Array.from({ length: 150 }, (_, i) => write(paper, `generated/g${i}.md`, `# Generated ${i}\n`)),
    );
    await write(paper, "theory.tex", THEORY.replace("Immediate.", "Immediate, on this branch."));
    await waitFor(async () => (await entities(["resource"])).filter((r) => r.key.startsWith("generated/")).length === 150, {
      timeout: 30_000,
      message: "mass change reconciled",
    });
    await synchronized("after the mass change");
    timings.massChange150FilesMs = Date.now() - massStarted;
    assert.equal((await byKey("lemma", "lem:key")).id, lemma.id);
    const reindex = await call("graph_reconcile", { force: true });
    assert.ok(reindex.read >= 150, JSON.stringify(reindex));
    timings.forcedReindexPaperMs = reindex.durationMs;
    assert.equal((await byKey("theorem", "thm:main")).id, theorem.id, "forced reindex keeps identities");
    const members = (await detail(collection)).incoming.filter((r) => r.kind === "memberOf");
    assert.equal(members.length, 2, "forced reindex never touches curated records");

    // ---- 6. Reopen: curated relations and collections preserved --------------------
    await call("workspace_close");
    started = Date.now();
    await call("workspace_open", { path: paper });
    await synchronized("reopened");
    timings.reopenToSynchronizedMs = Date.now() - started;
    const reopened = await detail(collection);
    assert.equal(reopened.incoming.filter((r) => r.kind === "memberOf").length, 2);
    assert.ok((await detail(theorem.id)).outgoing.some((r) => r.kind === "dependsOn" && r.status === "accepted"));
    assert.equal((await byKey("resource", "analysis/fit_model.py")).id, fit.id);
    const backups = await call("graph_backups");
    assert.ok(backups.length >= 1, "an automatic backup exists");

    // ---- 7. Hostile databases: refused without harm, restorable -------------------
    const manual = await call("graph_backup");
    await call("workspace_close");
    const db = join(paper, ".exposition", "graph.sqlite3");
    const hostileCases = {
      // Well-formed SQLite files whose contents break the schema, vocabulary, or limits.
      "relation breaking endpoint rules": (d) =>
        d.exec(`INSERT INTO relations (id, kind, from_id, to_id, origin, status, evidence, revision, created_at, updated_at)
                SELECT 'rel-crafted', 'proofOf', id, id, '{"kind":"user"}', 'accepted', '[]', 1, 0, 0
                FROM entities WHERE kind = 'theorem' LIMIT 1`),
      "unknown entity kind": (d) => d.exec(`UPDATE entities SET kind = 'shellCommand' WHERE kind = 'collection'`),
      "planted trigger": (d) => d.exec(`CREATE TRIGGER evil AFTER INSERT ON entities BEGIN DELETE FROM relations; END`),
      "oversized label": (d) => d.exec(`UPDATE entities SET label = printf('%.400000c', 'x') WHERE kind = 'collection'`),
      "not a database": null,
    };
    const pristine = await readFile(db);
    for (const [name, craft] of Object.entries(hostileCases)) {
      if (craft) {
        await writeFile(db, pristine);
        const d = new DatabaseSync(db);
        craft(d);
        d.close();
      } else {
        await writeFile(db, Buffer.from("SQLite format 3\0" + "x".repeat(4000)));
      }
      const crafted = await readFile(db);
      await call("workspace_open", { path: paper });
      const hostile = await status();
      assert.equal(hostile.state.kind, "unavailable", `${name}: ${JSON.stringify(hostile.state)}`);
      assert.deepEqual(await readFile(db), crafted, `${name}: the file is left unchanged until the user chooses`);
      await call("workspace_close");
    }
    timings.hostileDatabasesRefused = Object.keys(hostileCases).length;
    await call("workspace_open", { path: paper });
    const hostile = await status();
    assert.equal(hostile.state.kind, "unavailable");
    const restore = hostile.state.recovery.find((o) => o.kind === "restoreBackup" && o.backup === manual.name);
    assert.ok(restore, "the consistent backup is offered");
    // Source editing still works while the graph is unavailable.
    const stillEditable = await call("document_open", { path: "theory.tex" });
    assert.ok(stillEditable.state.text.length > 0);
    await call("graph_recover", { option: restore });
    await synchronized("restored");
    assert.equal((await detail(collection)).incoming.filter((r) => r.kind === "memberOf").length, 2, "curated state restored");
    await shot("04-restored");
    await call("workspace_close");

    // ---- 8. A research-only project ------------------------------------------------
    await call("workspace_open", { path: research });
    await synchronized("research project indexed");
    const rkeys = (await entities(["resource"])).map((r) => r.key);
    for (const expected of [".exposition/research/rs-1/sources/fit.py", ".exposition/research/rs-1/sources/helpers.py", ".exposition/research/rs-1/data/obs.csv", "notes.md", "figures/plot.png"]) {
      assert.ok(rkeys.includes(expected), `${expected} in ${rkeys}`);
    }
    assert.ok(!rkeys.some((k) => k.startsWith(".venv/")), "virtual environments are not entities");
    const fitPy = await byKey("resource", ".exposition/research/rs-1/sources/fit.py");
    const helpers = await byKey("resource", ".exposition/research/rs-1/sources/helpers.py");
    assert.ok((await detail(fitPy.id)).outgoing.some((r) => r.kind === "imports" && r.to === helpers.id), "local import");
    const notes = await byKey("resource", "notes.md");
    const plot = await byKey("resource", "figures/plot.png");
    assert.ok((await detail(notes.id)).outgoing.some((r) => r.kind === "usesAsset" && r.to === plot.id));
    await call("workspace_close");

    // ---- 9. Performance on a generated 3,000-file project ---------------------------
    started = Date.now();
    await call("workspace_open", { path: big });
    const opened = Date.now() - started;
    // Edits applied while the cold index runs (reconciliation is off the input path).
    const bigDoc = (await call("document_open", { path: "main.tex" })).state.info.id;
    const during = [];
    for (let i = 0; i < 20; i++) {
      during.push((await applyEdit(bigDoc, 0, "%")).ms);
      if ((await status()).freshness.state === "synchronized") break;
    }
    const big1 = await synchronized("generated project indexed", 120_000);
    timings.generatedProject = {
      files: 3000,
      workspaceOpenMs: opened,
      openToSynchronizedMs: Date.now() - started,
      entities: big1.entities,
      relations: big1.relations,
      documentApplyDuringIndexMs: { median: median(during), max: Math.max(...during), samples: during.length },
    };
    const idle = [];
    for (let i = 0; i < 20; i++) idle.push((await applyEdit(bigDoc, 0, "%")).ms);
    timings.generatedProject.documentApplyIdleMs = { median: median(idle), max: Math.max(...idle), samples: idle.length };
    const chapter = (await call("document_open", { path: "chapters/c3/ch3.tex" })).state.info.id;
    await applyEdit(chapter, 0, "\\begin{corollary}\\label{cor:perf}\nFrom \\ref{thm:3}.\n\\end{corollary}\n");
    started = Date.now();
    await call("document_save", { document: chapter, overwrite: false });
    await waitFor(() => byKey("corollary", "cor:perf"), { timeout: 30_000, interval: 10, message: "incremental reindex" });
    timings.generatedProject.saveToReconciledMs = Date.now() - started;
    const forced = await call("graph_reconcile", { force: true });
    timings.generatedProject.forcedReindexMs = forced.durationMs;
    timings.generatedProject.workingSetMb = workingSetMb(app.child.pid);
    // Typing in Monaco while a forced reindex runs on the reconciler thread.
    const reindexing = call("graph_reconcile", { force: true });
    await openInEditor(page, "main.tex");
    await focusEditor(page, "main.tex", "end");
    const busy = [];
    for (const ch of "%busy") {
      const before = (await call("document_state", { document: bigDoc })).info.revision;
      const t0 = performance.now();
      await page.keyboard.type(ch);
      await waitForAsync(
        page,
        async ([doc, rev]) => (await window.__TAURI_INTERNALS__.invoke("document_state", { document: doc })).info.revision > rev,
        [bigDoc, before],
        { timeout: 5000 },
      );
      busy.push(performance.now() - t0);
    }
    await reindexing;
    timings.generatedProject.monacoKeystrokeDuringReindexMs = { median: median(busy), max: Math.max(...busy), samples: busy.length };
    assert.ok(median(busy) < 250, `typing stays responsive while reindexing (${median(busy)} ms)`);
    await shot("05-generated");
  } catch (error) {
    await shot("failure").catch(() => {});
    throw error;
  } finally {
    await writeFile(join(artifactsDir, "timings.json"), JSON.stringify(timings, null, 2)).catch(() => {});
    const trace = app.logs
      .join("")
      .split(/\r?\n/)
      .filter((l) => /^(graph pass|reconcile:|plan:)/.test(l));
    await writeFile(join(artifactsDir, "reconciler-trace.txt"), trace.join("\n")).catch(() => {});
    console.log(`graph-core timings: ${JSON.stringify(timings)}`);
    await app.close();
  }
}
