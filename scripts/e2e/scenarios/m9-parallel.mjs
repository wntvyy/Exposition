// M9 bounded parallel workflow runs through the real IPC of the built app,
// with the real restricted Docker adapter.
//
// A. Overlap: a workflow with maxParallel 2 runs two independent restricted
//    branches (each sleeps in its container) and a join. The plan shows
//    maxParallel and the application-wide limits; while it runs both branch
//    steps are "running" at the same time; afterwards the dispatch/finish
//    times and the containers' own clocks overlap, and the join received both
//    branches' outputs.
// B. Cancel: with both long branches running in their containers,
//    workflow_run_cancel stops both (attempts canceled, the join skipped) and
//    no Exposition container is left.
// C. Condition: a step whose JSON condition does not hold is conditionNotMet
//    with its reason, the step that needs its output is skipped with a
//    reason (and so is its outcome), and the negated alternative runs.
//
// Requires Docker with the local image python:3.13-slim (never pulled);
// without it the scenario reports SKIPPED unless EXPOSITION_REQUIRE_DOCKER=1.
import { execFileSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { invoke, waitFor } from "../workspace-helpers.mjs";

const IMAGE = "python:3.13-slim";
const OBS = "x,y\n0,1\n1,3\n";

function dockerAvailable() {
  try {
    execFileSync("docker", ["image", "inspect", IMAGE], { stdio: "ignore", timeout: 60_000 });
    return true;
  } catch {
    return false;
  }
}

function managedContainers() {
  try {
    return execFileSync("docker", ["ps", "-aq", "--filter", "label=org.exposition.managed"], { encoding: "utf8", timeout: 30_000 })
      .split(/\s+/)
      .filter(Boolean);
  } catch {
    return ["<docker ps failed>"];
  }
}

const branchCode = (name, seconds) => `import json, os, time
out = os.environ["EXPOSITION_OUTPUT_DIR"]
start = time.time()
time.sleep(${seconds})
with open(os.path.join(out, "dataset.json"), "w") as f:
    json.dump({"branch": "${name}", "start": start, "end": time.time()}, f)
`;

const JOIN = `import glob, json, os
out = os.environ["EXPOSITION_OUTPUT_DIR"]
rows = [json.load(open(p)) for p in sorted(glob.glob(".exposition/artifacts/*/dataset.json"))]
with open(os.path.join(out, "dataset.json"), "w") as f:
    json.dump(rows, f)
`;

const jsonCode = (value) => `import os
with open(os.path.join(os.environ["EXPOSITION_OUTPUT_DIR"], "dataset.json"), "w") as f:
    f.write(${JSON.stringify(JSON.stringify(value))})
`;

const step = (id, label, source, extra = {}) => ({
  id,
  label,
  operation: { id: "research.pythonBatch", version: 1 },
  parameters: {},
  payload: { kind: "code", language: "python", source },
  ...extra,
});
const fromInput = (to) => ({ kind: "data", from: { kind: "input", input: "data" }, to: { step: to, port: "data" } });
const fromStep = (from, to) => ({ kind: "data", from: { kind: "step", step: from, port: "dataset" }, to: { step: to, port: "data" } });
const outcome = (id, stepId) => ({ id, kind: "transformedDataset", label: `Result of ${stepId}`, from: { step: stepId, port: "dataset" }, optional: false });

function v2(title, steps, connections, outcomes, maxParallel) {
  return {
    schema: "exposition.workflow/2",
    title,
    description: "",
    preset: null,
    inputs: [{ id: "data", label: "Observations", portType: "dataset", source: { kind: "resource", path: "data/obs.csv", version: { kind: "atRunStart" } } }],
    steps,
    connections,
    outcomes,
    ceiling: { execution: "restricted", modelRequests: false, retainResults: false, proposeChanges: false },
    execution: { maxParallel },
  };
}

const branches = (seconds) =>
  v2(
    `Two branches (${seconds} s)`,
    [step("a", "Branch A", branchCode("a", seconds)), step("b", "Branch B", branchCode("b", seconds)), step("join", "Join", JOIN)],
    [fromInput("a"), fromInput("b"), fromStep("a", "join"), fromStep("b", "join")],
    [outcome("joined", "join")],
    2,
  );

export default async function m9Parallel({ launchApp }) {
  if (!dockerAvailable()) {
    const message = `SKIPPED: Docker with the local image ${IMAGE} is not available`;
    if (process.env.EXPOSITION_REQUIRE_DOCKER === "1") throw new Error(message);
    console.log(message);
    return;
  }
  const failures = [];
  const check = (name, ok, detail = "") => {
    console.log(`${ok ? "ok" : "FAIL"}: ${name}${detail ? ` — ${typeof detail === "string" ? detail : JSON.stringify(detail).slice(0, 600)}` : ""}`);
    if (!ok) failures.push(name);
    return ok;
  };
  const base = await mkdtemp(join(tmpdir(), "xp-m9-parallel-"));
  const root = join(base, "project");
  await mkdir(join(root, "data"), { recursive: true });
  await writeFile(join(root, "data", "obs.csv"), OBS);
  const dataDir = join(base, "data");
  await mkdir(dataDir, { recursive: true });
  const before = managedContainers();

  const app = await launchApp({ dataDir });
  const page = app.page;
  const save = (definition) => invoke(page, "workflow_save", { request: { id: null, expectedRevision: null, definition } });
  const get = (run) => invoke(page, "workflow_run_get", { run });
  const finish = async (runId, timeout = 240_000) => {
    let run;
    await waitFor(async () => {
      run = await get(runId);
      return run.state !== "running";
    }, { timeout, interval: 300, message: `run ${runId} finishes` });
    return run;
  };
  const stepOf = (run, id) => run.steps.find((s) => s.step === id);
  const attempt = (run, id) => stepOf(run, id)?.attempts.at(-1);
  const datasetJson = async (att) => {
    const value = att?.outputs.find((o) => o.port === "dataset")?.value;
    if (!value?.path) return null;
    return JSON.parse(await readFile(join(root, ...value.path.split("/")), "utf8"));
  };
  const start = async (id) => {
    const plan = await invoke(page, "workflow_run_plan", { id, revision: null });
    return { plan, run: plan.blockers.length ? null : await invoke(page, "workflow_run_start", { request: { plan: plan.plan, approvalHash: plan.approvalHash } }) };
  };

  try {
    await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"]').waitFor({ timeout: 15_000 });
    const set = await invoke(page, "execution_settings_update", { change: { kind: "dockerImage", image: IMAGE } });
    check("docker image configured", set.settings.docker.image === IMAGE, set.settings.docker);

    // ---- A. two branches overlap -----------------------------------------
    const savedA = await save(branches(8));
    check("A saved as a runnable v2 draft", savedA.validation?.state === "wellFormed", savedA.validation?.diagnostics);
    const { plan: planA, run: runA0 } = await start(savedA.id);
    check("A plan: maxParallel 2, global limits, no blockers", planA.maxParallel === 2 && planA.limits?.restricted === 2 && planA.limits?.maxInFlightSteps === 4 && planA.blockers.length === 0, { maxParallel: planA.maxParallel, limits: planA.limits, blockers: planA.blockers });
    check("A plan says independent steps run at the same time", planA.notices.some((n) => n.includes("Up to 2 independent steps")), planA.notices[0]);
    let together = false;
    if (runA0) {
      await waitFor(async () => {
        const r = await get(runA0.id);
        together ||= stepOf(r, "a").state === "running" && stepOf(r, "b").state === "running";
        return together || r.state !== "running";
      }, { timeout: 120_000, interval: 250, message: "both branches running" }).catch(() => {});
      check("A both branch steps were running at the same time (workflow_run_get)", together);
      const runA = await finish(runA0.id);
      check("A run succeeded", runA.state === "succeeded", { code: runA.code, message: runA.message });
      const [a, b] = [attempt(runA, "a"), attempt(runA, "b")];
      check(
        "A dispatch/finish times overlap",
        a && b && a.dispatchedAtMs < b.finishedAtMs && b.dispatchedAtMs < a.finishedAtMs,
        { a: [a?.dispatchedAtMs, a?.finishedAtMs], b: [b?.dispatchedAtMs, b?.finishedAtMs] },
      );
      const [ja, jb] = [await datasetJson(a), await datasetJson(b)];
      const overlap = ja && jb ? Math.min(ja.end, jb.end) - Math.max(ja.start, jb.start) : -1;
      check("A the two containers ran at the same time (their own clocks)", overlap > 4, { overlapSeconds: overlap.toFixed?.(2), ja, jb });
      const joined = await datasetJson(attempt(runA, "join"));
      check("A the join received both branches", Array.isArray(joined) && joined.map((r) => r.branch).sort().join() === "a,b", joined);
      const total = (runA.finishedAtMs - runA.startedAtMs) / 1000;
      console.log(`info: A run took ${total.toFixed(1)} s for two 8 s branches and a join; overlap ${overlap.toFixed?.(1)} s`);
    }

    // ---- B. cancel stops both running branches -----------------------------
    const savedB = await save(branches(120));
    const { run: runB0 } = await start(savedB.id);
    if (check("B started", Boolean(runB0))) {
      await waitFor(async () => {
        const r = await get(runB0.id);
        const both = ["a", "b"].every((s) => attempt(r, s)?.state === "dispatched");
        return both && managedContainers().length - before.length >= 2;
      }, { timeout: 120_000, interval: 300, message: "both branch containers running" });
      await new Promise((resolve) => setTimeout(resolve, 2000));
      const asked = Date.now();
      await invoke(page, "workflow_run_cancel", { run: runB0.id });
      const runB = await finish(runB0.id, 60_000);
      const seconds = (Date.now() - asked) / 1000;
      check("B run canceled", runB.state === "canceled", { code: runB.code, seconds });
      check(
        "B both running branches canceled, the join skipped",
        stepOf(runB, "a").state === "canceled" && stepOf(runB, "b").state === "canceled" && stepOf(runB, "join").state === "skipped",
        runB.steps.map((s) => [s.step, s.state, s.reason]),
      );
      check("B both attempts canceled", ["a", "b"].every((s) => attempt(runB, s)?.state === "canceled"));
      await waitFor(async () => managedContainers().length <= before.length, { timeout: 30_000, interval: 500, message: "containers removed" }).catch(() => {});
      check("B no container left behind", managedContainers().length <= before.length, managedContainers());
      console.log(`info: B cancel took ${seconds.toFixed(1)} s`);
    }

    // ---- C. a condition skips a step and what needs it ----------------------
    const gate = (negate) => ({
      source: { step: "fit", port: "dataset" },
      test: { kind: "jsonCompare", pointer: "/r2", op: "ge", value: { kind: "number", value: "0.9" } },
      negate,
    });
    const conditional = v2(
      "Conditional publish",
      [
        step("fit", "Fit", jsonCode({ r2: 0.42 })),
        step("publish", "Publish the fit", jsonCode({ published: true }), { condition: gate(false) }),
        step("after", "After publishing", jsonCode({ done: true })),
        step("refit", "Refit", jsonCode({ refit: true }), { condition: gate(true) }),
      ],
      [fromInput("fit"), fromStep("fit", "publish"), fromStep("publish", "after"), fromStep("fit", "refit")],
      [outcome("published", "after"), outcome("refitted", "refit")],
      2,
    );
    const savedC = await save(conditional);
    check("C saved as a runnable v2 draft", savedC.validation?.state === "wellFormed", savedC.validation?.diagnostics);
    const { plan: planC, run: runC0 } = await start(savedC.id);
    check("C plan shows the conditions", planC.steps.find((s) => s.step === "publish")?.condition?.test?.kind === "jsonCompare", planC.blockers);
    if (runC0) {
      const runC = await finish(runC0.id);
      check("C run succeeded", runC.state === "succeeded", { code: runC.code, message: runC.message });
      const publish = stepOf(runC, "publish");
      check("C publish is conditionNotMet with its reason", publish.state === "conditionNotMet" && /0\.42/.test(publish.reason ?? "") && publish.attempts.length === 0, publish);
      const after = stepOf(runC, "after");
      check("C the step that needs it is skipped with a reason", after.state === "skipped" && /Publish the fit/.test(after.reason ?? ""), after);
      const out = runC.outcomes.find((o) => o.id === "published");
      check("C its outcome says why it was not produced", out && out.value === null && /did not run/.test(out.reason ?? ""), out);
      check("C the negated alternative ran", stepOf(runC, "refit").state === "succeeded");
      check("C only fit and refit were dispatched", runC.usage.steps === 2, runC.usage);
    }
  } finally {
    await app.close();
  }
  if (failures.length) throw new Error(`${failures.length} check(s) failed: ${failures.join("; ")}`);
}
