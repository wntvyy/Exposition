// M9 explicit workflow triggers against the built app (real IPC, real
// workspace watcher, real restricted Docker run).
//
// 1. A project opens and finishes indexing: nothing runs. The standing-plan
//    review states what may change; watching the code input is refused; a
//    wrong standing hash is refused.
// 2. The user enables a trigger on the data input (quiet period 3 s), then
//    saves the data file twice through the editor's document service within
//    the quiet period: exactly one run starts after it, records the cause
//    (trigger id, data sha256 old → new), and succeeds in Docker.
// 3. The user saves a change to the code input: the trigger is suspended with
//    a visible reason and no run starts; a later data save starts nothing.
// 4. Re-enabled, then the project is closed, the data changes on disk while
//    it is closed, and it is reopened: nothing runs (no catch-up, never on
//    open/indexing). The trigger record lives in the app data dir only.
//
// Requires Docker with the local image python:3.13-slim (never pulled);
// without it the scenario reports SKIPPED unless EXPOSITION_REQUIRE_DOCKER=1.
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, readdir, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { invoke, invokeError, waitFor } from "../workspace-helpers.mjs";

const IMAGE = "python:3.13-slim";
const OBS = "x,y\n0,1\n1,3\n";
const EXPERIMENT = `import csv, os
out = os.environ["EXPOSITION_OUTPUT_DIR"]
rows = list(csv.DictReader(open("data/obs.csv")))
with open(os.path.join(out, "dataset.csv"), "w") as f:
    f.write("rows\\n%d\\n" % len(rows))
`;

const sha256 = (text) => createHash("sha256").update(text, "utf8").digest("hex");

function dockerAvailable() {
  try {
    execFileSync("docker", ["image", "inspect", IMAGE], { stdio: "ignore", timeout: 60_000 });
    return true;
  } catch {
    return false;
  }
}

function workflow() {
  const input = (id, label, portType, path) => ({
    id,
    label,
    portType,
    source: { kind: "resource", path, version: { kind: "atRunStart" } },
  });
  return {
    schema: "exposition.workflow/1",
    title: "Triggered experiment",
    description: "",
    preset: null,
    inputs: [input("script", "Python script", "codeSource", "experiment.py"), input("data", "Observations", "dataset", "data/obs.csv")],
    steps: [
      {
        id: "run",
        label: "Run the experiment",
        operation: { id: "research.pythonBatch", version: 1 },
        parameters: { timeoutSeconds: { kind: "integer", value: 300 } },
        payload: null,
      },
    ],
    connections: [
      { kind: "data", from: { kind: "input", input: "script" }, to: { step: "run", port: "script" } },
      { kind: "data", from: { kind: "input", input: "data" }, to: { step: "run", port: "data" } },
    ],
    outcomes: [{ id: "out", kind: "transformedDataset", label: "Result", from: { step: "run", port: "dataset" }, optional: false }],
    ceiling: { execution: "restricted", modelRequests: false, retainResults: false, proposeChanges: false },
  };
}

async function listFiles(dir, prefix = "") {
  const out = [];
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const rel = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.isDirectory()) out.push(...(await listFiles(join(dir, entry.name), rel)));
    else out.push(rel);
  }
  return out;
}

export default async function m9Triggers({ launchApp, artifactsDir, assert }) {
  if (!dockerAvailable()) {
    const message = `SKIPPED: Docker with the local image ${IMAGE} is not available`;
    if (process.env.EXPOSITION_REQUIRE_DOCKER === "1") throw new Error(message);
    console.log(message);
    return;
  }
  const base = await mkdtemp(join(tmpdir(), "xp-m9-triggers-"));
  const root = join(base, "project");
  const dataDir = join(base, "data");
  await mkdir(join(root, "data"), { recursive: true });
  await mkdir(dataDir, { recursive: true });
  await writeFile(join(root, "data", "obs.csv"), OBS);
  await writeFile(join(root, "experiment.py"), EXPERIMENT);
  await writeFile(join(root, "main.typ"), "= Results\n");

  const app = await launchApp({ dataDir });
  const page = app.page;
  const events = [];
  const log = (step, detail) => {
    events.push({ step, detail });
    console.log(`ok: ${step}${detail ? ` — ${JSON.stringify(detail).slice(0, 400)}` : ""}`);
  };
  const call = (command, args) => invoke(page, command, args);
  const runsOf = (id) => call("workflow_runs", { workflow: id });
  const triggerOf = async (id) => (await call("workflow_trigger_list", { workflow: id }))[0];

  /** Replace a file's content through the document service and save it (as the editor does). */
  const saveThroughApp = async (path, text) => {
    const opened = await call("document_open", { path });
    const doc = opened.state.info.id;
    const state = await call("document_state", { document: doc });
    await call("document_apply", {
      request: {
        id: `t-trig-${Date.now()}-${Math.random()}`,
        origin: { kind: "source" },
        edits: [{ document: doc, baseRevision: state.info.revision, patches: [{ range: { start: 0, end: state.text.length }, text }] }],
        undo: { kind: "newGroup" },
        conflict: "reject",
        label: null,
        selection: null,
      },
    });
    await call("document_save", { document: doc, overwrite: false });
  };

  try {
    await call("execution_settings_update", { change: { kind: "dockerImage", image: IMAGE } });
    await call("workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"]').waitFor({ timeout: 15_000 });
    const saved = await call("workflow_save", { request: { id: null, expectedRevision: null, definition: workflow() } });
    const id = saved.id;

    // ---- 1. review --------------------------------------------------------
    const codeWatch = await invokeError(page, "workflow_trigger_plan", {
      request: { workflow: id, kind: { kind: "inputsChanged", inputs: ["script"] } },
    });
    assert.equal(codeWatch.error?.code, "trigger.codeInputWatched", JSON.stringify(codeWatch));
    log("watching the code input is refused", codeWatch.error.code);
    const review = await call("workflow_trigger_plan", { request: { workflow: id, kind: { kind: "inputsChanged", inputs: ["data"] } } });
    assert.match(review.standingHash, /^[0-9a-f]{64}$/);
    assert.deepEqual(review.blockers, [], JSON.stringify(review.blockers));
    assert.deepEqual(review.mayChange, ["The saved content of “Observations” (data/obs.csv, dataset)."]);
    assert.ok(review.suspendsOn.some((s) => s.includes("code in “Python script”")), JSON.stringify(review.suspendsOn));
    assert.ok(review.plan.inputs.find((i) => i.id === "script").text.includes("EXPOSITION_OUTPUT_DIR"), "the code is shown in full");
    log("standing plan review", { mayChange: review.mayChange, suspendsOn: review.suspendsOn.length });
    const settings = { debounceSeconds: 3, maxRunsPerHour: 12, maxRunsPerDay: 100, allowNative: false, maxModelRequestsPerDay: 0 };
    const wrong = await invokeError(page, "workflow_trigger_enable", {
      request: { plan: review.plan.plan, standingHash: "0".repeat(64), settings },
    });
    assert.equal(wrong.error?.code, "trigger.planStale", JSON.stringify(wrong));
    log("a wrong standing hash is refused", wrong.error.code);
    assert.deepEqual(await runsOf(id), [], "opening or reviewing started a run");

    // ---- 2. enable, two saves, one debounced run ---------------------------
    const again = await call("workflow_trigger_plan", { request: { workflow: id, kind: { kind: "inputsChanged", inputs: ["data"] } } });
    const enabled = await call("workflow_trigger_enable", { request: { plan: again.plan.plan, standingHash: again.standingHash, settings } });
    assert.equal(enabled.state.kind, "active");
    await waitFor(async () => (await triggerOf(id)).listening, { timeout: 30_000, message: "the trigger listens (project indexed)" });
    log("trigger enabled and listening", { id: enabled.id });
    const firstSave = "x,y\n0,1\n1,3\n2,5\n";
    const secondSave = "x,y\n0,1\n1,3\n2,5\n3,7\n";
    await saveThroughApp("data/obs.csv", firstSave);
    await page.waitForTimeout(800);
    await saveThroughApp("data/obs.csv", secondSave);
    const savedAt = Date.now();
    await page.waitForTimeout(1500);
    assert.deepEqual(await runsOf(id), [], "a run started inside the quiet period");
    const runs = await waitFor(async () => {
      const list = await runsOf(id);
      return list.length > 0 && list;
    }, { timeout: 30_000, interval: 250, message: "the triggered run" });
    const firedAfterMs = Date.now() - savedAt;
    const run = await waitFor(async () => {
      const r = await call("workflow_run_get", { run: runs[0].id });
      return r.state !== "running" && r;
    }, { timeout: 240_000, interval: 500, message: "the triggered run finishes" });
    assert.equal(run.state, "succeeded", `${run.code}: ${run.message}`);
    assert.deepEqual(run.cause, {
      kind: "trigger",
      trigger: enabled.id,
      changed: [{ input: "data", before: sha256(OBS), after: sha256(secondSave) }],
    });
    assert.equal(run.plan.inputs.find((i) => i.id === "data").sha256, sha256(secondSave), "the run read the latest save");
    await page.waitForTimeout(6000);
    assert.equal((await runsOf(id)).length, 1, "exactly one run for two saves");
    const fired = await triggerOf(id);
    assert.ok(fired.lastFiredMs, "last fired recorded");
    assert.ok(fired.history.some((h) => h.event.kind === "fired" && h.event.run === run.id), JSON.stringify(fired.history));
    log("two saves → exactly one debounced run with its cause", { run: run.id, firedAfterMs, cause: run.cause });

    // ---- 3. a code change suspends -----------------------------------------
    await saveThroughApp("experiment.py", `${EXPERIMENT}# edited\n`);
    const suspended = await waitFor(async () => {
      const t = await triggerOf(id);
      return t.state.kind === "suspended" && t;
    }, { timeout: 20_000, interval: 250, message: "the trigger is suspended" });
    assert.equal(suspended.state.code, "trigger.planChanged");
    assert.match(suspended.state.message, /“Python script” \(experiment\.py\) changed/);
    assert.equal(suspended.listening, false);
    await saveThroughApp("data/obs.csv", "x,y\n9,9\n");
    await page.waitForTimeout(6000);
    assert.equal((await runsOf(id)).length, 1, "a suspended trigger started a run");
    log("editing the code suspends the trigger; nothing runs", suspended.state);

    // ---- 4. re-enable; close; change while closed; reopen: nothing ----------
    const third = await call("workflow_trigger_plan", { request: { workflow: id, kind: { kind: "inputsChanged", inputs: ["data"] } } });
    const reenabled = await call("workflow_trigger_enable", { request: { plan: third.plan.plan, standingHash: third.standingHash, settings } });
    assert.equal(reenabled.id, enabled.id, "re-enabling keeps the trigger");
    assert.equal(reenabled.state.kind, "active");
    await call("workspace_close", {});
    await writeFile(join(root, "data", "obs.csv"), "x,y\nchanged,while closed\n");
    await page.waitForTimeout(1000);
    await call("workspace_open", { path: root });
    await waitFor(async () => (await triggerOf(id)).listening, { timeout: 30_000, message: "listening again after reopening" });
    await page.waitForTimeout(8000);
    assert.equal((await runsOf(id)).length, 1, "reopening (or a change made while closed) started a run");
    log("no run on reopen or for changes made while closed");

    // The trigger lives in the app data dir, never in the project.
    const store = join(dataDir, "workflow-triggers.json");
    assert.ok(existsSync(store), "app-local trigger store");
    assert.ok((await readFile(store, "utf8")).includes(enabled.id));
    for (const rel of await listFiles(root)) {
      if (rel.startsWith(".exposition/artifacts/") || rel.startsWith(".exposition/runs/")) continue;
      const text = await readFile(join(root, ...rel.split("/"))).catch(() => Buffer.alloc(0));
      assert.ok(!text.includes(Buffer.from(enabled.id)), `${rel} mentions the trigger`);
    }
    log("trigger stored app-locally only");
    const disabled = await call("workflow_trigger_disable", { trigger: enabled.id });
    assert.equal(disabled.state.kind, "disabled");
  } finally {
    await writeFile(join(artifactsDir, "events.json"), JSON.stringify(events, null, 2)).catch(() => {});
    await app.close();
  }
}
