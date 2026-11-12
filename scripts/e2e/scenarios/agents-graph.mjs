// Agents and the project graph in the real app (debug build) with the
// scripted provider — development only. The scripted provider replaces only
// the model; the task loop, central gate, graph service (SQLite in the
// project), document service, persistence, pages, and UI are the real ones.
//
// Covers: every task receives a scoped graph context (shown on the Agents
// page exactly as the agent got it); a scripted task reads the graph and
// proposes typed entities and a relation — the approval card shows the
// complete payload (every effect and operation) and the relation is applied
// as a suggestion; Editor ↔ Agents navigation keeps one task and its pending
// approval (answered from the page); keeping a task as an investigation;
// metadata-only consent hides labels in the context and in graph_query
// results; a damaged/unavailable graph pauses legacy read_file/edit_document
// with agents.graphUnavailable and refuses new tasks; after the graph is
// recovered as a new graph, the old task is refused (agents.projectChanged);
// a task of project A cannot act in project B. No console errors; reference
// screenshots in both themes (test-results/e2e/agents-graph/ and, with
// EXPOSITION_DEV_HOME, $EXPOSITION_DEV_HOME/ui-reference/agents/).
import { copyFile, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { runCommand } from "../palette.mjs";

const MAIN = "= Notes\n\nLemma: the squares grow quickly.\n";
const LEMMA = "Square monotonicity LEMMA-MARK";
const CLAIM = "Squares grow without bound CLAIM-MARK";

const invoke = (page, cmd, args) =>
  page.evaluate(([c, a]) => window.__TAURI_INTERNALS__.invoke(c, a), [cmd, args ?? {}]);

/** Invoke and capture a refusal instead of throwing. */
const attempt = (page, cmd, args) =>
  page.evaluate(
    async ([c, a]) => {
      try {
        return { ok: true, value: await window.__TAURI_INTERNALS__.invoke(c, a) };
      } catch (error) {
        return { ok: false, error };
      }
    },
    [cmd, args ?? {}],
  );

function consoleErrors(app) {
  return app.consoleMessages.filter((m) => m.startsWith("error:") || m.startsWith("pageerror:"));
}

async function setTheme(page, theme) {
  await runCommand(page, `color theme ${theme}`, theme === "dark" ? /Color Theme: Dark/ : /Color Theme: Light/);
  await page.waitForFunction((t) => document.documentElement.dataset.theme === t, theme);
  await page.waitForTimeout(250);
}

const tool = (id, name, input) => ({ toolUse: { id, name, input } });

async function createTask(page, root, { prompt, permission, consent, scope = { kind: "workspace" } }) {
  return invoke(page, "agents_task_create", {
    request: { prompt, workspaceRoot: root, scope, permission, execution: "disabled", consent, budgets: null },
  });
}

async function waitDone(page, id, timeout = 30_000) {
  const deadline = Date.now() + timeout;
  for (;;) {
    const task = (await invoke(page, "agents_task_list")).find((t) => t.id === id);
    if (task && !["queued", "running", "awaitingApproval"].includes(task.status.kind)) return task;
    if (Date.now() > deadline) throw new Error(`task ${id} did not finish: ${JSON.stringify(task?.status)}`);
    await page.waitForTimeout(150);
  }
}

async function waitStatus(page, id, kind, timeout = 30_000) {
  const deadline = Date.now() + timeout;
  for (;;) {
    const task = (await invoke(page, "agents_task_list")).find((t) => t.id === id);
    if (task?.status.kind === kind) return task;
    if (Date.now() > deadline) throw new Error(`task ${id} is ${JSON.stringify(task?.status)}, not ${kind}`);
    await page.waitForTimeout(100);
  }
}

function toolCall(detail, id) {
  const call = detail.transcript.find((i) => i.kind === "toolCall" && i.toolUseId === id);
  if (!call) throw new Error(`no tool call ${id}`);
  return call;
}

/** Full (unbounded) result of a call, from the transcript entry's JSON. */
function resultOf(call) {
  try {
    return JSON.parse(call.result ?? "null");
  } catch {
    return { raw: call.result };
  }
}

export default async function agentsGraph({ launchApp, artifactsDir, assert }) {
  const base = await mkdtemp(join(tmpdir(), "exposition-agents-graph-"));
  const project = join(base, "paper");
  const other = join(base, "other");
  await mkdir(join(project, "notes"), { recursive: true });
  await mkdir(other, { recursive: true });
  await writeFile(join(project, "main.typ"), MAIN);
  await writeFile(join(project, "notes", "a.typ"), "= A\n\nA note.\n");
  await writeFile(join(other, "main.typ"), "= Other\n");
  const namespace = `e2e-${Date.now()}`;
  const evidence = { project, namespace, steps: [] };
  const step = (name, data = {}) => evidence.steps.push({ name, ...data });
  const shots = [];
  const shot = async (page, name) => {
    const path = join(artifactsDir, `${name}.png`);
    await page.screenshot({ path });
    shots.push(path);
  };

  const app = await launchApp({ env: { EXPOSITION_CREDENTIAL_NAMESPACE: namespace } });
  const info = await app.page.evaluate(() => window.__TAURI_INTERNALS__.invoke("app_info"));
  if (!info.debugBuild) {
    await app.close();
    throw new Error("agents-graph requires a debug build (scripted provider)");
  }
  try {
    const { page } = app;
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.getByTestId("agents-panel").waitFor({ timeout: 15_000 });
    await setTheme(page, "light");
    await invoke(page, "agents_provider_update", {
      settings: { provider: "scripted", model: "claude-opus-5-5", effort: "medium", consent: "scopeContent" },
    });
    const opened = await invoke(page, "workspace_open", { path: project });
    const graph0 = await invoke(page, "graph_status");
    assert.equal(graph0.state.kind, "attached", JSON.stringify(graph0.state));
    step("project opened; its graph attached", { root: opened.root, project: graph0.project, revision: graph0.revision });

    const pagesBar = page.locator('[aria-label="Pages"]');
    const gotoPage = async (name) => {
      await pagesBar.getByRole("button", { name, exact: true }).click();
      await page.waitForTimeout(150);
    };
    const agentsPage = page.getByTestId("agents-page");
    const panel = page.getByTestId("agents-panel");

    // --- Task 1: graph context, graph_query, graph_propose with approval.
    await invoke(page, "agents_dev_load_script", {
      script: {
        turns: [
          { steps: [tool("read", "read_file", { path: "main.typ" })] },
          {
            steps: [
              tool("inspect", "graph_query", {
                query: "inspect",
                text: "",
                kinds: [],
                entity: "",
                path: "main.typ",
                depth: 1,
                limit: 50,
              }),
            ],
          },
          {
            steps: [
              { text: "Recording the lemma and the claim that depends on it." },
              tool("propose", "graph_propose", {
                summary: "Record the lemma and the claim",
                base_revision: "$ref:inspect/graph/revision",
                operations: [
                  { op: "create_entity", temp_id: "lemma", kind: "lemma", label: LEMMA, attributes: [], bind_path: "main.typ", bind_start: 8, bind_end: 14 },
                  { op: "create_entity", temp_id: "claim", kind: "claim", label: CLAIM, attributes: [], bind_path: "main.typ", bind_start: -1, bind_end: -1 },
                  { op: "add_relation", relation: "dependsOn", from: "claim", to: "lemma", evidence_kind: "", evidence: [], note: "" },
                ],
              }),
            ],
          },
          { steps: [{ text: "Recorded a lemma and a claim; the dependency is a suggestion for you to review." }] },
        ],
      },
    });
    const task1 = await createTask(page, opened.root, {
      prompt: "Record the main lemma in the project graph",
      permission: "reviewEdits",
      consent: "scopeContent",
    });
    await waitStatus(page, task1.id, "awaitingApproval");
    let detail = await invoke(page, "agents_task_detail", { task: task1.id });
    const context1 = detail.graph.context;
    assert.ok(context1, "the task has a graph context");
    assert.equal(context1.project, graph0.project, "bound to the open project's graph");
    assert.ok(context1.entities.some((e) => e.kind === "project"), "the project entity is in the context");
    const read = resultOf(toolCall(detail, "read"));
    assert.ok(read.graph?.resource?.startsWith("ent-"), `read_file resolved main.typ to its identity: ${JSON.stringify(read.graph)}`);
    const inspect = resultOf(toolCall(detail, "inspect"));
    assert.equal(inspect.resource?.id, read.graph.resource, "graph_query sees the admitted resource");
    step("task 1 got its graph context; read_file resolved main.typ to a graph identity", {
      contextRevision: context1.revision,
      resource: read.graph.resource,
    });

    // Editor ↔ Agents: one task, one pending approval, visible in both.
    await gotoPage("Agents");
    await agentsPage.waitFor();
    await agentsPage.locator(`[data-testid=agents-page-task][data-task-id="${task1.id}"]`).click();
    const pageApproval = agentsPage.getByTestId("agents-approval");
    await pageApproval.waitFor({ timeout: 15_000 });
    const payload = await agentsPage.getByTestId("agents-approval-payload").textContent();
    for (const needle of ["Effects:", "Suggest:", LEMMA, CLAIM, "Operations (complete):", '"op": "addRelation"', '"op": "createEntity"']) {
      assert.ok(payload.includes(needle), `the approval shows ${needle}: ${payload}`);
    }
    assert.equal(await agentsPage.getByTestId("agents-page-task").count(), 1);
    await page.waitForTimeout(200);
    await shot(page, "agents-graph-approval-page-light");
    await setTheme(page, "dark");
    await shot(page, "agents-graph-approval-page-dark");
    await setTheme(page, "light");
    step("Agents page shows the task and the complete graph_propose payload");
    await gotoPage("Editor");
    await panel.getByTestId("agents-show-tasks").click();
    await panel.getByTestId("agents-task-item").first().click();
    await panel.getByTestId("agents-approval").waitFor({ timeout: 15_000 });
    assert.equal((await invoke(page, "agents_task_list")).length, 1, "one task after navigating");
    const approvalsInPanel = await panel.getByTestId("agents-approval").count();
    assert.equal(approvalsInPanel, 1);
    step("Editor: the Agent panel shows the same task and the same pending approval");
    await gotoPage("Agents");
    await pageApproval.waitFor();
    await agentsPage.getByTestId("agents-approve").click();
    await waitDone(page, task1.id);
    detail = await invoke(page, "agents_task_detail", { task: task1.id });
    const proposed = resultOf(toolCall(detail, "propose"));
    assert.equal(proposed.applied, true, JSON.stringify(proposed));
    const lemmaId = proposed.created.find((c) => c.tempId === "lemma").id;
    const claimId = proposed.created.find((c) => c.tempId === "claim").id;
    const claimDetail = await invoke(page, "graph_entity", { entity: claimId });
    const dependsOn = claimDetail.outgoing.find((r) => r.kind === "dependsOn" && r.to === lemmaId);
    assert.ok(dependsOn, "the relation exists");
    assert.equal(dependsOn.status, "suggested", "agent relations are suggestions");
    assert.equal(dependsOn.origin.kind, "agent");
    assert.equal(dependsOn.origin.taskId, task1.id);
    assert.equal(detail.graph.proposals.at(-1).state, "applied");
    assert.equal((await invoke(page, "agents_task_list")).length, 1, "still one task");
    await agentsPage.getByRole("tab", { name: /Proposals/ }).click();
    await agentsPage.locator("[data-testid=agents-proposal][data-state=applied]").waitFor();
    await shot(page, "agents-graph-proposals-light");
    step("approved on the Agents page; applied once as a suggestion of this task", {
      lemma: lemmaId,
      claim: claimId,
      relation: dependsOn.id,
      status: dependsOn.status,
    });

    // Keep the task as an investigation (the user's deliberate admission).
    await agentsPage.getByTestId("agents-keep-investigation").click();
    await agentsPage.locator("[data-testid=agents-kept-entity][data-kind=investigation]").waitFor({ timeout: 10_000 });
    detail = await invoke(page, "agents_task_detail", { task: task1.id });
    const investigation = await invoke(page, "graph_entity", { entity: detail.graph.investigation });
    assert.equal(investigation.entity.kind, "investigation");
    assert.equal(investigation.entity.attributes.agentTask, task1.id);
    assert.equal(investigation.entity.origin.kind, "user");
    assert.ok(investigation.evidence.some((e) => e.operation.kind === "agentTask" && e.operation.id === task1.id));
    step("task kept as an investigation referencing the task", { investigation: investigation.entity.id });

    // --- Task 2: metadata-only consent hides labels everywhere the agent looks.
    await invoke(page, "agents_dev_load_script", {
      script: {
        turns: [
          {
            steps: [
              tool("search", "graph_query", { query: "search", text: "", kinds: ["lemma", "claim"], entity: "", path: "", depth: 1, limit: 50 }),
              tool("bytext", "graph_query", { query: "search", text: "MARK", kinds: [], entity: "", path: "", depth: 1, limit: 50 }),
              tool("entity", "graph_query", { query: "entity", text: "", kinds: [], entity: claimId, path: "", depth: 1, limit: 50 }),
              tool("readmeta", "read_file", { path: "main.typ" }),
            ],
          },
          { steps: [{ text: "Listed the entities (names withheld)." }] },
        ],
      },
    });
    const task2 = await createTask(page, opened.root, {
      prompt: "Which mathematical entities does the project have?",
      permission: "readOnly",
      consent: "metadataOnly",
    });
    await waitDone(page, task2.id);
    detail = await invoke(page, "agents_task_detail", { task: task2.id });
    const ctx2 = detail.graph.context;
    const ctxLemma = ctx2.entities.find((e) => e.id === lemmaId);
    assert.ok(ctxLemma, "the lemma is in the metadata-only context");
    assert.equal(ctxLemma.label, null, "its label is withheld");
    assert.equal(ctxLemma.kind, "lemma");
    const everything = JSON.stringify(detail);
    for (const mark of ["LEMMA-MARK", "CLAIM-MARK"]) {
      assert.ok(!JSON.stringify(ctx2).includes(mark), `context leaks ${mark}`);
      for (const id of ["search", "bytext", "entity"]) {
        assert.ok(!toolCall(detail, id).result.includes(mark), `${id} leaks ${mark}`);
      }
    }
    assert.deepEqual(resultOf(toolCall(detail, "bytext")).entities, [], "text search never matches labels");
    assert.ok(resultOf(toolCall(detail, "search")).entities.some((e) => e.id === lemmaId), "ids and kinds are given");
    assert.equal(resultOf(toolCall(detail, "readmeta")).code, "agents.metadataOnly");
    assert.ok(everything.length > 0);
    await agentsPage.locator(`[data-testid=agents-page-task][data-task-id="${task2.id}"]`).click();
    await agentsPage.getByRole("tab", { name: /Graph context/ }).click();
    await agentsPage.locator("[data-testid=agents-graph-context][data-consent=metadataOnly]").waitFor();
    await agentsPage.locator(`[data-testid=agents-graph-entity][data-entity-id="${lemmaId}"] [data-testid=agents-graph-withheld]`).waitFor();
    const ctxText = await agentsPage.getByTestId("agents-graph-context").textContent();
    assert.ok(!ctxText.includes("LEMMA-MARK"), "the page shows what the agent got: no label");
    await page.waitForTimeout(200);
    await shot(page, "agents-graph-context-metadata-light");
    await setTheme(page, "dark");
    await shot(page, "agents-graph-context-metadata-dark");
    await setTheme(page, "light");
    step("metadata-only: context and graph_query carry ids/kinds/paths only; read_file refused", {
      contextEntities: ctx2.entities.length,
    });

    // --- Task 3: the graph becomes unavailable while the task waits: legacy
    // file tools pause with agents.graphUnavailable, nothing changes.
    await invoke(page, "agents_dev_load_script", {
      script: {
        turns: [
          {
            steps: [
              { delayMs: 6000 },
              tool("read3", "read_file", { path: "main.typ" }),
              tool("edit3", "edit_document", { path: "main.typ", base_revision: 1, edits: [{ old_text: "quickly", new_text: "SLOWLY" }] }),
            ],
          },
          { steps: [{ text: "The graph is unavailable, so I could not read or change main.typ." }] },
        ],
      },
    });
    const task3 = await createTask(page, opened.root, {
      prompt: "Tighten the lemma's wording",
      permission: "autoApply",
      consent: "scopeContent",
    });
    // Close the project, damage its graph database, and reopen it.
    await invoke(page, "workspace_close");
    const db = join(project, ".exposition", "graph.sqlite3");
    for (const suffix of ["-wal", "-shm"]) await rm(`${db}${suffix}`, { force: true });
    await writeFile(db, "not a database ".repeat(64));
    await invoke(page, "workspace_open", { path: project });
    const damaged = await invoke(page, "graph_status");
    assert.equal(damaged.state.kind, "unavailable", JSON.stringify(damaged.state));
    await waitDone(page, task3.id, 40_000);
    detail = await invoke(page, "agents_task_detail", { task: task3.id });
    for (const id of ["read3", "edit3"]) {
      const call = toolCall(detail, id);
      assert.equal(resultOf(call).code, "agents.graphUnavailable", `${id}: ${call.result}`);
      assert.equal(call.status, "notRun", `${id} did not run`);
    }
    assert.equal(await readFile(join(project, "main.typ"), "utf8"), MAIN, "main.typ unchanged on disk");
    const docs = await invoke(page, "document_list");
    const mainDoc = docs.find((d) => d.path === "main.typ");
    if (mainDoc) {
      const state = await invoke(page, "document_state", { document: mainDoc.id });
      assert.equal(state.text, MAIN, "the open document is unchanged");
    }
    const refused = await attempt(page, "agents_task_create", {
      request: {
        prompt: "Try again",
        workspaceRoot: opened.root,
        scope: { kind: "workspace" },
        permission: "autoApply",
        execution: "disabled",
        consent: "scopeContent",
        budgets: null,
      },
    });
    assert.equal(refused.ok, false);
    assert.equal(refused.error.code, "agents.graphUnavailable", JSON.stringify(refused));
    await agentsPage.locator(`[data-testid=agents-page-task][data-task-id="${task3.id}"]`).click();
    await agentsPage.getByRole("tab", { name: /Run/ }).click();
    await page.waitForTimeout(300);
    await shot(page, "agents-graph-unavailable-light");
    step("graph unavailable: read_file and edit_document paused (agents.graphUnavailable, not run); new tasks refused", {
      reason: damaged.state.reason,
    });

    // Recover as a new graph: the task bound to the old graph identity is refused.
    const recovered = await invoke(page, "graph_recover", { option: { kind: "startNew" } });
    assert.equal(recovered.state.kind, "attached");
    assert.notEqual(recovered.project, graph0.project, "a new graph identity");
    const continued = await attempt(page, "agents_task_continue", { task: task3.id, prompt: "Continue", budgets: null });
    assert.equal(continued.ok, false);
    assert.equal(continued.error.code, "agents.projectChanged", JSON.stringify(continued));
    step("after recovery as a new graph the old task is refused (agents.projectChanged)", { project: recovered.project });

    // --- Cross-project: a task of this project cannot act in another one.
    await invoke(page, "workspace_open", { path: other });
    const elsewhere = await attempt(page, "agents_task_continue", { task: task1.id, prompt: "Continue", budgets: null });
    assert.equal(elsewhere.ok, false);
    assert.equal(elsewhere.error.code, "agents.workspaceNotOpen", JSON.stringify(elsewhere));
    const otherGraph = await invoke(page, "graph_status");
    const leaked = await invoke(page, "graph_snapshot", {
      query: { kinds: null, text: "MARK", around: null, includeInactive: true, limit: 50 },
    });
    assert.equal(leaked.entities.length, 0, "nothing of project A in project B's graph");
    step("project B open: project A's task is refused; B's graph holds nothing of A", { project: otherGraph.project });

    assert.deepEqual(consoleErrors(app), [], "no console errors");
  } finally {
    await app.close();
  }
  await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2));
  const devHome = process.env.EXPOSITION_DEV_HOME;
  if (devHome) {
    const target = join(devHome, "ui-reference", "agents");
    await mkdir(target, { recursive: true });
    for (const path of shots) await copyFile(path, join(target, path.split(/[\\/]/).pop()));
  }
}
