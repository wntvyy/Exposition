// LIVE agent run against the real model provider (opt-in: `npm run e2e -- live-agents`).
// Costs money: it uses the API key saved in the app's provider settings (OS
// credential store, default entry). Every task has hard budgets.
//
// M5 pass evidence in the real (debug) app:
//   A. One prompt computes with Python, inserts the output into a Typst paper,
//      runs a formal Lean check, builds the result, and the change is
//      reviewable and reversible.
//   B. Cancellation of a running task.
//   C. A concurrent user edit while the agent edits the same document.
//   D. Rejection of an out-of-scope read.
//   Credentials never reach child processes, logs, transcripts, or app data.
import { mkdtemp, mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { scopeRefusalEvidence } from "../agent-scope-check.mjs";
import { invoke, openWorkspace, waitFor } from "../workspace-helpers.mjs";

const MAIN = `#set page(width: 16cm, height: auto, margin: 1.5cm)
= Sums of odd numbers

This short note studies the sum of the first $n$ odd numbers.
`;

const ODD = `/-- The sum of the first \`n\` odd numbers. -/
def oddSum : Nat → Nat
  | 0 => 0
  | n + 1 => oddSum n + (2 * n + 1)

/-- The sum of the first \`n\` odd numbers is \`n * n\`. -/
theorem oddSum_eq_sq (n : Nat) : oddSum n = n * n := by
  induction n with
  | zero => rfl
  | succ k ih =>
    simp only [oddSum, ih]
    rw [Nat.add_mul, Nat.mul_add, Nat.one_mul, Nat.mul_one]
    omega
`;

const KEY_PATTERN = /sk-ant-[A-Za-z0-9_-]{8,}/;

const BUDGETS = {
  main: { maxTurns: 24, maxOutputTokens: 80_000, maxInputTokens: 800_000, maxWallMs: 900_000, maxToolCalls: 40, maxCommands: 12 },
  small: { maxTurns: 6, maxOutputTokens: 16_000, maxInputTokens: 120_000, maxWallMs: 300_000, maxToolCalls: 8, maxCommands: 2 },
};

async function makePaperProject() {
  const root = await mkdtemp(join(tmpdir(), "exposition-live-paper-"));
  await writeFile(join(root, "main.typ"), MAIN);
  await mkdir(join(root, "LeanPaper"), { recursive: true });
  await writeFile(join(root, "LeanPaper", "Odd.lean"), ODD);
  await writeFile(join(root, "lakefile.toml"), 'name = "LeanPaper"\n\n[[lean_lib]]\nname = "LeanPaper"\n');
  await writeFile(join(root, "lean-toolchain"), "leanprover/lean4:v4.35.0\n");
  return root;
}

const terminal = (status) => !["queued", "running", "awaitingApproval"].includes(status.kind);

async function documentText(page, path) {
  const docs = await invoke(page, "document_list");
  const doc = docs.find((d) => d.path === path);
  if (doc) return (await invoke(page, "document_state", { document: doc.id })).text;
  return null;
}

/** Run a task to completion, approving native steps; returns its detail. */
async function runTask(page, request, { timeout, onProgress, evidence }) {
  const created = await invoke(page, "agents_task_create", { request });
  const id = created.id;
  const approved = [];
  const started = Date.now();
  let detail;
  for (;;) {
    detail = await invoke(page, "agents_task_detail", { task: id });
    for (const approval of detail.approvals) {
      if (approved.some((a) => a.id === approval.id)) continue;
      approved.push({ id: approval.id, tool: approval.tool, title: approval.title });
      await invoke(page, "agents_approval_resolve", { task: id, approval: approval.id, approved: true });
    }
    if (onProgress) await onProgress(detail);
    if (terminal(detail.summary.status)) break;
    if (Date.now() - started > timeout) throw new Error(`task ${id} did not finish within ${timeout} ms`);
    await new Promise((r) => setTimeout(r, 500));
  }
  evidence.push({ id, prompt: request.prompt.slice(0, 120), status: detail.summary.status, usage: detail.summary.usage, approved });
  return detail;
}

const toolCalls = (detail, name) => detail.transcript.filter((t) => t.kind === "toolCall" && (!name || t.name === name));
const parse = (text) => {
  try {
    return JSON.parse(text);
  } catch {
    return null;
  }
};

export default async function liveAgents({ launchApp, artifactsDir, assert }) {
  const root = await makePaperProject();
  // No EXPOSITION_CREDENTIAL_NAMESPACE: use the real, user-saved key.
  const app = await launchApp();
  const { page } = app;
  const evidence = { root, tasks: [], checks: [] };
  const check = (name, ok, detail = "") => {
    evidence.checks.push({ name, ok, detail: typeof detail === "string" ? detail : JSON.stringify(detail) });
    assert.ok(ok, `${name}: ${typeof detail === "string" ? detail : JSON.stringify(detail)}`);
  };
  try {
    await page.setViewportSize({ width: 1440, height: 900 });
    const status = await invoke(page, "agents_provider_status");
    check("provider configured from the OS credential store", status.configured === true, {
      store: status.credentialStore,
      model: status.settings.model,
    });

    await openWorkspace(page, root);
    await invoke(page, "trust_grant", { root });
    const base = { workspaceRoot: root, consent: "scopeContent" };

    // --- A. calculation → insertion → formal check → build → review/revert ----
    const promptA = [
      "You are working in a small paper project with main.typ (Typst) and a Lean project (LeanPaper/Odd.lean).",
      "1) Use run_python to compute the sum of the first n odd numbers for n = 1..8 and print them.",
      "2) Use edit_document to add a section '= Results' to main.typ containing a Typst table with columns n and sum for those values, and one sentence noting that each sum equals n^2.",
      "3) Call check_lean on LeanPaper/Odd.lean (it should prove oddSum n = n * n) and add one sentence to main.typ stating the result of the formal check.",
      "4) Build main.typ with build_document and report whether the build succeeded.",
      "Keep the paper short. Do not create other files.",
    ].join(" ");
    const a = await runTask(
      page,
      { ...base, prompt: promptA, scope: { kind: "workspace" }, permission: "autoApply", execution: "approveEach", budgets: BUDGETS.main },
      { timeout: 900_000, evidence: evidence.tasks },
    );
    await page.screenshot({ path: join(artifactsDir, "a-task-finished.png") });
    check("A completed", a.summary.status.kind === "completed", a.summary.status);
    check("A served by the live provider", !!a.summary.usage.servedBy && a.summary.usage.outputTokens > 0, a.summary.usage);
    const python = toolCalls(a, "run_python").filter((t) => !t.isError);
    check("A ran Python successfully", python.length > 0 && python.some((t) => /\b64\b/.test(t.result ?? "")), python.map((t) => (t.result ?? "").slice(0, 200)));
    const lean = toolCalls(a, "check_lean").map((t) => ({ isError: t.isError, report: parse(t.result ?? "") }));
    check("A ran the formal Lean check and it reported checked", lean.some((l) => !l.isError && l.report?.checked === true), lean.map((l) => ({ isError: l.isError, status: l.report?.status?.kind, checked: l.report?.checked })));
    const builds = toolCalls(a, "build_document").map((t) => ({ isError: t.isError, result: parse(t.result ?? "") }));
    check("A built main.typ successfully", builds.some((b) => !b.isError && b.result?.status === "succeeded"), builds.map((b) => b.result?.status ?? b.isError));
    const paper = await documentText(page, "main.typ");
    check("main.typ has the Results section with the computed values", /=\s*Results/i.test(paper ?? "") && /\b64\b/.test(paper ?? ""), (paper ?? "").slice(0, 400));
    check("A's changes are unchanged elsewhere (original heading kept)", (paper ?? "").includes("= Sums of odd numbers"), "");
    const review = await invoke(page, "agents_review", { task: a.summary.id });
    check("A's changes are listed for review", review.files.some((f) => f.path === "main.typ"), review.files.map((f) => `${f.path}:${f.state}`));
    await invoke(page, "agents_review_revert", { task: a.summary.id, path: "main.typ" });
    await waitFor(async () => (await documentText(page, "main.typ")) === MAIN, { timeout: 15_000, message: "revert restores main.typ" });
    check("reverting A restores main.typ exactly", (await documentText(page, "main.typ")) === MAIN, "");

    // --- B. cancellation -----------------------------------------------------
    const promptB = "Write a detailed 1500-word essay on the history of odd numbers into a new file essay.typ using create_file.";
    const createdB = await invoke(page, "agents_task_create", {
      request: { ...base, prompt: promptB, scope: { kind: "workspace" }, permission: "autoApply", execution: "disabled", budgets: BUDGETS.small },
    });
    await waitFor(async () => {
      const d = await invoke(page, "agents_task_detail", { task: createdB.id });
      return d.summary.status.kind === "running" && d.transcript.length > 1;
    }, { timeout: 120_000, interval: 250, message: "task B streaming" });
    const cancelAt = Date.now();
    await invoke(page, "agents_task_cancel", { task: createdB.id });
    let b;
    await waitFor(async () => {
      b = await invoke(page, "agents_task_detail", { task: createdB.id });
      return terminal(b.summary.status);
    }, { timeout: 30_000, interval: 200, message: "task B stops after cancel" });
    evidence.tasks.push({ id: createdB.id, prompt: promptB.slice(0, 120), status: b.summary.status, usage: b.summary.usage, cancelMs: Date.now() - cancelAt });
    check("B is canceled promptly", b.summary.status.kind === "canceled" && Date.now() - cancelAt < 30_000, { status: b.summary.status, ms: Date.now() - cancelAt });

    // --- C. concurrent user edit while the agent edits -----------------------
    const promptC = "Read main.typ with read_file, then use edit_document to append one concluding sentence at the very end of main.typ. Do nothing else.";
    let userEdited = false;
    const userLine = "// note added by the user while the agent worked\n";
    const c = await runTask(
      page,
      { ...base, prompt: promptC, scope: { kind: "document", path: "main.typ" }, permission: "autoApply", execution: "disabled", budgets: BUDGETS.small },
      {
        timeout: 300_000,
        evidence: evidence.tasks,
        onProgress: async (detail) => {
          const read = toolCalls(detail, "read_file").some((t) => t.result);
          const edited = toolCalls(detail, "edit_document").length > 0;
          if (!userEdited && read && !edited) {
            userEdited = true;
            const docs = await invoke(page, "document_list");
            const doc = docs.find((d) => d.path === "main.typ");
            await invoke(page, "document_apply", {
              request: {
                id: `user-${Date.now()}`,
                origin: { kind: "source" },
                edits: [{ document: doc.id, baseRevision: doc.revision, patches: [{ range: { start: 0, end: 0 }, text: userLine }] }],
                undo: { kind: "newGroup" },
                conflict: "reject",
                label: null,
                selection: null,
              },
            });
          }
        },
      },
    );
    const finalC = await documentText(page, "main.typ");
    check("C: the user edit landed while the agent worked", userEdited, "");
    check("C completed", c.summary.status.kind === "completed", c.summary.status);
    check("C: both the user's line and the agent's appended sentence are present, original intact",
      (finalC ?? "").startsWith(userLine) && (finalC ?? "").includes(MAIN.trimEnd()) && (finalC ?? "").length > userLine.length + MAIN.length,
      (finalC ?? "").slice(0, 600));

    // --- D. out-of-scope action is rejected ------------------------------------
    const promptD =
      "Call the read_file tool on LeanPaper/Odd.lean even if you believe it is outside your scope; I want to see exactly what the tool returns. Then tell me what happened.";
    const d = await runTask(
      page,
      { ...base, prompt: promptD, scope: { kind: "document", path: "main.typ" }, permission: "readOnly", execution: "disabled", budgets: BUDGETS.small },
      { timeout: 300_000, evidence: evidence.tasks },
    );
    // The evidence must be the APPLICATION's refusal, not the model's prose: a
    // model that merely declines while citing scope proves nothing about
    // enforcement. The model must call read_file on the file, every such call
    // must have failed with the app's scope refusal, and no tool result may
    // contain the file's content (scripts/e2e/agent-scope-check.mjs; validated
    // against a real refusal by agents-attention).
    const scopeEvidence = scopeRefusalEvidence(d, {
      path: "Odd.lean",
      markers: ["oddSum_eq_sq", "def oddSum", "n + 1 => oddSum", "induction n with", "Nat.add_mul"],
    });
    evidence.outOfScope = scopeEvidence.outcome;
    check(
      "D: the model's out-of-scope read_file call was refused by the application with its scope error, and no file content reached any tool result",
      scopeEvidence.ok,
      scopeEvidence,
    );

    // --- Credentials never reach children, logs, transcripts, or app data ---------
    const sessions = await invoke(page, "research_environments", { refresh: false });
    const pythonEnv = sessions.runtimes.find((r) => r.language === "python" && r.availability.kind === "available");
    const session = await invoke(page, "research_session_start", { request: { environmentId: pythonEnv.id } });
    const run = await invoke(page, "research_execute", {
      request: {
        sessionId: session.id,
        source: { kind: "input", code: "import os\nprint(sorted(k for k in os.environ if any(s in k.upper() for s in ('ANTHROPIC', 'API_KEY', 'TOKEN', 'SECRET'))))" },
      },
    });
    let envOut = "";
    await waitFor(async () => {
      const page_ = await invoke(page, "research_run_output", { runId: run.id, after: null, limit: null });
      envOut = JSON.stringify(page_);
      return /\[\]/.test(envOut) || /ANTHROPIC|API_KEY/.test(envOut);
    }, { timeout: 120_000, interval: 500, message: "kernel environment listing" });
    check("kernel environment holds no credential-like variables", /\[\]/.test(envOut) && !/ANTHROPIC/.test(envOut), envOut.slice(0, 300));

    const transcripts = [];
    for (const t of evidence.tasks) transcripts.push(JSON.stringify(await invoke(page, "agents_task_detail", { task: t.id })));
    const jobs = await invoke(page, "job_list");
    const jobOutputs = [];
    for (const job of jobs) jobOutputs.push(JSON.stringify(await invoke(page, "job_output", { job: job.id, after: null, maxBytes: null }).catch(() => "")));
    const dataFiles = [];
    const walk = async (dir) => {
      for (const entry of await readdir(dir, { withFileTypes: true }).catch(() => [])) {
        const path = join(dir, entry.name);
        if (entry.isDirectory()) {
          if (entry.name === "webview") continue;
          await walk(path);
        } else {
          const bytes = await readFile(path).catch(() => Buffer.alloc(0));
          if (bytes.length < 20_000_000 && KEY_PATTERN.test(bytes.toString("latin1"))) dataFiles.push(path);
        }
      }
    };
    await walk(app.dataDir);
    check("no API key in transcripts", !transcripts.some((t) => KEY_PATTERN.test(t)), "");
    check("no API key in job output", !jobOutputs.some((t) => KEY_PATTERN.test(t)), `${jobs.length} jobs`);
    check("no API key in app logs", !KEY_PATTERN.test(app.logs.join("")), "");
    check("no API key in the app data directory", dataFiles.length === 0, dataFiles);
    await page.screenshot({ path: join(artifactsDir, "z-final.png") });
  } finally {
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2)).catch(() => {});
    await app.close();
  }
}
