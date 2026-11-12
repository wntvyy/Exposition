// Execution service in the real app: a supervised process tree is visible in
// the Jobs panel, the Cancel button kills the whole tree (verified from the OS
// process table, not from the app), output stays bounded, the Toolchains view
// reports real tools, and killing the app process itself takes the job tree
// down with it (job-object kill-on-close).
//
// Requires a debug build (`npm run build:app`) for the dev-only
// `dev_start_probe_job` command, and Python (the probe is a Python parent that
// spawns a sleeping Python grandchild).
import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

const PROBE_LOG_LIMIT = 16 * 1024;

/** Snapshot of the OS process table (pid → { parent, name }). */
function processTable() {
  if (process.platform !== "win32") {
    const out = execFileSync("ps", ["-eo", "pid=,ppid=,comm="], { encoding: "utf8" });
    return new Map(
      out
        .trim()
        .split("\n")
        .map((l) => l.trim().split(/\s+/))
        .map(([pid, ppid, ...name]) => [Number(pid), { parent: Number(ppid), name: name.join(" ") }]),
    );
  }
  const out = execFileSync(
    "powershell.exe",
    [
      "-NoProfile",
      "-NonInteractive",
      "-Command",
      "Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,Name | ConvertTo-Json -Compress",
    ],
    { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 },
  );
  return new Map(JSON.parse(out).map((p) => [p.ProcessId, { parent: p.ParentProcessId, name: p.Name }]));
}

/** The root pid plus all of its descendants (from one table snapshot). */
function processTree(rootPid) {
  const table = processTable();
  if (!table.has(rootPid)) return [];
  const tree = [{ pid: rootPid, ...table.get(rootPid) }];
  for (let i = 0; i < tree.length; i++) {
    for (const [pid, info] of table) {
      if (info.parent === tree[i].pid && !tree.some((t) => t.pid === pid)) tree.push({ pid, ...info });
    }
  }
  return tree;
}

async function waitFor(what, fn, timeoutMs = 20_000) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = await fn();
    if (value) return value;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 250));
  }
}

const invoke = (page, cmd, args) => page.evaluate(([c, a]) => window.__TAURI_INTERNALS__.invoke(c, a), [cmd, args]);
/** The command's rejection value (e.g. `{ code, message }`), or null if it succeeded. */
const rejection = (page, cmd, args) =>
  page.evaluate(
    ([c, a]) => window.__TAURI_INTERNALS__.invoke(c, a).then(() => null, (e) => e),
    [cmd, args],
  );

async function startProbeTree(page) {
  const jobId = await invoke(page, "dev_start_probe_job");
  const summary = await waitFor("probe job to start with a grandchild", async () => {
    const job = (await invoke(page, "job_list")).find((j) => j.info.id === jobId);
    if (job?.info.result) throw new Error(`probe job finished early: ${JSON.stringify(job.info.result)}`);
    if (!job?.pid) return null;
    const tree = processTree(job.pid);
    // The Python parent plus its Python grandchild (console hosts may also appear).
    return tree.filter((p) => /python/i.test(p.name)).length >= 2 ? { job, tree } : null;
  });
  return { jobId, ...summary };
}

export default async function executionJobs({ launchApp, artifactsDir, assert }) {
  const evidence = {};
  // Keep the extraction cache of self-extracting tools (biber) across runs.
  const app = await launchApp({ env: { EXPOSITION_TOOL_CACHE_DIR: join(artifactsDir, "..", "tool-cache") } });
  try {
    const { page } = app;
    // Give the bottom panel room for the Jobs view (persisted workbench layout).
    await page.evaluate(() =>
      localStorage.setItem(
        "exposition.workbench.layout",
        JSON.stringify({
          version: 1,
          sidebar: { visible: true, size: 220, activeView: null },
          panel: { visible: true, size: 560, activeView: "jobs.panel" },
          auxiliary: { visible: false, size: 320, activeView: null },
          editorSplit: 50,
        }),
      ),
    );
    await page.reload();
    await page.waitForFunction(() => (document.getElementById("root")?.childElementCount ?? 0) > 0);

    // --- Jobs panel: start a supervised process tree and see it running.
    await page.getByRole("tab", { name: "Jobs" }).click();
    const panel = page.getByTestId("jobs-panel");
    await panel.waitFor();
    const { jobId, job, tree } = await startProbeTree(page);
    evidence.processTreeBeforeCancel = tree;
    assert.ok(tree.filter((p) => /python/i.test(p.name)).length >= 2, `parent + grandchild: ${JSON.stringify(tree)}`);
    const row = panel.locator(`li.exp-jobs__row[data-job-id="${jobId}"]`);
    await row.locator('[data-testid="job-status"][data-job-status="running"]').waitFor({ timeout: 15_000 });
    await row.locator(".exp-jobs__select").click();
    const output = page.getByTestId("job-output");
    await waitFor("tick output in the viewer", async () => /tick \d+ parent=/.test(await output.innerText()));
    assert.match(await output.locator('[data-stream="stderr"]').first().innerText(), /stderr tick/);
    await page.getByTestId("jobs-status").filter({ hasText: "1 job running" }).waitFor();
    await page.screenshot({ path: join(artifactsDir, "jobs-running.png") });

    // --- Cancel from the UI; the whole tree must disappear from the OS.
    await row.getByRole("button", { name: `Cancel ${job.info.label}` }).click();
    await row.locator('[data-testid="job-status"][data-job-status="canceled"]').waitFor({ timeout: 15_000 });
    const survivors = await waitFor(
      "process tree to exit",
      () => {
        const table = processTable();
        const alive = tree.filter((p) => table.has(p.pid) && table.get(p.pid).name === p.name);
        return alive.length === 0 ? [] : null;
      },
      10_000,
    );
    assert.deepEqual(survivors, []);
    const finished = (await invoke(page, "job_list")).find((j) => j.info.id === jobId);
    assert.equal(finished.info.result.outcome.kind, "canceled");
    evidence.canceledJob = finished;

    // --- Output is bounded per stream by the job's log limit.
    const outputPage = await invoke(page, "job_output", { job: jobId, after: null, maxBytes: 1024 * 1024 });
    const bytes = { stdout: 0, stderr: 0 };
    for (const c of outputPage.chunks) bytes[c.stream] += new TextEncoder().encode(c.text).length;
    evidence.retainedBytes = bytes;
    assert.ok(bytes.stdout <= PROBE_LOG_LIMIT && bytes.stderr <= PROBE_LOG_LIMIT, JSON.stringify(bytes));
    assert.ok(outputPage.truncated && finished.info.result.outputTruncated, "burst output must have been truncated");
    await page.getByTestId("output-truncated").waitFor();
    await page.getByTestId("jobs-status").waitFor({ state: "detached" });
    await page.screenshot({ path: join(artifactsDir, "jobs-canceled.png") });

    // --- No generic command runner is exposed; unknown/invalid ids are refused.
    const refused = await rejection(page, "job_run", { program: "cmd.exe" });
    assert.ok(refused, "there must be no generic run command");
    evidence.genericRunRefusal = String(refused);
    const badId = await rejection(page, "job_cancel", { job: "../../x" });
    assert.equal(badId?.code, "job.invalidId");

    // --- Trust commands validate and persist (granting is a UI-dialog action).
    // Project folders live outside the app's data folder, which can never be
    // trusted or opened (it holds trust.json and settings.json).
    const projects = await mkdtemp(join(tmpdir(), "xp-exec-"));
    assert.equal((await rejection(page, "trust_grant", { root: app.dataDir }))?.code, "trust.tooBroad");
    assert.equal((await rejection(page, "trust_grant", { root: join(app.dataDir, "execution") }))?.code, "trust.tooBroad");
    const trustTarget = join(projects, "trust-target");
    await mkdir(trustTarget, { recursive: true });
    assert.equal((await invoke(page, "trust_status", { root: trustTarget })).trusted, false);
    assert.equal((await invoke(page, "trust_grant", { root: trustTarget })).trusted, true);
    assert.equal((await invoke(page, "trust_revoke", { root: trustTarget })).trusted, false);
    const broad = await rejection(page, "trust_grant", { root: trustTarget.slice(0, 3) });
    assert.equal(broad?.code, "trust.tooBroad");

    // --- An untrusted workspace never executes; trust needs the explicit dialog.
    const workspace = join(projects, "untrusted-project");
    const marker = join(workspace, "exposition-dev-ran.txt");
    await mkdir(workspace, { recursive: true });
    const refusedId = await invoke(page, "dev_start_workspace_job", { root: workspace });
    const refusedRow = panel.locator(`li.exp-jobs__row[data-job-id="${refusedId}"]`);
    await refusedRow.locator('[data-testid="job-status"][data-job-status="failedToStart"]').waitFor({ timeout: 15_000 });
    assert.ok(!existsSync(marker), "an untrusted workspace must not execute");
    const refusedJob = (await invoke(page, "job_list")).find((j) => j.info.id === refusedId);
    assert.equal(refusedJob.refusal.code, "untrustedWorkspace");
    assert.equal(refusedJob.info.startedAtMs, null);
    evidence.untrustedRefusal = refusedJob.refusal;
    await refusedRow.locator(".exp-jobs__select").click();
    // The refusal is also announced as a notification offering the same action.
    await page.getByTestId("notification").filter({ hasText: "Workspace not trusted" }).waitFor();
    const trustButton = page.getByTestId("job-detail").getByRole("button", { name: "Trust workspace…" });
    const dialog = page.getByRole("alertdialog", { name: "Trust this workspace?" });
    await trustButton.click();
    await dialog.waitFor();
    assert.ok(
      await dialog.getByRole("button", { name: /Don.t trust/ }).evaluate((el) => el === document.activeElement),
      "the safe choice is focused",
    );
    assert.ok((await dialog.getByTestId("trust-root").innerText()).endsWith("untrusted-project"));
    await page.waitForTimeout(400); // let the dialog's enter transition finish
    await page.screenshot({ path: join(artifactsDir, "trust-dialog.png") });
    await dialog.getByRole("button", { name: /Don.t trust/ }).click();
    await dialog.waitFor({ state: "detached" });
    assert.equal((await invoke(page, "trust_status", { root: workspace })).trusted, false, "declining keeps it untrusted");
    await trustButton.click();
    await dialog.getByRole("button", { name: "Trust workspace", exact: true }).click();
    await dialog.waitFor({ state: "detached" });
    assert.equal((await invoke(page, "trust_status", { root: workspace })).trusted, true);
    await page.getByTestId("notification").filter({ hasText: "Workspace not trusted" }).waitFor({ state: "detached" });
    const allowedId = await invoke(page, "dev_start_workspace_job", { root: workspace });
    await panel
      .locator(`li.exp-jobs__row[data-job-id="${allowedId}"] [data-testid="job-status"][data-job-status="succeeded"]`)
      .waitFor({ timeout: 30_000 });
    assert.ok(existsSync(marker), "the trusted workspace job ran");

    // --- Toolchains view lists real tools with versions and guidance.
    await page.getByRole("tab", { name: "Toolchains" }).click();
    const tools = page.getByTestId("toolchains-view");
    const toolRow = (id) => tools.locator(`tr[data-tool-id="${id}"]`);
    await toolRow("pdflatex").waitFor({ timeout: 180_000 }); // first run unpacks biber (~1 min)
    const toolchains = await invoke(page, "toolchains_list");
    evidence.toolchains = toolchains.map(({ id, status, version, path }) => ({ id, status, version, path }));
    for (const id of ["pdflatex", "lualatex", "python"]) {
      assert.equal(await toolRow(id).getAttribute("data-status"), "available", `${id} should be available`);
      const version = (await toolRow(id).getByTestId("tool-version").innerText()).trim();
      assert.ok(version.length > 3 && version !== "—", `${id} version shown: ${version}`);
    }
    for (const id of ["lean", "lake", "elan", "rscript", "julia", "irkernel", "ijulia"]) {
      const status = await toolRow(id).getAttribute("data-status");
      if (status === "available") {
        const version = (await toolRow(id).getByTestId("tool-version").innerText()).trim();
        assert.ok(version !== "—", `${id} is available, so it must show a version`);
      } else {
        const guidance = await toolRow(id).getByTestId("tool-guidance").innerText();
        assert.ok(guidance.length > 30, `${id} is ${status}; it must show guidance`);
      }
    }
    assert.equal(await toolRow("lean").getAttribute("data-status"), toolchains.find((t) => t.id === "lean").status);
    await page.screenshot({ path: join(artifactsDir, "toolchains.png"), fullPage: true });

    // --- Killing the app process (not its tree) must kill supervised jobs.
    await page.getByRole("tab", { name: "Jobs" }).click();
    const second = await startProbeTree(page);
    evidence.processTreeBeforeAppKill = second.tree;
    process.kill(app.child.pid); // TerminateProcess on exposition.exe only
    // Each process-table snapshot is a PowerShell/CIM query, which itself can
    // take seconds on a loaded machine; record every snapshot so a miss shows
    // whether the tree died late or never. Observe up to 60 s; the tree must
    // be gone within 10 s of the kill as measured by a snapshot that STARTED
    // after the tree was gone.
    const killedAt = Date.now();
    const snapshots = [];
    let goneAt = null;
    while (Date.now() - killedAt < 60_000) {
      const started = Date.now();
      const table = processTable();
      const alive = second.tree.filter((p) => table.has(p.pid) && table.get(p.pid).name === p.name);
      snapshots.push({
        startedMs: started - killedAt,
        tookMs: Date.now() - started,
        alive: alive.map((p) => `${p.pid} ${p.name}`),
      });
      if (alive.length === 0) {
        goneAt = started - killedAt;
        break;
      }
      await new Promise((r) => setTimeout(r, 250));
    }
    evidence.appKillSnapshots = snapshots;
    assert.ok(goneAt !== null, `job tree still alive 60 s after the app was killed: ${JSON.stringify(snapshots.at(-1))}`);
    // The first snapshot that saw the tree gone started at goneAt; the
    // previous one (if any) still saw it alive.
    const lastAlive = snapshots.length > 1 ? snapshots.at(-2).startedMs : 0;
    assert.ok(
      lastAlive <= 10_000,
      `job tree was still alive ${lastAlive} ms after the app was killed (gone by ${goneAt} ms): ${JSON.stringify(snapshots)}`,
    );
    evidence.appKillResult =
      snapshots.length === 1
        ? `all job processes were gone at the first snapshot after the app was terminated (query took ${snapshots[0].tookMs} ms)`
        : `all job processes exited after the app process was terminated (last seen alive at ${lastAlive} ms, gone by ${goneAt} ms)`;
  } finally {
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2));
    await app.close();
  }
}
