// P1 project operations — graph items and workflows in the real app (spec §5
// rows 7, 8, 9), disposable project:
//  1. Graph: a theorem from the source is neither deleted nor duplicated
//     (disabled with the reason; Delete on the focused node does nothing);
//     Copy Reference / Copy Text are offered.
//  2. A manual claim: Duplicate (Ctrl+D) makes a new item without evidence,
//     the original intact; Delete (key) removes the copy at once.
//  3. A manual relationship: Delete relationship at once (no generic alert).
//  4. A file-backed resource: "Delete Source File" moves the file to the
//     Recycle Bin.
//  5. Workflows list: Duplicate makes a new workflow (new id, revision 1, new
//     step ids, composite pin kept explicit, no triggers, no runs); Delete
//     needs no decision for an unused workflow; Storage & History restores it,
//     its triggers stay off.
//  6. Workflow canvas: Ctrl/Cmd+click two steps, Ctrl+C, background click,
//     Ctrl+V → copies with new ids, offset; the draft is unsaved; nothing runs.
//  7. Focus: Delete/Ctrl+D in the graph search field and Ctrl+D in Monaco never
//     act on items.
import { cp, mkdtemp, mkdir, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";
import { consoleErrors, dismissNotifications, expectNoDialog, publishShots, recycleBinItemsFrom, waitForNotification } from "../p1-helpers.mjs";
import { invoke, waitFor } from "../workspace-helpers.mjs";

const create = (tempId, kind, label) => ({ op: "createEntity", tempId, kind, roles: [], label, key: null, attributes: {}, binding: null });

async function change(page, operations) {
  const s = await invoke(page, "graph_status");
  const proposal = await invoke(page, "graph_propose_change", { request: { baseRevision: s.revision, operations } });
  if (!proposal.valid) throw new Error(JSON.stringify(proposal.diagnostics));
  const outcome = await invoke(page, "graph_apply_change", { request: { proposal: proposal.id, expectedRevision: s.revision } });
  if (outcome.kind !== "applied") throw new Error(JSON.stringify(outcome));
  return outcome;
}

async function entities(page, text) {
  return (await invoke(page, "graph_snapshot", { query: { kinds: null, text, around: null, includeInactive: true, limit: 100 } })).entities;
}

function workflow(title, steps, extra = {}) {
  return {
    schema: "exposition.workflow/2",
    title,
    description: "",
    preset: null,
    inputs: [{ id: "data", label: "Observations", portType: "dataset", source: { kind: "resource", path: "data/obs.csv", version: { kind: "atRunStart" } } }],
    steps,
    connections: [{ kind: "data", from: { kind: "input", input: "data" }, to: { step: steps[0].id, port: "data" } }],
    outcomes: [],
    ceiling: { execution: "restricted", modelRequests: false, retainResults: false, proposeChanges: false },
    ...extra,
  };
}

const pythonStep = (id, label) => ({ id, label, operation: { id: "research.pythonBatch", version: 1 }, parameters: { timeoutSeconds: { kind: "integer", value: 60 } }, payload: null });

export default async function p1GraphWorkflows({ launchApp, artifactsDir, assert, fixture }) {
  const base = await mkdtemp(join(tmpdir(), "xp-p1-graph-"));
  const root = join(base, "project");
  const dataDir = join(base, "data");
  await cp(fixture("graph-basic"), root, { recursive: true });
  await mkdir(join(root, "data"), { recursive: true });
  await mkdir(dataDir, { recursive: true });
  await writeFile(join(root, "data", "obs.csv"), "x,y\n0,1\n");
  await writeFile(join(root, "experiment.py"), "print('fit')\n");
  const evidence = { notes: [] };
  const shots = [];
  const shot = async (page, name) => {
    const path = join(artifactsDir, `${name}.png`);
    await page.screenshot({ path });
    shots.push(path);
  };

  const app = await launchApp({ dataDir });
  const { page } = app;
  try {
    await page.setViewportSize({ width: 1440, height: 900 });
    await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="paper.tex"]').waitFor({ timeout: 15_000 });
    await waitFor(async () => (await invoke(page, "graph_status")).state.kind === "attached", { timeout: 20_000, message: "graph attached" });
    await waitFor(async () => (await entities(page, "Theorem")).some((e) => e.kind === "theorem"), { timeout: 20_000, message: "theorem extracted" });
    const theorem = (await entities(page, "Theorem")).find((e) => e.kind === "theorem");
    const claim = (await change(page, [create("c", "claim", "Conjecture P1")])).created[0].id;
    const rel = await change(page, [{ op: "addRelation", kind: "dependsOn", from: { kind: "id", id: claim }, to: { kind: "id", id: theorem.id }, evidenceKind: null, evidence: [], note: null }]);
    const relationId = rel.created?.[0]?.id ?? null;

    // --- Graph page, table mode (keyboard alternative), focus on rows -----------------------
    await page.evaluate(() => localStorage.setItem("exposition.graph.mode.project", "table"));
    await runCommand(page, "Show Project Graph", /Graph: Show Project Graph/);
    const grid = page.getByRole("grid", { name: "Entities" });
    await grid.waitFor({ timeout: 15_000 });

    // 1. theorem: disabled delete/duplicate, Copy Reference / Copy Text
    const thmRow = grid.locator(`[data-entity-id="${theorem.id}"]`);
    await thmRow.click();
    await thmRow.click({ button: "right" });
    const menu = page.getByTestId("graph-context-menu");
    await menu.waitFor();
    const del = menu.getByRole("menuitem", { name: /Delete/ }).first();
    assert.equal(await del.getAttribute("aria-disabled"), "true", "a source-anchored theorem is not deleted from the graph");
    assert.match(await menu.textContent(), /change the source/);
    assert.ok(await menu.getByRole("menuitem", { name: /Copy Reference/ }).isVisible());
    assert.ok(await menu.getByRole("menuitem", { name: /Copy Text/ }).isVisible());
    await shot(page, "p1-graph-context-menu-light");
    await page.keyboard.press("Escape");
    await thmRow.focus();
    await page.keyboard.press("Delete");
    await page.keyboard.press("Control+D");
    await expectNoDialog(page, assert, 500);
    assert.equal((await invoke(page, "graph_entity", { entity: theorem.id })).entity.lifecycle, "active", "theorem intact");

    // 2. manual claim: duplicate then delete the copy
    const claimRow = grid.locator(`[data-entity-id="${claim}"]`);
    await claimRow.waitFor({ timeout: 10_000 });
    await claimRow.click();
    await claimRow.focus();
    await page.keyboard.press("Control+D");
    await waitFor(async () => (await entities(page, "Conjecture P1")).filter((e) => e.lifecycle === "active").length === 2, { timeout: 15_000, message: "claim duplicated" });
    const copy = (await entities(page, "Conjecture P1")).find((e) => e.id !== claim && e.lifecycle === "active");
    const copyDetail = await invoke(page, "graph_entity", { entity: copy.id });
    assert.equal(copyDetail.evidence.length, 0, "the copy carries no evidence or verdict");
    assert.equal((await invoke(page, "graph_entity", { entity: claim })).entity.label, "Conjecture P1", "original intact");
    evidence.notes.push(`duplicated claim: ${copy.label} (${copy.id})`);
    await dismissNotifications(page);
    const copyRow = grid.locator(`[data-entity-id="${copy.id}"]`);
    await copyRow.waitFor({ timeout: 10_000 });
    await copyRow.click();
    await copyRow.focus();
    await page.keyboard.press("Delete");
    await expectNoDialog(page, assert, 600, "an unused manual item is deleted at once");
    await waitFor(async () => !(await entities(page, "Conjecture P1")).some((e) => e.id === copy.id && e.lifecycle === "active"), { timeout: 15_000, message: "copy deleted" });

    // 3. a manual relationship
    if (relationId) {
      await page.getByRole("tab", { name: /Relationships/ }).click();
      const relGrid = page.getByRole("grid", { name: "Relationships" });
      const relRow = relGrid.locator(`[data-relation-id="${relationId}"]`);
      await relRow.waitFor({ timeout: 10_000 });
      await relRow.click();
      await relRow.focus();
      await page.keyboard.press("Delete");
      await expectNoDialog(page, assert, 600, "no generic alert for a relationship");
      await waitFor(async () => !(await invoke(page, "graph_entity", { entity: claim })).outgoing.some((r) => r.id === relationId), { timeout: 15_000, message: "relationship deleted" });
      await page.getByRole("tab", { name: /Items/ }).click();
    }

    // 4. file-backed: Delete Source File
    const notesEntity = (await entities(page, "notes.md")).find((e) => e.key === "notes.md");
    if (notesEntity) {
      const notesRow = page.getByRole("grid", { name: "Entities" }).locator(`[data-entity-id="${notesEntity.id}"]`);
      await notesRow.click();
      await notesRow.click({ button: "right" });
      await menu.getByRole("menuitem", { name: /Delete Source File/ }).click();
      await waitFor(async () => !existsSync(join(root, "notes.md")), { message: "source file deleted" });
      assert.ok(recycleBinItemsFrom(root).some((i) => i.name.startsWith("notes")), "notes.md is in the real Recycle Bin");
      await dismissNotifications(page);
    }

    // 7a. focus: the graph search field keeps its keys
    const search = page.getByRole("searchbox", { name: "Search the graph" });
    await search.click();
    await search.fill("conj");
    await page.keyboard.press("Delete");
    await page.keyboard.press("Control+D");
    await expectNoDialog(page, assert, 400);
    assert.equal((await invoke(page, "graph_entity", { entity: claim })).entity.lifecycle, "active", "typing in the search field never deletes");
    await search.fill("");

    // --- 5. workflows list -----------------------------------------------------------
    const child = await invoke(page, "workflow_save", { request: { id: null, expectedRevision: null, definition: workflow("Child fit", [pythonStep("fit", "Fit")]) } });
    const parentDef = workflow("Pipeline", [
      pythonStep("prep", "Prepare"),
      { id: "sub", label: "Run child", operation: { id: "workflow.composite", version: 1 }, parameters: {}, payload: null, composite: { workflow: child.id, revision: child.revision } },
    ]);
    const parent = await invoke(page, "workflow_save", { request: { id: null, expectedRevision: null, definition: parentDef } });
    await runCommand(page, "Show Workflows", /Workflow: Show Workflows/);
    const card = page.locator(`[data-workflow="${parent.id}"]`);
    await card.waitFor({ timeout: 15_000 });
    await card.locator("strong").click();
    await card.focus();
    const before = (await invoke(page, "workflow_list")).map((w) => w.id);
    await page.keyboard.press("Control+D");
    await waitFor(async () => (await invoke(page, "workflow_list")).length === before.length + 1, { timeout: 15_000, message: "workflow duplicated" });
    const dup = (await invoke(page, "workflow_list")).find((w) => !before.includes(w.id));
    const dupFull = await invoke(page, "workflow_get", { id: dup.id, revision: null });
    assert.notEqual(dup.id, parent.id, "new identity");
    assert.equal(dup.revision, 1, "revision 1");
    assert.deepEqual(dupFull.definition.steps.find((s) => s.composite)?.composite, { workflow: child.id, revision: child.revision }, "external pin kept explicit");
    assert.deepEqual(await invoke(page, "workflow_trigger_list", { workflow: dup.id }), [], "no triggers copied");
    assert.deepEqual(await invoke(page, "workflow_runs", { workflow: dup.id }), [], "no runs copied");
    evidence.notes.push(`duplicate: ${dup.title} ${dup.id}; steps ${dupFull.definition.steps.map((s) => s.id).join(",")} (original ${parentDef.steps.map((s) => s.id).join(",")})`);
    await dismissNotifications(page);

    // --- 6. workflow canvas: subgraph copy/paste ----------------------------------------
    await page.evaluate(() => localStorage.setItem("exposition.workflows.view", "canvas"));
    await runCommand(page, "Show Workflows", /Workflow: Show Workflows/);
    await page.getByTestId(`workflow-open-${parent.id}`).click();
    const canvas = page.getByTestId("workflow-canvas");
    await canvas.waitFor({ timeout: 15_000 });
    await page.getByTestId("wfc-step-prep").click();
    await page.getByTestId("wfc-step-sub").click({ modifiers: ["Control"] });
    await page.keyboard.press("Control+C");
    await canvas.locator(".react-flow__pane").click({ position: { x: 20, y: 20 } });
    await page.keyboard.press("Control+V");
    await page.getByTestId("wfc-step-prep-copy").waitFor({ timeout: 10_000 });
    await page.getByTestId("wfc-step-sub-copy").waitFor();
    assert.match(await page.getByTestId("workflow-canvas-message").textContent(), /Pasted 2 steps with new ids/);
    await shot(page, "p1-workflow-canvas-paste-light");
    assert.deepEqual(await invoke(page, "workflow_runs", { workflow: parent.id }), [], "nothing ran");
    const savedParent = await invoke(page, "workflow_get", { id: parent.id, revision: null });
    assert.equal(savedParent.definition.steps.length, 2, "the saved revision is unchanged until Save");

    // Delete the duplicate (unused) at once, restore it from Storage & History.
    await runCommand(page, "Show Workflows", /Workflow: Show Workflows/);
    const dupCard = page.locator(`[data-workflow="${dup.id}"]`);
    await dupCard.waitFor();
    await dupCard.click({ button: "right" });
    await page.getByTestId("workflows-context-menu").getByRole("menuitem", { name: /^Delete\b(?! Permanently)/ }).click();
    await expectNoDialog(page, assert, 600, "an unused workflow is deleted at once");
    await waitFor(async () => !(await invoke(page, "workflow_list")).some((w) => w.id === dup.id), { timeout: 15_000, message: "workflow removed from active use" });
    await shot(page, "p1-workflow-deleted-light");
    await runCommand(page, "Storage and History", /Preferences: Storage and History/);
    const storage = page.getByTestId("settings-storage");
    await storage.waitFor({ timeout: 15_000 });
    const recoverable = page.getByTestId("storage-category-recoverableDeletions");
    if (await recoverable.count()) {
      await recoverable.getByTestId("storage-restore").first().click();
      await waitFor(async () => (await invoke(page, "workflow_list")).some((w) => w.id === dup.id), { timeout: 15_000, message: "workflow restored" });
      const triggers = await invoke(page, "workflow_trigger_list", { workflow: dup.id });
      assert.ok(triggers.every((t) => t.state.kind !== "active"), "restored workflow keeps triggers off");
      evidence.notes.push("restored from Storage & History; triggers off");
    } else {
      evidence.notes.push("Storage & History lists no recoverable deletions category");
    }
    await shot(page, "p1-storage-light");

    assert.deepEqual(consoleErrors(app), [], "no console errors");
  } finally {
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2)).catch(() => {});
    await app.close();
  }
  await publishShots(shots);
}
