// Pre-review walkthrough, graph lane: where verification evidence is recorded
// and what "what is affected if this changes?" covers — in the real app
// (debug build), with the real graph, document, and Lean services and the
// scripted provider standing in for the model only.
//
// The walkthrough project: main.typ with a theorem, a standalone
// lean/OddSums.lean (no Lake package), a theorem and its proof recorded by an
// agent and bound to main.typ, a finding that references the theorem.
//
//   1 an agent cannot assert computational evidence itself; the refusal
//     names the real routes; the same change with a plain reference applies
//   2 impact of the theorem follows its relations (suggested ones labelled);
//     impact of the FILE main.typ lists the theorem and the proof located in
//     it, says so, and states that their dependents are not followed
//   3 a passing Lean check (lean_check on the saved file, as the agent's
//     check_lean) records NO evidence; the indexed declaration has none; an
//     accepted `formalizes` association does not create any; graph_query on
//     the theorem says where formal evidence lives and how it is recorded
//   4 the direct, service-backed path: the Lean panel's "Record evidence" for
//     the declaration; the theorem then shows the declaration's verdict by
//     association (Mathematics), never as evidence of its own, with
//     freshness never "current"; graph_query lists it under formalEvidence
//   5 the Lean source changes and is saved: the evidence is stale everywhere
//     (declaration, theorem row, agent result, file impact); after a restart
//     it is still stale and the association still accepted
//   6 the manuscript statement changes and is saved: the theorem's binding no
//     longer resolves; the formal evidence is untouched
//   7 a rejected relation drops out of impact
//
// Requires Lean through elan with leanprover/lean4:v4.35.0; without it the
// scenario reports SKIPPED unless EXPOSITION_REQUIRE_LEAN=1.
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";
import { invoke, openInEditor, waitFor } from "../workspace-helpers.mjs";

const TOOLCHAIN_DIR = "leanprover--lean4---v4.35.0";
const STATEMENT = "Theorem. The sum of the first n odd numbers is n squared.";
const MAIN = `= Odd sums\n\n${STATEMENT}\n\nProof. By induction on n.\n`;
const LEAN = "lean/OddSums.lean";
const LEAN_TEXT = `def oddSum : Nat → Nat
  | 0 => 0
  | n + 1 => oddSum n + (2 * n + 1)

theorem oddSum_eq_sq (n : Nat) : oddSum n = n * n := by
  induction n with
  | zero => rfl
  | succ k ih => simp [oddSum, ih, Nat.succ_mul, Nat.mul_succ]; omega
`;
const DECLARATION = "oddSum_eq_sq";

const tool = (id, name, input) => ({ toolUse: { id, name, input } });
const query = (id, q, { entity = "", path = "", depth = 1 } = {}) =>
  tool(id, "graph_query", { query: q, text: "", kinds: [], entity, path, depth, limit: 100 });

export default async function graphEvidenceImpact({ launchApp, artifactsDir, assert }) {
  const elanHome = process.env.ELAN_HOME || join(homedir(), ".elan");
  if (!existsSync(join(elanHome, "toolchains", TOOLCHAIN_DIR, "bin"))) {
    const message = `SKIPPED: Lean toolchain ${TOOLCHAIN_DIR} is not installed through elan`;
    if (process.env.EXPOSITION_REQUIRE_LEAN === "1") throw new Error(message);
    console.log(message);
    return;
  }
  const base = await mkdtemp(join(tmpdir(), "xp-graph-evidence-"));
  const root = join(base, "project");
  const dataDir = join(base, "data");
  await mkdir(join(root, "lean"), { recursive: true });
  await mkdir(dataDir, { recursive: true });
  await writeFile(join(root, "main.typ"), MAIN);
  await writeFile(join(root, ...LEAN.split("/")), LEAN_TEXT);
  const env = { EXPOSITION_CREDENTIAL_NAMESPACE: `e2e-graph-evidence-${Date.now()}` };
  const observations = [];
  const note = (id, detail) => {
    observations.push({ id, detail });
    console.log(`ok     ${id}: ${typeof detail === "string" ? detail : JSON.stringify(detail)}`);
  };

  let app = await launchApp({ dataDir, env });
  let page = app.page;
  const shot = (name) => page.screenshot({ path: join(artifactsDir, `${name}.png`) });

  /** Run a scripted task to its end and return its tool results by id. */
  const agent = async (prompt, permission, steps) => {
    await invoke(page, "agents_dev_load_script", {
      script: { turns: [...steps.map((s) => ({ steps: [s] })), { steps: [{ text: "done" }] }] },
    });
    const task = await invoke(page, "agents_task_create", {
      request: { prompt, workspaceRoot: root, scope: { kind: "workspace" }, permission, execution: "disabled", consent: "scopeContent", budgets: null },
    });
    await waitFor(
      async () => {
        const t = (await invoke(page, "agents_task_list")).find((x) => x.id === task.id);
        return t && !["queued", "running", "awaitingApproval"].includes(t.status.kind);
      },
      { timeout: 30_000, message: `task ${prompt}` },
    );
    const detail = await invoke(page, "agents_task_detail", { task: task.id });
    const results = {};
    for (const item of detail.transcript) {
      if (item.kind !== "toolCall") continue;
      let value;
      try {
        value = JSON.parse(item.result ?? "null");
      } catch {
        value = { raw: item.result };
      }
      results[item.toolUseId] = { value, isError: item.isError, status: item.status };
    }
    return results;
  };
  const mathRow = async (id) => (await invoke(page, "graph_math_overview")).rows.find((r) => r.entity.id === id);
  const impactOf = (id) => invoke(page, "graph_impact", { request: { entities: [id], maxDepth: null, maxItems: null } });
  const settle = () => invoke(page, "graph_reconcile", { force: false });
  /** Replace the whole text of an open document and save it. */
  const rewrite = async (document, edit) => {
    const before = await invoke(page, "document_state", { document });
    await invoke(page, "document_apply", {
      request: {
        id: `edit-${Date.now()}`,
        origin: { kind: "source" },
        edits: [{ document, baseRevision: before.info.revision, patches: [{ range: { start: 0, end: before.text.length }, text: edit(before.text) }] }],
        undo: { kind: "newGroup" },
        conflict: "reject",
        label: null,
        selection: null,
      },
    });
    await invoke(page, "document_save", { document, overwrite: false });
    await settle();
  };

  try {
    const info = await page.evaluate(() => window.__TAURI_INTERNALS__.invoke("app_info"));
    if (!info.debugBuild) throw new Error("graph-evidence-impact requires a debug build (scripted provider)");
    await page.setViewportSize({ width: 1440, height: 1000 });
    await runCommand(page, "color theme light", /Color Theme: Light/);
    await invoke(page, "agents_provider_update", {
      settings: { provider: "scripted", model: "claude-opus-5-5", effort: "medium", consent: "scopeContent" },
    });
    const opened = await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="main.typ"]').waitFor({ timeout: 15_000 });
    await invoke(page, "trust_grant", { root: opened.root });
    await settle();
    const start = MAIN.indexOf(STATEMENT);

    // ---- 1: the agent's first proposal asserts computational evidence -----
    const entities = [
      { op: "create_entity", temp_id: "thm", kind: "theorem", label: "Sum of odd numbers", attributes: [], bind_path: "main.typ", bind_start: start, bind_end: start + STATEMENT.length },
      { op: "create_entity", temp_id: "prf", kind: "proof", label: "Proof by induction", attributes: [], bind_path: "main.typ", bind_start: -1, bind_end: -1 },
      { op: "create_entity", temp_id: "fnd", kind: "finding", label: "Numerical check up to 1000", attributes: [], bind_path: "", bind_start: -1, bind_end: -1 },
      { op: "add_relation", relation: "proofOf", from: "prf", to: "thm", evidence_kind: "", evidence: [], note: "" },
    ];
    const first = await agent("Record the theorem, its proof, and the numerical check", "autoApply", [
      query("inspect", "inspect", { path: "main.typ" }),
      tool("forge", "graph_propose", {
        summary: "Demo: theorem, proof, and numerical check",
        base_revision: "$ref:inspect/graph/revision",
        operations: [...entities, { op: "add_relation", relation: "evidenceFor", from: "fnd", to: "thm", evidence_kind: "computational", evidence: [], note: "" }],
      }),
      tool("plain", "graph_propose", {
        summary: "Demo: theorem, proof, and numerical check",
        base_revision: "$ref:inspect/graph/revision",
        operations: [...entities, { op: "add_relation", relation: "references", from: "fnd", to: "thm", evidence_kind: "", evidence: [], note: "" }],
      }),
    ]);
    assert.equal(first.forge.isError, true, JSON.stringify(first.forge));
    assert.equal(first.forge.value.diagnostics[0].code, "graph.serviceOwnedEvidence");
    assert.match(first.forge.value.diagnostics[0].message, /computational evidence is recorded only by its service/);
    assert.match(first.forge.value.guidance, /keeps a run's result in Research \(Keep\)/);
    assert.match(first.forge.value.guidance, /Record evidence for a declaration in the Lean panel/);
    assert.match(first.forge.value.guidance, /do not claim it/);
    assert.equal(first.plain.value.applied, true, JSON.stringify(first.plain));
    const id = (t) => first.plain.value.created.find((c) => c.tempId === t).id;
    const theorem = id("thm");
    const proof = id("prf");
    const finding = id("fnd");
    const mainFile = first.inspect.value.resource.id;
    note("service-owned evidence refused with the real routes", first.forge.value.guidance);

    // ---- 2: impact of the theorem, and of the file it is in ----------------
    const second = await agent("What is affected if the theorem or main.typ changes?", "readOnly", [
      query("theorem", "impact", { entity: theorem, depth: 3 }),
      query("file", "impact", { entity: mainFile, depth: 3 }),
    ]);
    const byEntity = (result, entity) => result.value.items.find((i) => i.entity.id === entity);
    assert.equal(byEntity(second.theorem, proof).action, "needsReview");
    assert.match(byEntity(second.theorem, proof).reason, /^is a proof of “Sum of odd numbers”, whose statement changed \(suggested relationship\)$/);
    assert.equal(byEntity(second.theorem, finding).action, "citationReview");
    assert.equal(second.theorem.value.scope, undefined);
    const fileItems = second.file.value.items;
    assert.deepEqual(fileItems.map((i) => i.entity.id).sort(), [theorem, proof].sort(), `the file lists what is located in it: ${JSON.stringify(second.file.value)}`);
    for (const item of fileItems) {
      assert.equal(item.action, "informational");
      assert.equal(item.locatedIn, mainFile);
      assert.equal(item.reason, "is located in “main.typ”, which changed; check whether it changed");
    }
    assert.match(second.file.value.scope, /what\s+depends on it is not followed here: query impact for an item/);
    note("file impact lists the located items and states its scope", { items: fileItems.map((i) => i.entity.label), scope: second.file.value.scope });

    // ---- 3: a passing check records no evidence; neither does accepting ----
    const check = await invoke(page, "lean_check", { request: { target: { kind: "path", path: LEAN }, goalsAt: [], timeoutMs: 300_000 } });
    assert.equal(check.checked, true, `the saved Lean file checks: ${JSON.stringify(check.status)}`);
    await settle();
    const inspected = await agent("What did the graph derive from the Lean file?", "readOnly", [query("lean", "inspect", { path: LEAN })]);
    const leanFile = inspected.lean.value.resource.id;
    const declaration = inspected.lean.value.entities.find((e) => e.label === DECLARATION)?.id;
    assert.ok(declaration, `the graph derived the declaration from the file: ${JSON.stringify(inspected.lean.value.entities)}`);
    assert.match(inspected.lean.value.evidenceNote, /check_lean returns its result to you and records no evidence/);
    const linked = await agent("Link the Lean theorem to the manuscript theorem", "autoApply", [
      tool("link", "graph_propose", {
        summary: "Link the Lean theorem oddSum_eq_sq to the theorem in main.typ",
        base_revision: inspected.lean.value.graph.revision,
        operations: [{ op: "add_relation", relation: "formalizes", from: declaration, to: theorem, evidence_kind: "", evidence: [], note: "" }],
      }),
    ]);
    assert.equal(linked.link.value.applied, true, JSON.stringify(linked.link));
    const link = (await invoke(page, "graph_entity", { entity: theorem })).incoming.find((r) => r.kind === "formalizes");
    assert.equal(link.from, declaration);
    assert.equal(link.status, "suggested");
    assert.equal((await mathRow(theorem)).formalAssociation, "suggested");
    assert.equal((await mathRow(theorem)).verdict, null, "a check alone gives the theorem no verdict");
    // The user accepts the suggestion.
    const status = await invoke(page, "graph_status");
    const accept = await invoke(page, "graph_propose_change", {
      request: { baseRevision: status.revision, operations: [{ op: "setRelationStatus", relation: link.id, status: "accepted" }], summary: null },
    });
    await invoke(page, "graph_apply_change", { request: { proposal: accept.id, expectedRevision: accept.baseRevision } });
    const recheck = await invoke(page, "lean_check", { request: { target: { kind: "path", path: LEAN }, goalsAt: [], timeoutMs: 300_000 } });
    assert.equal(recheck.checked, true);
    const row3 = await mathRow(theorem);
    assert.equal(row3.formalAssociation, "reviewed");
    assert.equal(row3.verdict, null, "an accepted association and a passing check still record no evidence");
    assert.equal((await invoke(page, "graph_entity", { entity: declaration })).evidence.length, 0);
    const third = await agent("Why is there no evidence on the theorem?", "readOnly", [query("theorem", "entity", { entity: theorem })]);
    assert.deepEqual(third.theorem.value.evidence, []);
    assert.equal(third.theorem.value.formalEvidence[0].declaration, declaration);
    assert.equal(third.theorem.value.formalEvidence[0].association, "accepted");
    assert.deepEqual(third.theorem.value.formalEvidence[0].evidence, [], "nothing recorded yet, and the result says so per declaration");
    assert.match(third.theorem.value.evidenceNote, /chooses Record evidence \(or Associate with…\) for that\s+declaration in the Lean panel/);
    note("a passing check and an accepted association record no evidence; the agent is told where and how", third.theorem.value.evidenceNote);

    // ---- 4: the direct path — Record evidence in the Lean panel -----------
    await runCommand(page, "show lean infoview", /Show Lean Infoview/);
    const leanDoc = await openInEditor(page, LEAN);
    const panel = page.getByTestId("lean-panel");
    await waitFor(async () => (await panel.getAttribute("data-status")) === "checked", { timeout: 300_000, interval: 250, message: "OddSums.lean checked in the Lean panel" });
    const declRow = panel.locator(`[data-testid="lean-checked-declaration"][data-name="${DECLARATION}"]`);
    await declRow.waitFor({ timeout: 30_000 });
    assert.match(await declRow.innerText(), /No evidence recorded in the project graph\./);
    const association = declRow.getByTestId("lean-decl-association");
    await association.waitFor({ timeout: 15_000 });
    assert.match(await association.innerText(), /reviewed correspondence \(not a proof of equivalence\)/);
    await shot("lean-panel-before-record-light");
    await declRow.getByRole("button", { name: "Record evidence" }).click();
    const latest = declRow.getByTestId("lean-evidence-latest");
    await latest.waitFor({ timeout: 15_000 });
    assert.equal(await latest.getAttribute("data-freshness"), "unknown", "formal evidence is never shown current");
    const declDetail = await invoke(page, "graph_entity", { entity: declaration });
    assert.equal(declDetail.evidence.length, 1);
    const evidence = declDetail.evidence[0];
    assert.equal(evidence.kind, "formalCheck");
    assert.equal(evidence.recordedBy.kind, "service");
    assert.equal(evidence.subject, declaration, "the evidence is about the declaration");
    assert.equal(evidence.formal.declaration, DECLARATION);
    assert.equal(evidence.formal.sourcePath, LEAN);
    assert.equal(evidence.formal.declarationVerdict, "checked");
    assert.equal((await invoke(page, "graph_entity", { entity: theorem })).evidence.length, 0, "nothing is recorded on the manuscript theorem");
    const row4 = await mathRow(theorem);
    assert.equal(row4.verdict, "checked", "the declaration's verdict, shown by association");
    assert.equal(row4.evidence.id, evidence.id);
    assert.notEqual(row4.freshness.kind, "current");
    const fourth = await agent("Is the theorem checked now?", "readOnly", [
      query("theorem", "entity", { entity: theorem }),
      query("leanfile", "impact", { entity: leanFile }),
    ]);
    const listed = fourth.theorem.value.formalEvidence[0].evidence[0];
    assert.equal(listed.id, evidence.id);
    assert.deepEqual(listed.outcome, { verdict: "checked" });
    assert.equal(listed.freshness, "unknown");
    assert.deepEqual(fourth.theorem.value.evidence, [], "still not the theorem's own evidence");
    const pinned = fourth.leanfile.value.evidence.find((e) => e.id === evidence.id);
    assert.ok(pinned, `the file's impact names the evidence that records it: ${JSON.stringify(fourth.leanfile.value)}`);
    assert.match(pinned.reason, /records lean\/OddSums\.lean as an input; a change to the file makes this evidence historical/);
    note("Record evidence in the Lean panel records service evidence on the declaration", { evidence: evidence.id, digest: evidence.formal.sourceDigest });

    // The Graph page says the same: Mathematics → Verification.
    await page.keyboard.press("Control+4");
    await page.getByTestId("graph-page").waitFor();
    await page.getByRole("radio", { name: "Mathematics" }).click();
    await page.getByRole("radio", { name: "Verification" }).click();
    const grid = page.getByRole("grid", { name: "Verification" });
    const theoremRow = grid.locator(`[data-entity-id="${theorem}"]`);
    await theoremRow.waitFor({ timeout: 15_000 });
    await waitFor(async () => (await theoremRow.getAttribute("data-verdict")) === "checked", { timeout: 15_000, message: "theorem row verdict" });
    assert.match(await theoremRow.innerText(), /Reviewed association/);
    assert.notEqual(await theoremRow.getAttribute("data-freshness"), "current");
    await theoremRow.click();
    assert.match(await page.getByTestId("graph-correspondence-note").innerText(), /not a machine proof/);
    await page.getByRole("button", { name: "What needs review if this changes?" }).click();
    await page.getByTestId("graph-impact-item").first().waitFor({ timeout: 10_000 });
    await page.getByTestId("graph-impact").scrollIntoViewIfNeeded();
    await shot("graph-theorem-formal-evidence-light");
    await page.keyboard.press("Control+1");

    // ---- 5: the Lean source changes and is saved --------------------------
    await rewrite(leanDoc, (text) => `${text}\n-- a later edit\n`);
    const stale = async (where) => {
      const d = await invoke(page, "graph_entity", { entity: declaration });
      const fresh = d.evidenceFreshness.find(([e]) => e === evidence.id)[1];
      assert.equal(fresh.kind, "stale", `${where}: ${JSON.stringify(fresh)}`);
      assert.match(fresh.reasons[0], /lean\/OddSums\.lean changed since it was checked/);
      const row = await mathRow(theorem);
      assert.equal(row.freshness.kind, "stale", `${where}: the theorem's row follows the declaration's evidence`);
      assert.equal(row.formalAssociation, "reviewed", `${where}: the association is kept`);
      const hit = (await impactOf(leanFile)).evidence.find((e) => e.evidence === evidence.id);
      assert.equal(hit.freshness.kind, "stale", where);
    };
    await stale("after the save");
    await waitFor(async () => (await latest.getAttribute("data-freshness")) === "stale", { timeout: 300_000, interval: 250, message: "the Lean panel shows the record as history" });
    assert.match(await declRow.getByTestId("lean-evidence-history").innerText(), /changed since revision/);
    const fifth = await agent("Is the evidence still current?", "readOnly", [query("theorem", "entity", { entity: theorem })]);
    assert.equal(fifth.theorem.value.formalEvidence[0].evidence[0].freshness, "stale");
    await shot("lean-panel-evidence-history-light");
    note("a saved change to the Lean source makes the evidence stale on every surface", "stale");

    // ---- restart: stable ---------------------------------------------------
    await app.close();
    app = await launchApp({ dataDir, env });
    page = app.page;
    await page.setViewportSize({ width: 1440, height: 1000 });
    await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="main.typ"]').waitFor({ timeout: 15_000 });
    await settle();
    await stale("after a restart");
    assert.equal((await invoke(page, "graph_entity", { entity: theorem })).entity.id, theorem, "stable identity");
    note("after a restart the evidence is still stale and the association still reviewed", "stable");

    // ---- 6: the manuscript statement changes and is saved -----------------
    const mainDoc = await openInEditor(page, "main.typ");
    await rewrite(mainDoc, (text) => text.replace("n squared", "n cubed"));
    const changed = await invoke(page, "graph_entity", { entity: theorem });
    assert.equal(changed.bindings[0].state, "missing", JSON.stringify(changed.bindings[0]));
    assert.equal(changed.bindings[0].detail, "The bound text was changed or removed.");
    assert.equal((await invoke(page, "graph_entity", { entity: proof })).bindings[0].state, "resolved", "a whole-file binding follows the file");
    const row6 = await mathRow(theorem);
    assert.equal(row6.formalAssociation, "reviewed");
    assert.equal(row6.freshness.kind, "stale", "the formal evidence is about the Lean file, not the manuscript");
    const fileImpact = await impactOf(mainFile);
    assert.ok(fileImpact.unknownCoverage.includes("main.typ"), "an unresolved binding makes the file's impact unknown");
    note("a changed manuscript statement unbinds the theorem; the reviewed association is not flagged by itself", {
      binding: changed.bindings[0].state,
      association: row6.formalAssociation,
    });

    // ---- 7: a rejected relation drops out of impact -----------------------
    const reference = (await invoke(page, "graph_entity", { entity: finding })).outgoing.find((r) => r.kind === "references");
    const status7 = await invoke(page, "graph_status");
    const reject = await invoke(page, "graph_propose_change", {
      request: { baseRevision: status7.revision, operations: [{ op: "setRelationStatus", relation: reference.id, status: "rejected" }], summary: null },
    });
    await invoke(page, "graph_apply_change", { request: { proposal: reject.id, expectedRevision: reject.baseRevision } });
    const after = await impactOf(theorem);
    assert.ok(!after.items.some((i) => i.entity.id === finding), "the rejected reference is not followed");
    assert.ok(after.items.some((i) => i.entity.id === proof && /\(suggested relationship\)$/.test(i.reason)), "the suggested proof link still is, labelled");
    note("rejected relations drop out; suggested ones are labelled", after.items.map((i) => i.reason));

    const errors = app.consoleMessages.filter((m) => m.startsWith("error:") || m.startsWith("pageerror:"));
    assert.deepEqual(errors, [], "no console errors");
    await writeFile(join(artifactsDir, "observations.json"), JSON.stringify(observations, null, 2));
  } finally {
    await app.close();
  }
}
