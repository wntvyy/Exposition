// LIVE: a real model drafts the formal audit workflow of the 2026-11-15
// walkthrough (opt-in: `npm run e2e -- live-workflow-audit`). Costs money: it
// uses the API key saved in the app's provider settings. Small hard budgets.
//
// The request is the user's, in plain words: it names no port, parameter, or
// operation id. What is checked:
//   - application: saving ran nothing; every proposal of the task is one
//     workflow (a correction is a revision, never a second draft); the saved
//     draft opens by its own id.
//   - model (reported, depends on the model): the first workflow_propose is
//     already well-formed, because the tool schema lists each operation's
//     exact ports and parameters; the closing message does not claim a run
//     and says where the draft is.
// Nothing is executed: no Lean run, no plan is started.
import { mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { invoke } from "../workspace-helpers.mjs";

const BUDGET = { maxTurns: 6, maxOutputTokens: 12_000, maxInputTokens: 150_000, maxWallMs: 300_000, maxToolCalls: 6, maxCommands: 0 };
const LEAN = `def oddSum : Nat → Nat
  | 0 => 0
  | n + 1 => oddSum n + (2 * n + 1)

theorem oddSum_eq_sq (n : Nat) : oddSum n = n * n := by
  induction n with
  | zero => rfl
  | succ k ih =>
    simp only [oddSum, ih, Nat.add_mul, Nat.mul_add]
    omega
`;
const terminal = (status) => !["queued", "running", "awaitingApproval", "awaitingAnswer"].includes(status.kind);

export default async function liveWorkflowAudit({ launchApp, artifactsDir, assert }) {
  const root = await mkdtemp(join(tmpdir(), "exposition-live-wf-audit-"));
  await mkdir(join(root, "lean"), { recursive: true });
  await writeFile(join(root, "main.typ"), "= Odd sums\n\nThe sum of the first $n$ odd numbers is $n^2$.\n");
  await writeFile(join(root, "lean", "OddSums.lean"), LEAN);
  const app = await launchApp();
  const { page } = app;
  const evidence = { checks: [], proposals: [] };
  const check = (name, ok, detail = "") => {
    const text = typeof detail === "string" ? detail : JSON.stringify(detail);
    evidence.checks.push({ name, ok, detail: text.slice(0, 3000) });
    console.log(`${ok ? "ok" : "FAIL"}  ${name}: ${text.slice(0, 500)}`);
  };
  try {
    const provider = await invoke(page, "agents_provider_status");
    assert.ok(provider.configured, "an API key must be saved in the app's provider settings");
    const opened = await invoke(page, "workspace_open", { path: root });
    await invoke(page, "trust_grant", { root: opened.root });
    const prompt =
      "Exposition's workflows have a step that runs a Lean check and another that links its result to the graph. " +
      "Draft a workflow that checks lean/OddSums.lean and proposes attaching the result to the declarations it checked. " +
      "Do not create or edit any file and do not run anything. Then tell me in two or three sentences what you saved and how I run it.";
    const created = await invoke(page, "agents_task_create", {
      request: { prompt, workspaceRoot: opened.root, scope: { kind: "workspace" }, permission: "reviewEdits", execution: "approveEach", consent: "scopeContent", budgets: BUDGET },
    });
    const answered = new Set();
    let detail;
    for (const started = Date.now(); ; ) {
      detail = await invoke(page, "agents_task_detail", { task: created.id });
      for (const approval of detail.approvals) {
        if (answered.has(approval.id)) continue;
        answered.add(approval.id);
        // Only the draft itself is approved; anything else the model asks for is declined.
        await invoke(page, "agents_approval_resolve", { task: created.id, approval: approval.id, approved: approval.tool === "workflow_propose" });
      }
      if (terminal(detail.summary.status)) break;
      if (Date.now() - started > 360_000) throw new Error("task did not finish within 360 s");
      await new Promise((r) => setTimeout(r, 500));
    }
    evidence.usage = detail.summary.usage;
    evidence.status = detail.summary.status;
    const calls = detail.transcript.filter((t) => t.kind === "toolCall");
    const proposals = calls.filter((t) => t.name === "workflow_propose");
    const parsed = proposals.map((p) => {
      let result = null;
      try {
        result = JSON.parse(p.result ?? "null");
      } catch {
        result = { raw: String(p.result).slice(0, 400) };
      }
      return { status: p.status, input: p.input, result };
    });
    evidence.proposals = parsed;
    evidence.tools = calls.map((c) => `${c.name}:${c.status}`);
    evidence.messages = detail.transcript.filter((t) => t.kind !== "toolCall").map((t) => ({ kind: t.kind, role: t.role, text: (t.text ?? "").slice(0, 3000) }));
    check("model: the task called workflow_propose", proposals.length >= 1, evidence.tools);
    const first = parsed[0]?.result ?? {};
    check("model: the FIRST proposal was already well-formed (no guessed port or setting)", first.saved === true && first.state === "wellFormed" && first.errors === 0, {
      state: first.state,
      errors: first.errors,
      diagnostics: first.diagnostics,
      steps: parsed[0]?.input?.steps?.map((s) => [s.operation, s.parameters]),
      connections: parsed[0]?.input?.connections,
    });
    const saved = (await invoke(page, "workflow_list")).filter((w) => w.origin?.kind === "agent" && w.origin.taskId === created.id);
    check("every proposal of the task is one workflow (a correction is a revision)", saved.length === 1 && parsed.every((p) => !p.result?.workflow || p.result.workflow === saved[0].id), {
      workflows: saved.map((w) => [w.id, w.revision, w.state, w.title]),
      proposals: parsed.map((p) => [p.result?.workflow, p.result?.revision, p.result?.state]),
    });
    check("saving ran nothing", (await invoke(page, "workflow_runs", { workflow: null })).length === 0, "");
    if (saved[0]) {
      const full = await invoke(page, "workflow_get", { id: saved[0].id, revision: null });
      const listing = await invoke(page, "research_listing");
      check("the saved draft opens by its own id and is well-formed", full.validation.state === "wellFormed", {
        id: full.id,
        entity: full.entity,
        operations: full.definition.steps.map((s) => s.operation.id),
        researchItem: listing.workflows.find((w) => w.entity === full.entity) ?? "no research space in this project",
      });
    }
    const closing = evidence.messages.filter((m) => m.text).at(-1)?.text ?? "";
    // "Nothing was run" and "has not run" are the truthful statements; only a positive claim counts.
    const positive = closing.replace(/\b(nothing|not|never)\b[^.]*?\b(run|ran|verified)\b/gi, "");
    check("model: the closing message does not claim a run and says where the draft is", !/\b(has run|was run|ran successfully|is verified|was verified)\b/i.test(positive) && /Workflows|workflow editor|Review and run/i.test(closing), closing);
    await page.screenshot({ path: join(artifactsDir, "live-workflow-audit.png") }).catch(() => {});
  } finally {
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2));
    await app.close();
  }
  const failed = evidence.checks.filter((c) => !c.ok && !c.name.startsWith("model:"));
  assert.deepEqual(failed.map((c) => c.name), [], "application-enforced checks");
}
