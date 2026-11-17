// P1 project operations on research runs (XR-A-03) in the real app with a
// real Python kernel, disposable project:
//  1. Research › Runs: right click a run row → "Delete Run Record" (the
//     record only; outputs stay): no dialog, the run leaves the history (also
//     a run of this app session), its output file stays.
//  2. Delete key on a focused run row, then Undo from the notification: the
//     run is back.
//  3. "Delete Run and Its Outputs" from the row menu: the record and the
//     outputs are two targets of one request; the output file leaves its place
//     (held for restore), the inputs stay.
//  4. Command palette with a run row focused lists and runs "Delete Selected
//     Items" for that run.
//  5. Settings › Storage & History lists the deleted record and the held
//     outputs as recoverable; Restore brings the record back into the history
//     and the output file back to its place.
// Needs Python with ipykernel discoverable (SKIPPED otherwise).
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";
import {
  consoleErrors,
  decision,
  dismissNotifications,
  expectNoDialog,
  notifications,
  platformSkip,
  pressNotificationAction,
  publishShots,
  settle,
  waitForNotification,
} from "../p1-helpers.mjs";
import { invoke, waitFor } from "../workspace-helpers.mjs";

const KERNEL_TIMEOUT = 120_000;
const PNG = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
const figure = (n) =>
  `# figure ${n}\nimport base64\nfrom IPython.display import Image, display\ndisplay(Image(data=base64.b64decode("${PNG}")))\nprint("run ${n}")\n`;

export default async function p1ResearchRuns({ launchApp, artifactsDir, assert }) {
  const skip = platformSkip();
  if (skip) {
    console.log(`SKIPPED: p1-research-runs ${skip}`);
    return;
  }
  const base = await mkdtemp(join(tmpdir(), "xp-p1-research-runs-"));
  const root = join(base, "project");
  const dataDir = join(base, "data");
  await mkdir(root, { recursive: true });
  await mkdir(dataDir, { recursive: true });
  await writeFile(join(root, "main.typ"), "= Notes\n");
  const evidence = { real: [], notes: [] };
  const shots = [];
  const app = await launchApp({ dataDir });
  const { page } = app;
  const shot = async (name) => {
    const path = join(artifactsDir, `${name}.png`);
    await settle(page);
    await page.screenshot({ path });
    shots.push(path);
  };
  const listed = async () => (await invoke(page, "research_runs")).map((r) => r.id);
  const exists = (path) => existsSync(join(root, ...path.split("/")));

  try {
    await page.setViewportSize({ width: 1440, height: 900 });
    const opened = await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="main.typ"]').waitFor({ timeout: 15_000 });
    await invoke(page, "trust_grant", { root: opened.root });
    const report = await invoke(page, "research_environments", { refresh: true });
    const python = report.runtimes.find((r) => r.language === "python" && r.availability.kind === "available");
    if (!python) {
      console.log("SKIPPED: p1-research-runs: no Python with ipykernel discovered");
      return;
    }
    const session = await invoke(page, "research_session_start", { request: { environmentId: python.id } });
    const runs = [];
    for (const n of [1, 2, 3]) {
      const run = await invoke(page, "research_execute", { request: { sessionId: session.id, source: { kind: "input", code: figure(n) } } });
      const done = await waitFor(async () => {
        const m = await invoke(page, "research_run", { runId: run.id });
        return m.record.status.kind !== "running" ? m : null;
      }, { timeout: KERNEL_TIMEOUT, interval: 300, message: `run ${n}` });
      assert.equal(done.record.status.kind, "succeeded", `run ${n} succeeded`);
      const outputs = done.record.outputs.map((o) => o.path).filter(Boolean);
      assert.ok(outputs.length > 0, `run ${n} produced an output file`);
      runs.push({ id: run.id, outputs });
    }
    evidence.real.push(`3 Python runs (${python.displayName}) with outputs ${runs.map((r) => r.outputs.join(",")).join(" | ")}`);

    await runCommand(page, "Show Research Panel", /Show Research Panel/);
    const history = page.getByTestId("research-history");
    await history.waitFor({ timeout: 15_000 });
    const row = (id) => history.locator(`[data-op-item="${id}"]`);
    for (const r of runs) await row(r.id).waitFor({ timeout: 15_000 });

    // --- 1. context menu: Delete Run Record (outputs stay) -----------------------------
    const [r1, r2, r3] = runs;
    await row(r1.id).click({ button: "right" });
    const menu = page.getByTestId("research-runs-context-menu");
    await menu.waitFor({ timeout: 5_000 });
    const menuText = await menu.innerText();
    for (const label of ["Delete Run Record", "The record only; outputs stay", "Delete Run and Its Outputs", "Delete Permanently"]) {
      assert.ok(menuText.includes(label), `the run menu offers ${label}: ${menuText}`);
    }
    await shot("p1-research-run-context-menu-light");
    await menu.getByRole("menuitem", { name: /^Delete Run Record/ }).click();
    await expectNoDialog(page, assert, 600, "a finished run record is deleted at once");
    await waitFor(async () => !(await listed()).includes(r1.id), { timeout: 15_000, message: "run 1 left the history (this session's run)" });
    await row(r1.id).waitFor({ state: "detached", timeout: 10_000 });
    for (const p of r1.outputs) assert.ok(exists(p), `run 1 output ${p} stays (record only)`);
    const n1 = (await notifications(page)).map((n) => `${n.title} — ${n.body}`);
    evidence.notes.push(`record delete: ${n1.join(" | ")}`);
    assert.ok(n1.some((t) => /Storage & History/.test(t)), `the notification names the recovery: ${n1}`);
    await dismissNotifications(page);

    // --- 2. Delete key on a focused row, then Undo -----------------------------------
    await row(r2.id).focus();
    await page.keyboard.press("Delete");
    await expectNoDialog(page, assert, 600, "Delete key deletes the record at once");
    await waitFor(async () => !(await listed()).includes(r2.id), { timeout: 15_000, message: "run 2 deleted with the Delete key" });
    const deleted = await waitForNotification(page, /./);
    await pressNotificationAction(page, deleted.title, "Undo");
    await waitFor(async () => (await listed()).includes(r2.id), { timeout: 15_000, message: "Undo restored run 2" });
    await row(r2.id).waitFor({ timeout: 10_000 });
    evidence.notes.push(`Delete key + Undo: "${deleted.title}" undone; run 2 listed again`);
    await dismissNotifications(page);

    // --- 3. record and outputs ----------------------------------------------------------
    await row(r2.id).click({ button: "right" });
    await menu.waitFor({ timeout: 5_000 });
    await menu.getByRole("menuitem", { name: /^Delete Run and Its Outputs$/ }).click();
    if (await decision(page).waitFor({ timeout: 2500 }).then(() => true, () => false)) {
      evidence.notes.push(`record + outputs decision: ${(await decision(page).textContent()).slice(0, 200)}`);
      await decision(page).locator(".ui-dialog__footer button").last().click();
    }
    await waitFor(async () => !(await listed()).includes(r2.id), { timeout: 15_000, message: "run 2 record deleted with its outputs" });
    await waitFor(async () => r2.outputs.every((p) => !exists(p)), { timeout: 15_000, message: "run 2 outputs left their place" });
    assert.ok(exists("main.typ"), "project files stay");
    const n3 = (await notifications(page)).map((n) => `${n.title} — ${n.body}`);
    evidence.notes.push(`record + outputs: ${n3.join(" | ")}`);
    await dismissNotifications(page);

    // --- 4. palette on a focused run row --------------------------------------------------
    await row(r3.id).focus();
    await runCommand(page, "Delete Selected Items", /Delete Selected Items$/);
    await expectNoDialog(page, assert, 600, "palette delete applies at once");
    await waitFor(async () => !(await listed()).includes(r3.id), { timeout: 15_000, message: "run 3 deleted from the palette" });
    evidence.notes.push("palette: Edit: Delete Selected Items deleted the focused run's record");
    await dismissNotifications(page);

    // --- 5. Storage & History: restorable, Restore works ------------------------------
    await runCommand(page, "Storage & History", /Preferences: Storage & History/);
    await page.getByTestId("settings-storage").waitFor({ timeout: 15_000 });
    const recordRow = page.locator(`[data-testid="storage-item"][data-key="recoverableDeletions|recoverable:researchRun:${r1.id}"]`);
    const outputsRow = page.locator(`[data-testid="storage-item"][data-key^="recoverableDeletions|recoverable:runOutputs:"]`).filter({ hasText: r2.id });
    await recordRow.waitFor({ timeout: 15_000 });
    await outputsRow.first().waitFor({ timeout: 15_000 });
    assert.ok(
      await page.locator(`[data-testid="storage-item"][data-key="recoverableDeletions|recoverable:researchRun:${r2.id}"]`).count(),
      "run 2's record is restorable too",
    );
    await shot("p1-research-runs-storage-light");
    await recordRow.getByTestId("storage-restore").click();
    await waitFor(async () => (await listed()).includes(r1.id), { timeout: 15_000, message: "run 1 restored from Storage & History" });
    await outputsRow.first().getByTestId("storage-restore").click();
    await waitFor(async () => r2.outputs.every((p) => exists(p)), { timeout: 15_000, message: "run 2 outputs restored to their place" });
    evidence.notes.push(`Storage restore: ${(await notifications(page)).map((n) => n.title).join(" | ")}`);
    evidence.real.push("research run records deleted from the run history (menu, Delete key, palette), Undo, record+outputs, restored from Storage & History");

    await invoke(page, "research_session_shutdown", { sessionId: session.id }).catch(() => {});
    assert.deepEqual(consoleErrors(app), [], "no console errors");
  } finally {
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2)).catch(() => {});
    await app.close();
  }
  await publishShots(shots);
}
