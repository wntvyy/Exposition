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
//     freshness never "current"; graph_query lists it under formalEvidence;
//     through a link that is only suggested the same verdict reads
//     "Checked (suggested link)" (Mathematics) and the agent is told so
//  4b that record cannot be moved or relabelled by a relationship (people and
//     agents: a record on another kind of relation, a formal check cited as a
//     review, the declaration's record cited from the proof are refused); an
//     agent's legitimate citation (declaration is-evidence-for theorem) is a
//     suggestion that gives the theorem no evidence of its own; once the user
//     rejects it no agent can propose it again; the declaration cannot be
//     renamed as graph metadata; all of it is the same after a restart
//   5 the Lean source changes and is saved: the evidence is stale everywhere
//     (declaration, theorem row, agent result, file impact); after a restart
//     it is still stale and the association still accepted
//   6 the manuscript statement changes and is saved: the theorem's binding no
//     longer resolves and the reviewed association stops reading as reviewed
//     ("Association needs re-review", verdict "Checked (link needs
//     re-review)", the reason in the inspector, in graph_query, and in the
//     Lean panel's data), also after a restart; the formal evidence is
//     untouched; binding the theorem to the new text and choosing Confirm
//     link records the new statement and the association is reviewed again
//   7 a rejected relation drops out of impact
//   8 archiving the indexed declaration is the user's choice: it survives a
//     save of its Lean file and a restart
//
// Requires Lean through elan with leanprover/lean4:v4.35.0; without it the
// scenario reports SKIPPED unless EXPOSITION_REQUIRE_LEAN=1.
//
// Requires a debug build: it calls the debug-only probe commands lean_check,
// graph_reconcile.
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

/**
 * One top-level field of a tool result. The task transcript shows a bounded
 * preview of each result (the model gets all of it), so a long result is not
 * whole JSON there: the field is then read from the preview, and must lie
 * entirely inside it.
 */
function field(result, key) {
  if (result.value?.raw === undefined) return result.value[key];
  const raw = result.value.raw;
  const start = raw.indexOf(`"${key}":`);
  if (start < 0) throw new Error(`${key} is not in the transcript preview: ${raw.slice(0, 300)}`);
  let i = start + key.length + 3;
  let depth = 0;
  let inString = false;
  for (let j = i; j < raw.length; j += 1) {
    const c = raw[j];
    if (inString) {
      if (c === "\\") j += 1;
      else if (c === '"') inString = false;
    } else if (c === '"') inString = true;
    else if (c === "[" || c === "{") depth += 1;
    else if (c === "]" || c === "}") depth -= 1;
    if (!inString && depth === 0) return JSON.parse(raw.slice(i, j + 1));
  }
  throw new Error(`${key} is cut off in the transcript preview`);
}

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
        return t && !["queued", "running", "awaitingApproval", "awaitingAnswer"].includes(t.status.kind);
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

    // ---- 4b: the record cannot be moved, relabelled, or re-proposed --------
    const proposeNow = async (operations) =>
      invoke(page, "graph_propose_change", { request: { baseRevision: (await invoke(page, "graph_status")).revision, operations, summary: null } });
    const relate = (kind, from, to, evidenceKind, cited) => ({ op: "addRelation", kind, from: { kind: "id", id: from }, to: { kind: "id", id: to }, evidenceKind, evidence: cited, note: null });
    const agentRelate = (relation, from, to, evidenceKind) => ({ op: "add_relation", relation, from, to, evidence_kind: evidenceKind, evidence: [evidence.id], note: "" });
    for (const [what, op, code] of [
      ["a record on a reference", relate("references", finding, theorem, null, [evidence.id]), "graph.evidenceNotCarried"],
      ["a formal check cited as a review", relate("evidenceFor", proof, theorem, "review", [evidence.id]), "graph.evidenceKind"],
      ["the declaration's record cited from the proof", relate("evidenceFor", proof, theorem, "formalCheck", [evidence.id]), "graph.evidenceSubject"],
    ]) {
      const refusedChange = await proposeNow([op]);
      assert.equal(refusedChange.valid, false, `${what}: ${JSON.stringify(refusedChange.diagnostics)}`);
      assert.equal(refusedChange.diagnostics[0].code, code, what);
    }
    const declarationBefore = (await invoke(page, "graph_entity", { entity: declaration })).entity;
    const renameRefused = await proposeNow([{ op: "updateEntity", entity: declaration, expectedRevision: declarationBefore.revision, label: "Main theorem", attributes: null }]);
    assert.equal(renameRefused.diagnostics[0]?.code, "graph.sourceDerived", JSON.stringify(renameRefused.diagnostics));
    const propose = (id, operations) => tool(id, "graph_propose", { summary: "", base_revision: "$ref:inspect/graph/revision", operations });
    const forging = await agent("Make the theorem read as verified", "autoApply", [
      query("inspect", "inspect", { path: LEAN }),
      propose("carried", [agentRelate("references", finding, theorem, "")]),
      propose("relabel", [agentRelate("evidenceFor", proof, theorem, "review")]),
      propose("othersubject", [agentRelate("evidenceFor", proof, theorem, "formalCheck")]),
      propose("rename", [{ op: "update_entity", entity: declaration, expected_revision: declarationBefore.revision, label: "Main theorem", attributes: [], replace_attributes: false }]),
      propose("cite", [agentRelate("evidenceFor", declaration, theorem, "formalCheck")]),
    ]);
    for (const [call, code] of [["carried", "graph.evidenceNotCarried"], ["relabel", "graph.evidenceKind"], ["othersubject", "graph.evidenceSubject"], ["rename", "graph.sourceDerived"]]) {
      assert.equal(forging[call].isError, true, `${call}: ${JSON.stringify(forging[call])}`);
      assert.equal(forging[call].value.diagnostics[0].code, code, call);
    }
    assert.equal(forging.cite.value.applied, true, JSON.stringify(forging.cite));
    assert.deepEqual((await invoke(page, "graph_entity", { entity: declaration })).entity, declarationBefore, "the declaration is as its source states it");
    const supported = await invoke(page, "graph_entity", { entity: theorem });
    const citation = supported.incoming.find((r) => r.kind === "evidenceFor");
    assert.equal(citation.status, "suggested");
    assert.deepEqual(citation.evidence, [evidence.id], "the relationship carries the citation");
    assert.equal(supported.evidence.length, 0, "a relationship citing the record gives the theorem no evidence of its own");
    assert.deepEqual(supported.citedEvidence.map((e) => e.id), [evidence.id], "the cited record is returned apart, as cited support");
    const rejectCitation = await proposeNow([{ op: "setRelationStatus", relation: citation.id, status: "rejected" }]);
    await invoke(page, "graph_apply_change", { request: { proposal: rejectCitation.id, expectedRevision: rejectCitation.baseRevision } });
    const reproposed = await agent("Cite the Lean check for the theorem again", "autoApply", [
      query("inspect", "inspect", { path: LEAN }),
      propose("again", [agentRelate("evidenceFor", declaration, theorem, "formalCheck")]),
    ]);
    assert.equal(reproposed.again.isError, true, JSON.stringify(reproposed.again));
    assert.equal(reproposed.again.value.diagnostics[0].code, "graph.rejected");
    const rejectedCitation = (await invoke(page, "graph_entity", { entity: theorem })).incoming.find((r) => r.id === citation.id);
    assert.equal(rejectedCitation.status, "rejected", "the user's rejection stands");
    assert.ok(rejectedCitation.revision > citation.revision, "and is the later revision");
    assert.equal((await invoke(page, "graph_entity", { entity: theorem })).evidence.length, 0);
    assert.deepEqual((await invoke(page, "graph_entity", { entity: theorem })).citedEvidence, [], "a rejected citation shows nothing");
    assert.ok(!(await impactOf(theorem)).evidence.some((e) => e.subject === theorem), "impact names no evidence of the theorem");
    note("the declaration's record cannot be moved, relabelled, or re-proposed after a rejection", { refused: ["graph.evidenceNotCarried", "graph.evidenceKind", "graph.evidenceSubject", "graph.sourceDerived", "graph.rejected"] });

    // A second item linked to the same declaration only by an agent's
    // suggestion: the verdict is shown, labelled as coming through a
    // suggested link, and never like the reviewed association's.
    const restated = (
      await invoke(page, "graph_apply_change", {
        request: await (async () => {
          const s = await invoke(page, "graph_status");
          const p = await invoke(page, "graph_propose_change", {
            request: { baseRevision: s.revision, operations: [{ op: "createEntity", tempId: "c", kind: "claim", roles: [], label: "Odd sums, restated", key: null, attributes: {}, binding: null }], summary: null },
          });
          return { proposal: p.id, expectedRevision: p.baseRevision };
        })(),
      })
    ).created[0].id;
    const suggestedLink = await agent("Link the Lean theorem to the restated claim", "autoApply", [
      query("inspect", "inspect", { path: LEAN }),
      tool("link", "graph_propose", {
        summary: "Link oddSum_eq_sq to the restated claim",
        base_revision: "$ref:inspect/graph/revision",
        operations: [{ op: "add_relation", relation: "formalizes", from: declaration, to: restated, evidence_kind: "", evidence: [], note: "" }],
      }),
      query("claim", "entity", { entity: restated }),
    ]);
    assert.equal(suggestedLink.link.value.applied, true, JSON.stringify(suggestedLink.link));
    const rowSuggested = await mathRow(restated);
    assert.equal(rowSuggested.formalAssociation, "suggested");
    assert.equal(rowSuggested.verdict, "checked", "the verdict stays visible");
    assert.equal(rowSuggested.verdictAssociation, "suggested");
    assert.equal(row4.verdictAssociation, "reviewed");
    const toldSuggested = suggestedLink.claim.value.formalEvidence[0];
    assert.equal(toldSuggested.association, "suggested");
    assert.equal(toldSuggested.associationNote, "Suggested link: no person has reviewed this correspondence.");
    assert.equal(fourth.theorem.value.formalEvidence[0].review, "current");
    assert.equal(fourth.theorem.value.formalEvidence[0].associationNote, undefined);
    note("a verdict through a suggested link is labelled for the agent", toldSuggested.associationNote);

    // The Graph page says the same: Mathematics → Verification.
    const openVerification = async () => {
      await page.keyboard.press("Control+4");
      await page.getByTestId("graph-page").waitFor();
      await page.getByRole("radio", { name: "Mathematics" }).click();
      await page.getByRole("radio", { name: "Verification" }).click();
      return page.getByRole("grid", { name: "Verification" });
    };
    const grid = await openVerification();
    const theoremRow = grid.locator(`[data-entity-id="${theorem}"]`);
    await theoremRow.waitFor({ timeout: 15_000 });
    await waitFor(async () => (await theoremRow.getAttribute("data-verdict")) === "checked", { timeout: 15_000, message: "theorem row verdict" });
    assert.match(await theoremRow.innerText(), /Reviewed association/);
    assert.match(await theoremRow.innerText(), /Checked(?! \()/, "a reviewed association's verdict is unqualified");
    assert.equal(await theoremRow.getByTestId("graph-verdict-qualified").count(), 0);
    assert.notEqual(await theoremRow.getAttribute("data-freshness"), "current");
    const restatedRow = grid.locator(`[data-entity-id="${restated}"]`);
    await restatedRow.waitFor({ timeout: 15_000 });
    assert.match(await restatedRow.innerText(), /Suggested association/);
    assert.equal(await restatedRow.getByTestId("graph-verdict-qualified").innerText(), "Checked (suggested link)");
    assert.equal(await restatedRow.getAttribute("data-verdict-link"), "suggested");
    await shot("graph-verdict-suggested-link-light");
    note("Mathematics labels a verdict shown through a suggested link", await restatedRow.getByTestId("graph-verdict-qualified").innerText());
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
    assert.equal(field(fifth.theorem, "formalEvidence")[0].evidence[0].freshness, "stale");
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
    const reopened = await invoke(page, "graph_entity", { entity: theorem });
    assert.equal(reopened.evidence.length, 0, "after a restart the theorem still has no evidence of its own");
    assert.deepEqual(reopened.incoming.find((r) => r.id === citation.id), rejectedCitation, "the rejected citation is stored as it was");
    assert.equal((await invoke(page, "graph_entity", { entity: declaration })).entity.label, declarationBefore.label);
    note("after a restart the evidence is still stale and the association still reviewed", "stable");

    // ---- 6: the manuscript statement changes and is saved -----------------
    const mainDoc = await openInEditor(page, "main.typ");
    await rewrite(mainDoc, (text) => text.replace("n squared", "n cubed"));
    const changed = await invoke(page, "graph_entity", { entity: theorem });
    assert.equal(changed.bindings[0].state, "missing", JSON.stringify(changed.bindings[0]));
    assert.equal(changed.bindings[0].detail, "The bound text was changed or removed.");
    assert.equal((await invoke(page, "graph_entity", { entity: proof })).bindings[0].state, "resolved", "a whole-file binding follows the file");
    const fileImpact = await impactOf(mainFile);
    assert.ok(fileImpact.unknownCoverage.includes("main.typ"), "an unresolved binding makes the file's impact unknown");
    // The reviewed association was reviewed against the old statement: it
    // stops reading as reviewed, with the reason, on every surface.
    const formalLink = async () => (await invoke(page, "graph_entity", { entity: theorem })).incoming.find((r) => r.kind === "formalizes");
    const flagged = async (where, state, reason) => {
      const l = await formalLink();
      assert.equal(l.status, "accepted", `${where}: the assertion is kept`);
      assert.equal(l.review.state, state, `${where}: ${JSON.stringify(l.review)}`);
      assert.match(l.review.reason, reason, where);
      const row = await mathRow(theorem);
      assert.equal(row.formalAssociation, "needsReReview", where);
      assert.equal(row.associationReason, l.review.reason, where);
      assert.equal(row.verdict, "checked", `${where}: the verdict is what it was`);
      assert.equal(row.verdictAssociation, "needsReReview", where);
      assert.equal(row.evidence.id, evidence.id, `${where}: the evidence is untouched`);
      assert.equal(row.freshness.kind, "stale", `${where}: the formal evidence is about the Lean file, not the manuscript`);
      return l;
    };
    const missing = await flagged("after the statement changed", "statementMissing", /^The statement cannot be located, .*main\.typ: The bound text was changed or removed/);
    assert.equal((await invoke(page, "graph_entity", { entity: declaration })).evidence.length, 1, "nothing was recorded or removed");
    const sixth = await agent("Is the theorem still formally checked?", "readOnly", [
      query("theorem", "entity", { entity: theorem }),
      query("near", "neighborhood", { entity: theorem }),
      query("impact", "impact", { entity: theorem }),
    ]);
    const told = field(sixth.theorem, "formalEvidence")[0];
    assert.equal(told.association, "accepted");
    assert.equal(told.review, "statementMissing");
    assert.match(told.associationNote, /^Link needs re-review: The statement cannot be located/);
    assert.deepEqual(told.evidence[0].outcome, { verdict: "checked" }, "the verdict is still reported, with the note");
    assert.equal(field(sixth.near, "relations").find((r) => r.kind === "formalizes").review, "statementMissing");
    assert.match(sixth.impact.value.items.find((i) => i.entity.id === declaration).reason, /\(reviewed link needs re-review\)$/);
    assert.match(field(sixth.theorem, "evidenceNote"), /needs the user's re-review/);
    note("a changed manuscript statement flags the reviewed association for re-review", { binding: changed.bindings[0].state, review: missing.review });

    // It is not something an agent can clear: it cannot accept, and a fresh
    // suggestion of the same link is a duplicate.
    const clearing = await agent("Mark the link as reviewed again", "autoApply", [
      query("inspect", "inspect", { path: LEAN }),
      tool("again", "graph_propose", {
        summary: "Link oddSum_eq_sq to the theorem again",
        base_revision: "$ref:inspect/graph/revision",
        operations: [{ op: "add_relation", relation: "formalizes", from: declaration, to: theorem, evidence_kind: "", evidence: [], note: "" }],
      }),
    ]);
    assert.equal(clearing.again.isError, true, JSON.stringify(clearing.again));
    assert.equal(clearing.again.value.diagnostics[0].code, "graph.duplicateRelation");
    await flagged("after an agent tried", "statementMissing", /cannot be located/);

    // The state is stored: it is the same after a restart.
    await app.close();
    app = await launchApp({ dataDir, env });
    page = app.page;
    await page.setViewportSize({ width: 1440, height: 1000 });
    await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="main.typ"]').waitFor({ timeout: 15_000 });
    await settle();
    await flagged("after a restart", "statementMissing", /cannot be located/);

    // Mathematics: the association and the verdict are both qualified.
    const grid6 = await openVerification();
    const row6 = grid6.locator(`[data-entity-id="${theorem}"]`);
    await row6.waitFor({ timeout: 15_000 });
    await waitFor(async () => (await row6.getAttribute("data-association")) === "needsReReview", { timeout: 15_000, message: "theorem row association" });
    assert.match(await row6.innerText(), /Association needs re-review/);
    assert.doesNotMatch(await row6.innerText(), /Reviewed association/);
    assert.equal(await row6.getByTestId("graph-verdict-qualified").innerText(), "Checked (link needs re-review)");
    await row6.click();
    const inspector = page.locator('[data-region="graph-inspector"]');
    const linkRow = inspector.locator('[data-testid="graph-relation"][data-kind="formalizes"]');
    await linkRow.waitFor({ timeout: 15_000 });
    assert.match(await linkRow.innerText(), /Needs re-review/);
    assert.match(await linkRow.getByTestId("graph-link-review").innerText(), /^The statement cannot be located/);
    assert.match(await linkRow.getByTestId("graph-link-review").innerText(), /Rebind the statement to its current text, then confirm the link\./);
    // There is no statement to confirm against: the action is unavailable,
    // and a confirmation sent anyway is refused and changes nothing.
    assert.equal(await linkRow.getByRole("button", { name: "Confirm link" }).isDisabled(), true, "Confirm link is disabled while the statement cannot be located");
    assert.equal(await linkRow.getByRole("button", { name: "Reject link" }).isDisabled(), false);
    const unlocated = await formalLink();
    const refused = await invoke(page, "graph_propose_change", {
      request: { baseRevision: (await invoke(page, "graph_status")).revision, operations: [{ op: "setRelationStatus", relation: unlocated.id, status: "accepted" }], summary: null },
    });
    assert.equal(refused.valid, false, JSON.stringify(refused));
    assert.equal(refused.diagnostics[0].code, "graph.statementNotLocated");
    assert.match(refused.diagnostics[0].message, /^Nothing was confirmed\. The statement cannot be located.*Rebind the statement to its current text, then confirm the link\./);
    assert.deepEqual(await formalLink(), unlocated, "the refused confirmation changed nothing");
    const formalSection = inspector.getByTestId("graph-formal-section");
    assert.match(await formalSection.getByTestId("graph-formalization").innerText(), /Needs re-review/);
    assert.match(await formalSection.getByTestId("graph-verdict-line").innerText(), /Checked \(link needs re-review\)/);
    await formalSection.scrollIntoViewIfNeeded();
    await shot("graph-link-needs-re-review-light");

    // The user binds the theorem to the statement as it reads now: it is
    // located again, as a text nobody reviewed.
    const NEW_STATEMENT = STATEMENT.replace("n squared", "n cubed");
    const at = MAIN.replace("n squared", "n cubed").indexOf(NEW_STATEMENT);
    assert.ok(at >= 0, "the edited statement is in main.typ");
    const status6 = await invoke(page, "graph_status");
    const rebind = await invoke(page, "graph_propose_change", {
      request: {
        baseRevision: status6.revision,
        operations: [{ op: "bindSource", entity: { kind: "id", id: theorem }, binding: { path: "main.typ", selector: { kind: "region" }, range: { start: at, end: at + NEW_STATEMENT.length } } }],
        summary: null,
      },
    });
    await invoke(page, "graph_apply_change", { request: { proposal: rebind.id, expectedRevision: rebind.baseRevision } });
    await invoke(page, "graph_reconcile", { force: true });
    assert.equal((await invoke(page, "graph_entity", { entity: theorem })).bindings[0].state, "resolved");
    await flagged("after binding the new text", "statementChanged", /^The statement changed since this link was reviewed\./);
    await waitFor(async () => /^The statement changed since/.test(await linkRow.getByTestId("graph-link-review").innerText()), { timeout: 15_000, message: "the inspector follows the graph" });
    assert.match(await linkRow.innerText(), /Needs re-review/, "binding does not review the link");

    // Only the user's Confirm clears it, and it records the new statement.
    const before = (await formalLink()).reviewedStatement;
    await linkRow.getByRole("button", { name: "Confirm link" }).click();
    await waitFor(async () => (await formalLink()).review.state === "current", { timeout: 15_000, message: "the link is reviewed again" });
    const confirmed = await formalLink();
    assert.equal(confirmed.review.reason, null);
    assert.ok(confirmed.reviewedStatement && confirmed.reviewedStatement !== before, "the new statement is the reviewed one");
    await waitFor(async () => (await row6.getAttribute("data-association")) === "reviewed", { timeout: 15_000, message: "theorem row reviewed again" });
    assert.match(await row6.innerText(), /Reviewed association/);
    assert.equal(await row6.getByTestId("graph-verdict-qualified").count(), 0, "the verdict is unqualified again");
    assert.equal(await linkRow.getByRole("button", { name: "Confirm link" }).count(), 0);
    const rowConfirmed = await mathRow(theorem);
    assert.equal(rowConfirmed.verdictAssociation, "reviewed");
    assert.equal(rowConfirmed.evidence.id, evidence.id, "confirming records nothing about the Lean check");
    assert.equal(rowConfirmed.freshness.kind, "stale");
    await shot("graph-link-confirmed-light");
    await page.keyboard.press("Control+1");
    note("binding the new text and Confirm link make the association reviewed again", { reviewedStatement: confirmed.reviewedStatement });

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

    // ---- 8: archiving an indexed declaration survives indexing -------------
    const toArchive = (await invoke(page, "graph_entity", { entity: declaration })).entity;
    const archive = await invoke(page, "graph_propose_change", {
      request: { baseRevision: (await invoke(page, "graph_status")).revision, operations: [{ op: "setLifecycle", entity: declaration, expectedRevision: toArchive.revision, lifecycle: "archived" }], summary: null },
    });
    assert.equal(archive.valid, true, JSON.stringify(archive.diagnostics));
    await invoke(page, "graph_apply_change", { request: { proposal: archive.id, expectedRevision: archive.baseRevision } });
    const leanDoc8 = await openInEditor(page, LEAN);
    await rewrite(leanDoc8, (text) => `${text}-- another edit\n`);
    await invoke(page, "graph_reconcile", { force: true });
    assert.equal((await invoke(page, "graph_entity", { entity: declaration })).entity.lifecycle, "archived", "indexing the file does not undo the archive");
    await app.close();
    app = await launchApp({ dataDir, env });
    page = app.page;
    await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="main.typ"]').waitFor({ timeout: 15_000 });
    await settle();
    const archived = (await invoke(page, "graph_entity", { entity: declaration })).entity;
    assert.equal(archived.lifecycle, "archived", "still archived after a restart");
    assert.equal(archived.label, declarationBefore.label);
    note("an archived indexed declaration stays archived across a save of its file and a restart", archived.lifecycle);

    const errors = app.consoleMessages.filter((m) => m.startsWith("error:") || m.startsWith("pageerror:"));
    assert.deepEqual(errors, [], "no console errors");
    await writeFile(join(artifactsDir, "observations.json"), JSON.stringify(observations, null, 2));
  } finally {
    await app.close();
  }
}
