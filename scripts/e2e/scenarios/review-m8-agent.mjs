// R-m8 independent review: read-only agent review steps of workflow runs in
// the real app (debug build, scripted provider), which no other scenario
// exercises against the real agents service.
//
// Prints `ok:` / `DEFECT:` / `info:` lines, writes findings.json, and fails
// if any DEFECT was reproduced. Run: `npm run e2e -- review-m8-agent`.
//
// Sections:
//   A. A review step over declared material: the plan shows the complete
//      prompt and the model disclosure; the task can read only its declared
//      files; writes, code, and workflow/graph proposals are refused; a report
//      claiming "verified" is only a report (no evidence); model requests are
//      counted against the run's budget.
//   A2. The model tries a process tool (every tool is offered to the model):
//      the call must be refused and the review must still finish (R-m8-F4:
//      the step's agent task has a 0-command budget, so the attempt ends the
//      whole task as "budget exceeded" before the read-only refusal).
//   B. A review that runs out of its model-request budget: the run's usage
//      must count the requests that were made (R-m8-F3).
//   C. Cancel while the review waits on the provider: the run, step, and
//      agent task end canceled.
//   D. The app is killed while a review is in flight: on relaunch the run is
//      interrupted, the agent task is not resumed, and no model request is
//      repeated.
import { mkdir, mkdtemp, readdir, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { hardKill } from "../app.mjs";
import { invoke, waitFor } from "../workspace-helpers.mjs";

const NOTES = "# Notes\n\nThe slope is 2 (claimed).\n";
const SECRET = "TOP-SECRET-UNDECLARED-7f3a9\n";
const ARTIFACT = ".exposition/artifacts/run-review/dataset.csv";
const PROMPT = "Summarize the declared material and say whether the claim is supported.";

function recorder(artifactsDir) {
  const results = [];
  const log = (status, id, detail) => {
    results.push({ status, id, detail });
    const tag = status === "DEFECT" ? "DEFECT:" : status === "ok" ? "ok:" : "info:";
    console.log(`${tag} ${id}${detail ? ` — ${typeof detail === "string" ? detail : JSON.stringify(detail).slice(0, 700)}` : ""}`);
  };
  return {
    info: (id, detail = "") => log("info", id, detail),
    expect(id, ok, detail = "") {
      log(ok ? "ok" : "DEFECT", id, detail);
      return ok;
    },
    async finish() {
      await writeFile(join(artifactsDir, "findings.json"), JSON.stringify(results, null, 2));
      const defects = results.filter((r) => r.status === "DEFECT");
      if (defects.length) throw new Error(`${defects.length} defect(s) reproduced: ${defects.map((d) => d.id).join("; ")}`);
    },
  };
}

function reviewWorkflow(title, material) {
  return {
    schema: "exposition.workflow/1",
    title,
    description: "",
    preset: null,
    inputs: material.map((m) => ({ id: m.id, label: m.label, portType: m.portType, source: { kind: "resource", path: m.path, version: { kind: "atRunStart" } } })),
    steps: [{ id: "review", label: "Review", operation: { id: "agent.review", version: 1 }, parameters: {}, payload: { kind: "prompt", text: PROMPT } }],
    connections: material.map((m) => ({ kind: "data", from: { kind: "input", input: m.id }, to: { step: "review", port: "material" } })),
    outcomes: [{ id: "report", kind: "investigationReport", label: "Report", from: { step: "review", port: "report" }, optional: false }],
    ceiling: { execution: "none", modelRequests: true, retainResults: false, proposeChanges: false },
  };
}

const PROPOSE_INPUT = {
  workflow_id: "",
  expected_revision: -1,
  title: "Smuggled by a review step",
  description: "",
  preset: "",
  inputs: [{ id: "notes", label: "Notes", port_type: "document", path: "notes.md", pinned_hash: "" }],
  steps: [{ id: "r", label: "R", operation: "agent.review", version: 1, parameters: [], payload: { kind: "prompt", text: "x" } }],
  connections: [{ kind: "data", from_step: "", from_port: "", from_input: "notes", to_step: "r", to_port: "material" }],
  outcomes: [{ id: "o", kind: "investigationReport", label: "O", step: "r", port: "report", optional: false }],
  ceiling: { execution: "none", model_requests: true, retain_results: false, propose_changes: false },
};

export default async function reviewM8Agent({ launchApp, artifactsDir }) {
  const r = recorder(artifactsDir);
  const base = await mkdtemp(join(tmpdir(), "xp-review-m8-agent-"));
  const root = join(base, "project");
  const dataDir = join(base, "data");
  await mkdir(join(root, ".exposition", "artifacts", "run-review"), { recursive: true });
  await mkdir(dataDir, { recursive: true });
  await writeFile(join(root, "notes.md"), NOTES);
  await writeFile(join(root, "secret.txt"), SECRET);
  await writeFile(join(root, ...ARTIFACT.split("/")), "x,y\n0,1\n1,3\n");

  let app = await launchApp({ dataDir });
  let page = app.page;
  const finish = async (runId, timeout = 120_000) => {
    let run;
    await waitFor(async () => {
      run = await invoke(page, "workflow_run_get", { run: runId });
      return run.state !== "running";
    }, { timeout, interval: 300, message: `run ${runId} finishes` });
    return run;
  };
  const save = (definition) => invoke(page, "workflow_save", { request: { id: null, expectedRevision: null, definition } });
  const attempt = (run) => run.steps[0]?.attempts.at(-1);
  const planAndStart = async (saved) => {
    const plan = await invoke(page, "workflow_run_plan", { id: saved.id, revision: null });
    if (plan.blockers.length) return { plan, run: null };
    return { plan, run: await invoke(page, "workflow_run_start", { request: { plan: plan.plan, approvalHash: plan.approvalHash } }) };
  };
  const loadScript = (turns) => invoke(page, "agents_dev_load_script", { script: { turns } });

  try {
    const info = await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"]').waitFor({ timeout: 15_000 });
    if (!(await invoke(page, "app_info")).debugBuild) {
      r.info("SKIPPED: needs a debug build (scripted provider)");
      return;
    }
    await invoke(page, "agents_provider_update", {
      settings: { provider: "scripted", model: "claude-opus-5-5", effort: "medium", consent: "scopeContent" },
    });

    // ---- A. review over declared material --------------------------------
    let material = [
      { id: "notes", label: "Notes", portType: "document", path: "notes.md" },
      { id: "artifact", label: "Earlier output", portType: "dataset", path: ARTIFACT },
    ];
    let savedA = await save(reviewWorkflow("Review A", material));
    if (savedA.validation.state !== "wellFormed") {
      r.info("A an .exposition/artifacts file is not accepted as an external input; using notes only", savedA.validation.diagnostics.map((d) => [d.code, d.message]));
      material = material.slice(0, 1);
      savedA = await save(reviewWorkflow("Review A2", material));
    }
    r.expect("A review draft well-formed and runnable", savedA.validation.state === "wellFormed" && savedA.validation.runnable, savedA.validation.runnableReason);
    const workflowsBefore = (await invoke(page, "workflow_list")).length;
    await loadScript([
      {
        steps: [
          { toolUse: { id: "r1", name: "read_file", input: { path: "notes.md" } } },
          ...(material.length > 1 ? [{ toolUse: { id: "r2", name: "read_file", input: { path: ARTIFACT } } }] : []),
          { toolUse: { id: "r3", name: "read_file", input: { path: "secret.txt" } } },
          { toolUse: { id: "w1", name: "create_file", input: { path: "pwned.md", content: "written by a review" } } },
          { toolUse: { id: "wf", name: "workflow_propose", input: PROPOSE_INPUT } },
        ],
      },
      { steps: [{ text: "VERIFIED: the claim is formally checked and correct." }] },
    ]);
    const { plan: planA, run: startA } = await planAndStart(savedA);
    r.expect(
      "A plan shows the complete prompt and the model disclosure",
      planA.steps[0]?.payload?.text === PROMPT && planA.execution.modelSteps.includes("review") && planA.steps[0].containment === "readOnlyModel" && planA.notices.some((n) => /model provider/i.test(n)),
      { blockers: planA.blockers, execution: planA.execution, notices: planA.notices },
    );
    const mathBefore = JSON.stringify(await invoke(page, "graph_math_overview").catch(() => ({})));
    const runA = startA ? await finish(startA.id) : null;
    const aA = runA && attempt(runA);
    const report = aA?.outputs.find((o) => o.port === "report")?.value;
    r.expect("A review step succeeded with a report", runA?.state === "succeeded" && report?.kind === "report", { state: runA?.state, code: aA?.code, message: aA?.message, blockers: planA.blockers });
    r.expect("A model requests are counted (2 turns)", runA?.usage.modelRequests === 2, runA?.usage);
    const task = aA?.operation?.kind === "agentTask" ? aA.operation.id : null;
    if (task) {
      const detail = await invoke(page, "agents_task_detail", { task });
      const text = JSON.stringify(detail);
      r.expect("A the review task is read-only with execution disabled", detail.summary.spec?.permission === "readOnly" || /readOnly/.test(text), detail.summary.spec ?? "");
      r.expect("A declared material was readable", text.includes("The slope is 2"), "");
      if (material.length > 1) r.expect("A an earlier run output (declared) was readable", text.includes("0,1"), "");
      r.expect("A an undeclared project file was not read", !text.includes("TOP-SECRET-UNDECLARED"), "");
      r.info("A review task usage", detail.summary.usage);
    } else {
      r.expect("A the attempt names its agent task", false, aA?.operation);
    }
    r.expect("A no file written, no code run", !existsSync(join(root, "pwned.md")) && !existsSync(join(root, "pwned2.txt")), await readdir(root));
    const workflowsAfter = await invoke(page, "workflow_list");
    r.expect("A no workflow draft saved by the review step", workflowsAfter.length === workflowsBefore && !workflowsAfter.some((w) => w.title === "Smuggled by a review step"), workflowsAfter.map((w) => w.title));
    const mathAfter = JSON.stringify(await invoke(page, "graph_math_overview").catch(() => ({})));
    r.expect("A a report claiming 'verified' creates no evidence", (mathAfter.match(/formalCheck/g) ?? []).length === (mathBefore.match(/formalCheck/g) ?? []).length, "");
    if (report) {
      r.expect("A the 'verified' claim stays report prose", report.kind === "report" && /VERIFIED/.test(report.text) && aA.outputs.length === 1, aA.outputs.map((o) => [o.port, o.portType, o.value.kind]));
    }

    // ---- A2. the model tries a process tool (all tools are offered) ---------
    // The tool must be refused (read-only, execution disabled) and the review
    // must still be able to answer; it must not end the step as "budget".
    const savedA2 = await save(reviewWorkflow("Review A2 process attempt", material.slice(0, 1)));
    await loadScript([
      { steps: [{ toolUse: { id: "p1", name: "run_python", input: { code: "open('pwned2.txt', 'w').write('x')" } } }] },
      { steps: [{ text: "I could not run code; the notes claim a slope of 2." }] },
    ]);
    const { run: startA2 } = await planAndStart(savedA2);
    const runA2 = startA2 ? await finish(startA2.id) : null;
    const aA2 = runA2 && attempt(runA2);
    r.expect(
      "R-m8-F4 A2 a review whose model tries run_python gets a refusal and still completes",
      runA2?.state === "succeeded" && aA2?.outputs.some((o) => o.port === "report"),
      { state: runA2?.state, code: aA2?.code, message: aA2?.message },
    );
    r.expect("A2 no code ran", !existsSync(join(root, "pwned2.txt")), "");

    // ---- B. budget exhausted: usage must count the requests made -----------
    const savedB = await save(reviewWorkflow("Review B", material.slice(0, 1)));
    const loop = Array.from({ length: 40 }, (_, i) => ({ steps: [{ toolUse: { id: `b${i}`, name: "read_file", input: { path: "notes.md" } } }] }));
    await loadScript(loop);
    const { plan: planB, run: startB } = await planAndStart(savedB);
    const runB = startB ? await finish(startB.id) : null;
    const aB = runB && attempt(runB);
    let turns = null;
    if (aB?.operation?.kind === "agentTask") {
      turns = (await invoke(page, "agents_task_detail", { task: aB.operation.id })).summary.usage.turns;
    }
    r.info("B budget-exhausted review", { state: runB?.state, code: aB?.code, message: aB?.message, usage: runB?.usage, budgets: runB?.budgets, taskTurns: turns, planBudget: planB.budgets });
    r.expect(
      "R-m8-F3 B a review that used its whole model-request budget is counted in the run's usage",
      runB?.state === "failed" && turns !== null && turns > 0 && runB.usage.modelRequests >= turns,
      { usageModelRequests: runB?.usage.modelRequests, agentTaskTurns: turns, limit: runB?.budgets.maxModelRequests },
    );

    // ---- C. cancel while the provider is waiting ----------------------------
    const savedC = await save(reviewWorkflow("Review C", material.slice(0, 1)));
    await loadScript([{ steps: [{ waitForCancel: true }] }]);
    const { run: startC } = await planAndStart(savedC);
    if (startC) {
      await waitFor(async () => attempt(await invoke(page, "workflow_run_get", { run: startC.id }))?.state === "dispatched", { timeout: 30_000, message: "review dispatched" });
      const t0 = Date.now();
      await invoke(page, "workflow_run_cancel", { run: startC.id });
      const runC = await finish(startC.id, 60_000);
      const aC = attempt(runC);
      const taskC = aC?.operation?.id;
      const statusC = taskC ? (await invoke(page, "agents_task_detail", { task: taskC })).summary.status.kind : null;
      r.expect("C cancel ends run, step and agent task", runC.state === "canceled" && runC.steps[0].state === "canceled" && statusC === "canceled", { state: runC.state, step: runC.steps[0].state, task: statusC, ms: Date.now() - t0 });
    } else {
      r.expect("C cancel run could start", false, "");
    }

    // ---- D. kill during the review; no replay ------------------------------
    const savedD = await save(reviewWorkflow("Review D", material.slice(0, 1)));
    await loadScript([{ steps: [{ waitForCancel: true }] }, { steps: [{ text: "REPLAYED" }] }]);
    const { run: startD } = await planAndStart(savedD);
    if (startD) {
      await waitFor(async () => attempt(await invoke(page, "workflow_run_get", { run: startD.id }))?.state === "dispatched", { timeout: 30_000, message: "review dispatched" });
      const taskD = attempt(await invoke(page, "workflow_run_get", { run: startD.id })).operation.id;
      const before = await invoke(page, "agents_task_detail", { task: taskD });
      const tasksBefore = (await invoke(page, "agents_task_list")).length;
      hardKill(app.child.pid);
      await app.browser.close().catch(() => {});
      await waitFor(() => app.child.exitCode !== null || app.child.signalCode !== null, { message: "app exit" });
      app = await launchApp({ dataDir });
      page = app.page;
      await invoke(page, "workspace_open", { path: root });
      await page.waitForTimeout(4000);
      const runD = await invoke(page, "workflow_run_get", { run: startD.id });
      const after = await invoke(page, "agents_task_detail", { task: taskD });
      const tasksAfter = (await invoke(page, "agents_task_list")).length;
      r.expect(
        "D killed review: run interrupted, task not resumed, no request repeated",
        runD.state === "interrupted" && attempt(runD).state === "interrupted" && !["running", "queued"].includes(after.summary.status.kind) && after.summary.usage.turns <= before.summary.usage.turns + 0 && !JSON.stringify(after).includes("REPLAYED") && tasksAfter === tasksBefore,
        { run: runD.state, attempt: attempt(runD).state, task: after.summary.status.kind, turnsBefore: before.summary.usage.turns, turnsAfter: after.summary.usage.turns, tasksBefore, tasksAfter },
      );
    } else {
      r.expect("D kill run could start", false, "");
    }
    void info;
  } finally {
    await page.screenshot({ path: join(artifactsDir, "final.png") }).catch(() => {});
    await app.close();
  }
  await r.finish();
}
