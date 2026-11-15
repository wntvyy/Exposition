// An agent-drafted formal audit workflow, end to end through the UI with the
// real Lean toolchain (leanprover/lean4:v4.35.0) on a standalone Lean file
// (no Lake project), as in the 2026-11-15 walkthrough:
//
//   draft by workflow_propose (scripted provider; nothing runs) → open from
//   Research → Workflows → inspector → "Review and run…" → the plan names the
//   native step → Start run → Lean audit → association proposal (pending,
//   nothing recorded) → accept → FormalCheck evidence on the audited
//   declarations → change the Lean file → Run again asks for a new review
//   (the input changed) → the new run audits the new content.
//
// It also records the execution jobs listed while the run and the Lean
// server are active and afterwards (what "N jobs running" counts).
//
// Requires Lean through elan with leanprover/lean4:v4.35.0; without it the
// scenario reports SKIPPED unless EXPOSITION_REQUIRE_LEAN=1.
import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";

import { invoke, waitFor } from "../workspace-helpers.mjs";
import { agentDraft, toolResults, waitTaskDone } from "./workflow-identity.mjs";

const TOOLCHAIN_DIR = "leanprover--lean4---v4.35.0";
const LEAN = "lean/OddSums.lean";
const LEAN_TEXT = `def oddSum : Nat → Nat
  | 0 => 0
  | n + 1 => oddSum n + (2 * n + 1)

theorem oddSum_eq_sq (n : Nat) : oddSum n = n * n := by
  induction n with
  | zero => rfl
  | succ k ih =>
    simp only [oddSum, ih, Nat.add_mul, Nat.mul_add]
    omega
`;
const EXTRA = `
theorem oddSum_zero : oddSum 0 = 0 := rfl
`;

const tool = (id, name, input) => ({ toolUse: { id, name, input } });
const sha = (text) => createHash("sha256").update(text).digest("hex");

export default async function workflowAgentAudit({ launchApp, artifactsDir, assert }) {
  const elanHome = process.env.ELAN_HOME || join(homedir(), ".elan");
  if (!existsSync(join(elanHome, "toolchains", TOOLCHAIN_DIR, "bin"))) {
    const message = `SKIPPED: Lean toolchain ${TOOLCHAIN_DIR} is not installed through elan`;
    if (process.env.EXPOSITION_REQUIRE_LEAN === "1") throw new Error(message);
    console.log(message);
    return;
  }
  const base = await mkdtemp(join(tmpdir(), "xp-wf-audit-"));
  const root = join(base, "project");
  const dataDir = join(base, "data");
  await mkdir(join(root, "lean"), { recursive: true });
  await mkdir(dataDir, { recursive: true });
  await writeFile(join(root, "main.typ"), "= Odd sums\n\nThe sum of the first $n$ odd numbers is $n^2$.\n");
  await writeFile(join(root, ...LEAN.split("/")), LEAN_TEXT);

  const observations = [];
  const note = (id, ok, detail) => {
    observations.push({ id, ok: !!ok, detail });
    console.log(`${ok ? "ok    " : "FAIL  "} ${id}: ${typeof detail === "string" ? detail : JSON.stringify(detail)?.slice(0, 1200)}`);
  };
  const app = await launchApp({ dataDir, env: { EXPOSITION_CREDENTIAL_NAMESPACE: `e2e-${Date.now()}` } });
  const { page } = app;
  const shot = (name) => page.screenshot({ path: join(artifactsDir, `${name}.png`) }).catch(() => {});
  const gotoPage = async (name) => {
    await page.locator('nav[aria-label="Pages"]').getByRole("button", { name, exact: true }).click();
    await page.waitForTimeout(200);
  };
  const jobs = async () =>
    (await invoke(page, "job_list")).map((j) => ({ label: j.info.label, kind: j.kind, state: j.state, finished: j.info.result !== null }));
  const statusBarJobs = async () => (await page.locator('[data-testid="jobs-status"]').textContent().catch(() => null)) ?? null;
  const formalRows = async () => {
    const math = await invoke(page, "graph_math_overview");
    return math.rows.filter((r) => r.entity.kind === "formalDeclaration").map((r) => ({ label: r.entity.label, entity: r.entity.id, evidence: r.evidence?.kind ?? null, verdict: r.verdict?.kind ?? r.verdict ?? null, freshness: r.freshness?.kind ?? r.freshness ?? null }));
  };
  const finish = async (runId, timeout = 600_000) => {
    let run;
    await waitFor(
      async () => {
        run = await invoke(page, "workflow_run_get", { run: runId });
        return run.state !== "running";
      },
      { timeout, interval: 500, message: `run ${runId} finishes` },
    );
    return run;
  };
  const attempt = (run, step) => run.steps.find((s) => s.step === step)?.attempts.at(-1);
  try {
    await page.setViewportSize({ width: 1440, height: 1000 });
    const info = await invoke(page, "app_info");
    if (!info.debugBuild) throw new Error("workflow-agent-audit requires a debug build (scripted provider)");
    await invoke(page, "agents_provider_update", {
      settings: { provider: "scripted", model: "claude-opus-5-5", effort: "medium", consent: "scopeContent" },
    });
    const ws = await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="main.typ"]').waitFor({ timeout: 15_000 });
    await waitFor(async () => (await invoke(page, "graph_status")).state.kind === "attached", { timeout: 20_000, message: "graph attached" });
    await invoke(page, "trust_grant", { root: ws.root });
    await invoke(page, "research_space_enable");
    note("jobs-before-anything", true, { jobs: await jobs(), statusBar: await statusBarJobs() });

    // ----- the agent drafts the workflow (nothing runs) -----------------------------------
    await invoke(page, "agents_dev_load_script", {
      script: {
        turns: [
          { steps: [tool("inspect", "graph_query", { query: "inspect", text: "", kinds: [], entity: "", path: "main.typ", depth: 1, limit: 50 })] },
          {
            steps: [
              tool("experiment", "graph_propose", {
                summary: "Record the numerical check as an experiment",
                base_revision: "$ref:inspect/graph/revision",
                operations: [{ op: "create_entity", temp_id: "check", kind: "experiment", label: "Python check of the odd sums", attributes: [], bind_path: "", bind_start: -1, bind_end: -1 }],
              }),
            ],
          },
          { steps: [tool("wf", "workflow_propose", agentDraft())] },
          { steps: [{ text: "Drafted; it has not run." }] },
        ],
      },
    });
    const task = await invoke(page, "agents_task_create", {
      request: { prompt: "Draft the audit workflow", workspaceRoot: ws.root, scope: { kind: "workspace" }, permission: "autoApply", execution: "approveEach", consent: "scopeContent", budgets: null },
    });
    await waitTaskDone(page, task.id);
    const result = (await toolResults(page, task.id)).wf?.value ?? {};
    const runsBefore = await invoke(page, "workflow_runs", { workflow: null });
    note("agent-draft-saved-well-formed-and-nothing-ran", result.saved === true && result.state === "wellFormed" && runsBefore.length === 0, {
      ...result,
      diagnostics: undefined,
      runs: runsBefore.length,
    });
    // Research counts and their scope: an experiment the agent recorded in the
    // graph is not a member of the research space; results are retained artifacts.
    const all = await toolResults(page, task.id);
    const listing = await invoke(page, "research_listing");
    const experiments = (await invoke(page, "graph_snapshot", { query: { kinds: ["experiment"], text: null, around: null, includeInactive: false, limit: 20 } })).entities;
    note("research-counts-scope", true, {
      graphPropose: { status: all.experiment?.status, isError: all.experiment?.isError, error: all.experiment?.value?.error },
      graphExperiments: experiments.map((e) => e.label),
      research: { experiments: listing.experiments.length, results: listing.results.length, workflows: listing.workflows.length, notes: listing.notes.length },
    });
    const wf = (await invoke(page, "workflow_list")).find((w) => w.id === result.workflow);
    assert.ok(wf, "the draft is listed");

    // ----- open it where the tool result says, review the plan, start ---------------------
    await gotoPage("Research");
    await page.getByTestId("research-tree").locator(`[data-item-key="workflow:${wf.entity}"]`).first().locator(".ui-tree-item__title").click();
    await page.locator('[data-testid="research-inspector"][data-kind="workflow"]').getByRole("button", { name: "Open in the workflow editor" }).click();
    const editor = page.locator('[data-testid="workflow-editor"]:visible');
    await editor.waitFor({ timeout: 15_000 });
    assert.equal(await editor.getAttribute("data-workflow"), wf.id, "the editor loaded the workflow by its own id");
    const status = editor.getByTestId("workflow-status");
    await waitFor(async () => (await status.getAttribute("data-can-review")) === "true", { timeout: 30_000, message: "the saved revision can be reviewed" });
    note("editor-shows-agent-origin-and-reviewable-status", (await editor.getByTestId("workflow-origin").getAttribute("data-origin")) === "agent", {
      status: (await status.textContent())?.trim(),
      origin: (await editor.getByTestId("workflow-origin").textContent())?.trim(),
    });
    await shot("agent-draft-in-editor");
    await editor.getByTestId("workflow-review-run").click();
    const review = page.getByTestId("workflow-plan-review");
    await waitFor(async () => (await review.getAttribute("data-state")) === "ready", { timeout: 60_000, message: "plan ready" });
    const planText = await review.textContent();
    const leanSha = sha(await readFile(join(root, ...LEAN.split("/"))));
    note("plan-names-the-native-lean-step-and-the-exact-input", /privileges/i.test(planText) && planText.includes(LEAN) && (await review.locator(`[data-full="${leanSha}"]`).count()) > 0, {
      blockers: await page.getByTestId("plan-blockers").textContent().catch(() => null),
      notices: await page.getByTestId("plan-notices").textContent().catch(() => null),
    });
    await shot("plan-review");
    const before = await formalRows();
    await page.getByTestId("workflow-start-run").click();
    const runView = page.locator('[data-testid="workflow-run"]:visible');
    await runView.waitFor({ timeout: 30_000 });
    const runId = await runView.getAttribute("data-run");
    await page.waitForTimeout(1500);
    note("jobs-while-the-audit-runs", true, { jobs: await jobs(), statusBar: await statusBarJobs() });
    let run = await finish(runId);
    const evidence = attempt(run, "audit")?.outputs.find((o) => o.port === "evidence")?.value;
    note("run-succeeded-and-audited-the-saved-file", run.state === "succeeded" && evidence?.kind === "formalEvidence" && evidence.fileSha256 === leanSha, {
      state: run.state,
      steps: run.steps.map((s) => [s.step, s.state, s.attempts.at(-1)?.code, s.attempts.at(-1)?.message]),
      verdict: evidence?.verdict,
      declarations: evidence?.declarations?.map((d) => [d.name, d.verdict, d.axioms]),
    });
    const proposal = attempt(run, "associate")?.outputs[0]?.value;
    const afterRun = await formalRows();
    note(
      "association-is-a-pending-proposal-nothing-recorded-yet",
      proposal?.kind === "associationProposal" && proposal.status === "pending" && afterRun.filter((r) => r.evidence === "formalCheck").length === before.filter((r) => r.evidence === "formalCheck").length,
      { proposal, before, afterRun },
    );
    // Accept in the run view (the user's review).
    await waitFor(async () => (await runView.getAttribute("data-state")) === "succeeded", { timeout: 15_000, message: "run view shows the end state" });
    const card = runView.getByTestId("workflow-association-proposal");
    await card.scrollIntoViewIfNeeded();
    await shot("run-association-pending");
    // Nothing is preselected: the user chooses the declarations to associate.
    for (const box of await card.getByTestId("workflow-association-checklist").locator("label").all()) await box.click();
    await card.getByTestId("workflow-association-accept").click();
    await waitFor(async () => (await card.getAttribute("data-status")) === "accepted", { timeout: 30_000, message: "association accepted" });
    const afterAccept = await formalRows();
    const claims = (await invoke(page, "graph_math_overview")).rows.filter((r) => r.entity.kind !== "formalDeclaration").map((r) => ({ kind: r.entity.kind, label: r.entity.label, evidence: r.evidence?.kind ?? null }));
    note("accepted-evidence-lands-on-the-formal-declarations", afterAccept.some((r) => /oddSum_eq_sq/.test(r.label) && r.evidence === "formalCheck"), {
      formalDeclarations: afterAccept,
      otherMathRows: claims,
      outcomes: run.outcomes,
    });
    await shot("run-association-accepted");
    await page.waitForTimeout(1500);
    note("jobs-after-the-run", true, { jobs: await jobs(), statusBar: await statusBarJobs() });

    // ----- change the input, run again ----------------------------------------------------
    await writeFile(join(root, ...LEAN.split("/")), LEAN_TEXT + EXTRA);
    const newSha = sha(LEAN_TEXT + EXTRA);
    await page.waitForTimeout(1500);
    await runView.getByTestId("workflow-run-again").click();
    const review2 = page.getByTestId("workflow-plan-review");
    const reviewed = await review2.waitFor({ timeout: 30_000 }).then(() => true, () => false);
    let notice = null;
    if (reviewed) {
      await waitFor(async () => (await review2.getAttribute("data-state")) === "ready", { timeout: 60_000, message: "new plan ready" });
      notice = await page.getByTestId("plan-review-notice").textContent().catch(() => null);
      await shot("run-again-review");
      await page.getByTestId("workflow-start-run").click();
    }
    let run2Id;
    await waitFor(
      async () => {
        const runs = await invoke(page, "workflow_runs", { workflow: wf.id });
        run2Id = runs.find((r) => r.id !== runId)?.id;
        return !!run2Id;
      },
      { timeout: 30_000, message: "second run started" },
    );
    const run2 = await finish(run2Id);
    const evidence2 = attempt(run2, "audit")?.outputs.find((o) => o.port === "evidence")?.value;
    note(
      "run-again-after-an-input-change-needs-a-new-review-and-audits-the-new-content",
      reviewed && /differs|changed/i.test(notice ?? "") && run2.state === "succeeded" && evidence2?.fileSha256 === newSha && evidence2.declarations.some((d) => /oddSum_zero/.test(d.name)),
      { reviewed, notice, state: run2.state, sha: [evidence2?.fileSha256, newSha], declarations: evidence2?.declarations?.map((d) => [d.name, d.verdict]) },
    );
    const stale = await formalRows();
    note("first-evidence-after-the-file-changed", true, { formalDeclarations: stale });
    await page.waitForTimeout(3000);
    note("jobs-at-the-end", true, { jobs: await jobs(), statusBar: await statusBarJobs() });
    // An idle Python kernel is a long-lived interactive job too: with the Lean
    // server it is what "2 jobs running" counted in the walkthrough's screenshot.
    try {
      const report = await invoke(page, "research_environments", { refresh: false });
      const python = report.runtimes.find((r) => r.language === "python" && r.availability.kind === "available");
      if (!python) throw new Error("no usable Python environment was discovered");
      const session = await invoke(page, "research_session_start", { request: { environmentId: python.id } });
      await waitFor(async () => (await invoke(page, "research_sessions")).some((x) => x.id === session.id && x.state?.kind === "idle"), { timeout: 60_000, interval: 500, message: "kernel idle" });
      await page.waitForTimeout(1500);
      const list = await jobs();
      const open = list.filter((j) => !j.finished);
      note("an-idle-kernel-and-the-lean-server-are-the-jobs-counted-as-running", open.length === 2 && open.every((j) => j.kind === "interactive") && (await statusBarJobs()) === "2 jobs running", {
        open,
        statusBar: await statusBarJobs(),
        sessions: (await invoke(page, "research_sessions")).map((x) => x.state),
      });
      await shot("idle-sessions-status-bar");
      await invoke(page, "research_session_shutdown", { sessionId: session.id });
    } catch (error) {
      note("kernel-jobs-probe-skipped", true, String(error).slice(0, 300));
    }
    const errors = app.consoleMessages.filter((m) => m.startsWith("pageerror:"));
    note("no-page-errors", errors.length === 0, errors.slice(0, 5));
  } finally {
    await writeFile(join(artifactsDir, "observations.json"), JSON.stringify(observations, null, 2));
    await app.close();
  }
  const failed = observations.filter((o) => !o.ok);
  assert.deepEqual(failed.map((d) => d.id), [], `violations (see ${join(artifactsDir, "observations.json")})`);
}
