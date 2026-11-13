// LIVE: a real model proposes a workflow, the user reviews and runs it
// (opt-in: `npm run e2e -- live-workflow`). Costs money: it uses the API key
// saved in the app's provider settings. Hard budgets on the task.
//
// M8 evidence in the real app with a real model:
//   W1. A whole-project task (restricted execution, so its ceiling may be
//       restricted) proposes a restricted
//       research Python workflow over the declared data with workflow_propose;
//       the approval shows the complete definition; saving it runs nothing.
//   W2. The user plans the saved draft (full payload, restricted containment,
//       inputs pinned by hash) and starts it; it runs in the Docker sandbox and
//       its validated output exists in the project. The agent itself cannot
//       start a run (no such tool).
//   W3. A user-saved workflow with a read-only agent review step runs against
//       the real model: the plan shows it sends material to the model and
//       runs nothing; the review is a read-only task with execution disabled,
//       its model requests count against the run's budget, it returns a
//       report, and no project file changes.
// Application-enforced properties are hard checks; steps that depend on the
// model following the request are reported as `model:` checks.
import { execFileSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { invoke, waitFor } from "../workspace-helpers.mjs";

const IMAGE = "python:3.13-slim";
const BUDGET = { maxTurns: 10, maxOutputTokens: 30_000, maxInputTokens: 300_000, maxWallMs: 600_000, maxToolCalls: 12, maxCommands: 0 };
const terminal = (status) => !["queued", "running", "awaitingApproval"].includes(status.kind);
const toolCalls = (detail, name) => detail.transcript.filter((t) => t.kind === "toolCall" && (!name || t.name === name));

export default async function liveWorkflow({ launchApp, artifactsDir, assert }) {
  execFileSync("docker", ["image", "inspect", IMAGE], { stdio: "ignore", timeout: 30_000 });
  const root = await mkdtemp(join(tmpdir(), "exposition-live-workflow-"));
  await mkdir(join(root, "data"), { recursive: true });
  await writeFile(join(root, "data", "obs.csv"), "x,y\n0,1\n1,3\n2,5\n3,7\n");
  await writeFile(join(root, "README.md"), "# Observations\n\nA small numerical study.\n");
  const app = await launchApp();
  const { page } = app;
  const evidence = { root, approvals: [], checks: [] };
  const check = (name, ok, detail = "") => {
    const text = typeof detail === "string" ? detail : JSON.stringify(detail);
    evidence.checks.push({ name, ok, detail: text.slice(0, 2000) });
    console.log(`${ok ? "ok" : "FAIL"}  ${name}: ${text.slice(0, 300)}`);
  };
  try {
    const provider = await invoke(page, "agents_provider_status");
    assert.ok(provider.configured, "an API key must be saved in the app's provider settings");
    const opened = await invoke(page, "workspace_open", { path: root });
    await invoke(page, "execution_settings_update", { change: { kind: "dockerImage", image: IMAGE } });
    const runsBefore = await invoke(page, "workflow_runs", { workflow: null });

    const prompt = [
      "Use workflow_propose to save a workflow titled 'Slope of the observations'.",
      "It has one external input with id data, port_type dataset, path data/obs.csv, and one step with id fit using operation research.pythonBatch version 1",
      "whose payload is Python code (kind code, language python) that reads data/obs.csv (it is available at that relative path), computes the least-squares slope of y on x",
      "using only the standard library, and writes a CSV with columns slope,intercept to the file dataset.csv inside the directory named by the environment variable EXPOSITION_OUTPUT_DIR.",
      "Connect the input data to the step's data port, declare one required outcome of kind transformedDataset from the step's dataset port,",
      "and set the ceiling to execution restricted with model_requests, retain_results and propose_changes false. Do not create or edit files. Finish with one sentence.",
    ].join(" ");
    const created = await invoke(page, "agents_task_create", {
      request: { prompt, workspaceRoot: opened.root, scope: { kind: "workspace" }, permission: "reviewEdits", execution: "restricted", consent: "scopeContent", budgets: BUDGET },
    });
    const answered = new Set();
    let detail;
    for (const started = Date.now(); ; ) {
      detail = await invoke(page, "agents_task_detail", { task: created.id });
      for (const approval of detail.approvals) {
        if (answered.has(approval.id)) continue;
        answered.add(approval.id);
        const approved = approval.tool === "workflow_propose";
        evidence.approvals.push({ tool: approval.tool, approved, detail: approval.detail.slice(0, 1500) });
        await invoke(page, "agents_approval_resolve", { task: created.id, approval: approval.id, approved });
      }
      if (terminal(detail.summary.status)) break;
      if (Date.now() - started > 600_000) throw new Error("task did not finish within 600 s");
      await new Promise((r) => setTimeout(r, 500));
    }
    evidence.usage = detail.summary.usage;
    const proposals = toolCalls(detail, "workflow_propose");
    check("model: the task called workflow_propose", proposals.length >= 1, toolCalls(detail).map((c) => c.name));
    const card = evidence.approvals.find((a) => a.tool === "workflow_propose");
    check("the approval showed the complete definition", !!card && /EXPOSITION_OUTPUT_DIR|research\.pythonBatch/.test(card.detail), card?.detail.slice(0, 300));
    const runsAfterProposal = await invoke(page, "workflow_runs", { workflow: null });
    check("saving the proposal ran nothing", runsAfterProposal.length === runsBefore.length, runsAfterProposal.length);
    const draft = (await invoke(page, "workflow_list")).find((w) => w.origin?.kind === "agent" && w.origin.taskId === created.id);
    check("model: an agent-attributed draft was saved", !!draft, (await invoke(page, "workflow_list")).map((w) => [w.title, w.origin]));
    if (draft) {
      const plan = await invoke(page, "workflow_run_plan", { id: draft.id, revision: null });
      check("model: the draft is runnable (no blockers)", plan.blockers.length === 0, plan.blockers);
      check("plan: restricted containment and pinned input", plan.steps.every((s) => s.containment === "restrictedContainer") && plan.inputs.every((i) => i.sha256.length === 64), plan.steps.map((s) => s.containment));
      if (plan.blockers.length === 0) {
        let run = await invoke(page, "workflow_run_start", { request: { plan: plan.plan, approvalHash: plan.approvalHash } });
        await waitFor(async () => (run = await invoke(page, "workflow_run_get", { run: run.id })).state !== "running", { timeout: 240_000, interval: 500, message: "run finishes" });
        const out = run.steps.flatMap((s) => s.attempts.at(-1)?.outputs ?? []).find((o) => o.port === "dataset");
        const text = out?.value?.path ? await readFile(join(root, ...out.value.path.split("/")), "utf8") : "";
        check("model: the agent-generated workflow ran in the sandbox and produced its dataset", run.state === "succeeded" && /slope/i.test(text), { state: run.state, text: text.slice(0, 200), log: run.steps[0]?.attempts.at(-1)?.log?.slice(-300) });
        if (/slope/i.test(text)) {
          const value = Number(text.trim().split(/\r?\n/)[1]?.split(",")[0]);
          check("model: slope computed correctly (2)", Math.abs(value - 2) < 1e-6, value);
        }
      }
    }
    // --- W3: a read-only agent review step runs against the real model ------
    const readmeBefore = await readFile(join(root, "README.md"), "utf8");
    const csvBefore = await readFile(join(root, "data", "obs.csv"), "utf8");
    const reviewDef = {
      schema: "exposition.workflow/1",
      title: "Review the observations",
      description: "",
      preset: null,
      inputs: [
        { id: "data", label: "Observations", portType: "dataset", source: { kind: "resource", path: "data/obs.csv", version: { kind: "atRunStart" } } },
        { id: "readme", label: "Notes", portType: "document", source: { kind: "resource", path: "README.md", version: { kind: "atRunStart" } } },
      ],
      steps: [
        {
          id: "review",
          label: "Review",
          operation: { id: "agent.review", version: 1 },
          parameters: {},
          payload: { kind: "prompt", text: "In two sentences, say how many data rows obs.csv has and whether y looks linear in x. Do not use any tools other than reading the material." },
        },
      ],
      connections: [
        { kind: "data", from: { kind: "input", input: "data" }, to: { step: "review", port: "material" } },
        { kind: "data", from: { kind: "input", input: "readme" }, to: { step: "review", port: "material" } },
      ],
      outcomes: [{ id: "report", kind: "investigationReport", label: "Review", from: { step: "review", port: "report" }, optional: false }],
      ceiling: { execution: "none", modelRequests: true, retainResults: false, proposeChanges: false },
    };
    const saved = await invoke(page, "workflow_save", { request: { id: null, expectedRevision: null, definition: reviewDef } });
    const reviewPlan = await invoke(page, "workflow_run_plan", { id: saved.id, revision: null });
    check("W3 plan: no blockers", reviewPlan.blockers.length === 0, reviewPlan.blockers);
    check(
      "W3 plan: the review step sends material to the model and runs nothing",
      reviewPlan.steps.length === 1 && reviewPlan.steps[0].containment === "readOnlyModel" && !reviewPlan.steps[0].effects.some((e) => /execute/i.test(e)),
      reviewPlan.steps.map((s) => [s.containment, s.effects]),
    );
    if (reviewPlan.blockers.length === 0) {
      let reviewRun = await invoke(page, "workflow_run_start", { request: { plan: reviewPlan.plan, approvalHash: reviewPlan.approvalHash } });
      await waitFor(async () => (reviewRun = await invoke(page, "workflow_run_get", { run: reviewRun.id })).state !== "running", { timeout: 600_000, interval: 1000, message: "review run finishes" });
      const attempt = reviewRun.steps[0]?.attempts.at(-1);
      const report = attempt?.outputs.find((o) => o.port === "report")?.value;
      evidence.review = { state: reviewRun.state, code: reviewRun.code, message: reviewRun.message, usage: reviewRun.usage, budgets: reviewRun.budgets, report: report?.text?.slice(0, 1000) };
      check("W3: the review run succeeded with a report", reviewRun.state === "succeeded" && report?.kind === "report" && report.text.trim().length > 0, evidence.review);
      check(
        "W3: model requests counted against the run's budget",
        reviewRun.usage.modelRequests >= 1 && reviewRun.usage.modelRequests <= reviewRun.budgets.maxModelRequests,
        { usage: reviewRun.usage, budgets: reviewRun.budgets },
      );
      if (report?.task) {
        const task = await invoke(page, "agents_task_detail", { task: report.task });
        const spec = task.summary.spec;
        check("W3: the review ran as a read-only task with execution disabled", spec.permission === "readOnly" && spec.execution === "disabled", { permission: spec.permission, execution: spec.execution, scope: spec.scope });
        const used = toolCalls(task).map((c) => c.name);
        check("W3: the review task called no tool that writes or runs", !used.some((n) => /write|edit|create|run_|build|propose|insert|lean_check|command/.test(n)), used);
      } else {
        check("W3: the report names its agent task", false, report);
      }
      check(
        "W3: the project files are unchanged",
        (await readFile(join(root, "README.md"), "utf8")) === readmeBefore && (await readFile(join(root, "data", "obs.csv"), "utf8")) === csvBefore,
        "README.md, data/obs.csv",
      );
      check("model: the report mentions four rows", /\bfour\b|\b4\b/i.test(report?.text ?? ""), report?.text?.slice(0, 300));
    }

    const hard = evidence.checks.filter((c) => !c.name.startsWith("model:"));
    assert.deepEqual(hard.filter((c) => !c.ok).map((c) => c.name), [], "application-enforced checks");
  } finally {
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2));
    await app.close();
  }
}
