// LIVE: concurrent read-only agent review steps against the real model
// (opt-in: `npm run e2e -- live-m9-agents`). Costs money: it uses the API key
// saved in the app's provider settings; every review step is bounded by the
// run's reserved model-request budget (25 per model step).
//
// M9 acceptance item 3 (multi-agent) in the real app with a real model:
//   A user-saved workflow (schema /2, maxParallel 2, ceiling: model requests
//   only) with two agent.review steps over different declared material (a CSV
//   and a notes file) and a third agent.review step whose `multiple` material
//   port receives both reports.
//   (a) the two reviews overlapped in time (both seen running at once over
//       IPC, and by their attempt timestamps);
//   (b) each was its own read-only task, execution disabled, scoped to exactly
//       its declared material;
//   (c) the consumer received both reports, in binding order;
//   (d) model requests were reserved per step (25 each) and the run's usage
//       equals the requests its tasks made, within the run's budget;
//   (e) no project file changed, nothing was saved (no workflow, no graph
//       proposal), and no process or writing tool was attempted.
// Application-enforced properties are hard checks; what depends on the model
// following the instructions is reported as `model:` checks.
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, readdir, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, relative } from "node:path";

import { invoke, waitFor } from "../workspace-helpers.mjs";

const OBS = "x,y\n0,1\n1,3\n2,5\n3,7\n";
const NOTES = "# Method\n\nWe fit a straight line to the observations by least squares and report the slope.\n";
const toolCalls = (detail) => detail.transcript.filter((t) => t.kind === "toolCall");
const WRITING_OR_RUNNING = /write|edit|create|delete|rename|run_|python|lean|build|command|propose|insert|retain|promote/i;

async function snapshot(root) {
  const files = {};
  const walk = async (dir) => {
    for (const entry of await readdir(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) {
        // Application metadata (graph, run records) is not project content.
        if (entry.name !== ".exposition") await walk(path);
      } else {
        files[relative(root, path).split("\\").join("/")] = createHash("sha256").update(await readFile(path)).digest("hex");
      }
    }
  };
  await walk(root);
  return files;
}

const review = (id, label, text) => ({
  id,
  label,
  operation: { id: "agent.review", version: 1 },
  parameters: {},
  payload: { kind: "prompt", text },
});
const material = (from, to) => ({ kind: "data", from, to: { step: to, port: "material" } });
const input = (id) => ({ kind: "input", input: id });
const report = (step) => ({ kind: "step", step, port: "report" });

const DEFINITION = {
  schema: "exposition.workflow/2",
  title: "Parallel reviews (live)",
  description: "",
  preset: null,
  inputs: [
    { id: "data", label: "Observations", portType: "dataset", source: { kind: "resource", path: "data/obs.csv", version: { kind: "atRunStart" } } },
    { id: "notes", label: "Method notes", portType: "document", source: { kind: "resource", path: "notes/method.md", version: { kind: "atRunStart" } } },
  ],
  steps: [
    review("reviewData", "Review the data", "Answer in one sentence that starts with 'DATA REVIEW:'. How many data rows (not counting the header) does obs.csv have, and does y look linear in x? Only read the material."),
    review("reviewNotes", "Review the notes", "Answer in one sentence that starts with 'NOTES REVIEW:'. What fitting method do the notes describe? Only read the material."),
    review("combine", "Combine the reviews", "In two sentences, combine the two earlier review reports. Mention the row count and the fitting method. Only read the material."),
  ],
  connections: [
    material(input("data"), "reviewData"),
    material(input("notes"), "reviewNotes"),
    // Binding order: the data review's report first, then the notes review's.
    material(report("reviewData"), "combine"),
    material(report("reviewNotes"), "combine"),
    material(input("notes"), "combine"),
  ],
  outcomes: [{ id: "summary", kind: "investigationReport", label: "Combined review", from: { step: "combine", port: "report" }, optional: false }],
  ceiling: { execution: "none", modelRequests: true, retainResults: false, proposeChanges: false },
  execution: { maxParallel: 2 },
};

export default async function liveM9Agents({ launchApp, artifactsDir, assert }) {
  const root = await mkdtemp(join(tmpdir(), "exposition-live-m9-agents-"));
  await mkdir(join(root, "data"), { recursive: true });
  await mkdir(join(root, "notes"), { recursive: true });
  await writeFile(join(root, "data", "obs.csv"), OBS);
  await writeFile(join(root, "notes", "method.md"), NOTES);
  const app = await launchApp();
  const { page } = app;
  const evidence = { root, checks: [], tasks: {} };
  const check = (name, ok, detail = "") => {
    const text = typeof detail === "string" ? detail : JSON.stringify(detail);
    evidence.checks.push({ name, ok, detail: text.slice(0, 2000) });
    console.log(`${ok ? "ok" : "FAIL"}  ${name}: ${text.slice(0, 300)}`);
  };
  try {
    const provider = await invoke(page, "agents_provider_status");
    assert.ok(provider.configured, "an API key must be saved in the app's provider settings");
    await invoke(page, "workspace_open", { path: root });
    const before = await snapshot(root);

    const saved = await invoke(page, "workflow_save", { request: { id: null, expectedRevision: null, definition: DEFINITION } });
    check("the user's v2 workflow is well-formed", saved.validation.state === "wellFormed", saved.validation.diagnostics);
    const workflowsBefore = (await invoke(page, "workflow_list")).length;
    const plan = await invoke(page, "workflow_run_plan", { id: saved.id, revision: null });
    check("plan: no blockers", plan.blockers.length === 0, plan.blockers);
    check("plan: maxParallel 2 and the agent limit 2", plan.maxParallel === 2 && plan.limits.agent === 2, { maxParallel: plan.maxParallel, limits: plan.limits });
    check(
      "plan: three read-only model steps, none executes anything, 25 requests reserved per step",
      plan.steps.length === 3 &&
        plan.steps.every((s) => s.containment === "readOnlyModel" && !s.effects.some((e) => /execute/i.test(e)) && s.maxModelRequests === 25),
      plan.steps.map((s) => [s.step, s.containment, s.effects, s.maxModelRequests]),
    );
    check("plan: the run's model budget is 3 × 25", plan.budgets.maxModelRequests === 75, plan.budgets);
    check("plan: names the steps that send material to the model", plan.execution.modelSteps.length === 3, plan.execution.modelSteps);
    if (plan.blockers.length) throw new Error("the plan is blocked");

    // ---- run: approve the plan, watch both reviews run at once -------------
    let run = await invoke(page, "workflow_run_start", { request: { plan: plan.plan, approvalHash: plan.approvalHash } });
    let together = false;
    let sawCombineEarly = false;
    await waitFor(async () => {
      run = await invoke(page, "workflow_run_get", { run: run.id });
      const state = (id) => run.steps.find((s) => s.step === id).state;
      if (state("reviewData") === "running" && state("reviewNotes") === "running") together = true;
      if (state("combine") === "running" && (state("reviewData") !== "succeeded" || state("reviewNotes") !== "succeeded")) sawCombineEarly = true;
      return run.state !== "running";
    }, { timeout: 900_000, interval: 250, message: "the run finishes" });
    const attempt = (id) => run.steps.find((s) => s.step === id).attempts.at(-1);
    const reportOf = (id) => attempt(id)?.outputs.find((o) => o.port === "report")?.value;
    evidence.run = { state: run.state, code: run.code, message: run.message, usage: run.usage, budgets: run.budgets };
    check("the run succeeded", run.state === "succeeded", evidence.run);

    // (a) overlap
    check("(a) both reviews were running at the same time (workflow_run_get)", together);
    const [a, b] = [attempt("reviewData"), attempt("reviewNotes")];
    check(
      "(a) their attempts overlap in time",
      !!a && !!b && a.dispatchedAtMs < b.finishedAtMs && b.dispatchedAtMs < a.finishedAtMs,
      { data: [a?.dispatchedAtMs, a?.finishedAtMs], notes: [b?.dispatchedAtMs, b?.finishedAtMs] },
    );
    check("(c) the consumer started only after both reviews succeeded", !sawCombineEarly && attempt("combine").dispatchedAtMs >= Math.max(a.finishedAtMs, b.finishedAtMs), {
      combine: attempt("combine")?.dispatchedAtMs,
    });

    // (b) separate read-only tasks scoped to the declared material
    const expectedScope = { reviewData: ["data/obs.csv"], reviewNotes: ["notes/method.md"], combine: ["notes/method.md"] };
    const tasks = {};
    for (const id of ["reviewData", "reviewNotes", "combine"]) {
      const value = reportOf(id);
      if (!value?.task) {
        check(`(b) ${id}: the report names its agent task`, false, value);
        continue;
      }
      const detail = await invoke(page, "agents_task_detail", { task: value.task });
      tasks[id] = { detail, report: value.text };
      const spec = detail.summary.spec;
      evidence.tasks[id] = {
        task: value.task,
        spec: { permission: spec.permission, execution: spec.execution, scope: spec.scope, consent: spec.consent, budgets: spec.budgets },
        usage: detail.summary.usage,
        tools: toolCalls(detail).map((c) => c.name),
        report: value.text.slice(0, 800),
      };
      check(`(b) ${id}: the attempt's operation is this agent task`, attempt(id).operation?.kind === "agentTask" && attempt(id).operation.id === value.task, attempt(id).operation);
      check(`(b) ${id}: read-only, execution disabled`, spec.permission === "readOnly" && spec.execution === "disabled", { permission: spec.permission, execution: spec.execution });
      check(
        `(b) ${id}: scoped to exactly its declared material files`,
        spec.scope.kind === "files" && JSON.stringify([...spec.scope.paths].sort()) === JSON.stringify(expectedScope[id]),
        spec.scope,
      );
      check(`(d) ${id}: its task may make at most the 25 requests reserved for the step`, spec.budgets.maxTurns <= 25 && spec.budgets.maxCommands === 0, spec.budgets);
    }
    const ids = Object.values(tasks).map((t) => t.detail.summary.id);
    check("(b) three distinct tasks", new Set(ids).size === 3, ids);

    // (c) both reports reached the consumer, in binding order
    if (tasks.combine && tasks.reviewData && tasks.reviewNotes) {
      const prompt = tasks.combine.detail.summary.spec.prompt;
      const first = prompt.indexOf(tasks.reviewData.report.trim().slice(0, 60));
      const second = prompt.indexOf(tasks.reviewNotes.report.trim().slice(0, 60));
      check("(c) the consumer received both reports, the data review's first (binding order)", first >= 0 && second > first, { first, second });
    }

    // (d) usage
    const turns = Object.values(tasks).reduce((n, t) => n + t.detail.summary.usage.turns, 0);
    check("(d) the run's model requests are exactly what its tasks made", run.usage.modelRequests === turns, { run: run.usage.modelRequests, tasks: turns });
    check("(d) within the run's budget", run.usage.modelRequests >= 3 && run.usage.modelRequests <= run.budgets.maxModelRequests, { usage: run.usage, budgets: run.budgets });
    check("(d) three steps dispatched", run.usage.steps === 3, run.usage);

    // (e) nothing changed or saved, no process/writing tool
    const after = await snapshot(root);
    check("(e) no project file changed or appeared", JSON.stringify(after) === JSON.stringify(before), { before: Object.keys(before), after: Object.keys(after) });
    check("(e) no workflow was saved by the run", (await invoke(page, "workflow_list")).length === workflowsBefore);
    const used = Object.values(tasks).flatMap((t) => toolCalls(t.detail).map((c) => c.name));
    check("(e) no task attempted a process or writing tool", !used.some((n) => WRITING_OR_RUNNING.test(n)), used);
    check("(e) no task ran a command", Object.values(tasks).every((t) => t.detail.summary.usage.commands === 0));
    const pending = Object.values(tasks).flatMap((t) => t.detail.approvals ?? []);
    check("(e) no task asked for an approval", pending.length === 0, pending.map((p) => p.tool));

    // model-dependent observations
    check("model: the data review starts with its marker and counts four rows", /DATA REVIEW/.test(tasks.reviewData?.report ?? "") && /\bfour\b|\b4\b/i.test(tasks.reviewData?.report ?? ""), tasks.reviewData?.report?.slice(0, 300));
    check("model: the notes review names least squares", /NOTES REVIEW/.test(tasks.reviewNotes?.report ?? "") && /least[- ]squares/i.test(tasks.reviewNotes?.report ?? ""), tasks.reviewNotes?.report?.slice(0, 300));
    check("model: the combined review mentions both", /\bfour\b|\b4\b/i.test(tasks.combine?.report ?? "") && /least[- ]squares|straight line|linear/i.test(tasks.combine?.report ?? ""), tasks.combine?.report?.slice(0, 400));

    const usage = Object.values(tasks).map((t) => t.detail.summary.usage);
    const sum = (k) => usage.reduce((n, u) => n + (u[k] ?? 0), 0);
    evidence.tokens = { input: sum("inputTokens"), output: sum("outputTokens"), cacheRead: sum("cacheReadTokens"), cacheWrite: sum("cacheWriteTokens"), turns: sum("turns"), servedBy: usage.map((u) => u.servedBy) };
    console.log(`info  tokens: ${JSON.stringify(evidence.tokens)}`);

    const hard = evidence.checks.filter((c) => !c.name.startsWith("model:"));
    assert.deepEqual(hard.filter((c) => !c.ok).map((c) => c.name), [], "application-enforced checks");
  } finally {
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2));
    await app.close();
  }
}
