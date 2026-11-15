// Workflow identity and lifecycle in the real app (debug build; the scripted
// provider replaces only the model): every way to open a saved workflow must
// hand the editor the workflow's own id (wf-…), never a graph entity id.
//
// A. Drafts are created manually (the Formal audit preset, from the palette)
//    and by an agent task (two workflow_propose calls: a first draft with a
//    setting the step does not have, then a corrected revision of the SAME
//    workflow). The tool results state saved / state / runnable / where it is
//    opened; no duplicate draft appears.
// B. Each draft is opened from every entry and the identifier at each
//    boundary is recorded: Research page explorer → inspector, the Editor
//    rail's Research section → inspector, the Workflows list, the Agents
//    page's kept list, a duplicate, an imported definition.
// C. An invalid draft stays editable with an accurate status.
// D. Stale references fail safely with a recovery: a tab whose project is no
//    longer open, a workflow deleted while its tab has unsaved edits, and tabs
//    after closing and reopening the app.
// Nothing runs here (see workflow-agent-audit for the run).
import { mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";
import { invoke, invokeError, waitFor } from "../workspace-helpers.mjs";

const LEAN = "lean/OddSums.lean";
const LEAN_TEXT = `def oddSum : Nat → Nat
  | 0 => 0
  | n + 1 => oddSum n + (2 * n + 1)

theorem oddSum_eq_sq (n : Nat) : oddSum n = n * n := by
  induction n with
  | zero => rfl
  | succ k ih =>
    simp only [oddSum, ih, Nat.add_mul, Nat.mul_add]
    omega
`;
const AGENT_TITLE = "Check OddSums.lean and attach the result to the theorem";

const tool = (id, name, input) => ({ toolUse: { id, name, input } });

/** The agent's draft; `guess` adds a setting the association step does not have. */
export function agentDraft({ workflowId = "", expectedRevision = -1, title = AGENT_TITLE, guess = false } = {}) {
  return {
    workflow_id: workflowId,
    expected_revision: expectedRevision,
    title,
    description: "Audit lean/OddSums.lean and propose its evidence associations for review.",
    preset: "",
    inputs: [{ id: "lean", label: "Lean file", port_type: "formalSource", path: LEAN, pinned_hash: "" }],
    steps: [
      { id: "audit", label: "Check the Lean file", operation: "lean.audit", version: 1, parameters: [], payload: { kind: "none" } },
      {
        id: "associate",
        label: "Propose attaching it to the checked declarations",
        operation: "formal.associateEvidence",
        version: 1,
        parameters: guess ? [{ name: "target", kind: "text", text: "the theorem in main.typ", integer: 0, boolean: false }] : [],
        payload: { kind: "none" },
      },
    ],
    connections: [
      { kind: "data", from_step: "", from_port: "", from_input: "lean", to_step: "audit", to_port: "source" },
      { kind: "data", from_step: "audit", from_port: "evidence", from_input: "", to_step: "associate", to_port: "evidence" },
    ],
    outcomes: [
      { id: "evidence", kind: "auditEvidence", label: "Audited declarations", step: "audit", port: "evidence", optional: false },
      { id: "associations", kind: "reviewableProposal", label: "Evidence associations for review", step: "associate", port: "proposal", optional: true },
    ],
    ceiling: { execution: "trustedNative", model_requests: false, retain_results: true, propose_changes: true },
  };
}

export async function waitTaskDone(page, id, timeout = 60_000) {
  let task;
  await waitFor(
    async () => {
      task = (await invoke(page, "agents_task_list")).find((t) => t.id === id);
      return task && !["queued", "running", "awaitingApproval"].includes(task.status.kind);
    },
    { timeout, interval: 150, message: `task ${id} finishes` },
  );
  return task;
}

export async function toolResults(page, taskId) {
  const detail = await invoke(page, "agents_task_detail", { task: taskId });
  const out = {};
  for (const item of detail.transcript) {
    if (item.kind !== "toolCall") continue;
    let value;
    try {
      value = JSON.parse(item.result ?? "null");
    } catch {
      value = { raw: item.result };
    }
    out[item.toolUseId] = { status: item.status, isError: item.isError, value };
  }
  return out;
}

export default async function workflowIdentity({ launchApp, artifactsDir, assert }) {
  const base = await mkdtemp(join(tmpdir(), "xp-wf-identity-"));
  const root = join(base, "project");
  const other = join(base, "other");
  const dataDir = join(base, "data");
  await mkdir(join(root, "lean"), { recursive: true });
  await mkdir(other, { recursive: true });
  await mkdir(dataDir, { recursive: true });
  await writeFile(join(root, "main.typ"), "= Odd sums\n\nThe sum of the first $n$ odd numbers is $n^2$.\n");
  await writeFile(join(root, ...LEAN.split("/")), LEAN_TEXT);
  await writeFile(join(other, "main.typ"), "= Other\n");

  const observations = [];
  const note = (id, ok, detail) => {
    observations.push({ id, ok: !!ok, detail });
    console.log(`${ok ? "ok    " : "FAIL  "} ${id}: ${typeof detail === "string" ? detail : JSON.stringify(detail)?.slice(0, 900)}`);
  };
  const namespace = `e2e-${Date.now()}`;
  let app = await launchApp({ dataDir, env: { EXPOSITION_CREDENTIAL_NAMESPACE: namespace } });
  let page = app.page;
  const shot = (name) => page.screenshot({ path: join(artifactsDir, `${name}.png`) }).catch(() => {});
  const gotoPage = async (name) => {
    await page.locator('nav[aria-label="Pages"]').getByRole("button", { name, exact: true }).click();
    await page.waitForTimeout(200);
  };
  const tabKeys = () => page.locator("[data-editor-key]").evaluateAll((tabs) => tabs.map((t) => t.getAttribute("data-editor-key")));
  const workflowTabs = async () => (await tabKeys()).filter((k) => k.startsWith("workflow:"));
  const closeTab = async (key) => {
    const tab = page.locator(`[data-editor-key="${key}"]`).first();
    if (!(await tab.count())) return;
    await tab.locator(".ui-editor-tab__close").click();
    await waitFor(async () => !(await tabKeys()).includes(key), { message: `tab ${key} closes` });
  };
  const closeWorkflowTabs = async () => {
    for (const key of await workflowTabs()) await closeTab(key);
  };
  /**
   * What the editor area shows for the workflow tab an entry just opened:
   * the editor (with the id it loaded) or a region state (kind, text, actions).
   */
  const opened = async () => {
    let result;
    await waitFor(
      async () => {
        const editor = page.locator('[data-testid="workflow-editor"]:visible').first();
        if (await editor.count()) {
          result = { ok: true, workflow: await editor.getAttribute("data-workflow"), revision: Number(await editor.getAttribute("data-revision")) };
          return true;
        }
        const state = page.locator(".xp-wf .ui-state:visible, .xp-wf.ui-state:visible").first();
        if ((await state.count()) && (await state.getAttribute("data-kind")) !== "loading") {
          result = {
            ok: false,
            kind: await state.getAttribute("data-kind"),
            text: (await state.textContent())?.trim(),
            actions: await state.getByRole("button").evaluateAll((buttons) => buttons.map((b) => b.textContent?.trim())),
          };
          return true;
        }
        return false;
      },
      { timeout: 15_000, message: "the workflow tab shows the editor or a region state" },
    );
    return { ...result, tabs: await workflowTabs() };
  };
  /** Open through `entry`, record what the tab got, and close it again. */
  const openVia = async (id, expected, entry) => {
    await closeWorkflowTabs();
    await entry();
    const result = await opened();
    const ok = result.ok && result.workflow === expected.id && result.tabs.length === 1 && result.tabs[0] === `workflow:${expected.id}`;
    note(id, ok, { expected: expected.id, entity: expected.entity, ...result });
    if (!ok) await shot(`fail-${id}`);
    await closeWorkflowTabs();
    return result;
  };

  try {
    await page.setViewportSize({ width: 1440, height: 950 });
    const info = await invoke(page, "app_info");
    if (!info.debugBuild) throw new Error("workflow-identity requires a debug build (scripted provider)");
    await invoke(page, "agents_provider_update", {
      settings: { provider: "scripted", model: "claude-opus-5-5", effort: "medium", consent: "scopeContent" },
    });
    const ws = await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="main.typ"]').waitFor({ timeout: 15_000 });
    await waitFor(async () => (await invoke(page, "graph_status")).state.kind === "attached", { timeout: 20_000, message: "graph attached" });
    await invoke(page, "trust_grant", { root: ws.root });
    await invoke(page, "research_space_enable");

    // ----- A. manual draft (palette → Formal audit preset) -------------------------------
    await runCommand(page, "New Formal Audit Draft");
    const manualOpen = await opened();
    const list0 = await invoke(page, "workflow_list");
    const manual = list0[0];
    note("manual-preset-draft-opens-with-its-own-id", manualOpen.ok && list0.length === 1 && manualOpen.workflow === manual.id && manualOpen.tabs[0] === `workflow:${manual.id}`, {
      ...manualOpen,
      saved: { id: manual.id, entity: manual.entity, state: manual.state, errors: manual.errors },
    });
    await closeWorkflowTabs();

    // ----- A. agent drafts: a guessed setting, then a corrected revision ------------------
    await invoke(page, "agents_dev_load_script", {
      script: {
        turns: [
          { steps: [tool("wf1", "workflow_propose", agentDraft({ guess: true }))] },
          { steps: [tool("wf2", "workflow_propose", agentDraft({ workflowId: "$ref:wf1/workflow", expectedRevision: "$ref:wf1/revision" }))] },
          { steps: [{ text: "Drafted the workflow." }] },
        ],
      },
    });
    const task = await invoke(page, "agents_task_create", {
      request: { prompt: "Draft the audit workflow", workspaceRoot: ws.root, scope: { kind: "workspace" }, permission: "autoApply", execution: "approveEach", consent: "scopeContent", budgets: null },
    });
    const finished = await waitTaskDone(page, task.id);
    const results = await toolResults(page, task.id);
    const r1 = results.wf1?.value ?? {};
    const r2 = results.wf2?.value ?? {};
    const list1 = await invoke(page, "workflow_list");
    const agentWf = list1.find((w) => w.id === r1.workflow);
    note(
      "agent-first-propose-saved-an-invalid-draft-and-says-so",
      r1.saved === true && r1.state === "incomplete" && r1.errors >= 1 && r1.reviewable === false && r1.ran === false && /INCOMPLETE/.test(r1.summary ?? "") && r1.revision === 1 && /^wf-/.test(r1.workflow ?? ""),
      { task: finished.status, result: r1 },
    );
    note(
      "agent-corrected-propose-is-a-new-revision-of-the-same-workflow",
      r2.saved === true && r2.workflow === r1.workflow && r2.revision === 2 && r2.state === "wellFormed" && r2.errors === 0 && r2.reviewable === true && list1.length === 2,
      { result: { ...r2, diagnostics: undefined }, workflows: list1.map((w) => [w.id, w.revision, w.title, w.state, w.origin.kind]) },
    );
    // What the model is told, exactly (saved vs valid vs runnable vs where).
    note(
      "agent-result-states-saved-valid-runnable-and-where-distinctly",
      typeof r2.title === "string" && typeof r2.summary === "string" && /Workflows/.test(r2.open ?? "") && /plan/i.test(r2.run ?? "") && r2.ran === false,
      { title: r2.title, summary: r2.summary, next: r2.next, open: r2.open, run: r2.run, ran: r2.ran, reviewable: r2.reviewable },
    );
    const listing = await invoke(page, "research_listing");
    const item = (wf) => listing.workflows.find((w) => w.entity === wf.entity);
    note(
      "research-listing-carries-the-workflow-id",
      listing.workflows.length === 2 && listing.workflows.every((w) => /^ent-/.test(w.entity)) && item(manual)?.workflow === manual.id && item(agentWf)?.workflow === agentWf.id,
      listing.workflows,
    );
    // The backend contract stays strict: an entity id is not a workflow id.
    const refused = await invokeError(page, "workflow_get", { id: agentWf.entity, revision: null });
    note("workflow-get-still-refuses-an-entity-id", "error" in refused && /invalid workflow identifier/.test(JSON.stringify(refused.error)), refused);

    // ----- B. open from every entry -------------------------------------------------------
    for (const [name, wf] of [
      ["manual", manual],
      ["agent", agentWf],
    ]) {
      const label = wf.title;
      await openVia(`open-${name}-from-research-page-inspector`, wf, async () => {
        await gotoPage("Research");
        const tree = page.getByTestId("research-tree");
        await tree.locator(`[data-item-key="workflow:${wf.entity}"]`).first().locator(".ui-tree-item__title").click();
        const inspector = page.locator('[data-testid="research-inspector"][data-kind="workflow"]');
        await inspector.waitFor({ timeout: 10_000 });
        assert.equal(await inspector.getAttribute("data-entity"), wf.entity);
        await inspector.getByRole("button", { name: "Open in the workflow editor" }).click();
      });
      await openVia(`open-${name}-from-editor-rail-research-section`, wf, async () => {
        await gotoPage("Editor");
        const section = page.locator('[data-testid="files-research"]');
        await section.waitFor({ timeout: 10_000 });
        const row = section.locator(`[data-research-item="workflow:${wf.entity}"]`).first();
        if (!(await row.count())) await section.locator('[data-research-group="workflows"] .ui-tree-item__chevron').first().click();
        await row.locator(".ui-tree-item__title").click();
        const inspector = page.locator('[data-testid="research-inspector"][data-kind="workflow"]');
        await inspector.waitFor({ timeout: 10_000 });
        await inspector.getByRole("button", { name: "Open in the workflow editor" }).click();
      });
      await openVia(`open-${name}-from-workflows-list`, wf, async () => {
        await gotoPage("Editor");
        await runCommand(page, "Show Workflows");
        await page.getByTestId(`workflow-open-${wf.id}`).click();
      });
      await openVia(`open-${name}-from-agents-kept-list`, wf, async () => {
        await gotoPage("Agents");
        const row = page.locator(`[data-testid="agents-kept-entity"][data-kind="workflow"][data-entity-id="${wf.entity}"]`);
        await row.waitFor({ timeout: 10_000 });
        assert.ok((await row.textContent()).includes(label));
        await row.getByTestId("agents-kept-open-workflow").click();
      });
    }
    await gotoPage("Editor");
    await page.locator('[data-editor-key="workflows:"]').first().click();
    await shot("workflows-list");

    // A duplicate and an imported definition are new workflows with their own ids.
    const before = (await invoke(page, "workflow_list")).map((w) => w.id);
    await page.locator(`[data-testid="workflow-drafts"] [data-workflow="${agentWf.id}"]`).first().click();
    await page.keyboard.press("Control+D");
    await waitFor(async () => (await invoke(page, "workflow_list")).length === before.length + 1, { timeout: 15_000, message: "workflow duplicated" });
    const dup = (await invoke(page, "workflow_list")).find((w) => !before.includes(w.id));
    await openVia("open-duplicate-from-workflows-list", dup, async () => {
      await page.getByTestId(`workflow-open-${dup.id}`).click();
    });
    await openVia("open-duplicate-from-research-page-inspector", dup, async () => {
      await gotoPage("Research");
      await page.getByTestId("research-tree").locator(`[data-item-key="workflow:${dup.entity}"]`).first().locator(".ui-tree-item__title").click();
      await page.locator('[data-testid="research-inspector"][data-kind="workflow"]').getByRole("button", { name: "Open in the workflow editor" }).click();
    });
    const full = await invoke(page, "workflow_get", { id: agentWf.id, revision: null });
    const imported = await invoke(page, "workflow_import", { text: JSON.stringify({ ...full.definition, title: "Imported audit" }) });
    await openVia("open-imported-from-research-page-inspector", imported, async () => {
      await gotoPage("Research");
      await page.getByTestId("research-tree").locator(`[data-item-key="workflow:${imported.entity}"]`).first().locator(".ui-tree-item__title").click();
      await page.locator('[data-testid="research-inspector"][data-kind="workflow"]').getByRole("button", { name: "Open in the workflow editor" }).click();
    });

    // ----- C. an invalid draft stays editable with an accurate status ---------------------
    await gotoPage("Editor");
    await page.locator('[data-editor-key="workflows:"]').first().click();
    await page.getByTestId(`workflow-open-${agentWf.id}`).click();
    await opened();
    const rev1 = await invoke(page, "workflow_get", { id: agentWf.id, revision: 1 });
    note(
      "invalid-first-revision-is-kept-with-its-diagnostics",
      rev1.validation.state === "incomplete" && rev1.validation.errors >= 1 && rev1.validation.diagnostics.some((d) => d.step === "associate"),
      rev1.validation.diagnostics.map((d) => [d.severity, d.code, d.message]),
    );
    // Make the open (valid) draft invalid again in the editor and save it.
    const title = page.locator('[data-testid="workflow-editor"]:visible').getByTestId("workflow-title").locator("input");
    await title.fill("");
    await page.getByTestId("workflow-save").click();
    await waitFor(async () => (await invoke(page, "workflow_get", { id: agentWf.id, revision: null })).revision === 3, { timeout: 15_000, message: "invalid revision saved" });
    const status = page.locator('[data-testid="workflow-editor"]:visible [data-testid="workflow-status"]');
    await waitFor(async () => (await status.getAttribute("data-state")) === "incomplete", { message: "status follows the saved revision" });
    const reason = await page.locator('[data-testid="workflow-editor"]:visible [data-testid="workflow-run-reason"]').textContent();
    note(
      "invalid-draft-is-saved-editable-and-not-reviewable",
      (await status.getAttribute("data-can-review")) === "false" && (await status.getAttribute("data-runnable")) === "false" && /incomplete/i.test(reason ?? ""),
      { status: await status.textContent(), reason },
    );
    await shot("invalid-draft-editable");
    await title.fill(AGENT_TITLE);
    await page.getByTestId("workflow-save").click();
    await waitFor(async () => (await status.getAttribute("data-state")) === "wellFormed", { timeout: 15_000, message: "fixed again" });
    await closeWorkflowTabs();

    // ----- D. stale references ----------------------------------------------------------
    // D1. A workflow deleted while its tab has unsaved edits: saving cannot bring it back silently.
    await page.getByTestId(`workflow-open-${dup.id}`).click();
    await opened();
    await page.locator('[data-testid="workflow-editor"]:visible').getByTestId("workflow-title").locator("input").fill("Edited after copy");
    await waitFor(async () => (await page.locator(`[data-editor-key="workflow:${dup.id}"]`).first().getAttribute("data-dirty")) === "true", { message: "tab is dirty" });
    await page.locator('[data-editor-key="workflows:"]').first().click();
    await page.locator(`[data-testid="workflow-drafts"] [data-workflow="${dup.id}"]`).first().click({ button: "right" });
    await page.getByTestId("workflows-context-menu").getByRole("menuitem", { name: /^Delete\b(?! Permanently)/ }).click();
    const decision = page.getByTestId("op-decision");
    if (await decision.waitFor({ timeout: 2_000 }).then(() => true, () => false)) {
      note("deleting-a-workflow-with-unsaved-edits-asks-once", true, (await decision.textContent())?.slice(0, 300));
      await decision.getByRole("button", { name: /^Delete/ }).click();
    }
    await waitFor(async () => !(await invoke(page, "workflow_list")).some((w) => w.id === dup.id), { timeout: 15_000, message: "duplicate deleted" });
    const staleTabs = await workflowTabs();
    let deletedView = null;
    if (staleTabs.includes(`workflow:${dup.id}`)) {
      await page.locator(`[data-editor-key="workflow:${dup.id}"]`).first().click();
      await page.waitForTimeout(500);
      const gone = page.locator('[data-testid="workflow-missing"]:visible');
      if (await gone.count()) {
        deletedView = { kind: "missing", text: (await gone.textContent())?.trim(), actions: await gone.getByRole("button").evaluateAll((b) => b.map((x) => x.textContent?.trim())) };
      } else {
        // The draft is still shown: saving must say that the workflow is gone.
        await page.getByTestId("workflow-save").click().catch(() => {});
        await page.waitForTimeout(800);
        deletedView = { kind: "editor", notSaved: await page.locator(".xp-wf:visible .ui-banner[data-tone=danger]").first().textContent().catch(() => null) };
      }
    }
    note(
      "deleted-workflow-tab-keeps-edits-and-save-says-it-was-deleted",
      // A clean tab is closed with its workflow; a tab with unsaved edits keeps them and Save says why it cannot save.
      deletedView === null ||
        (deletedView.kind === "missing" && deletedView.actions.includes("Show Workflows")) ||
        (deletedView.kind === "editor" && /deleted from this project/.test(deletedView.notSaved ?? "") && !/invalid args/.test(deletedView.notSaved ?? "")),
      { tabsAfterDelete: staleTabs, deletedView },
    );
    await shot("deleted-workflow-tab");
    // A second view of that tab has to load the workflow again: it is gone, and the region says so with a recovery.
    const missingState = async () => {
      const state = page.locator('[data-testid="workflow-missing"]:visible').first();
      if (!(await state.waitFor({ timeout: 10_000 }).then(() => true, () => false))) {
        return { shown: false, text: (await page.locator(".xp-wf:visible").last().textContent().catch(() => null))?.slice(0, 300) };
      }
      return { shown: true, text: (await state.textContent())?.trim(), actions: await state.getByRole("button").evaluateAll((b) => b.map((x) => x.textContent?.trim())) };
    };
    if (staleTabs.includes(`workflow:${dup.id}`)) {
      await runCommand(page, "Split Editor Right");
      const gone = await missingState();
      note(
        "a-new-view-of-a-deleted-workflow-says-so-with-recovery",
        gone.shown && /This workflow is not in the open project/.test(gone.text) && /deleted from this project/.test(gone.text) && gone.actions.includes("Show Workflows") && gone.actions.includes("Close") && !gone.actions.includes("Retry"),
        gone,
      );
      await shot("deleted-workflow-missing-state");
      await runCommand(page, "Close Right Editor Group");
    }
    // Discard and close whatever is left of it.
    for (const key of await workflowTabs()) {
      await page.locator(`[data-editor-key="${key}"]`).first().locator(".ui-editor-tab__close").click();
      const ask = page.getByRole("alertdialog");
      if (await ask.waitFor({ timeout: 1_000 }).then(() => true, () => false)) await ask.getByRole("button", { name: /Don.t Save|Discard/ }).click();
    }
    await waitFor(async () => (await workflowTabs()).length === 0, { message: "workflow tabs closed" });

    // D2. A tab of project A after project B is opened: no edit, no cross-project load.
    await page.getByTestId(`workflow-open-${agentWf.id}`).click();
    await opened();
    await invoke(page, "workspace_open", { path: other });
    await waitFor(async () => (await invoke(page, "graph_status")).state.kind === "attached", { timeout: 20_000, message: "other graph attached" });
    await page.waitForTimeout(500);
    const detached = await page.locator(".xp-wf:visible").first().textContent().catch(() => null);
    // Leave the Editor page and come back (the tab is mounted again).
    await gotoPage("Research");
    await gotoPage("Editor");
    await page.waitForTimeout(800);
    const tabStill = (await workflowTabs()).includes(`workflow:${agentWf.id}`);
    const remounted = tabStill ? await opened().catch((e) => ({ ok: false, text: String(e) })) : null;
    const otherList = await invoke(page, "workflow_list");
    note(
      "other-project-tab-fails-safely-with-recovery",
      otherList.length === 0 && (!tabStill || (remounted && !remounted.ok && !/invalid args|workflow_get/.test(remounted.text ?? "") && (remounted.actions ?? []).length > 0)),
      { detached, remounted, otherProjectWorkflows: otherList.length },
    );
    await shot("other-project-tab");
    if (tabStill) {
      // A new view of the tab loads in the project that is open now: the workflow is not there.
      await page.locator(`[data-editor-key="workflow:${agentWf.id}"]`).first().click();
      await runCommand(page, "Split Editor Right");
      const elsewhere = await missingState();
      note(
        "a-new-view-of-another-projects-workflow-says-so-with-recovery",
        elsewhere.shown && /This workflow is not in the open project/.test(elsewhere.text) && !/invalid args|workflow_get/.test(elsewhere.text) && elsewhere.actions.includes("Show Workflows"),
        elsewhere,
      );
      await shot("other-project-missing-state");
      await page.locator('[data-testid="workflow-missing"]:visible').first().getByRole("button", { name: "Show Workflows" }).click();
      await page.locator('[data-testid="workflows-view"]:visible').first().waitFor({ timeout: 10_000 });
      note("show-workflows-recovery-opens-the-open-projects-list", (await invoke(page, "workflow_list")).length === 0, await tabKeys());
      await runCommand(page, "Close Right Editor Group").catch(() => {});
    }
    await closeWorkflowTabs();

    // D3. Close and reopen the app and the project: what a restored session shows.
    await app.close();
    app = await launchApp({ dataDir, env: { EXPOSITION_CREDENTIAL_NAMESPACE: namespace } });
    page = app.page;
    await page.setViewportSize({ width: 1440, height: 950 });
    await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="main.typ"]').waitFor({ timeout: 15_000 });
    await waitFor(async () => (await invoke(page, "graph_status")).state.kind === "attached", { timeout: 20_000, message: "graph attached after restart" });
    const restoredTabs = await tabKeys();
    const list2 = await invoke(page, "workflow_list");
    const again = list2.find((w) => w.id === agentWf.id);
    note("after-restart-the-workflow-keeps-its-id-and-revisions", again && again.entity === agentWf.entity && again.revision === 4 && again.state === "wellFormed", {
      restoredTabs,
      workflows: list2.map((w) => [w.id, w.revision, w.title, w.state]),
    });
    await openVia("open-agent-after-restart-from-research-page-inspector", agentWf, async () => {
      await gotoPage("Research");
      await page.getByTestId("research-tree").locator(`[data-item-key="workflow:${agentWf.entity}"]`).first().locator(".ui-tree-item__title").click();
      await page.locator('[data-testid="research-inspector"][data-kind="workflow"]').getByRole("button", { name: "Open in the workflow editor" }).click();
    });
    const errors = app.consoleMessages.filter((m) => m.startsWith("pageerror:"));
    note("no-page-errors", errors.length === 0, errors.slice(0, 5));
  } finally {
    await writeFile(join(artifactsDir, "observations.json"), JSON.stringify(observations, null, 2));
    await app.close();
  }
  const failed = observations.filter((o) => !o.ok);
  assert.deepEqual(failed.map((d) => d.id), [], `violations (see ${join(artifactsDir, "observations.json")})`);
}
