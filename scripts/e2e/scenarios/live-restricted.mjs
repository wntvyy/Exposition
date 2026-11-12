// LIVE restricted execution through a real agent task (opt-in:
// `npm run e2e -- live-restricted`). Costs money: it uses the API key saved in
// the app's provider settings (OS credential store, default entry). Both tasks
// have small hard budgets. Uses no development-only command, so it runs
// against release and installed builds (EXPOSITION_E2E_EXECUTABLE), where the
// scripted provider does not exist.
//
// Checks, in the app under test:
//   A. An unattended ("Restricted sandbox only") task's run_python runs in the
//      Docker sandbox: Linux, uid 65534, no network, read-only project, no
//      credential variables, and the job is recorded under the restricted policy.
//   B. With no image configured the same request is refused by the application
//      and never runs natively: no job, no kernel, nothing written.
//   The API key never appears in transcripts, job output, logs, or app data.
import { existsSync } from "node:fs";
import { mkdtemp, readFile, readdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { invoke, openWorkspace, waitFor } from "../workspace-helpers.mjs";

const IMAGE = "python:3.13-slim";
const KEY_PATTERN = /sk-ant-[A-Za-z0-9_-]{8,}/;
const BUDGETS = { maxTurns: 4, maxOutputTokens: 8_000, maxInputTokens: 60_000, maxWallMs: 300_000, maxToolCalls: 3, maxCommands: 2 };

const CODE = [
  "import os, platform, socket",
  'print("platform", platform.system())',
  'print("uid", os.getuid() if hasattr(os, "getuid") else "none")',
  "try:",
  '    socket.create_connection(("1.1.1.1", 53), timeout=3)',
  '    print("network open")',
  "except OSError as e:",
  '    print("network blocked", type(e).__name__)',
  "try:",
  '    open("restricted-ran.txt", "w").write("x")',
  '    print("workspace writable")',
  "except OSError as e:",
  '    print("workspace read-only", type(e).__name__)',
  'print("credential variables:", sorted(k for k in os.environ if k.upper() in ("ANTHROPIC_API_KEY", "GITHUB_TOKEN")))',
].join("\n");

const PROMPT = [
  "Call the run_python tool exactly once with exactly the following code, unchanged, and then reply with the tool's result.",
  "Do not call any other tool and do not retry if it fails; just report what the tool returned.",
  "",
  "```python",
  CODE,
  "```",
].join("\n");

const terminal = (status) => !["queued", "running", "awaitingApproval"].includes(status.kind);
const toolCalls = (detail, name) => detail.transcript.filter((t) => t.kind === "toolCall" && (!name || t.name === name));
const parse = (text) => {
  try {
    return JSON.parse(text);
  } catch {
    return null;
  }
};

async function runTask(page, request, evidence) {
  const created = await invoke(page, "agents_task_create", { request });
  let detail;
  await waitFor(async () => {
    detail = await invoke(page, "agents_task_detail", { task: created.id });
    return terminal(detail.summary.status);
  }, { timeout: 300_000, interval: 500, message: `task ${created.id} finishes` });
  evidence.push({ id: created.id, status: detail.summary.status, usage: detail.summary.usage, approvals: detail.approvals.length });
  return detail;
}

export default async function liveRestricted({ launchApp, artifactsDir, assert }) {
  const root = await mkdtemp(join(tmpdir(), "exposition-live-restricted-"));
  await writeFile(join(root, "main.typ"), "= Sandbox probe\n");
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
    evidence.build = status.scriptedAvailable ? "debug" : "release";
    check("provider configured from the OS credential store", status.configured === true, { store: status.credentialStore, model: status.settings.model });

    await openWorkspace(page, root);
    await invoke(page, "trust_grant", { root });
    const request = { workspaceRoot: root, consent: "scopeContent", prompt: PROMPT, scope: { kind: "workspace" }, permission: "autoApply", execution: "restricted", budgets: BUDGETS };

    // --- A. unattended run_python in the Docker sandbox ------------------------
    const set = await invoke(page, "execution_settings_update", { change: { kind: "dockerImage", image: IMAGE } });
    check("Docker image configured", set.settings.docker.image === IMAGE, set.settings.docker);
    const jobsBefore = (await invoke(page, "job_list")).length;
    const a = await runTask(page, request, evidence.tasks);
    await page.screenshot({ path: join(artifactsDir, "a-restricted-run.png") });
    check("A finished without approvals (unattended)", a.summary.status.kind === "completed" && a.approvals.length === 0, { status: a.summary.status, approvals: a.approvals.length });
    check("A served by the live provider", !!a.summary.usage.servedBy && a.summary.usage.outputTokens > 0, a.summary.usage);
    const runs = toolCalls(a, "run_python");
    check("A called run_python", runs.length >= 1, toolCalls(a).map((t) => t.name));
    const boxed = parse(runs[0].result ?? "");
    evidence.restrictedResult = boxed ? { policy: boxed.policy, stdout: boxed.stdout } : (runs[0].result ?? "").slice(0, 400);
    check("run_python succeeded under restricted:docker", !runs[0].isError && boxed?.policy === "restricted:docker", evidence.restrictedResult);
    const out = boxed?.stdout ?? "";
    check("sandbox is Linux as uid 65534", /platform Linux/.test(out) && /uid 65534/.test(out), out);
    check("sandbox has no network", /network blocked/.test(out) && !/network open/.test(out), out);
    check("project is read-only in the sandbox", /workspace read-only/.test(out) && !existsSync(join(root, "restricted-ran.txt")), out);
    check("no credential variables in the sandbox", /credential variables: \[\]/.test(out), out);
    const jobs = await invoke(page, "job_list");
    const job = jobs.find((j) => j.info.id === boxed?.job);
    check("the job is recorded under the restricted Docker policy", job?.info.policy?.kind === "restricted" && job?.info.policy?.backend === "docker", job?.info.policy ?? null);
    evidence.jobs = { before: jobsBefore, after: jobs.length, program: job?.info.program, label: job?.info.label };

    // --- B. no image: refused, never native ------------------------------------
    const cleared = await invoke(page, "execution_settings_update", { change: { kind: "dockerImage", image: null } });
    check("Docker image cleared", cleared.settings.docker.image === null, cleared.settings.docker);
    const jobsMid = (await invoke(page, "job_list")).length;
    const sessionsMid = (await invoke(page, "research_sessions")).length;
    const b = await runTask(page, request, evidence.tasks);
    await page.screenshot({ path: join(artifactsDir, "b-restricted-refused.png") });
    const refused = toolCalls(b, "run_python");
    evidence.refusal = refused.map((t) => ({ isError: t.isError, result: (t.result ?? "").slice(0, 300) }));
    check("B's run_python calls were all refused by the application", refused.length >= 1 && refused.every((t) => t.isError && /never falls back/i.test(t.result ?? "")), evidence.refusal);
    check("B launched no job and no kernel", (await invoke(page, "job_list")).length === jobsMid && (await invoke(page, "research_sessions")).length === sessionsMid, "");
    check("B wrote nothing into the project", !existsSync(join(root, "restricted-ran.txt")), "");

    // --- credentials ------------------------------------------------------------
    const transcripts = [];
    for (const t of evidence.tasks) transcripts.push(JSON.stringify(await invoke(page, "agents_task_detail", { task: t.id })));
    const outputs = [];
    for (const j of await invoke(page, "job_list")) outputs.push(JSON.stringify(await invoke(page, "job_output", { job: j.id, after: null, maxBytes: null }).catch(() => "")));
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
    check("no API key in job output", !outputs.some((t) => KEY_PATTERN.test(t)), "");
    check("no API key in app logs", !KEY_PATTERN.test(app.logs.join("")), "");
    check("no API key in the app data directory", dataFiles.length === 0, dataFiles);
  } finally {
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2)).catch(() => {});
    await app.close();
  }
}
