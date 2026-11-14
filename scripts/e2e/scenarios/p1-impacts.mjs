// P1 project operations — deletions with concrete consequences (spec §5 rows
// 3, 4, 5), in the real app on a disposable project:
// Order: D, E, F first (file side), then A, B, C (data side).
//  A. A bibliography the manuscript uses: ONE decision naming the manuscript;
//     Cancel changes nothing; Proceed deletes it without touching the
//     manuscript (no cascade).
//  B. A required workflow input: the decision names the workflow; after
//     proceeding the workflow still exists, unchanged, and is not runnable.
//  C. A batch (bibliography + workflow input + unused file): exactly one
//     combined decision for the batch.
//  D. A dirty buffer: the decision offers keep-in-editor/discard; keeping
//     preserves the typed text, the file stays deleted (no resurrection).
//  E. Partial failure: one file held open without sharing by another process
//     (real Windows sharing violation): the other is deleted, the locked one is
//     reported with its reason — no false success, nothing permanent.
//  F. Interruption: the app is killed right after a delete and relaunched;
//     disk and graph agree after reconciliation.
// Screens: impact decision (light/dark, details open), partial failure.
import { spawn } from "node:child_process";
import { cp, mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import {
  consoleErrors,
  decision,
  dismissNotifications,
  expectNoDialog,
  fileRow,
  publishShots,
  recycleBinItemsFrom,
  selectFiles,
  setTheme,
  waitForNotification, settle, platformSkip } from "../p1-helpers.mjs";
import { focusEditor, invoke, monacoState, openInEditor, waitFor } from "../workspace-helpers.mjs";

const OBS = "x,y\n0,1\n1,3\n";

function workflow() {
  const input = (id, label, portType, path) => ({ id, label, portType, source: { kind: "resource", path, version: { kind: "atRunStart" } } });
  return {
    schema: "exposition.workflow/1",
    title: "Fit observations",
    description: "",
    preset: null,
    inputs: [input("script", "Python script", "codeSource", "experiment.py"), input("data", "Observations", "dataset", "data/obs.csv")],
    steps: [{ id: "run", label: "Run the experiment", operation: { id: "research.pythonBatch", version: 1 }, parameters: { timeoutSeconds: { kind: "integer", value: 300 } }, payload: null }],
    connections: [
      { kind: "data", from: { kind: "input", input: "script" }, to: { step: "run", port: "script" } },
      { kind: "data", from: { kind: "input", input: "data" }, to: { step: "run", port: "data" } },
    ],
    outcomes: [{ id: "out", kind: "transformedDataset", label: "Result", from: { step: "run", port: "dataset" }, optional: false }],
    ceiling: { execution: "restricted", modelRequests: false, retainResults: false, proposeChanges: false },
  };
}

/** Hold a file open with no sharing (a real Windows sharing violation for delete/move). */
function lockFile(path) {
  const script = `$f=[System.IO.File]::Open('${path.replace(/'/g, "''")}','Open','Read','None'); Write-Output 'locked'; Start-Sleep -Seconds 120; $f.Close()`;
  const child = spawn("powershell", ["-NoProfile", "-NonInteractive", "-Command", script], { stdio: ["ignore", "pipe", "ignore"] });
  const ready = new Promise((resolve) => child.stdout.on("data", (d) => String(d).includes("locked") && resolve()));
  return { child, ready };
}

async function openProject(page, root) {
  await invoke(page, "workspace_open", { path: root });
  await page.locator('[data-testid="file-tree"] [data-path="paper.tex"]').waitFor({ timeout: 15_000 });
  await waitFor(async () => (await invoke(page, "graph_status")).state.kind === "attached", { timeout: 20_000, message: "graph attached" });
  // Let indexing record the manuscript's bibliography use.
  await waitFor(async () => {
    const s = await invoke(page, "graph_snapshot", { query: { kinds: null, text: "refs.bib", around: null, includeInactive: false, limit: 20 } });
    return s.entities.some((e) => e.key === "refs.bib");
  }, { timeout: 20_000, message: "refs.bib indexed" });
}

export default async function p1Impacts({ launchApp, artifactsDir, assert, fixture }) {
  const skip = platformSkip();
  if (skip) {
    console.log(`SKIPPED: p1-impacts ${skip}`);
    return;
  }
  const base = await mkdtemp(join(tmpdir(), "xp-p1-impacts-"));
  const root = join(base, "project");
  const dataDir = join(base, "data");
  await cp(fixture("graph-basic"), root, { recursive: true });
  await mkdir(join(root, "data"), { recursive: true });
  await mkdir(join(root, "scratch"), { recursive: true });
  await mkdir(dataDir, { recursive: true });
  await writeFile(join(root, "data", "obs.csv"), OBS);
  await writeFile(join(root, "experiment.py"), "print('fit')\n");
  for (const n of ["free.txt", "locked.txt", "unused.txt", "x.txt"]) await writeFile(join(root, "scratch", n), `${n}\n`);
  const paper = await readFile(join(root, "paper.tex"), "utf8");
  const evidence = { real: [], notes: [] };
  const shots = [];
  const shot = async (page, name) => {
    const path = join(artifactsDir, `${name}.png`);
    await settle(page);
    await page.screenshot({ path });
    shots.push(path);
  };

  let app = await launchApp({ dataDir });
  let locker = null;
  try {
    let { page } = app;
    await page.setViewportSize({ width: 1440, height: 900 });
    await openProject(page, root);
    const saved = await invoke(page, "workflow_save", { request: { id: null, expectedRevision: null, definition: workflow() } });

    // --- D. a dirty buffer ----------------------------------------------------------------
    await openInEditor(page, "notes.md");
    await focusEditor(page, "notes.md", "end");
    await page.keyboard.type(" UNSAVED-P1");
    await waitFor(async () => (await monacoState(page, "notes.md"))?.text.includes("UNSAVED-P1"), { message: "typed" });
    await selectFiles(page, ["notes.md"]);
    await page.keyboard.press("Delete");
    await decision(page).waitFor({ timeout: 15_000 });
    assert.ok((await decision(page).locator('[data-testid="op-impact"][data-kind="unsavedBuffer"]').count()) >= 1, "unsaved buffer impact");
    const choice = decision(page).locator("[data-choice]");
    evidence.notes.push(`dirty buffer choices: ${await choice.evaluateAll((els) => els.map((e) => `${e.getAttribute("data-choice")}=${e.getAttribute("data-value")}`).join(", "))}`);
    await shot(page, "p1-impact-unsaved-light");
    await decision(page).getByRole("button", { name: /^Delete$/ }).click();
    await waitFor(async () => !existsSync(join(root, "notes.md")), { message: "notes.md deleted" });
    await page.waitForTimeout(3000);
    assert.ok(!existsSync(join(root, "notes.md")), "no autosave resurrection");
    const kept = await monacoState(page, "notes.md");
    assert.ok(kept?.text.includes("UNSAVED-P1"), "the typed text is preserved in the editor");
    evidence.notes.push("dirty buffer kept in the editor; the file stayed deleted for 3 s");
    await dismissNotifications(page);

    // --- E. partial failure (real sharing violation) -------------------------------------
    if (process.platform === "win32") {
      locker = lockFile(join(root, "scratch", "locked.txt"));
      await locker.ready;
      await selectFiles(page, ["scratch/free.txt", "scratch/locked.txt"]);
      await page.keyboard.press("Delete");
      const partial = await waitForNotification(page, /1 of 2 items|Could not delete/, { timeout: 30_000 });
      assert.equal(partial.tone, "warning", "partial success is a warning, not a success");
      assert.match(partial.body, /locked\.txt/, "the failed file is named with its reason");
      await waitFor(async () => !existsSync(join(root, "scratch", "free.txt")), { message: "free.txt deleted" });
      assert.ok(existsSync(join(root, "scratch", "locked.txt")), "locked.txt still there (nothing permanent, no false success)");
      assert.ok(!recycleBinItemsFrom(join(root, "scratch")).some((i) => i.name.startsWith("locked")), "the locked file is not in the Recycle Bin");
      evidence.real.push("Windows sharing violation (FileShare.None held by another process) failed one target; the other went to the Recycle Bin");
      await shot(page, "p1-partial-failure-light");
      locker.child.kill();
      locker = null;
      await dismissNotifications(page);
    }

    // --- F. interruption: kill right after a delete, relaunch, reconcile ------------------
    await selectFiles(page, ["scratch/x.txt"]);
    await page.keyboard.press("Delete");
    await page.waitForTimeout(150);
    await app.close();
    app = await launchApp({ dataDir });
    page = app.page;
    await page.setViewportSize({ width: 1440, height: 900 });
    await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"]').waitFor({ timeout: 15_000 });
    await waitFor(async () => (await invoke(page, "graph_status")).state.kind === "attached", { timeout: 20_000, message: "graph attached after restart" });
    const onDisk = existsSync(join(root, "scratch", "x.txt"));
    await waitFor(
      async () => {
        const s = await invoke(page, "graph_snapshot", { query: { kinds: null, text: "x.txt", around: null, includeInactive: true, limit: 20 } });
        const active = s.entities.some((e) => e.key === "scratch/x.txt" && e.lifecycle === "active");
        return active === onDisk;
      },
      { timeout: 30_000, message: "graph agrees with the disk after restart" },
    );
    evidence.notes.push(`after a kill right after Delete: x.txt on disk=${onDisk}; graph agrees`);

    // Data-side consequences (manuscript, workflow input, batch) come from the graph lane.
    // --- A. manuscript dependency: one decision; Cancel changes nothing --------------------
    await selectFiles(page, ["refs.bib"]);
    await page.keyboard.press("Delete");
    const dialog = decision(page);
    await dialog.waitFor({ timeout: 15_000 });
    assert.equal(await page.getByRole("alertdialog").count(), 1, "one decision");
    const text = await dialog.textContent();
    assert.match(text, /paper\.tex/, "names the manuscript that uses it");
    assert.doesNotMatch(text, /are you sure/i);
    assert.ok((await dialog.locator('[data-testid="op-impact"][data-kind="manuscriptDependency"]').count()) >= 1, "manuscript impact listed");
    await shot(page, "p1-impact-decision-light");
    await dialog.getByRole("button", { name: /^Details/ }).click();
    await shot(page, "p1-impact-decision-details-light");
    await dialog.getByRole("button", { name: "Cancel" }).click();
    await page.waitForTimeout(400);
    assert.ok(existsSync(join(root, "refs.bib")), "Cancel changed nothing");
    // Proceed: the bibliography goes, the manuscript is untouched.
    await selectFiles(page, ["refs.bib"]);
    await page.keyboard.press("Delete");
    await decision(page).waitFor();
    await setTheme(page, "dark");
    await shot(page, "p1-impact-decision-dark");
    await setTheme(page, "light");
    await decision(page).getByRole("button", { name: /^Delete$/ }).click();
    await waitFor(async () => !existsSync(join(root, "refs.bib")), { message: "refs.bib deleted after the decision" });
    assert.equal(await readFile(join(root, "paper.tex"), "utf8"), paper, "no cascade into the manuscript");
    evidence.notes.push("manuscript dependency: one decision naming paper.tex; Cancel kept the file; Delete removed only refs.bib");
    await dismissNotifications(page);

    // --- B. a required workflow input ------------------------------------------------------
    await selectFiles(page, ["data/obs.csv"]);
    await page.keyboard.press("Delete");
    await decision(page).waitFor({ timeout: 15_000 });
    assert.match(await decision(page).textContent(), /Fit observations/, "names the workflow that requires it");
    assert.ok((await decision(page).locator('[data-testid="op-impact"][data-kind="workflowInput"]').count()) >= 1, "a required-workflow-input impact");
    await shot(page, "p1-workflow-input-decision-light");
    await decision(page).getByRole("button", { name: /^Delete$/ }).click();
    await waitFor(async () => !existsSync(join(root, "data", "obs.csv")), { message: "input deleted" });
    const after = await invoke(page, "workflow_get", { id: saved.id, revision: null });
    assert.deepEqual(after.definition.inputs, workflow().inputs, "the workflow is unchanged (still names its input)");
    const summary = (await invoke(page, "workflow_list")).find((w) => w.id === saved.id);
    assert.ok(summary, "the workflow still exists");
    assert.equal(summary.runnable, false, "the workflow cannot run without its input");
    const plan = await invoke(page, "workflow_run_plan", { id: saved.id, revision: null }).catch((e) => e);
    assert.ok(!(plan instanceof Error) && plan.blockers.some((b) => b.code === "workflow.inputMissing" && b.input === "data"), "the plan shows the input missing");
    evidence.notes.push(`workflow after its input was deleted: runnable=${summary.runnable}; plan=${plan instanceof Error ? plan.message.slice(0, 160) : JSON.stringify(plan.blockers ?? plan).slice(0, 160)}`);
    await dismissNotifications(page);

    // --- C. one combined decision for a batch ----------------------------------------------
    await writeFile(join(root, "data", "obs.csv"), OBS);
    await waitFor(async () => (await fileRow(page, "data/obs.csv").count()) > 0, { message: "obs.csv back in the tree" });
    await selectFiles(page, ["data/obs.csv", "experiment.py", "scratch/unused.txt"]);
    await page.keyboard.press("Delete");
    await decision(page).waitFor({ timeout: 15_000 });
    await page.waitForTimeout(300);
    assert.equal(await page.getByRole("alertdialog").count(), 1, "exactly one decision for the batch");
    assert.match(await decision(page).textContent(), /Delete 3 items/);
    await decision(page).getByRole("button", { name: "Cancel" }).click();
    assert.ok(existsSync(join(root, "scratch", "unused.txt")) && existsSync(join(root, "experiment.py")), "Cancel kept the whole batch");
    await expectNoDialog(page, assert, 400, "no second confirmation");

    assert.deepEqual(consoleErrors(app), [], "no console errors");
  } finally {
    locker?.child.kill();
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2)).catch(() => {});
    await app.close();
  }
  await publishShots(shots);
}
