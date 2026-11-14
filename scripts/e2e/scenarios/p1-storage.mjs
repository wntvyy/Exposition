// P1 project operations — records, outputs and Storage & History in the real
// app (spec §5 row 10b, §2 runs/agents), disposable project; scripted agent
// provider (debug build) and, when Docker has the local image, a real
// restricted workflow run:
//  1. Agent task records: Delete Task Record from the Agents page card menu
//     (no decision for a finished task); the files the task read stay.
//  2. Workflow run (Docker): Delete Run Record keeps the outputs; "Delete Run
//     and Its Outputs…" is reviewed with outputs=delete preset, and removes
//     exactly the run's output files — never the inputs.
//  3. Storage & History: categories with sizes; "Select unreferenced" never
//     selects referenced items; Purge removes exactly the displayed scope;
//     the reported space is honest (no claim beyond what the result says).
import { execFileSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";
import { consoleErrors, decision, dismissNotifications, expectNoDialog, notifications, publishShots, waitForNotification } from "../p1-helpers.mjs";
import { invoke, waitFor } from "../workspace-helpers.mjs";

const IMAGE = "python:3.13-slim";
const EXPERIMENT = `import csv, os
out = os.environ["EXPOSITION_OUTPUT_DIR"]
rows = list(csv.DictReader(open("data/obs.csv")))
with open(os.path.join(out, "dataset.csv"), "w") as f:
    f.write("rows\\n%d\\n" % len(rows))
`;

function dockerAvailable() {
  try {
    execFileSync("docker", ["image", "inspect", IMAGE], { stdio: "ignore", timeout: 60_000 });
    return true;
  } catch {
    return false;
  }
}

function workflow() {
  const input = (id, label, portType, path) => ({ id, label, portType, source: { kind: "resource", path, version: { kind: "atRunStart" } } });
  return {
    schema: "exposition.workflow/1",
    title: "Count observations",
    description: "",
    preset: null,
    inputs: [input("script", "Python script", "codeSource", "experiment.py"), input("data", "Observations", "dataset", "data/obs.csv")],
    steps: [{ id: "run", label: "Run", operation: { id: "research.pythonBatch", version: 1 }, parameters: { timeoutSeconds: { kind: "integer", value: 300 } }, payload: null }],
    connections: [
      { kind: "data", from: { kind: "input", input: "script" }, to: { step: "run", port: "script" } },
      { kind: "data", from: { kind: "input", input: "data" }, to: { step: "run", port: "data" } },
    ],
    outcomes: [{ id: "out", kind: "transformedDataset", label: "Result", from: { step: "run", port: "dataset" }, optional: false }],
    ceiling: { execution: "restricted", modelRequests: false, retainResults: false, proposeChanges: false },
  };
}

export default async function p1Storage({ launchApp, artifactsDir, assert }) {
  const base = await mkdtemp(join(tmpdir(), "xp-p1-storage-"));
  const root = join(base, "project");
  const dataDir = join(base, "data");
  await mkdir(join(root, "data"), { recursive: true });
  await mkdir(dataDir, { recursive: true });
  await writeFile(join(root, "main.typ"), "= Notes\n\nA line.\n");
  await writeFile(join(root, "data", "obs.csv"), "x,y\n0,1\n1,3\n");
  await writeFile(join(root, "experiment.py"), EXPERIMENT);
  const evidence = { real: [], notes: [] };
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
    const info = await invoke(page, "app_info");
    await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="main.typ"]').waitFor({ timeout: 15_000 });

    // --- 1. agent task record (scripted provider, debug builds) ------------------------
    if (info.debugBuild) {
      await invoke(page, "agents_provider_update", { settings: { provider: "scripted", model: "claude-opus-5-5", effort: "medium", consent: "scopeContent" } });
      await invoke(page, "agents_dev_load_script", {
        script: { turns: [{ steps: [{ toolUse: { id: "r", name: "read_file", input: { path: "main.typ" } } }] }, { steps: [{ text: "main.typ has a title and a line." }] }] },
      });
      const task = await invoke(page, "agents_task_create", {
        request: { prompt: "Summarize main.typ", workspaceRoot: root, scope: { kind: "workspace" }, permission: "readOnly", execution: "disabled", consent: "scopeContent", budgets: null },
      });
      await waitFor(async () => {
        const t = (await invoke(page, "agents_task_list")).find((x) => x.id === task.id);
        return t && !["queued", "running", "awaitingApproval"].includes(t.status.kind);
      }, { timeout: 30_000, message: "scripted task finished" });
      await runCommand(page, "Open Agents Page", /Agent: Open Agents Page/);
      const card = page.locator(`[data-op-item="${task.id}"]`);
      await card.waitFor({ timeout: 15_000 });
      await page.getByTestId(`op-more-${task.id}`).click();
      await page.getByRole("menuitem", { name: /Delete Task Record/ }).click();
      await expectNoDialog(page, assert, 600, "a finished task record is deleted at once");
      await waitFor(async () => !(await invoke(page, "agents_task_list")).some((t) => t.id === task.id), { timeout: 15_000, message: "task record removed" });
      assert.ok(existsSync(join(root, "main.typ")), "the file the task read stays");
      evidence.notes.push("agent task record deleted from the Agents page card menu; files untouched");
      await dismissNotifications(page);
    } else {
      evidence.notes.push("agent task step skipped: release build (scripted provider is debug-only)");
    }

    // --- 2. a real restricted run: record vs outputs ------------------------------------
    let runId = null;
    if (dockerAvailable()) {
      await invoke(page, "execution_settings_update", { change: { kind: "dockerImage", image: IMAGE } });
      const saved = await invoke(page, "workflow_save", { request: { id: null, expectedRevision: null, definition: workflow() } });
      const plan = await invoke(page, "workflow_run_plan", { id: saved.id, revision: null });
      if (plan.blockers.length === 0) {
        const run = await invoke(page, "workflow_run_start", { request: { plan: plan.plan, approvalHash: plan.approvalHash } });
        runId = run.id;
        const done = await waitFor(async () => {
          const r = await invoke(page, "workflow_run_get", { run: run.id });
          return r.state !== "running" ? r : null;
        }, { timeout: 240_000, interval: 500, message: "run finished" });
        const outputs = done.steps.flatMap((s) => s.attempts.flatMap((a) => a.outputs.map((o) => o.value?.path).filter(Boolean)));
        evidence.real.push(`restricted Docker run ${done.state}; outputs ${outputs.join(", ")}`);
        await runCommand(page, "Show Workflows", /Workflow: Show Workflows/);
        const item = page.getByTestId(`workflow-run-item-${run.id}`);
        await item.waitFor({ timeout: 15_000 });
        await page.getByTestId(`op-more-${run.id}`).click();
        await page.getByRole("menuitem", { name: /Delete Run and Its Outputs Permanently/ }).click();
        const d = decision(page);
        await d.waitFor({ timeout: 15_000 });
        assert.equal(await d.locator('[data-choice="outputs"]').getAttribute("data-value"), "delete", "outputs scope preset and reviewed");
        await shot(page, "p1-run-outputs-decision-light");
        await d.getByRole("button", { name: /^Delete/ }).click();
        await waitFor(async () => !(await invoke(page, "workflow_runs", { workflow: null })).some((r) => r.id === run.id), { timeout: 15_000, message: "run record removed" });
        for (const p of outputs) assert.ok(!existsSync(join(root, ...p.split("/"))), `output ${p} removed`);
        assert.equal(await readFile(join(root, "data", "obs.csv"), "utf8"), "x,y\n0,1\n1,3\n", "the input is never deleted");
        assert.ok(existsSync(join(root, "experiment.py")), "the script input stays");
        await dismissNotifications(page);
      } else {
        evidence.notes.push(`run step skipped: plan blockers ${JSON.stringify(plan.blockers)}`);
      }
    } else {
      evidence.notes.push(`run step skipped: Docker image ${IMAGE} not available`);
    }

    // --- 3. Storage & History ------------------------------------------------------------
    await runCommand(page, "Storage and History", /Preferences: Storage and History/);
    const storage = page.getByTestId("settings-storage");
    await storage.waitFor({ timeout: 15_000 });
    await page.waitForFunction(() => document.querySelector('[data-testid="storage-scope"]') || document.querySelector('[data-testid="settings-storage"] .ui-state'), null, { timeout: 15_000 });
    const overview = await invoke(page, "storage_overview");
    evidence.notes.push(`storage categories: ${overview.categories.map((c) => `${c.id}(${c.items.length}, ${c.bytes ?? "n/a"} B)`).join(", ")}; excluded: ${overview.excluded.length}`);
    await shot(page, "p1-storage-light");
    const withItems = overview.categories.find((c) => c.items.some((i) => !i.referenced && !i.active));
    if (withItems) {
      const group = page.getByTestId(`storage-category-${withItems.id}`);
      await group.getByRole("button", { name: "Select unreferenced" }).click();
      const scope = page.getByTestId("storage-scope");
      const expected = withItems.items.filter((i) => !i.referenced);
      await waitFor(async () => (await scope.getAttribute("data-count")) === String(expected.length), { message: "scope = unreferenced items" });
      assert.match(await scope.textContent(), /frees database space only when the database is compacted/);
      await shot(page, "p1-storage-scope-light");
      await page.getByTestId("storage-purge").click();
      if (await decision(page).count()) {
        await shot(page, "p1-storage-purge-decision-light");
        await decision(page).getByRole("button", { name: /^Delete/ }).click();
      }
      const rowKeys = (o) => new Set(o.categories.flatMap((c) => c.items.map((i) => `${c.id}|${JSON.stringify(i.target)}`)));
      const before = rowKeys(overview);
      await waitFor(async () => {
        const keys = rowKeys(await invoke(page, "storage_overview"));
        return expected.every((i) => !keys.has(`${withItems.id}|${JSON.stringify(i.target)}`));
      }, { timeout: 20_000, message: "purged exactly the selected items" });
      const after = await invoke(page, "storage_overview");
      const keysAfterRows = rowKeys(after);
      const untouched = [...before].filter((k) => !k.startsWith(`${withItems.id}|`));
      for (const k of untouched) {
        if (!keysAfterRows.has(k)) evidence.notes.push(`row outside the purged category changed: ${k}`);
      }
      const referencedBefore = withItems.items.filter((i) => i.referenced).map((i) => JSON.stringify(i.target));
      const keysAfter = new Set(after.categories.flatMap((c) => c.items.map((i) => JSON.stringify(i.target))));
      for (const k of referencedBefore) assert.ok(keysAfter.has(k), "referenced items were kept");
      const result = (await notifications(page)).map((n) => `${n.title} — ${n.body}`);
      evidence.notes.push(`purge result: ${result.join(" | ")}`);
    } else {
      evidence.notes.push("nothing unreferenced to purge");
    }

    assert.deepEqual(consoleErrors(app), [], "no console errors");
  } finally {
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2)).catch(() => {});
    await app.close();
  }
  await publishShots(shots);
}
