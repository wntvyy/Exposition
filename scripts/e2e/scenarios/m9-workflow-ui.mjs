// M9: the workflow UI end to end in the real app (restricted Docker batches).
//
// A. Authoring in the editor: a saved workflow (probe, sweep, collect,
//    flagged) is extended through the UI — max parallel 2, a composite step
//    running another saved workflow at a pinned revision (its port bound in the
//    form), a condition on "flagged" (JSON value of probe's dataset), and a
//    forEach over the sweep's parameter — and saved as a schema /2 revision.
// B. Plan review: the composite's child steps grouped under it with their
//    complete code, the condition and iteration bounds in plain words, max
//    parallel and the global limits; started from the dialog only.
// C. Run monitor: steps in flight at once, the forEach instances with their
//    values, "Condition not met" with its reason (distinct from Skipped), the
//    unproduced optional outcome's reason, the manual cause, a recorded
//    attempt environment (image identity), Export manifest, and Reproduce
//    (reviewed plan pinned to the recorded inputs; the new run's cause).
// D. Triggers panel: a standing-plan review (what may change without asking),
//    enabled by the user; saving a watched input starts a run whose cause names
//    the trigger and the changed input; turned off again.
//
// Requires Docker with the local image python:3.13-slim; without it the
// scenario reports SKIPPED unless EXPOSITION_REQUIRE_DOCKER=1.
// Observations: test-results/e2e/m9-workflow-ui/observations.json; screens
// (both themes for the main surfaces) copied to $EXPOSITION_DEV_HOME/ui-reference/workflows-m9.
import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { cp, mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";
import { invoke, waitFor } from "../workspace-helpers.mjs";

const IMAGE = "python:3.13-slim";
const RUN_TIMEOUT = 300_000;
const OBS = "x,y\n0,1\n1,3\n2,5\n";

const out = (body) => `import json, os, time\nOUT = os.environ["EXPOSITION_OUTPUT_DIR"]\n${body}\n`;
const PROBE = out('time.sleep(4)\njson.dump({"ok": False, "n": 3}, open(os.path.join(OUT, "dataset.json"), "w"))');
const SWEEP = out('time.sleep(4)\nopen(os.path.join(OUT, "dataset.csv"), "w").write("seed,v\\n" + os.environ.get("EXPOSITION_SEED", "") + ",1\\n")');
const COLLECT = out('open(os.path.join(OUT, "dataset.csv"), "w").write("k,v\\nall,1\\n")');
const FLAGGED = out('open(os.path.join(OUT, "dataset.csv"), "w").write("k,v\\nflagged,1\\n")');
const FIT = out('time.sleep(4)\nopen(os.path.join(OUT, "dataset.csv"), "w").write("k,v\\nfit,1\\n")');

function dockerAvailable() {
  try {
    execFileSync("docker", ["image", "inspect", IMAGE], { stdio: "ignore", timeout: 30_000 });
    return true;
  } catch {
    return false;
  }
}

const batch = (id, label, source) => ({
  id,
  label,
  operation: { id: "research.pythonBatch", version: 1 },
  parameters: {},
  payload: { kind: "code", language: "python", source },
});
const dataInput = { id: "data", label: "Observations", portType: "dataset", source: { kind: "resource", path: "data/obs.csv", version: { kind: "atRunStart" } } };
const ceiling = { execution: "restricted", modelRequests: false, retainResults: false, proposeChanges: false };

const childDefinition = {
  schema: "exposition.workflow/1",
  title: "Fit model",
  description: "Child workflow run as one composite step.",
  preset: null,
  inputs: [{ ...dataInput, id: "obs" }],
  steps: [batch("fit", "Fit", FIT)],
  connections: [{ kind: "data", from: { kind: "input", input: "obs" }, to: { step: "fit", port: "data" } }],
  outcomes: [{ id: "fitted", kind: "transformedDataset", label: "Fitted values", from: { step: "fit", port: "dataset" }, optional: false }],
  ceiling,
};

const parentDefinition = {
  schema: "exposition.workflow/1",
  title: "Parameter study",
  description: "",
  preset: null,
  inputs: [dataInput],
  steps: [batch("probe", "Probe", PROBE), batch("sweep", "Sweep", SWEEP), batch("collect", "Collect", COLLECT), batch("flagged", "Flagged follow-up", FLAGGED)],
  connections: [
    { kind: "data", from: { kind: "input", input: "data" }, to: { step: "probe", port: "data" } },
    { kind: "data", from: { kind: "input", input: "data" }, to: { step: "sweep", port: "data" } },
    { kind: "data", from: { kind: "step", step: "probe", port: "dataset" }, to: { step: "collect", port: "data" } },
    { kind: "data", from: { kind: "step", step: "sweep", port: "dataset" }, to: { step: "collect", port: "data" } },
    { kind: "data", from: { kind: "input", input: "data" }, to: { step: "flagged", port: "data" } },
  ],
  outcomes: [
    { id: "summary", kind: "transformedDataset", label: "Summary", from: { step: "collect", port: "dataset" }, optional: false },
    { id: "followup", kind: "transformedDataset", label: "Follow-up", from: { step: "flagged", port: "dataset" }, optional: true },
  ],
  ceiling,
};

export default async function m9WorkflowUi({ launchApp, artifactsDir, assert }) {
  if (!dockerAvailable()) {
    const message = `SKIPPED: Docker with the local image ${IMAGE} is not available`;
    if (process.env.EXPOSITION_REQUIRE_DOCKER === "1") throw new Error(message);
    console.log(message);
    return;
  }
  const base = await mkdtemp(join(tmpdir(), "xp-m9-ui-"));
  const root = join(base, "project");
  const dataDir = join(base, "data");
  await mkdir(join(root, "data"), { recursive: true });
  await mkdir(dataDir, { recursive: true });
  await writeFile(join(root, "data", "obs.csv"), OBS);

  const observations = [];
  const note = (id, ok, detail) => {
    observations.push({ id, ok: !!ok, detail });
    console.log(`${ok ? "ok    " : "FAIL  "} ${id}: ${typeof detail === "string" ? detail : JSON.stringify(detail)?.slice(0, 600)}`);
  };
  const shots = [];
  const app = await launchApp({ dataDir });
  const page = app.page;
  const setTheme = (theme) => page.evaluate((t) => document.documentElement.setAttribute("data-theme", t), theme);
  const shot = async (name, { themes = ["light"] } = {}) => {
    for (const theme of themes) {
      await setTheme(theme);
      await page.waitForTimeout(250);
      const path = join(artifactsDir, `${name}-${theme}.png`);
      await page.screenshot({ path }).catch(() => {});
      shots.push(path);
    }
    await setTheme("light");
  };
  const visible = (testId) => page.locator(`[data-testid="${testId}"]:visible`).first();
  /** Choose `option` in the react-aria Select `select` (a locator of its container or button). */
  const choose = async (select, option) => {
    const button = (await select.getAttribute("class"))?.includes("ui-select__button") ? select : select.getByRole("button").first();
    await button.click();
    await page.getByRole("option", { name: option }).first().click();
  };
  const finish = async (runId) => {
    let run;
    await waitFor(async () => {
      run = await invoke(page, "workflow_run_get", { run: runId });
      return run.state !== "running";
    }, { timeout: RUN_TIMEOUT, interval: 500, message: `run ${runId} finishes` });
    return run;
  };
  const section = async (name, body) => {
    try {
      await body();
    } catch (e) {
      note(`${name}-section-ran`, false, String(e?.stack ?? e).slice(0, 800));
      await shot(`failure-${name}`).catch(() => {});
      await page.keyboard.press("Escape").catch(() => {});
    }
  };

  let parent = null;
  let firstRunId = null;
  try {
    await page.setViewportSize({ width: 1600, height: 1000 }).catch(() => {});
    await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"]').waitFor({ timeout: 15_000 });
    await invoke(page, "execution_settings_update", { change: { kind: "dockerImage", image: IMAGE } });
    await invoke(page, "research_space_enable").catch(() => {});

    const child = await invoke(page, "workflow_save", { request: { id: null, expectedRevision: null, definition: childDefinition } });
    parent = await invoke(page, "workflow_save", { request: { id: null, expectedRevision: null, definition: parentDefinition } });
    note("base-drafts-saved", child.revision === 1 && parent.revision === 1 && parent.validation.state === "wellFormed", {
      child: child.validation.diagnostics.map((d) => d.code),
      parent: parent.validation.diagnostics.map((d) => d.code),
    });

    // ---- A. authoring through the editor ---------------------------------------------------
    await section("authoring", async () => {
      await runCommand(page, "Show Workflows", /Show Workflows/);
      await page.getByTestId("workflows-view").waitFor({ timeout: 15_000 });
      await page.getByTestId(`workflow-open-${parent.id}`).click();
      const editor = visible("workflow-editor");
      await editor.waitFor({ timeout: 15_000 });

      await choose(page.getByTestId("workflow-max-parallel"), /^2/);

      // Composite: a step that runs "Fit model" at its latest revision (pinned).
      await choose(page.getByTestId("workflow-new-step"), /Run another workflow|composite/i);
      await choose(page.getByTestId("workflow-new-composite"), /Fit model/);
      await page.getByTestId("workflow-add-step").click();
      const composite = page.locator('[data-testid^="workflow-step-"][data-composite="true"]').first();
      await composite.waitFor({ timeout: 15_000 });
      const compId = (await composite.getAttribute("data-testid")).replace("workflow-step-", "");
      await page.getByTestId(`composite-revision-${compId}`).waitFor({ timeout: 15_000 });
      const pinned = await page.getByTestId(`composite-revision-${compId}`).innerText();
      // Its ports come from the child: bind the child's input, feed its outcome to Collect.
      await page.getByTestId(`bind-${compId}-obs`).waitFor({ timeout: 15_000 });
      await choose(page.getByTestId(`bind-${compId}-obs`), /Input: Observations/);
      await choose(page.getByTestId("workflow-step-collect").getByRole("button", { name: /Add a source to/ }), /Fit model → Fitted values/);
      await page.getByTestId(`composite-expand-${compId}`).click();
      const childSteps = await page.getByTestId(`composite-children-${compId}`).innerText();
      note("composite-step-shows-child-and-pin", pinned === "1" && /Fit/.test(childSteps), { compId, pinned, childSteps });

      // Condition on the follow-up: probe's JSON value /ok must be true (it is false).
      await page.getByTestId("condition-flagged").getByText("Run this step only when a condition holds").click();
      await choose(page.getByTestId("condition-source-flagged"), /Probe → Transformed dataset/);
      await page.getByTestId("condition-flagged-pointer").locator("input").fill("/ok");
      await choose(page.getByTestId("condition-flagged-test").getByRole("button", { name: /Value type/ }), "True or false");
      const conditionText = await page.getByTestId("condition-summary-flagged").innerText();
      note("condition-authored", /\/ok/.test(conditionText) && /true/.test(conditionText), conditionText);

      // forEach over the sweep's declared parameter, two values.
      await choose(page.getByTestId("repeat-kind-sweep"), /Once for each value of a parameter/);
      const values = page.getByTestId("repeat-values-sweep");
      await values.locator("input").first().fill("60");
      await page.getByTestId("repeat-add-value-sweep").click();
      await values.locator("input").nth(1).fill("61");
      const repeatText = await page.getByTestId("repeat-summary-sweep").innerText();
      note("for-each-authored", /2 values/.test(repeatText), repeatText);
      await shot("m9-editor-authored", { themes: ["light", "dark"] });

      await page.getByTestId("workflow-save").click();
      await waitFor(async () => (await editor.getAttribute("data-revision")) === "2", { timeout: 30_000, message: "revision 2 saved" });
      const saved = await invoke(page, "workflow_get", { id: parent.id, revision: null });
      const steps = Object.fromEntries(saved.definition.steps.map((s) => [s.id, s]));
      note(
        "saved-as-schema-2-with-composite-condition-repeat-parallel",
        saved.definition.schema === "exposition.workflow/2" &&
          saved.definition.execution?.maxParallel === 2 &&
          steps[compId]?.composite?.workflow === child.id &&
          steps[compId]?.composite?.revision === 1 &&
          steps.flagged?.condition?.test?.kind === "jsonCompare" &&
          steps.sweep?.repeat?.kind === "forEach" &&
          steps.sweep.repeat.values.length === 2,
        { schema: saved.definition.schema, execution: saved.definition.execution, comp: steps[compId], flagged: steps.flagged?.condition, sweep: steps.sweep?.repeat },
      );
      const status = page.getByTestId("workflow-status").first();
      await waitFor(async () => (await status.getAttribute("data-can-review")) === "true", { timeout: 30_000, message: "revision 2 reviewable" }).catch(() => {});
      note("revision-2-runnable", (await status.getAttribute("data-can-review")) === "true", {
        state: await status.getAttribute("data-state"),
        reason: await page.getByTestId("workflow-run-reason").first().innerText().catch(() => null),
        diagnostics: saved.validation.diagnostics.map((d) => `${d.code}: ${d.message}`),
      });

      // Canvas: the composite node with its pin, condition edge, repeat badge.
      await page.getByRole("radio", { name: "Canvas" }).click();
      await page.getByTestId(`wfc-composite-${compId}`).waitFor({ timeout: 15_000 });
      await shot("m9-canvas", { themes: ["light", "dark"] });
      await page.getByRole("radio", { name: "Form" }).click();
    });

    // ---- B. plan review ---------------------------------------------------------------------
    await section("plan-review", async () => {
      await page.getByTestId("workflow-review-run").first().click();
      const details = page.getByTestId("workflow-plan-details");
      await details.waitFor({ timeout: 30_000 });
      const plan = await invoke(page, "workflow_run_plan", { id: parent.id, revision: null });
      const expanded = plan.steps.filter((s) => s.step.includes("/"));
      const groups = await page.locator('[data-testid^="plan-group-"]').count();
      const groupText = groups ? await page.locator('[data-testid^="plan-group-"]').first().innerText() : "";
      note("plan-groups-composite-children-with-code", groups >= 1 && expanded.length >= 1 && /Inside the workflow step/.test(groupText) && groupText.includes("EXPOSITION_OUTPUT_DIR"), {
        expanded: expanded.map((s) => [s.step, s.compositePath]),
        groupText: groupText.slice(0, 300),
      });
      const condition = await page.getByTestId("plan-condition-flagged").innerText().catch(() => "");
      const repeat = await page.getByTestId("plan-repeat-sweep").innerText().catch(() => "");
      const concurrency = page.getByTestId("plan-concurrency");
      note("plan-states-condition-and-iterations", /Runs only if/.test(condition) && /2 values/.test(repeat), { condition, repeat });
      note(
        "plan-states-max-parallel-and-global-limits",
        (await concurrency.getAttribute("data-max-parallel")) === "2" && /active runs/.test(await concurrency.innerText()),
        await concurrency.innerText(),
      );
      await shot("m9-plan-review", { themes: ["light", "dark"] });
      await page.getByTestId("plan-group-" + (expanded[0]?.step.split("/")[0] ?? "x")).scrollIntoViewIfNeeded().catch(() => {});
      await shot("m9-plan-review-composite");
      const runsBefore = (await invoke(page, "workflow_runs", { workflow: parent.id })).length;
      note("nothing-started-before-start-run", runsBefore === 0, runsBefore);
      await page.getByTestId("workflow-start-run").click();
    });

    // ---- C. run monitor ---------------------------------------------------------------------
    await section("run-monitor", async () => {
      const monitor = visible("workflow-run");
      await monitor.waitFor({ timeout: 30_000 });
      firstRunId = await monitor.getAttribute("data-run");
      let maxConcurrent = 0;
      let concurrentShot = false;
      await waitFor(
        async () => {
          const now = page.locator('[data-testid="workflow-run-concurrent"]:visible');
          if (await now.count()) {
            const n = Number(await now.first().getAttribute("data-count"));
            if (n > maxConcurrent) maxConcurrent = n;
            if (n >= 2 && !concurrentShot) {
              concurrentShot = true;
              await shot("m9-run-concurrent", { themes: ["light", "dark"] });
            }
          }
          return ["succeeded", "failed", "canceled", "interrupted"].includes(await monitor.getAttribute("data-state"));
        },
        { timeout: RUN_TIMEOUT, interval: 250, message: "run monitor ends" },
      );
      const run = await finish(firstRunId);
      note("run-succeeded", run.state === "succeeded", { state: run.state, code: run.code, message: run.message, steps: run.steps.map((s) => [s.step, s.state, s.reason ?? null]) });
      const intervals = run.steps.flatMap((s) => [...s.attempts, ...(s.instances ?? []).flatMap((i) => i.attempts)].map((a) => [a.dispatchedAtMs ?? a.intentAtMs, a.finishedAtMs]));
      const overlap = intervals.some(([s1, e1], i) => intervals.some(([s2], j) => j !== i && s2 >= s1 && e1 !== null && s2 < e1));
      note("steps-ran-concurrently-and-the-ui-showed-it", maxConcurrent >= 2 && overlap, { maxConcurrent, overlap });

      const instances = await page.locator('[data-testid^="workflow-instance-sweep-"]:visible').evaluateAll((els) => els.map((e) => [e.getAttribute("data-index"), e.getAttribute("data-state"), e.textContent.slice(0, 40)]));
      note("for-each-instances-shown-with-values", instances.length === 2 && instances.every(([, state]) => state === "succeeded") && /60/.test(instances[0]?.[2] ?? ""), instances);

      const flagged = visible("workflow-run-step-flagged");
      const reason = await visible("workflow-step-reason-flagged").innerText().catch(() => "");
      note("condition-not-met-with-reason", (await flagged.getAttribute("data-state")) === "conditionNotMet" && /condition was not met/.test(reason) && !/Skipped/.test(await flagged.innerText()), {
        state: await flagged.getAttribute("data-state"),
        reason,
      });
      const outcomeReason = await visible("workflow-outcome-reason-followup").innerText().catch(() => "");
      note("unproduced-outcome-has-a-reason", outcomeReason.length > 0, outcomeReason);
      const cause = visible("workflow-run-cause");
      note("manual-cause-shown", (await cause.getAttribute("data-kind")) === "manual", await cause.innerText());
      await visible("workflow-run-step-flagged").scrollIntoViewIfNeeded();
      await shot("m9-run-condition-not-met", { themes: ["light", "dark"] });

      // Environment of the probe's attempt: runner, image identity, seed.
      const attempt = visible("workflow-attempt-probe-1");
      await attempt.getByRole("button", { name: "Environment" }).click();
      const env = visible("workflow-env-probe-1");
      await env.waitFor({ timeout: 10_000 });
      const imageId = await env.locator('[data-fact="Image"] [data-full]').getAttribute("data-full").catch(() => null);
      const recorded = run.steps.find((s) => s.step === "probe")?.attempts.at(-1)?.environment;
      note("attempt-environment-shown", !!imageId && recorded?.image?.id?.endsWith(imageId) && /Runner/.test(await env.innerText()), { imageId, recorded });
      await env.scrollIntoViewIfNeeded();
      await shot("m9-run-environment", { themes: ["light", "dark"] });

      // Export manifest.
      await visible("workflow-run-export-manifest").click();
      const manifest = visible("workflow-run-manifest");
      await manifest.waitFor({ timeout: 30_000 });
      const manifestPath = await manifest.getAttribute("data-path");
      const absolute = /^[A-Za-z]:|^\//.test(manifestPath) ? manifestPath : join(root, ...manifestPath.split("/"));
      const text = existsSync(absolute) ? await readFile(absolute, "utf8") : "";
      let json = null;
      try {
        json = JSON.parse(text);
      } catch {
        json = null;
      }
      note("manifest-exported", !!json && text.includes(run.plan.approvalHash) && text.includes(run.definitionHash), { manifestPath, keys: json ? Object.keys(json) : null });
      note("manifest-carries-no-secrets", !/ANTHROPIC|sk-ant|api[_-]?key/i.test(text), "");

      // Reproduce: a reviewed plan pinned to the recorded inputs; the new run's cause.
      await visible("workflow-run-reproduce").click();
      await page.getByTestId("workflow-plan-details").waitFor({ timeout: 30_000 });
      const reproduces = await page.getByTestId("plan-reproduces").innerText().catch(() => "");
      const drift = await page.getByTestId("plan-drift").innerText().catch(() => null);
      note("reproduce-plan-reviewed", reproduces.includes(firstRunId), { reproduces, drift });
      await shot("m9-reproduce-plan", { themes: ["light", "dark"] });
      await page.getByTestId("workflow-start-run").click();
      await waitFor(async () => {
        const m = visible("workflow-run");
        return (await m.count()) > 0 && (await m.getAttribute("data-run")) !== firstRunId;
      }, { timeout: 30_000, message: "reproduction monitor opened" });
      const reproRunId = await visible("workflow-run").getAttribute("data-run");
      const repro = await finish(reproRunId);
      const reproCause = await visible("workflow-run-cause").getAttribute("data-kind");
      note("reproduction-runs-with-its-cause", repro.cause?.kind === "reproduce" && repro.cause.of === firstRunId && reproCause === "reproduce", { cause: repro.cause, state: repro.state });
    });

    // ---- D. triggers ------------------------------------------------------------------------
    await section("triggers", async () => {
      await runCommand(page, "Show Workflows", /Show Workflows/);
      await page.getByTestId(`workflow-open-${parent.id}`).click();
      await visible("workflow-triggers").scrollIntoViewIfNeeded();
      await visible("workflow-trigger-add").click();
      await page.getByTestId("trigger-dialog").waitFor({ timeout: 15_000 });
      await page.getByTestId("trigger-watch-data").click();
      await page.getByTestId("trigger-debounce").locator("input").fill("2");
      await page.getByTestId("trigger-review").click();
      await page.getByTestId("trigger-standing").waitFor({ timeout: 30_000 });
      const may = await page.getByTestId("trigger-may-change").innerText();
      note("standing-plan-review-says-what-may-change", /data\/obs\.csv/.test(may) && /Anything else suspends/.test(may), may.slice(0, 400));
      await shot("m9-trigger-standing-plan", { themes: ["light", "dark"] });
      const before = await invoke(page, "workflow_trigger_list", { workflow: parent.id }).catch(() => []);
      note("no-trigger-before-enable", before.length === 0, before);
      await page.getByTestId("trigger-enable").click();
      const card = page.locator('[data-testid^="workflow-trigger-"][data-state]').first();
      await waitFor(async () => (await card.count()) > 0 && (await card.getAttribute("data-state")) === "active", { timeout: 30_000, message: "trigger active" });
      const triggers = await invoke(page, "workflow_trigger_list", { workflow: parent.id });
      const trigger = triggers[0];
      note("trigger-enabled-by-the-user", trigger?.state?.kind === "active" && trigger.kind?.kind === "inputsChanged", trigger);

      const runsBefore = (await invoke(page, "workflow_runs", { workflow: parent.id })).map((r) => r.id);
      await writeFile(join(root, "data", "obs.csv"), `${OBS}3,7\n`);
      let fired = null;
      await waitFor(async () => {
        const runs = await invoke(page, "workflow_runs", { workflow: parent.id });
        fired = runs.find((r) => !runsBefore.includes(r.id)) ?? null;
        return fired !== null;
      }, { timeout: 120_000, interval: 1000, message: "trigger fired" }).catch(() => {});
      const firedRun = fired ? await finish(fired.id) : null;
      note(
        "saving-a-watched-input-fires-with-its-cause",
        firedRun?.cause?.kind === "trigger" && firedRun.cause.trigger === trigger?.id && firedRun.cause.changed.some((c) => c.input === "data" && c.before !== c.after),
        firedRun?.cause ?? "no run",
      );
      await waitFor(async () => /Last fired/.test(await page.getByTestId(`trigger-last-fired-${trigger.id}`).innerText()), { timeout: 30_000, message: "last fired shown" }).catch(() => {});
      await visible("workflow-triggers").scrollIntoViewIfNeeded();
      await shot("m9-triggers-panel", { themes: ["light", "dark"] });
      await page.getByTestId(`trigger-disable-${trigger.id}`).click();
      await waitFor(async () => (await page.getByTestId(`workflow-trigger-${trigger.id}`).getAttribute("data-state")) === "disabled", { timeout: 30_000, message: "trigger off" });
      note("trigger-turned-off", (await invoke(page, "workflow_trigger_list", { workflow: parent.id }))[0]?.state?.kind === "disabled", "");
    });

    const errors = app.consoleMessages.filter((m) => m.startsWith("error") || m.startsWith("pageerror"));
    note("no-console-errors", errors.length === 0, errors.slice(0, 5));
  } finally {
    await writeFile(join(artifactsDir, "observations.json"), JSON.stringify(observations, null, 2));
    if (process.env.EXPOSITION_DEV_HOME && shots.length) {
      const dir = join(process.env.EXPOSITION_DEV_HOME, "ui-reference", "workflows-m9");
      await mkdir(dir, { recursive: true });
      for (const shotPath of shots) if (existsSync(shotPath)) await cp(shotPath, join(dir, shotPath.split(/[\\/]/).pop()));
    }
    await app.close();
  }
  const failed = observations.filter((o) => !o.ok);
  assert.deepEqual(failed.map((d) => d.id), [], `violations (see ${join(artifactsDir, "observations.json")})`);
}
