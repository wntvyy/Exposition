// Graph dependency status in the real app (debug build; the scripted
// provider stands in for the model only):
//
// XR-D-02 The Graph page header ("N items · M relationships · revision R")
//         follows every change without a reopen: an agent's suggestions
//         (event only), a person's change through the service, and a
//         rejection pressed in the inspector — it always equals graph_status.
// XR-D-01 Mathematics › Depends on: an agent suggests Balanced energy →
//         Nonnegative energy → Pair energy. Both are listed, labelled
//         Suggested. The person rejects lemma → definition in the lemma's
//         inspector: the lemma's Depends on no longer lists the definition,
//         and the theorem's no longer reaches it transitively; Relationships
//         still shows the rejected suggestion, labelled; change impact of the
//         definition excludes both. The same after changing the selection
//         and after relaunching the app.
import { mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";
import { invoke, waitFor } from "../workspace-helpers.mjs";

const tool = (id, name, input) => ({ toolUse: { id, name, input } });
const MACRO_DEFS = String.raw`\newtheorem{externaltheorem}{External Theorem}
\newcommand{\hiddenstatement}[1]{\begin{theorem}#1\end{theorem}}
`;
const COVERAGE = String.raw`\documentclass{article}
\usepackage{amsthm}
\newtheorem{theorem}{Theorem}
\input{macro-defs}
\begin{document}
\hiddenstatement{A theorem hidden by a macro.}
\begin{externaltheorem}A declaration whose environment is defined in another file.\end{externaltheorem}
\end{document}
`;

export default async function graphDependencyStatus({ launchApp, artifactsDir, assert }) {
  const base = await mkdtemp(join(tmpdir(), "xp-graph-deps-"));
  const root = join(base, "project");
  const dataDir = join(base, "data");
  await mkdir(root, { recursive: true });
  await mkdir(dataDir, { recursive: true });
  await writeFile(join(root, "notes.md"), "# Energy\n\nNotes on balanced pairs.\n");
  // XR-D-03: the review's two-file fixture (a theorem environment and a macro defined in an input file).
  await writeFile(join(root, "macro-defs.tex"), MACRO_DEFS);
  await writeFile(join(root, "coverage.tex"), COVERAGE);
  const env = { EXPOSITION_CREDENTIAL_NAMESPACE: `e2e-graph-deps-${Date.now()}` };
  const observations = [];
  const note = (id, detail) => {
    observations.push({ id, detail });
    console.log(`ok     ${id}: ${typeof detail === "string" ? detail : JSON.stringify(detail)}`);
  };

  let app = await launchApp({ dataDir, env });
  let page = app.page;
  const shot = (name) => page.screenshot({ path: join(artifactsDir, `${name}.png`) });

  /** The header must say what graph_status says, now (no reopen, no navigation). */
  const headerAgrees = async (when) => {
    let expected = "";
    let shown = "";
    await waitFor(
      async () => {
        const s = await invoke(page, "graph_status");
        expected = `${s.entities} items · ${s.relations} relationships · revision ${s.revision}`;
        shown = ((await page.getByTestId("graph-counts").textContent()) ?? "").trim();
        return shown === expected;
      },
      { timeout: 10_000, message: `header after ${when} (shown "${shown}", status "${expected}")` },
    );
    note(`header agrees after ${when}`, shown);
    return shown;
  };

  const openMathematics = async () => {
    await runCommand(page, "Show Mathematics", /Graph: Show Mathematics/);
    await page.getByTestId("graph-mathematics").waitFor({ timeout: 15_000 });
    await page.getByRole("radio", { name: "Verification" }).click();
    await page.getByRole("grid", { name: "Verification" }).waitFor({ timeout: 10_000 });
  };
  const selectRow = async (id) => {
    const inspector = page.getByTestId("graph-entity-inspector").and(page.locator(`[data-entity-id="${id}"]`));
    // Clicking the selected row again would clear the selection.
    if (!(await inspector.count())) await page.getByRole("grid", { name: "Verification" }).locator(`[data-entity-id="${id}"]`).click();
    await inspector.waitFor({ timeout: 10_000 });
  };
  /** Depends on rows of the selected item: [entity id, status]. */
  const dependsOn = async () => {
    const section = page.getByTestId("graph-entity-inspector");
    await waitFor(async () => (await section.getByText("Tracing dependencies…").count()) === 0, { message: "dependencies traced" });
    return page.getByTestId("graph-depends-on").evaluateAll((els) => els.map((e) => [e.dataset.entityId, e.dataset.status]).sort());
  };
  const expectDependsOn = async (id, expected, when) => {
    await selectRow(id);
    let rows = [];
    await waitFor(async () => JSON.stringify((rows = await dependsOn())) === JSON.stringify([...expected].sort()), {
      timeout: 10_000,
      message: `depends on of ${id} ${when}: ${JSON.stringify(rows)} vs ${JSON.stringify(expected)}`,
    });
    note(`depends on ${when}`, rows);
  };

  try {
    const info = await invoke(page, "app_info");
    if (!info.debugBuild) throw new Error("graph-dependency-status requires a debug build (scripted provider)");
    await page.setViewportSize({ width: 1440, height: 1000 });
    await runCommand(page, "color theme light", /Color Theme: Light/);
    await invoke(page, "agents_provider_update", {
      settings: { provider: "scripted", model: "claude-opus-5-5", effort: "medium", consent: "scopeContent" },
    });
    const opened = await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="notes.md"]').waitFor({ timeout: 15_000 });
    await invoke(page, "trust_grant", { root: opened.root });
    await waitFor(async () => (await invoke(page, "graph_status")).state.kind === "attached", { timeout: 20_000, message: "graph attached" });
    await invoke(page, "graph_reconcile", { force: false });
    await openMathematics();
    await headerAgrees("opening the graph");

    // ---- XR-D-03: the consuming file's coverage is partial, and says why ----------
    const coverageOf = async (path) => (await invoke(page, "graph_inspect_at_source", { request: { path, offset: null, revision: null } })).coverage;
    const consuming = await waitFor(
      async () => {
        const c = await coverageOf("coverage.tex");
        return c && c.state !== "pending" ? c : null;
      },
      { timeout: 20_000, message: "coverage.tex indexed" },
    );
    assert.equal(consuming.state, "partial", JSON.stringify(consuming));
    assert.ok(
      (consuming.detail ?? "").includes("environment externaltheorem is not interpreted (it is not defined in this file; definitions in other files, such as macro-defs, are not read)"),
      JSON.stringify(consuming),
    );
    const defining = await coverageOf("macro-defs.tex");
    assert.equal(defining.state, "partial", JSON.stringify(defining));
    assert.ok((defining.detail ?? "").includes(String.raw`macro \hiddenstatement is not interpreted`), JSON.stringify(defining));
    note("coverage.tex is partial and names the environment from another file", consuming.detail);

    // ---- An agent suggests the dependencies (arrives as an event) ---------------
    await invoke(page, "agents_dev_load_script", {
      script: {
        turns: [
          { steps: [tool("inspect", "graph_query", { query: "inspect", text: "", kinds: [], entity: "", path: "notes.md", depth: 1, limit: 50 })] },
          {
            steps: [
              tool("propose", "graph_propose", {
                summary: "Energy results and their dependencies",
                base_revision: "$ref:inspect/graph/revision",
                operations: [
                  { op: "create_entity", temp_id: "thm", kind: "theorem", label: "Balanced energy", attributes: [], bind_path: "", bind_start: -1, bind_end: -1 },
                  { op: "create_entity", temp_id: "lem", kind: "lemma", label: "Nonnegative energy", attributes: [], bind_path: "", bind_start: -1, bind_end: -1 },
                  { op: "create_entity", temp_id: "def", kind: "definition", label: "Pair energy", attributes: [], bind_path: "", bind_start: -1, bind_end: -1 },
                  { op: "add_relation", relation: "dependsOn", from: "thm", to: "lem", evidence_kind: "", evidence: [], note: "" },
                  { op: "add_relation", relation: "dependsOn", from: "lem", to: "def", evidence_kind: "", evidence: [], note: "" },
                ],
              }),
            ],
          },
          { steps: [{ text: "done" }] },
        ],
      },
    });
    const task = await invoke(page, "agents_task_create", {
      request: { prompt: "Record the energy results", workspaceRoot: root, scope: { kind: "workspace" }, permission: "autoApply", execution: "disabled", consent: "scopeContent", budgets: null },
    });
    await waitFor(
      async () => {
        const t = (await invoke(page, "agents_task_list")).find((x) => x.id === task.id);
        return t && !["queued", "running", "awaitingApproval"].includes(t.status.kind);
      },
      { timeout: 30_000, message: "agent task finishes" },
    );
    const detail = await invoke(page, "agents_task_detail", { task: task.id });
    const proposed = JSON.parse(detail.transcript.find((x) => x.kind === "toolCall" && x.toolUseId === "propose").result);
    assert.equal(proposed.applied, true, JSON.stringify(proposed));
    const id = (t) => proposed.created.find((c) => c.tempId === t).id;
    const [thm, lem, def] = [id("thm"), id("lem"), id("def")];
    await headerAgrees("an agent's suggestions");

    // ---- A person's change through the service (also only an event here) ------
    const status = await invoke(page, "graph_status");
    const proposal = await invoke(page, "graph_propose_change", {
      request: {
        baseRevision: status.revision,
        operations: [{ op: "createEntity", tempId: "a", kind: "assumption", roles: [], label: "Finite pairs", key: null, attributes: {}, binding: null }],
        summary: "e2e: an assumption",
      },
    });
    await invoke(page, "graph_apply_change", { request: { proposal: proposal.id, expectedRevision: proposal.baseRevision } });
    await headerAgrees("a person's change");

    // ---- Depends on before the rejection: suggested, labelled ---------------------
    await expectDependsOn(thm, [[lem, "suggested"], [def, "suggested"]], "of the theorem through suggestions");
    await expectDependsOn(lem, [[def, "suggested"]], "of the lemma through a suggestion");
    await shot("graph-depends-on-suggested-light");

    // ---- Reject lemma → definition in the lemma's inspector ------------------------
    const lemDetail = await invoke(page, "graph_entity", { entity: lem });
    const rejected = lemDetail.outgoing.find((r) => r.to === def && r.kind === "dependsOn");
    const row = page.getByTestId("graph-entity-inspector").locator(`[data-testid="graph-relation"][data-relation-id="${rejected.id}"]`);
    await row.getByRole("button", { name: "Reject suggestion" }).click();
    await waitFor(async () => (await row.getAttribute("data-status")) === "rejected", { timeout: 10_000, message: "the suggestion reads rejected" });
    assert.match(await row.innerText(), /· rejected/);
    await headerAgrees("rejecting in the inspector");
    await expectDependsOn(lem, [], "of the lemma after the rejection");
    assert.match(await page.getByTestId("graph-entity-inspector").innerText(), /No recorded dependencies or assumptions/);
    await shot("graph-depends-on-after-reject-light");
    await expectDependsOn(thm, [[lem, "suggested"]], "of the theorem after the rejection (not transitively)");
    const impact = await invoke(page, "graph_impact", { request: { entities: [def], maxDepth: null, maxItems: null } });
    assert.ok(!impact.items.some((i) => i.entity.id === lem || i.entity.id === thm), JSON.stringify(impact.items.map((i) => i.entity.label)));
    note("change impact of the definition excludes the rejected edge", impact.items.map((i) => i.entity.label));
    // Selection change and back.
    await selectRow(thm);
    await expectDependsOn(lem, [], "of the lemma after changing the selection");

    // ---- Relaunch --------------------------------------------------------------------
    await app.close();
    app = await launchApp({ dataDir, env });
    page = app.page;
    await page.setViewportSize({ width: 1440, height: 1000 });
    await invoke(page, "workspace_open", { path: root });
    await waitFor(async () => (await invoke(page, "graph_status")).state.kind === "attached", { timeout: 20_000, message: "graph attached again" });
    await openMathematics();
    await headerAgrees("relaunching");
    await expectDependsOn(lem, [], "of the lemma after relaunching");
    await expectDependsOn(thm, [[lem, "suggested"]], "of the theorem after relaunching");
    const kept = page.getByTestId("graph-entity-inspector");
    await selectRow(lem);
    assert.match(await kept.locator(`[data-relation-id="${rejected.id}"]`).innerText(), /· rejected/, "the rejected suggestion stays in Relationships, labelled");

    const errors = app.consoleMessages.filter((m) => m.startsWith("error") || m.startsWith("pageerror"));
    assert.deepEqual(errors, [], "no console errors");
  } finally {
    await writeFile(join(artifactsDir, "observations.json"), JSON.stringify(observations, null, 2));
    await app.close();
  }
}
