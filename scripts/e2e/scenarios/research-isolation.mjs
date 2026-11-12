// Regression for the R-m4 research trust/isolation findings (#1, #4, #5, #7),
// in the real app through the webview IPC (as a compromised or buggy webview
// could issue it) and through the UI:
//
// - a session belongs to one workspace: after switching to an untrusted
//   project, Run File on that project's script does not run in the previous
//   project's kernel; the old session is detached and refuses code, also
//   after switching back; trust revocation and workspace close stop sessions;
// - research_environments cannot be pointed at an executable (a planted
//   `.venv` interpreter in an untrusted project never runs);
// - a webview cannot create agent-owned sessions or run as an agent;
// - Python, R, and Julia kernels run in the workspace root (plain path).
//
// Each check is recorded in test-results/e2e/research-isolation/observations.json
// and the scenario fails if any is violated. Set EXPOSITION_E2E_EXECUTABLE to run it
// against another build (e.g. the pre-fix binary, where these checks fail).
import { cp, mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { invoke, invokeError, openInEditor, waitFor } from "../workspace-helpers.mjs";
import { runCommand } from "../palette.mjs";

const KERNEL_TIMEOUT = 120_000;

/** Start a session; `owner` and `requester` fields are sent as a hostile webview would. */
const startSession = (page, environmentId, owner = { kind: "user" }) =>
  invoke(page, "research_session_start", { request: { environmentId, owner } });

const execute = (page, sessionId, code, requester = { kind: "user" }) =>
  invokeError(page, "research_execute", { request: { sessionId, source: { kind: "input", code }, requester } });

async function settle(page, runId) {
  return waitFor(
    async () => {
      const m = await invoke(page, "research_run", { runId });
      return m.record.status.kind !== "running" ? m : null;
    },
    { timeout: KERNEL_TIMEOUT, message: `run ${runId}` },
  );
}

async function streamText(page, runId) {
  const out = await invoke(page, "research_run_output", { runId, after: null, limit: 100 });
  return out.entries.filter((e) => e.item.kind === "stream").map((e) => e.item.text).join("").trim();
}

export default async function researchIsolation({ launchApp, artifactsDir, fixture, assert }) {
  const base = await mkdtemp(join(tmpdir(), "xp-research-iso-"));
  const rootA = join(base, "projectA");
  const rootB = join(base, "projectB");
  const dataDir = join(base, "data");
  await cp(fixture("research-python"), rootA, { recursive: true });
  await cp(fixture("research-python"), rootB, { recursive: true });
  await mkdir(dataDir, { recursive: true });
  // Untrusted project B: a script that would leave a marker in A if it ran in A's kernel.
  const markerB = join(rootA, "ran-code-from-untrusted-B.txt");
  await writeFile(
    join(rootB, "analysis", "plot.py"),
    `open(r"${markerB}", "w").write("project B code ran in project A's kernel")\nprint("B code executed")\n`,
  );
  // A planted interpreter inside untrusted B.
  const planted = join(rootB, ".venv", "Scripts", "python.exe");
  await mkdir(join(rootB, ".venv", "Scripts"), { recursive: true });
  await writeFile(planted, "not a real interpreter");

  const observations = [];
  const note = (id, ok, detail) => {
    observations.push({ id, ok, detail });
    console.log(`${ok ? "ok    " : "FAIL  "} ${id}: ${typeof detail === "string" ? detail : JSON.stringify(detail)}`);
  };
  const norm = (p) => p.replace(/\\/g, "/").replace(/^\/\/\?\//, "").replace(/\/$/, "").toLowerCase();

  const app = await launchApp({ dataDir });
  try {
    const { page } = app;

    // ---- Untrusted B: no kernels, no planted interpreter ------------------
    await invoke(page, "workspace_open", { path: rootB });
    await page.locator('[data-testid="file-tree"]').waitFor({ timeout: 15_000 });
    const refused = await invokeError(page, "research_session_start", {
      request: { environmentId: "python:any", owner: { kind: "user" } },
    });
    note("untrusted-session-refused", refused.error?.code === "research.notTrusted", refused.error ?? "accepted");
    await invokeError(page, "research_environments", { refresh: true, extraInterpreters: [planted] });
    const plantedJobs = (await invoke(page, "job_list")).filter((j) => norm(j.info.program) === norm(planted));
    note(
      "planted-interpreter-never-runs",
      plantedJobs.length === 0,
      plantedJobs.length ? `ran ${planted} as ${plantedJobs.map((j) => j.kind).join(", ")} job(s)` : "not launched",
    );

    // ---- Trusted A: webview cannot be an agent; kernel cwd ------------------
    await invoke(page, "workspace_open", { path: rootA });
    await page.locator('[data-testid="file-tree"] [data-path="paper.typ"]').waitFor({ timeout: 15_000 });
    const status = await invoke(page, "research_workspace");
    await invoke(page, "trust_grant", { root: status.root });
    const report = await invoke(page, "research_environments", { refresh: true });
    const python = report.runtimes.find((r) => r.language === "python" && r.availability.kind === "available");
    assert.ok(python, "a usable Python environment");

    const forgedSession = await startSession(page, python.id, { kind: "agent", taskId: "forged-task" });
    note("webview-session-is-user-owned", forgedSession.owner.kind === "user", forgedSession.owner);
    const forgedRun = await execute(page, forgedSession.id, 'print("hi")', { kind: "agent", taskId: "forged-task" });
    if (forgedRun.unexpectedSuccess) {
      const m = await settle(page, forgedRun.unexpectedSuccess.id);
      note("webview-run-is-recorded-as-user", m.session.owner.kind === "user", m.session.owner);
    } else {
      note("webview-run-is-recorded-as-user", true, `refused: ${forgedRun.error?.code}`);
    }
    await invoke(page, "research_session_shutdown", { sessionId: forgedSession.id });

    const session = await startSession(page, python.id);
    for (const [language, code] of [
      ["python", "import os\nprint(os.getcwd())"],
      ["r", 'cat(getwd(), "\\n")'],
      ["julia", "println(pwd())"],
    ]) {
      const env = report.runtimes.find((r) => r.language === language && r.availability.kind === "available");
      if (!env) {
        note(`${language}-kernel-cwd`, true, "runtime unavailable; skipped");
        continue;
      }
      const s = language === "python" ? session : await startSession(page, env.id);
      const run = await execute(page, s.id, code);
      const m = await settle(page, run.unexpectedSuccess.id);
      const cwd = await streamText(page, m.record.id);
      note(
        `${language}-kernel-cwd-is-workspace`,
        m.record.status.kind === "succeeded" && norm(cwd) === norm(rootA) && !cwd.startsWith("\\\\?\\"),
        `${cwd} (workspace ${rootA})`,
      );
      if (s !== session) await invoke(page, "research_session_shutdown", { sessionId: s.id });
    }

    // ---- Switch to untrusted B: A's session must not run B's code ---------
    await invoke(page, "workspace_open", { path: rootB });
    await page.locator('[data-testid="file-tree"] [data-path="paper.typ"]').waitFor({ timeout: 15_000 });
    await runCommand(page, "Show Research Panel", /Show Research Panel/);
    await page.getByTestId("research-panel").waitFor({ timeout: 15_000 });
    await openInEditor(page, "analysis/plot.py");
    await runCommand(page, "Run File in Research", /Run File in Research Session/);
    await page.waitForTimeout(6000);
    await page.screenshot({ path: join(artifactsDir, "switched-to-untrusted.png") });
    note("untrusted-project-code-never-runs-in-other-session", !existsSync(markerB), existsSync(markerB) ? "B's script ran in A's kernel" : "not run");
    const fromB = await execute(page, session.id, 'open("ran-after-switch.txt", "w").write("x")');
    note("old-session-refuses-code-after-switch", !!fromB.error, fromB.error ?? "accepted");

    // ---- Back in A: the detached session stays refused --------------------
    await invoke(page, "workspace_open", { path: rootA });
    await page.locator('[data-testid="file-tree"] [data-path="paper.typ"]').waitFor({ timeout: 15_000 });
    const back = await execute(page, session.id, 'open("ran-after-return.txt", "w").write("x")');
    await page.waitForTimeout(1500);
    note(
      "detached-session-refuses-code-after-return",
      !!back.error && !existsSync(join(rootA, "ran-after-return.txt")),
      back.error ?? "accepted",
    );

    // ---- Trust revoked: sessions stop -----------------------------------------
    const fresh = await startSession(page, python.id);
    await invoke(page, "trust_revoke", { root: status.root });
    const afterRevoke = await execute(page, fresh.id, 'open("ran-after-revoke.txt", "w").write("x")');
    await page.waitForTimeout(1500);
    note(
      "session-refuses-code-after-trust-revoked",
      !!afterRevoke.error && !existsSync(join(rootA, "ran-after-revoke.txt")),
      afterRevoke.error ?? "accepted",
    );
    const revokedState = (await invoke(page, "research_sessions")).find((s) => s.id === fresh.id)?.state;
    note("session-detached-after-trust-revoked", revokedState?.kind === "detached", revokedState);

    // ---- Workspace closed: sessions stop ----------------------------------
    await invoke(page, "trust_grant", { root: status.root });
    const third = await startSession(page, python.id);
    await invoke(page, "workspace_close");
    const afterClose = await execute(page, third.id, 'open("ran-after-close.txt", "w").write("x")');
    await page.waitForTimeout(1500);
    note(
      "session-refuses-code-after-workspace-close",
      !!afterClose.error && !existsSync(join(rootA, "ran-after-close.txt")),
      afterClose.error ?? "accepted",
    );
  } finally {
    await writeFile(join(artifactsDir, "observations.json"), JSON.stringify(observations, null, 2));
    await app.close();
  }
  const failed = observations.filter((o) => !o.ok);
  assert.deepEqual(failed.map((d) => d.id), [], `violations (see ${join(artifactsDir, "observations.json")})`);
}
