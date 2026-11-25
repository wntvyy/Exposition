// Independent review of the "Could not be checked" impact (`consumersUnknown`,
// a delete plan whose consumer sources could not all be read) in the real app.
// Routine deletion must stay immediate in a healthy project; disposable
// temporary projects only.
//  A. A fresh folder (no .exposition yet): the first plan right after
//     workspace_open resolves, and a Delete from the tree at once, ask nothing.
//  B. A project switch: the same in the second project, at once.
//  C. Plans sent while a project switch is still in flight (not awaited):
//     how many say "could not be checked" (the window between the workspace
//     switch and the graph attaching), recorded as evidence.
//  D. A plain folder (no manuscript, no Lean, no workflow) and a path the
//     graph does not index (node_modules): nothing asks.
//  E. Rapid successive deletes from the tree: no dialog.
//  F. The project graph made unavailable: three files at once ask once, one
//     "Could not be checked" line per file naming it; Cancel changes nothing.
//     The graph recovered (Start new) while the decision is open, then
//     Delete: the plan is stale, the decision comes back with the note and
//     without the impact; Delete applies.
//  G. A project switch while a trusted native workflow batch of the previous
//     project runs: delete plans in the new project for 15 s. A plan that
//     says "could not be checked" is a review finding: printed, and failing
//     the scenario only with EXPOSITION_REVIEW_FINDINGS=1. Skipped when the
//     batch cannot start (no Python found).
import { cp, mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { consoleErrors, decision, dismissNotifications, expectNoDialog, fileRow, selectFiles, settle, platformSkip } from "../p1-helpers.mjs";
import { invoke, waitFor } from "../workspace-helpers.mjs";

const user = { kind: "user" };
const del = { kind: "delete", permanent: false };
const pathTarget = (path) => ({ kind: "path", path });

async function plan(page, paths) {
  return invoke(page, "project_op_plan", { request: { kind: del, targets: paths.map(pathTarget), actor: user } });
}

function unchecked(plan) {
  return plan.impacts.filter((i) => i.kind === "consumersUnknown");
}

/** A trusted native Python batch that runs for a minute (reads data/obs.csv). */
function slowWorkflow() {
  return {
    schema: "exposition.workflow/1",
    title: "Slow count",
    description: "",
    preset: null,
    inputs: [{ id: "data", label: "Observations", portType: "dataset", source: { kind: "resource", path: "data/obs.csv", version: { kind: "atRunStart" } } }],
    steps: [
      {
        id: "run",
        label: "Count slowly",
        operation: { id: "research.nativePython", version: 1 },
        parameters: { timeoutSeconds: { kind: "integer", value: 300 } },
        payload: {
          kind: "code",
          language: "python",
          source: 'import os, time\ntime.sleep(60)\nopen(os.path.join(os.environ["EXPOSITION_OUTPUT_DIR"], "dataset.csv"), "w").write("rows\\n1\\n")\n',
        },
      },
    ],
    connections: [{ kind: "data", from: { kind: "input", input: "data" }, to: { step: "run", port: "data" } }],
    outcomes: [{ id: "out", kind: "transformedDataset", label: "Result", from: { step: "run", port: "dataset" }, optional: false }],
    ceiling: { execution: "trustedNative", modelRequests: false, retainResults: false, proposeChanges: false },
  };
}

async function project(fixture, base, name, files) {
  const root = join(base, name);
  if (fixture) await cp(fixture, root, { recursive: true });
  else await mkdir(root, { recursive: true });
  for (const [path, text] of Object.entries(files)) {
    await mkdir(join(root, ...path.split("/").slice(0, -1)), { recursive: true });
    await writeFile(join(root, ...path.split("/")), text);
  }
  return root;
}

export default async function reviewConsumersUnknown({ launchApp, artifactsDir, assert, fixture }) {
  const skip = platformSkip();
  if (skip) {
    console.log(`SKIPPED: review-consumers-unknown ${skip}`);
    return;
  }
  const base = await mkdtemp(join(tmpdir(), "xp-review-unknown-"));
  const dataDir = join(base, "data");
  await mkdir(dataDir, { recursive: true });
  const scratch = { "scratch/a.txt": "a\n", "scratch/b.txt": "b\n", "scratch/c.txt": "c\n", "scratch/d.txt": "d\n", "scratch/e.txt": "e\n" };
  const first = await project(fixture("workspace-basic"), base, "first", scratch);
  const second = await project(fixture("graph-basic"), base, "second", { ...scratch, "node_modules/pkg/index.js": "module.exports = 1;\n" });
  const plain = await project(null, base, "plain", { "readme.txt": "plain\n", "data/x.csv": "x\n1\n" });
  const evidence = { notes: [] };

  const app = await launchApp({ dataDir });
  const { page } = app;
  try {
    await page.setViewportSize({ width: 1440, height: 900 });

    // --- A. a fresh folder: the very first plan, and a Delete at once ------------------------
    assert.ok(!existsSync(join(first, ".exposition")), "no project metadata yet");
    await invoke(page, "workspace_open", { path: first });
    const firstPlan = await plan(page, ["scratch/a.txt"]);
    assert.deepEqual(unchecked(firstPlan), [], "the first plan after opening a fresh folder");
    assert.equal(firstPlan.needsDecision, false);
    await page.locator('[data-testid="file-tree"] [data-path="scratch"]').waitFor({ timeout: 10_000 });
    await selectFiles(page, ["scratch/b.txt"]);
    await page.keyboard.press("Delete");
    await expectNoDialog(page, assert, 800, "a delete right after opening a fresh folder asks nothing");
    await waitFor(async () => !existsSync(join(first, "scratch", "b.txt")), { message: "b.txt deleted" });
    await dismissNotifications(page);

    // --- B. a project switch: at once in the second project ---------------------------------
    await invoke(page, "workspace_open", { path: second });
    const switched = await plan(page, ["scratch/a.txt"]);
    assert.deepEqual(unchecked(switched), [], "the first plan after a project switch");
    assert.equal(switched.needsDecision, false);
    await page.locator('[data-testid="file-tree"] [data-path="scratch"]').waitFor({ timeout: 10_000 });
    await selectFiles(page, ["scratch/b.txt"]);
    await page.keyboard.press("Delete");
    await expectNoDialog(page, assert, 800, "a delete right after a project switch asks nothing");
    await waitFor(async () => !existsSync(join(second, "scratch", "b.txt")), { message: "b.txt deleted in the second project" });
    await dismissNotifications(page);

    // --- C. plans while a switch is in flight --------------------------------------------------
    const counts = { plans: 0, unchecked: 0, errors: 0, codes: new Set() };
    for (const destination of [first, second, first]) {
      let opened = false;
      const opening = invoke(page, "workspace_open", { path: destination }).then(() => (opened = true));
      while (!opened) {
        try {
          const p = await plan(page, ["scratch/c.txt"]);
          counts.plans += 1;
          if (unchecked(p).length) {
            counts.unchecked += 1;
            for (const i of unchecked(p)) counts.codes.add(i.message.replace(/“[^”]*”/, "“…”"));
          }
        } catch {
          counts.errors += 1;
        }
      }
      await opening;
      const after = await plan(page, ["scratch/c.txt"]);
      assert.deepEqual(unchecked(after), [], "once the switch is done");
    }
    evidence.notes.push(
      `during switches: ${counts.plans} plans, ${counts.unchecked} said "could not be checked", ${counts.errors} failed` +
        (counts.codes.size ? ` (${[...counts.codes].join(" | ")})` : ""),
    );

    // --- D. a plain folder; a path the graph does not index -----------------------------------
    await invoke(page, "workspace_open", { path: plain });
    const plainPlan = await plan(page, ["readme.txt", "data/x.csv", "data"]);
    assert.deepEqual(unchecked(plainPlan), [], "a folder without manuscripts, Lean or workflows");
    await invoke(page, "workspace_open", { path: second });
    const ignored = await plan(page, ["node_modules/pkg/index.js"]).catch((e) => ({ error: e.message, impacts: [] }));
    assert.deepEqual(unchecked(ignored), [], `a path the graph does not index: ${JSON.stringify(ignored).slice(0, 300)}`);

    // --- E. rapid successive deletes from the tree ---------------------------------------------
    await page.locator('[data-testid="file-tree"] [data-path="scratch"]').waitFor({ timeout: 10_000 });
    for (const name of ["c.txt", "d.txt", "e.txt"]) {
      await selectFiles(page, [`scratch/${name}`]);
      await page.keyboard.press("Delete");
      await expectNoDialog(page, assert, 150, `rapid delete of ${name} asks nothing`);
      await waitFor(async () => !existsSync(join(second, "scratch", name)), { timeout: 10_000, message: `${name} deleted` });
    }
    await expectNoDialog(page, assert, 600, "no dialog after rapid deletes");
    await dismissNotifications(page);

    // --- F. the project graph unavailable: one decision; recovery while it is open -------------
    for (const n of ["x1.txt", "x2.txt", "x3.txt"]) await writeFile(join(second, "scratch", n), `${n}\n`);
    await invoke(page, "workspace_close");
    await writeFile(join(second, ".exposition", "graph.sqlite3"), Buffer.from("SQLite format 3\0" + "x".repeat(4000)));
    await invoke(page, "workspace_open", { path: second });
    await waitFor(async () => (await invoke(page, "graph_status")).state.kind === "unavailable", { timeout: 20_000, message: "graph unavailable" });
    const unavailable = await plan(page, ["scratch/x1.txt"]);
    assert.equal(unchecked(unavailable).length, 1, JSON.stringify(unavailable.impacts));
    await page.locator('[data-testid="file-tree"] [data-path="scratch"]').waitFor({ timeout: 10_000 });
    const three = ["scratch/x1.txt", "scratch/x2.txt", "scratch/x3.txt"];
    await selectFiles(page, three);
    await page.keyboard.press("Delete");
    await decision(page).waitFor({ timeout: 15_000 });
    const group = decision(page).locator('[data-impact="consumersUnknown"]');
    assert.equal(await group.locator("h4").textContent(), "Could not be checked");
    const lines = decision(page).locator('[data-testid="op-impact"][data-kind="consumersUnknown"]');
    assert.equal(await lines.count(), 3, "one line per file");
    for (let i = 0; i < 3; i++) {
      const text = await lines.nth(i).textContent();
      const name = three[i].split("/").pop();
      assert.ok(text.startsWith(`${name}: `), `per-target prefix: ${text}`);
      assert.ok(text.includes(`“${three[i]}”`), `names its file: ${text}`);
    }
    const lead = await decision(page).locator(".xp-ops-decision__lead").textContent();
    evidence.notes.push(`decision lead: "${lead}"; first line: "${await lines.first().textContent()}"`);
    await settle(page);
    await page.screenshot({ path: join(artifactsDir, "review-unchecked-three.png") });
    await decision(page).getByRole("button", { name: "Cancel" }).click();
    await page.waitForTimeout(400);
    assert.ok(three.every((p) => existsSync(join(second, ...p.split("/")))), "Cancel changed nothing");

    // Asked again; the graph is recovered while the decision is open.
    await selectFiles(page, three);
    await page.keyboard.press("Delete");
    await decision(page).waitFor({ timeout: 15_000 });
    const recovered = await invoke(page, "graph_recover", { option: { kind: "startNew" } });
    assert.equal(recovered.state.kind, "attached", JSON.stringify(recovered.state));
    assert.deepEqual(unchecked(await plan(page, ["scratch/x1.txt"])), [], "readable again");
    await decision(page).getByRole("button", { name: /^Delete$/ }).click();
    // The plan went stale: re-planned, and asked again with the note (never applied blindly).
    await waitFor(async () => ((await decision(page).count()) ? (await decision(page).textContent()).includes("changed since this was planned") : false), { timeout: 15_000, message: "the stale note" });
    assert.equal(await decision(page).locator('[data-kind="consumersUnknown"]').count(), 0, "no 'could not be checked' after recovery");
    assert.ok(three.every((p) => existsSync(join(second, ...p.split("/")))), "nothing applied on the stale plan");
    await page.screenshot({ path: join(artifactsDir, "review-unchecked-stale.png") });
    await decision(page).getByRole("button", { name: /^Delete$/ }).click();
    await waitFor(async () => three.every((p) => !existsSync(join(second, ...p.split("/")))), { timeout: 15_000, message: "deleted after the second decision" });
    evidence.notes.push("graph recovered while the decision was open: the stale plan was re-planned and asked again with the note, then applied");
    await dismissNotifications(page);

    // --- G. a project switch while a workflow batch of the previous project runs ---------------
    // The switch only asks the previous project's workflow run to stop; its
    // batch stays live until the job is killed and its record written.
    const slow = await project(null, base, "slow", { "data/obs.csv": "x\n1\n2\n", "notes.md": "slow\n" });
    await invoke(page, "workspace_open", { path: slow });
    await invoke(page, "trust_grant", { root: (await invoke(page, "workspace_current")).root });
    await invoke(page, "research_environments", { refresh: true });
    const saved = await invoke(page, "workflow_save", { request: { id: null, expectedRevision: null, definition: slowWorkflow() } });
    const wplan = await invoke(page, "workflow_run_plan", { id: saved.id, revision: null });
    if (wplan.blockers.length) {
      evidence.notes.push(`G skipped (the native batch cannot start here): ${JSON.stringify(wplan.blockers)}`);
    } else {
      const run = await invoke(page, "workflow_run_start", { request: { plan: wplan.plan, approvalHash: wplan.approvalHash } });
      await waitFor(async () => (await invoke(page, "workflow_run_get", { run: run.id })).steps.some((s) => s.state === "running"), { timeout: 120_000, interval: 200, message: "the batch step is running" });
      await page.waitForTimeout(2_000);
      await invoke(page, "workspace_open", { path: plain });
      const t0 = Date.now();
      let asked = 0;
      let total = 0;
      let last = -1;
      let sample = null;
      while (Date.now() - t0 < 15_000) {
        const p = await plan(page, ["readme.txt"]);
        total += 1;
        if (unchecked(p).length) {
          asked += 1;
          last = Date.now() - t0;
          sample ??= unchecked(p)[0].message;
        }
        await page.waitForTimeout(50);
      }
      const finding = `after switching away from a running workflow batch: ${asked} of ${total} delete plans in the new (healthy) project said "could not be checked" (the last ${last} ms after the switch): ${sample}`;
      evidence.notes.push(finding);
      if (asked > 0) {
        console.log(`REVIEW FINDING: ${finding}`);
        if (process.env.EXPOSITION_REVIEW_FINDINGS === "1") assert.equal(asked, 0, finding);
      }
    }

    assert.deepEqual(consoleErrors(app), [], "no console errors");
  } finally {
    console.log(JSON.stringify(evidence, null, 2));
    await app.close();
  }
}
