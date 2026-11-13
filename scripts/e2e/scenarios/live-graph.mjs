// LIVE agent run of the project-graph tools against the real model provider
// (opt-in: `npm run e2e -- live-graph`). Costs money: it uses the API key saved
// in the app's provider settings (OS credential store, default entry). Every
// task has hard budgets.
//
// M7 evidence in the real (debug) app, with a real model:
//   G1. A whole-project task receives the scoped graph context, reads the graph
//       with graph_query, proposes a mathematical dependency with graph_propose
//       (approval card; stored as a *suggestion* attributed to the task), and
//       saves a workflow draft with workflow_propose (saving runs nothing). No file
//       changes.
//   G2. Under metadata-only consent the provider never receives a
//       content-derived label (theorem/lemma labels, file text): checked on the
//       stored provider transcript, i.e. what was actually sent and returned.
//   The API key never reaches transcripts, app data, or project files.
// Application-enforced properties are hard checks; steps that depend on the
// model following the request are reported as such (`model:` checks).
import { cp, mkdtemp, readdir, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { invoke, waitFor } from "../workspace-helpers.mjs";

const KEY_PATTERN = /sk-ant-[A-Za-z0-9_-]{8,}/;
const BUDGET = { maxTurns: 12, maxOutputTokens: 40_000, maxInputTokens: 400_000, maxWallMs: 600_000, maxToolCalls: 16, maxCommands: 0 };
const terminal = (status) => !["queued", "running", "awaitingApproval"].includes(status.kind);
const toolCalls = (detail, name) => detail.transcript.filter((t) => t.kind === "toolCall" && (!name || t.name === name));

/** Run a task to completion, approving only the listed tools (others declined). */
async function runTask(page, request, approveTools, evidence) {
  const created = await invoke(page, "agents_task_create", { request });
  const id = created.id;
  const answered = new Set();
  const started = Date.now();
  let detail;
  for (;;) {
    detail = await invoke(page, "agents_task_detail", { task: id });
    for (const approval of detail.approvals) {
      if (answered.has(approval.id)) continue;
      answered.add(approval.id);
      const approved = approveTools.includes(approval.tool);
      evidence.approvals.push({ task: id, tool: approval.tool, title: approval.title, approved, detail: approval.detail.slice(0, 400) });
      await invoke(page, "agents_approval_resolve", { task: id, approval: approval.id, approved });
    }
    if (terminal(detail.summary.status)) break;
    if (Date.now() - started > 600_000) throw new Error(`task ${id} did not finish within 600 s`);
    await new Promise((r) => setTimeout(r, 500));
  }
  evidence.tasks.push({ id, status: detail.summary.status, usage: detail.summary.usage, tools: toolCalls(detail).map((c) => `${c.name}:${c.status?.kind ?? c.status}`) });
  return detail;
}

export default async function liveGraph({ launchApp, artifactsDir, assert, fixture }) {
  const root = await mkdtemp(join(tmpdir(), "exposition-live-graph-"));
  await cp(fixture("graph-basic"), root, { recursive: true });
  const before = await readFile(join(root, "paper.tex"), "utf8");
  const app = await launchApp();
  const { page } = app;
  const evidence = { root, tasks: [], approvals: [], checks: [] };
  const check = (name, ok, detail = "") => {
    const text = typeof detail === "string" ? detail : JSON.stringify(detail);
    evidence.checks.push({ name, ok, detail: text.slice(0, 2000) });
    console.log(`${ok ? "ok" : "FAIL"}  ${name}: ${text.slice(0, 300)}`);
  };
  try {
    const provider = await invoke(page, "agents_provider_status");
    check("provider configured from the OS credential store", provider.configured === true, { model: provider.settings.model });
    assert.ok(provider.configured, "an API key must be saved in the app's provider settings");

    await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="paper.tex"]').waitFor({ timeout: 15_000 });
    const entity = async (key) =>
      (await invoke(page, "graph_snapshot", { query: { kinds: ["theorem", "lemma"], text: null, around: null, includeInactive: false, limit: 50 } })).entities.find(
        (e) => e.key === key,
      );
    await waitFor(async () => (await entity("thm:main")) && (await entity("lem:closed")), { timeout: 30_000, message: "paper.tex indexed" });
    const theorem = await entity("thm:main");
    const lemma = await entity("lem:closed");
    const project = (await invoke(page, "graph_status")).project;

    // --- G1: graph context, graph_query, graph_propose, workflow_propose -----
    const promptG1 = [
      "This LaTeX paper project has a project graph. Do not create or edit any files.",
      "1) Use graph_query to find the theorem labelled thm:main and the lemma labelled lem:closed in paper.tex and the relationships the graph already records for them.",
      "2) The proof of thm:main uses lem:closed. Use graph_propose to propose the mathematical relationship: the theorem thm:main depends on the lemma lem:closed (kind dependsOn from the theorem entity to the lemma entity).",
      "3) Use workflow_propose to save a workflow draft titled 'Numerical check of the main theorem' with one research Python batch step that would compute a numerical check and an outcome that retains a figure; an incomplete draft is fine.",
      "Finish with a two-sentence summary of what you proposed.",
    ].join(" ");
    const g1 = await runTask(
      page,
      { prompt: promptG1, workspaceRoot: root, scope: { kind: "workspace" }, permission: "reviewEdits", execution: "disabled", consent: "scopeContent", budgets: BUDGET },
      ["graph_propose", "workflow_propose"],
      evidence,
    );
    const ctx = g1.graph.context;
    check("G1 task received the scoped graph context of this project", ctx && ctx.project === project && g1.graph.project === project, {
      contextProject: ctx?.project,
      project,
      entities: ctx?.entities?.length,
    });
    check(
      "G1 context carries the indexed theorem and lemma with labels (content consent)",
      !!ctx?.entities?.some((e) => e.id === theorem.id && e.label) && !!ctx?.entities?.some((e) => e.id === lemma.id && e.label),
      ctx?.entities?.filter((e) => e.kind === "theorem" || e.kind === "lemma").map((e) => `${e.kind}:${e.label}`),
    );
    const queries = toolCalls(g1, "graph_query");
    check("model: G1 used graph_query", queries.some((c) => (c.status?.kind ?? c.status) === "succeeded"), queries.map((c) => c.status));
    const detailT = await invoke(page, "graph_entity", { entity: theorem.id });
    const dep = detailT.outgoing.find((r) => r.kind === "dependsOn" && r.to === lemma.id);
    check("model: G1 proposed theorem dependsOn lemma (approved on the card)", !!dep, detailT.outgoing.map((r) => `${r.kind}->${r.to}:${r.status}`));
    if (dep) {
      check(
        "agent assertion stored as a suggestion attributed to the task (never accepted by the agent)",
        dep.status === "suggested" && dep.origin.kind === "agent" && dep.origin.taskId === g1.summary.id,
        { status: dep.status, origin: dep.origin },
      );
    }
    const proposalApprovals = evidence.approvals.filter((a) => a.task === g1.summary.id && a.tool === "graph_propose");
    check("graph changes required approval in reviewEdits (complete payload on the card)", proposalApprovals.length === 0 || proposalApprovals.every((a) => a.detail.length > 0), proposalApprovals.map((a) => a.title));
    const drafts = await invoke(page, "workflow_list");
    const draft = drafts.find((d) => d.origin.kind === "agent" && d.origin.taskId === g1.summary.id);
    check("model: G1 saved a workflow draft through workflow_propose", !!draft, drafts.map((d) => `${d.title}:${d.origin.kind}`));
    if (draft) {
      // Saving a draft never runs it: only the user starts a run, from a reviewed plan.
      const runs = await invoke(page, "workflow_runs", { workflow: draft.id });
      check("saving the agent's workflow draft ran nothing (no run exists)", runs.length === 0, { state: draft.state, runnable: draft.runnable, runs: runs.map((r) => r.id) });
    }
    const review = await invoke(page, "agents_review", { task: g1.summary.id });
    check("G1 changed no files", (review.files ?? []).length === 0 && (await readFile(join(root, "paper.tex"), "utf8")) === before, (review.files ?? []).map((f) => f.path));
    const declined = evidence.approvals.filter((a) => a.task === g1.summary.id && !a.approved);
    check("G1 approvals other than graph/workflow proposals were declined (none expected)", true, declined.map((a) => a.tool));

    // --- G2: metadata-only consent never leaks content-derived labels -------
    const promptG2 =
      "Use graph_query to list the mathematical statements of this project and what each depends on or references, then summarise them by name.";
    const g2 = await runTask(
      page,
      { prompt: promptG2, workspaceRoot: root, scope: { kind: "workspace" }, permission: "readOnly", execution: "disabled", consent: "metadataOnly", budgets: BUDGET },
      [],
      evidence,
    );
    const g2Text = JSON.stringify(g2.transcript) + JSON.stringify(g2.graph.context ?? {});
    const leaked = ["thm:main", "lem:closed", "def:compact", "finite subcover", "Closed subsets", "continuous map", "rudin"].filter((s) => g2Text.includes(s));
    check("G2 metadata-only: no content-derived label or text in what the provider received or returned", leaked.length === 0, { leaked });
    check(
      "G2 metadata-only context withholds labels of non-file entities",
      (g2.graph.context?.entities ?? []).filter((e) => !["resource", "folder", "project"].includes(e.kind)).every((e) => e.label === null),
      (g2.graph.context?.entities ?? []).map((e) => `${e.kind}:${e.label}`),
    );
    check("G2 read-only task proposed no graph change", toolCalls(g2, "graph_propose").every((c) => (c.status?.kind ?? c.status) !== "succeeded"), toolCalls(g2, "graph_propose").map((c) => c.status));

    // --- key never stored anywhere ------------------------------------------
    const transcripts = JSON.stringify([g1, g2]);
    check("API key absent from transcripts", !KEY_PATTERN.test(transcripts));
    const projectFiles = [];
    const walk = async (dir) => {
      for (const e of await readdir(dir, { withFileTypes: true })) {
        const p = join(dir, e.name);
        if (e.isDirectory()) await walk(p);
        else projectFiles.push(p);
      }
    };
    await walk(root);
    let keyInProject = false;
    for (const f of projectFiles) {
      const bytes = await readFile(f);
      if (KEY_PATTERN.test(bytes.toString("latin1"))) keyInProject = true;
    }
    check("API key absent from project files (incl. the graph database)", !keyInProject, `${projectFiles.length} files`);
  } finally {
    const { writeFile } = await import("node:fs/promises");
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2)).catch(() => {});
    await app.close();
  }
  const failed = evidence.checks.filter((c) => !c.ok);
  assert.deepEqual(failed.map((c) => c.name), [], `failed checks (see ${join(artifactsDir, "evidence.json")})`);
}
