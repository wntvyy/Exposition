// P1 project operations — the decisions that need real running work, real
// triggers and real composites, in the real app on a disposable project
// (spec §5 rows 3, 4, 8; the impacts are computed by the backend):
//  A. ActiveWork: a restricted Docker batch is running and reads data/slow.csv.
//     Delete → ONE decision naming the run, default "leave it running and keep";
//     keeping changes nothing (the file stays, the run keeps running); deleting
//     again with "stop" cancels the run first, then the file goes to the Recycle
//     Bin, and nothing writes afterwards (no output, no resurrection).
//  B. EnabledTrigger: a trigger watches data/watched.csv. Delete → the decision
//     names it, default "turn the triggers off, then delete"; the file goes, the
//     trigger is off, no run fires and nothing comes back.
//  C. PinnedRevision: a composite runs revision 1 of a child. Storage & History →
//     include that (referenced) revision → Purge → the decision names the
//     composite; choosing to delete it too purges it; the composite cannot run
//     (its child revision is missing).
//  D. Copy Text / Copy Reference on a source-anchored theorem: without its
//     source open the app says so (and opens it on request); with it open the
//     exact source text reaches the REAL clipboard.
// Requires Docker with the local image python:3.13-slim for A and B (never
// pulled); without it those parts are reported SKIPPED unless
// EXPOSITION_REQUIRE_DOCKER=1.
import { execFileSync } from "node:child_process";
import { cp, mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";
import {
  consoleErrors,
  decision,
  dismissNotifications,
  ps,
  publishShots,
  recycleBinItemsFrom,
  selectFiles,
  settle,
  waitForNotification,
} from "../p1-helpers.mjs";
import { invoke, waitFor } from "../workspace-helpers.mjs";

const IMAGE = "python:3.13-slim";
const SLOW = `import csv, os, time
rows = list(csv.DictReader(open("data/slow.csv")))
time.sleep(90)
out = os.environ["EXPOSITION_OUTPUT_DIR"]
with open(os.path.join(out, "dataset.csv"), "w") as f:
    f.write("rows\\n%d\\n" % len(rows))
`;
const FAST = `import csv, os
rows = list(csv.DictReader(open("data/watched.csv")))
with open(os.path.join(os.environ["EXPOSITION_OUTPUT_DIR"], "dataset.csv"), "w") as f:
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

const input = (id, label, portType, path) => ({ id, label, portType, source: { kind: "resource", path, version: { kind: "atRunStart" } } });

function batch(title, script, data) {
  return {
    schema: "exposition.workflow/1",
    title,
    description: "",
    preset: null,
    inputs: [input("script", "Python script", "codeSource", script), input("data", "Observations", "dataset", data)],
    steps: [{ id: "run", label: "Run", operation: { id: "research.pythonBatch", version: 1 }, parameters: { timeoutSeconds: { kind: "integer", value: 300 } }, payload: null }],
    connections: [
      { kind: "data", from: { kind: "input", input: "script" }, to: { step: "run", port: "script" } },
      { kind: "data", from: { kind: "input", input: "data" }, to: { step: "run", port: "data" } },
    ],
    outcomes: [{ id: "out", kind: "transformedDataset", label: "Result", from: { step: "run", port: "dataset" }, optional: false }],
    ceiling: { execution: "restricted", modelRequests: false, retainResults: false, proposeChanges: false },
  };
}

/** Pick an option of one of the decision's choices (a Select). */
async function choose(page, key, optionName) {
  await decision(page).locator(`[data-choice="${key}"] button`).click();
  await page.getByRole("option", { name: optionName }).click();
  await waitFor(async () => (await decision(page).locator(`[data-choice="${key}"]`).getAttribute("data-value")) !== null, { message: `choice ${key}` });
}

/** Press the decision's primary action; re-confirm a re-planned decision (choices kept). Returns the number of re-plans. */
async function confirm(page) {
  let replans = 0;
  await decision(page).locator(".ui-dialog__footer button").last().click();
  for (let i = 0; i < 3; i++) {
    const again = await decision(page)
      .filter({ hasText: "changed since this was planned" })
      .waitFor({ timeout: 2500 })
      .then(() => true)
      .catch(() => false);
    if (!again) break;
    replans += 1;
    await decision(page).locator(".ui-dialog__footer button").last().click();
  }
  return replans;
}

export default async function p1Decisions({ launchApp, artifactsDir, assert, fixture }) {
  const docker = dockerAvailable();
  if (!docker && process.env.EXPOSITION_REQUIRE_DOCKER === "1") throw new Error(`Docker with the local image ${IMAGE} is required`);
  const base = await mkdtemp(join(tmpdir(), "xp-p1-decisions-"));
  const root = join(base, "project");
  const dataDir = join(base, "data");
  await cp(fixture("graph-basic"), root, { recursive: true });
  await mkdir(join(root, "data"), { recursive: true });
  await mkdir(dataDir, { recursive: true });
  await writeFile(join(root, "data", "slow.csv"), "x,y\n0,1\n1,3\n");
  await writeFile(join(root, "data", "watched.csv"), "x,y\n0,1\n");
  await writeFile(join(root, "data", "pinned.csv"), "x,y\n0,1\n");
  await writeFile(join(root, "slow.py"), SLOW);
  await writeFile(join(root, "fast.py"), FAST);
  const evidence = { real: [], notes: [], skipped: [] };
  const shots = [];
  const app = await launchApp({ dataDir });
  const { page } = app;
  const shot = async (name) => {
    const path = join(artifactsDir, `${name}.png`);
    await settle(page);
    await page.screenshot({ path });
    shots.push(path);
  };
  const call = (command, args) => invoke(page, command, args);
  try {
    await page.setViewportSize({ width: 1440, height: 900 });
    if (docker) await call("execution_settings_update", { change: { kind: "dockerImage", image: IMAGE } });
    await call("workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="paper.tex"]').waitFor({ timeout: 15_000 });
    await waitFor(async () => (await call("graph_status")).state.kind === "attached", { timeout: 20_000, message: "graph attached" });

    // --- A. ActiveWork ----------------------------------------------------------------------
    if (docker) {
      const slow = await call("workflow_save", { request: { id: null, expectedRevision: null, definition: batch("Slow count", "slow.py", "data/slow.csv") } });
      const plan = await call("workflow_run_plan", { id: slow.id, revision: null });
      assert.deepEqual(plan.blockers, [], JSON.stringify(plan.blockers));
      const run = await call("workflow_run_start", { request: { plan: plan.plan, approvalHash: plan.approvalHash } });
      await waitFor(async () => (await call("workflow_run_get", { run: run.id })).steps.some((s) => s.state === "running"), { timeout: 120_000, interval: 300, message: "the batch step is running" });

      // Keep (the default): nothing changes.
      await selectFiles(page, ["data/slow.csv"]);
      await page.keyboard.press("Delete");
      await decision(page).waitFor({ timeout: 15_000 });
      assert.equal(await page.getByRole("alertdialog").count(), 1, "one decision");
      const active = decision(page).locator('[data-testid="op-impact"][data-kind="activeWork"]');
      assert.ok((await active.count()) >= 1, "active work named");
      assert.match(await active.first().textContent(), /Slow count/);
      assert.equal(await decision(page).locator('[data-choice="activeWork"]').getAttribute("data-value"), "skip", "default keeps the work running");
      await shot("p1-active-work-decision-light");
      await decision(page).locator(".ui-dialog__footer button").last().click();
      await waitForNotification(page, "Nothing was deleted");
      assert.ok(existsSync(join(root, "data", "slow.csv")), "kept: the file stays");
      assert.equal((await call("workflow_run_get", { run: run.id })).state, "running", "kept: the run keeps running");
      await dismissNotifications(page);

      // Stop, then delete.
      await selectFiles(page, ["data/slow.csv"]);
      await page.keyboard.press("Delete");
      await decision(page).waitFor({ timeout: 15_000 });
      await choose(page, "activeWork", /Stop it, then delete/);
      assert.equal(await decision(page).locator('[data-choice="activeWork"]').getAttribute("data-value"), "stop");
      const replansA = await confirm(page);
      evidence.notes.push(`ActiveWork stop: re-planned ${replansA} time(s) before applying (the running work changed the plan)`);
      await waitFor(async () => !existsSync(join(root, "data", "slow.csv")), { timeout: 30_000, message: "deleted after stopping" });
      const stopped = await waitFor(async () => {
        const r = await call("workflow_run_get", { run: run.id });
        return r.state !== "running" ? r : null;
      }, { timeout: 30_000, message: "the run stopped" });
      const deletedAt = Date.now();
      assert.notEqual(stopped.state, "succeeded", `the run was stopped (${stopped.state})`);
      assert.ok(recycleBinItemsFrom(join(root, "data")).some((i) => i.name.startsWith("slow")), "slow.csv is in the real Recycle Bin");
      await page.waitForTimeout(6000);
      const after = await call("workflow_run_get", { run: run.id });
      const outputs = after.steps.flatMap((s) => s.attempts.flatMap((a) => a.outputs));
      assert.deepEqual(outputs, [], "nothing was written by the stopped worker");
      assert.ok(!existsSync(join(root, "data", "slow.csv")), "no resurrection");
      evidence.real.push(`ActiveWork: a running restricted Docker batch was stopped (${stopped.state}) before data/slow.csv went to the Recycle Bin; no output in the ${Math.round((Date.now() - deletedAt) / 1000)} s after`);
      await dismissNotifications(page);
    } else {
      evidence.skipped.push(`A ActiveWork: Docker image ${IMAGE} not available`);
    }

    // --- B. EnabledTrigger ---------------------------------------------------------------------
    if (docker) {
      const trig = await call("workflow_save", { request: { id: null, expectedRevision: null, definition: batch("Watched count", "fast.py", "data/watched.csv") } });
      const review = await call("workflow_trigger_plan", { request: { workflow: trig.id, kind: { kind: "inputsChanged", inputs: ["data"] } } });
      assert.deepEqual(review.blockers, [], JSON.stringify(review.blockers));
      const settings = { debounceSeconds: 3, maxRunsPerHour: 12, maxRunsPerDay: 100, allowNative: false, maxModelRequestsPerDay: 0 };
      const enabled = await call("workflow_trigger_enable", { request: { plan: review.plan.plan, standingHash: review.standingHash, settings } });
      await waitFor(async () => (await call("workflow_trigger_list", { workflow: trig.id }))[0]?.listening, { timeout: 30_000, message: "the trigger listens" });
      await selectFiles(page, ["data/watched.csv"]);
      await page.keyboard.press("Delete");
      await decision(page).waitFor({ timeout: 15_000 });
      const trigImpact = decision(page).locator('[data-testid="op-impact"][data-kind="enabledTrigger"]');
      assert.ok((await trigImpact.count()) >= 1, "the trigger is named");
      assert.equal(await decision(page).locator('[data-choice="triggers"]').getAttribute("data-value"), "disable", "default turns the trigger off first");
      await shot("p1-trigger-decision-light");
      const replansB = await confirm(page);
      if (replansB) evidence.notes.push(`EnabledTrigger: re-planned ${replansB} time(s)`);
      await waitFor(async () => !existsSync(join(root, "data", "watched.csv")), { timeout: 30_000, message: "watched file deleted" });
      const off = (await call("workflow_trigger_list", { workflow: trig.id }))[0];
      assert.equal(off.state.kind, "disabled", `the trigger is off (${JSON.stringify(off.state)})`);
      await page.waitForTimeout(8000);
      assert.deepEqual(await call("workflow_runs", { workflow: trig.id }), [], "no run fired");
      assert.ok(!existsSync(join(root, "data", "watched.csv")), "nothing came back");
      evidence.real.push(`EnabledTrigger: trigger ${enabled.id} turned off before data/watched.csv was deleted; no run fired in 8 s`);
      await dismissNotifications(page);
    } else {
      evidence.skipped.push(`B EnabledTrigger: Docker image ${IMAGE} not available`);
    }

    // --- C. PinnedRevision ----------------------------------------------------------------------
    const childDef = batch("Child count", "fast.py", "data/pinned.csv");
    const child1 = await call("workflow_save", { request: { id: null, expectedRevision: null, definition: childDef } });
    await call("workflow_save", { request: { id: child1.id, expectedRevision: child1.revision, definition: { ...childDef, description: "revision 2" } } });
    const parentDef = {
      schema: "exposition.workflow/2",
      title: "Pipeline",
      description: "",
      preset: null,
      inputs: [input("script", "Python script", "codeSource", "fast.py"), input("data", "Observations", "dataset", "data/pinned.csv")],
      steps: [{ id: "sub", label: "Run child", operation: { id: "workflow.composite", version: 1 }, parameters: {}, payload: null, composite: { workflow: child1.id, revision: child1.revision } }],
      connections: [
        { kind: "data", from: { kind: "input", input: "script" }, to: { step: "sub", port: "script" } },
        { kind: "data", from: { kind: "input", input: "data" }, to: { step: "sub", port: "data" } },
      ],
      outcomes: [{ id: "out", kind: "transformedDataset", label: "Result", from: { step: "sub", port: "out" }, optional: false }],
      ceiling: { execution: "restricted", modelRequests: false, retainResults: false, proposeChanges: false },
    };
    const parent = await call("workflow_save", { request: { id: null, expectedRevision: null, definition: parentDef } });
    const before = await call("workflow_get", { id: parent.id, revision: null });
    await runCommand(page, "Storage and History", /Preferences: Storage and History/);
    const revisions = page.getByTestId("storage-category-workflowRevisions");
    await revisions.waitFor({ timeout: 15_000 });
    const pinnedRow = revisions.locator('[data-testid="storage-item"][data-referenced="true"]').filter({ hasText: "Child count" }).first();
    await pinnedRow.waitFor({ timeout: 15_000 });
    assert.equal(await revisions.getByRole("button", { name: "Select unreferenced" }).count(), 0, "a pinned revision is never selected by default");
    await pinnedRow.getByRole("checkbox").check();
    assert.match(await page.getByTestId("storage-scope").textContent(), /referenced item is included/);
    await page.getByTestId("storage-purge").click();
    await decision(page).waitFor({ timeout: 15_000 });
    const pin = decision(page).locator('[data-testid="op-impact"][data-kind="pinnedRevision"]');
    assert.ok((await pin.count()) >= 1, "the pinning composite is named");
    assert.match(await pin.first().textContent(), /Pipeline/);
    assert.equal(await decision(page).locator('[data-choice="pinnedRevisions"]').getAttribute("data-value"), "keep", "default keeps pinned revisions");
    await choose(page, "pinnedRevisions", /Delete them too/);
    await shot("p1-pinned-revision-decision-light");
    const replansC = await confirm(page);
    if (replansC) evidence.notes.push(`PinnedRevision: re-planned ${replansC} time(s)`);
    await waitFor(async () => {
      const o = await call("storage_overview");
      return !o.categories.some((c) => c.items.some((i) => i.target.kind === "workflowRevision" && i.target.workflow === child1.id && i.target.revision === child1.revision));
    }, { timeout: 20_000, message: "the pinned revision is purged" });
    const parentAfter = await call("workflow_get", { id: parent.id, revision: null });
    const planAfter = await call("workflow_run_plan", { id: parent.id, revision: null }).catch((e) => ({ error: e.payload ?? e.message }));
    const diagnostics = (w) => (w.validation?.diagnostics ?? []).map((d) => d.code);
    evidence.notes.push(`composite before: runnable=${before.validation?.runnable} diagnostics=${diagnostics(before).join(",")}; after: runnable=${parentAfter.validation?.runnable} diagnostics=${diagnostics(parentAfter).join(",")}`);
    if (before.validation?.runnable) assert.equal(parentAfter.validation?.runnable, false, "runnable before, not after");
    const cannotRun = parentAfter.validation?.runnable === false || Boolean(planAfter.error) || (planAfter.blockers?.length ?? 0) > 0;
    assert.ok(diagnostics(parentAfter).length > diagnostics(before).length || Boolean(planAfter.error), "the composite now reports its missing child");
    assert.ok(cannotRun, `the composite cannot run without its child revision (${JSON.stringify(planAfter.error ?? planAfter.blockers ?? parentAfter.validation).slice(0, 300)})`);
    evidence.notes.push(
      `PinnedRevision: purged ${child1.id}@${child1.revision}; composite before runnable=${before.validation?.runnable}, after: ${JSON.stringify(planAfter.error ?? planAfter.blockers ?? parentAfter.validation?.diagnostics).slice(0, 240)}`,
    );
    await dismissNotifications(page);

    // --- D. Copy Text / Copy Reference ------------------------------------------------------------
    await page.evaluate(() => localStorage.setItem("exposition.graph.mode.project", "table"));
    const thm = (await call("graph_snapshot", { query: { kinds: ["theorem"], text: null, around: null, includeInactive: false, limit: 10 } })).entities[0];
    const detail = await call("graph_entity", { entity: thm.id });
    const binding = detail.bindings.find((b) => b.state === "resolved" && b.range);
    const expected = (await readFile(join(root, ...binding.path.split("/")), "utf8")).replace(/\r\n/g, "\n").slice(binding.range.start, binding.range.end);
    await runCommand(page, "Show Project Graph", /Graph: Show Project Graph/);
    const grid = page.getByRole("grid", { name: "Entities" });
    await grid.waitFor({ timeout: 15_000 });
    const thmRow = grid.locator(`[data-entity-id="${thm.id}"]`);
    await thmRow.click();
    await thmRow.click({ button: "right" });
    await page.getByTestId("graph-context-menu").getByRole("menuitem", { name: /Copy Text/ }).click();
    const notOpen = await waitForNotification(page, "Open the source to copy its text");
    assert.ok(notOpen.actions.includes("Open Source"), "offers to open the source");
    await page.locator('[data-testid="notification"]').filter({ hasText: "Open the source" }).getByRole("button", { name: "Open Source" }).click();
    await waitFor(async () => (await call("document_list")).some((d) => d.path === binding.path), { timeout: 15_000, message: "source opened" });
    await runCommand(page, "Show Project Graph", /Graph: Show Project Graph/);
    await grid.waitFor();
    await thmRow.click();
    await thmRow.click({ button: "right" });
    await page.getByTestId("graph-context-menu").getByRole("menuitem", { name: /Copy Text/ }).click();
    await waitForNotification(page, /^Copied the text of/);
    const clip = ps("Get-Clipboard -Raw").replace(/\r\n/g, "\n").replace(/\n$/, "");
    assert.equal(clip, expected.replace(/\n$/, ""), "the exact source text is on the real clipboard");
    await thmRow.click({ button: "right" });
    await page.getByTestId("graph-context-menu").getByRole("menuitem", { name: /Copy Reference/ }).click();
    await waitForNotification(page, "Copied reference");
    assert.equal(ps("Get-Clipboard -Raw").trim(), thm.key ?? thm.label, "the reference is on the real clipboard");
    evidence.real.push(`Copy Text put ${expected.length} characters of ${binding.path} on the Windows clipboard; Copy Reference put "${thm.key}"`);
    assert.equal((await call("graph_entity", { entity: thm.id })).entity.lifecycle, "active", "the theorem is unchanged");

    assert.deepEqual(consoleErrors(app), [], "no console errors");
  } finally {
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2)).catch(() => {});
    await app.close();
  }
  await publishShots(shots);
}
