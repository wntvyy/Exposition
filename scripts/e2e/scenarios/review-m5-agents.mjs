// R-m5 independent review: agent workflows in the real debug app with the
// scripted provider (development only; no model-provider call is made).
//
// Runs only when named: `npm run e2e -- review-m5-agents`.
//
// 1. Approval visibility: a task waiting for approval while the Agent panel is
//    hidden — is the user told anywhere (status bar, notification)?
// 2. Review UI: revert of a file the agent created and then edited (F5); the
//    Monaco diff gutter's icon font (codicon) is loaded.
// 3. Scope/consent: Typst build diagnostics of an agent-written file read an
//    out-of-scope file under "File names only" consent (F2).
// 4. Removing the stored key from the provider dialog asks for confirmation.
//    Credential separation: a fake key in a throwaway credential namespace
//    never reaches a native command's environment, an agent research kernel,
//    task transcripts, or the app data directory.
// 5. Crash: the app is killed while an agent command (and, if a local Docker
//    image is present, a restricted container) runs; what survives, and how
//    the tasks look after relaunch.
//
// Every observation prints `ok` or `DEFECT:`; observations go to
// test-results/e2e/review-m5-agents/observations.json. DEFECT observations
// fail the scenario at the end.
import { execFileSync } from "node:child_process";
import { mkdir, mkdtemp, readdir, readFile, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { hardKill } from "../app.mjs";
import { waitForAsync } from "../wait.mjs";

const NAME = "review-m5-agents";
const FAKE_KEY = "sk-ant-rm5-not-a-real-key-4c1d8e2f9a7b";
const SECRET = "PRIVATESECRETrm5x9";
const DOCKER_IMAGE = "python:3.13-slim";
const ORPHAN_MARKER = "rm5orphanmarker";

const invoke = (page, cmd, args) =>
  page.evaluate(([c, a]) => window.__TAURI_INTERNALS__.invoke(c, a), [cmd, args ?? {}]);

async function runCommand(page, query) {
  await page.keyboard.press("Control+Shift+P");
  const input = page.getByRole("textbox", { name: "Command" });
  await input.waitFor({ state: "visible", timeout: 5000 });
  await input.fill(query);
  await page.locator(".wb-palette__item").first().waitFor({ state: "visible" });
  await page.keyboard.press("Enter");
  await input.waitFor({ state: "detached", timeout: 5000 });
}

async function setTheme(page, theme) {
  await runCommand(page, theme === "dark" ? "color theme dark" : "color theme light");
  await page.waitForFunction((t) => document.documentElement.dataset.theme === t, theme);
  await page.waitForTimeout(250);
}

function request(project, overrides = {}) {
  return {
    prompt: "Review task",
    workspaceRoot: project,
    scope: { kind: "workspace" },
    permission: "autoApply",
    execution: "disabled",
    consent: "scopeContent",
    budgets: null,
    ...overrides,
  };
}

async function startTask(page, script, req) {
  await invoke(page, "agents_dev_load_script", { script });
  return invoke(page, "agents_task_create", { request: req });
}

const detail = (page, id) => invoke(page, "agents_task_detail", { task: id });

async function waitStatus(page, id, kinds, timeout = 60_000) {
  return waitForAsync(
    page,
    async ([task, wanted]) => {
      const d = await window.__TAURI_INTERNALS__.invoke("agents_task_detail", { task });
      return wanted.includes(d.summary.status.kind) ? d : null;
    },
    [id, kinds],
    { timeout, message: `task ${id} status in ${kinds}` },
  );
}

async function waitApproval(page, id, timeout = 30_000) {
  return waitForAsync(
    page,
    async (task) => {
      const d = await window.__TAURI_INTERNALS__.invoke("agents_task_detail", { task });
      return d.approvals[0] ?? null;
    },
    id,
    { timeout, message: `approval for ${id}` },
  );
}

const toolCalls = (d) => d.transcript.filter((i) => i.kind === "toolCall");

async function filesContaining(dir, needle) {
  const hits = [];
  async function walk(current) {
    let entries;
    try {
      entries = await readdir(current, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of entries) {
      const path = join(current, entry.name);
      if (entry.isDirectory()) await walk(path);
      else if (entry.isFile()) {
        try {
          if ((await stat(path)).size > 64 * 1024 * 1024) continue;
          const bytes = await readFile(path);
          if (bytes.includes(needle) || bytes.includes(Buffer.from(needle, "utf16le"))) hits.push(path);
        } catch {
          // locked file (e.g. WebView2 profile)
        }
      }
    }
  }
  await walk(dir);
  return hits;
}

function processesWith(marker) {
  try {
    const out = execFileSync(
      "powershell",
      [
        "-NoProfile",
        "-Command",
        `Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -like '*${marker}*' -and $_.Name -ne 'powershell.exe' } | ForEach-Object { "$($_.ProcessId) $($_.Name)" }`,
      ],
      { encoding: "utf8" },
    );
    return out
      .split(/\r?\n/)
      .map((l) => l.trim())
      .filter(Boolean);
  } catch {
    return [];
  }
}

function dockerManaged() {
  try {
    return execFileSync("docker", ["ps", "--filter", "label=org.exposition.managed=1", "--format", "{{.Names}}"], {
      encoding: "utf8",
    })
      .split(/\r?\n/)
      .filter(Boolean);
  } catch {
    return null;
  }
}

function dockerImagePresent(image) {
  try {
    execFileSync("docker", ["image", "inspect", "--format", "{{.Id}}", image], { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
}

export default async function reviewM5Agents({ launchApp, artifactsDir }) {
  if (!process.argv.slice(2).includes(NAME)) {
    console.log(`SKIPPED: ${NAME} runs only when named`);
    return;
  }
  const observations = [];
  const note = (id, ok, detailText) => {
    observations.push({ id, ok, detail: detailText });
    console.log(`${ok ? "ok     " : "DEFECT:"} ${id}: ${typeof detailText === "string" ? detailText : JSON.stringify(detailText)}`);
  };
  const notVerified = (id, why) => {
    observations.push({ id, ok: null, detail: why });
    console.log(`NOT VERIFIED ${id}: ${why}`);
  };
  const shot = (page, name) => page.screenshot({ path: join(artifactsDir, `${name}.png`) });

  const base = await mkdtemp(join(tmpdir(), "xp-rm5-"));
  const project = join(base, "project");
  const dataDir = join(base, "data");
  await mkdir(join(project, "private"), { recursive: true });
  await mkdir(join(project, "drafts"), { recursive: true });
  await mkdir(dataDir, { recursive: true });
  await writeFile(join(project, "main.typ"), "= Notes\n\nThe squares grow quickly.\n");
  await writeFile(join(project, "private", "secret.txt"), `${SECRET}\n`);
  await writeFile(join(project, "drafts", "readme.txt"), "drafts\n");
  // A throwaway credential entry: debug builds honor the namespace, so the
  // user's real entry is never read, written, or cleared.
  const env = { EXPOSITION_CREDENTIAL_NAMESPACE: `rm5-${Date.now()}` };

  let app = await launchApp({ env, dataDir });
  const info = await invoke(app.page, "app_info");
  if (!info.debugBuild) {
    await app.close();
    throw new Error(`${NAME} requires a debug build (it sets and clears a credential entry)`);
  }
  const managedBefore = dockerManaged();
  let crashTasks = [];
  let dockerStarted = false;
  try {
    const { page } = app;
    await page.setViewportSize({ width: 1440, height: 900 });
    const panel = page.getByTestId("agents-panel");
    await panel.waitFor({ timeout: 15_000 });
    await setTheme(page, "light");
    await invoke(page, "workspace_open", { path: project });
    await invoke(page, "agents_provider_update", {
      settings: { provider: "scripted", model: "claude-opus-5-5", effort: "medium", consent: "scopeContent" },
    });

    // --- 1. Approval while the Agent panel is hidden. ------------------------
    await page.locator("body").click({ position: { x: 700, y: 400 } });
    await page.keyboard.press("Control+Alt+B");
    await page.waitForTimeout(400);
    const panelHidden = !(await panel.isVisible().catch(() => false));
    const created = await startTask(
      page,
      {
        turns: [
          { steps: [{ toolUse: { id: "c", name: "create_file", input: { path: "notes/new.typ", content: "= Draft\nAlpha\n" } } }] },
          { steps: [{ toolUse: { id: "r", name: "read_file", input: { path: "notes/new.typ" } } }] },
          {
            steps: [
              {
                toolUse: {
                  id: "e",
                  name: "edit_document",
                  input: { path: "notes/new.typ", base_revision: "$ref:r/revision", edits: [{ old_text: "Alpha", new_text: "Beta" }] },
                },
              },
            ],
          },
          { steps: [{ text: "Created notes/new.typ and refined it." }] },
        ],
      },
      request(project, { permission: "reviewEdits", prompt: "Draft a note file" }),
    );
    await waitStatus(page, created.id, ["awaitingApproval"]);
    await page.waitForTimeout(1500);
    const statusBar = (await page.locator("[data-status-item='agents.status']").textContent().catch(() => "")) ?? "";
    const toasts = await page.$$eval("[data-testid=notification]", (els) => els.map((e) => e.textContent ?? ""));
    await shot(page, "approval-pending-panel-hidden-light");
    const told = /approv/i.test(statusBar) || toasts.some((t) => /approv/i.test(t));
    note(
      "approval-visible-when-agent-panel-hidden",
      !panelHidden || told,
      { panelHidden, statusBar, notifications: toasts },
    );

    // Show the panel; the approval card in both themes; approve by keyboard.
    await page.keyboard.press("Control+Alt+B");
    await panel.waitFor({ state: "visible", timeout: 5000 });
    await panel.getByTestId("agents-task-item").first().click().catch(() => {});
    await page.waitForSelector("[data-testid=agents-approval]", { timeout: 10_000 });
    await page.waitForTimeout(300);
    await shot(page, "approval-card-light");
    await setTheme(page, "dark");
    await shot(page, "approval-card-dark");
    await setTheme(page, "light");
    await page.getByTestId("agents-approve").focus();
    await page.keyboard.press("Enter");
    const second = await waitApproval(page, created.id);
    note("approve-by-keyboard", second.tool === "edit_document", `next approval: ${second.title}`);
    await invoke(page, "agents_approval_resolve", { task: created.id, approval: second.id, approved: true });
    await waitStatus(page, created.id, ["completed"]);

    // --- 2. Review UI: revert the created-then-edited file. ------------------
    await panel.getByTestId("agents-open-review").click();
    await page.waitForSelector("[data-testid=agents-review-file][data-path='notes/new.typ']", { timeout: 10_000 });
    await page.waitForSelector("[data-testid=agents-panel] [data-testid=agents-diff] .monaco-diff-editor", { timeout: 20_000 });
    await page.waitForTimeout(500);
    await shot(page, "review-created-file-light");
    // Monaco's diff gutter (+/- indicators) uses the codicon font.
    const fonts = await page.evaluate(async () => {
      await document.fonts.ready;
      const faces = [...document.fonts].map((f) => `${f.family}:${f.status}`);
      const glyph = document.querySelector("[data-testid=agents-diff] .codicon");
      const glyphFont = glyph ? getComputedStyle(glyph, "::before").fontFamily : null;
      return { faces, codiconLoaded: [...document.fonts].some((f) => /codicon/i.test(f.family) && f.status === "loaded"), glyphFont };
    });
    note("diff-gutter-icon-font-loaded", fonts.codiconLoaded, fonts);
    await panel.getByTestId("agents-revert-file").first().click();
    await page.waitForSelector("[data-testid=agents-review-results]", { timeout: 10_000 });
    const resultText = await page.textContent("[data-testid=agents-review-results]");
    await shot(page, "review-revert-result-light");
    await setTheme(page, "dark");
    await shot(page, "review-revert-result-dark");
    await setTheme(page, "light");
    const stillThere = await stat(join(project, "notes", "new.typ")).then(() => true, () => false);
    note("revert-created-then-edited-file", !stillThere && !/Refused/.test(resultText ?? ""), {
      result: resultText,
      fileStillOnDisk: stillThere,
    });

    // --- 3. Typst build diagnostics vs scope and metadata-only consent. ------
    const leak = await startTask(
      page,
      {
        turns: [
          {
            steps: [
              { toolUse: { id: "r", name: "read_file", input: { path: "private/secret.txt" } } },
              { toolUse: { id: "c", name: "create_file", input: { path: "drafts/leak.typ", content: '#panic(read("/private/secret.txt"))\n' } } },
            ],
          },
          { steps: [{ toolUse: { id: "b", name: "build_document", input: { entry: "drafts/leak.typ", engine: "auto" } } }] },
          { steps: [{ text: "done" }] },
        ],
      },
      request(project, {
        scope: { kind: "files", paths: ["drafts"] },
        consent: "metadataOnly",
        prompt: "Draft in drafts/ (file names only)",
      }),
    );
    const leakDetail = await waitStatus(page, leak.id, ["completed", "failed", "budgetExceeded"]);
    const [directRead, , build] = toolCalls(leakDetail);
    note("control-direct-out-of-scope-read-refused", directRead.isError, directRead.result);
    note("typst-build-does-not-reveal-out-of-scope-content", !(build.result ?? "").includes(SECRET), build.result);

    // --- 4. Credential separation. -------------------------------------------
    // Save the fake key through the dialog, then remove it: is the destructive
    // action confirmed?
    await panel.getByRole("button", { name: "Agent provider settings" }).click();
    const dialog = page.getByRole("dialog", { name: "Agent provider" });
    await dialog.waitFor();
    await dialog.getByLabel("API key").fill(FAKE_KEY);
    await dialog.getByTestId("agents-key-save").click();
    await page.waitForFunction(() =>
      /Configured/.test(document.querySelector("[data-testid=agents-key-status]")?.textContent?.replace("Not configured", "") ?? ""),
    );
    let status = await invoke(page, "agents_provider_status");
    note("fake-key-stored-in-namespace", status.configured === true, status.credentialStore);
    await page.waitForTimeout(200);
    await shot(page, "provider-settings-configured-light");
    await dialog.getByTestId("agents-key-remove").click();
    await page.waitForTimeout(500);
    const confirmShown = await page.getByRole("alertdialog").isVisible().catch(() => false);
    status = await invoke(page, "agents_provider_status");
    note("remove-key-asks-for-confirmation", confirmShown || status.configured, {
      confirmShown,
      configuredAfterOneClick: status.configured,
    });
    if (confirmShown) await page.keyboard.press("Escape");
    await page.keyboard.press("Escape");
    await dialog.waitFor({ state: "detached", timeout: 5000 }).catch(() => {});
    status = await invoke(page, "agents_provider_set_key", { key: FAKE_KEY });
    await invoke(page, "trust_grant", { root: project });
    const envTask = await startTask(
      page,
      {
        turns: [
          {
            steps: [
              {
                toolUse: {
                  id: "env",
                  name: "run_command",
                  input: { program: "python", args: ["-c", "import os,json;print(json.dumps(dict(os.environ)))"], working_directory: "" },
                },
              },
              { toolUse: { id: "kenv", name: "run_python", input: { code: "import os,json\nprint(json.dumps(dict(os.environ)))" } } },
            ],
          },
          { steps: [{ text: "done" }] },
        ],
      },
      request(project, { execution: "approveEach", prompt: "Show the environment" }),
    );
    for (let i = 0; i < 2; i++) {
      const approval = await waitApproval(page, envTask.id, 60_000);
      await invoke(page, "agents_approval_resolve", { task: envTask.id, approval: approval.id, approved: true });
      await page.waitForTimeout(300);
    }
    const envDetail = await waitStatus(page, envTask.id, ["completed", "failed"], 180_000);
    const [cmdEnv, kernelEnv] = toolCalls(envDetail);
    for (const [label, call] of [
      ["native-command", cmdEnv],
      ["agent-research-kernel", kernelEnv],
    ]) {
      if (call.status !== "succeeded") {
        notVerified(`${label}-environment`, `tool did not run: ${call.result?.slice(0, 300)}`);
        continue;
      }
      const text = call.result ?? "";
      const credentialNames = (text.match(/"([A-Z0-9_]*(?:KEY|TOKEN|SECRET|PASSWORD|ANTHROPIC)[A-Z0-9_]*)"\s*:/gi) ?? []).slice(0, 10);
      note(`${label}-environment-has-no-key`, !text.includes(FAKE_KEY) && credentialNames.length === 0, {
        credentialLikeNames: credentialNames,
      });
    }
    const allDetails = await Promise.all((await invoke(page, "agents_task_list")).map((t) => detail(page, t.id)));
    note("transcripts-have-no-key", !JSON.stringify(allDetails).includes(FAKE_KEY), `${allDetails.length} tasks`);

    // --- 5. Crash while agent processes run. ---------------------------------
    const sleeper = await startTask(
      page,
      {
        turns: [
          {
            steps: [
              {
                toolUse: {
                  id: "sleep",
                  name: "run_command",
                  input: { program: "python", args: ["-c", `import time; time.sleep(600)  # ${ORPHAN_MARKER}`], working_directory: "" },
                },
              },
            ],
          },
          { steps: [{ text: "done" }] },
        ],
      },
      request(project, { execution: "approveEach", prompt: "Sleep natively" }),
    );
    const approval = await waitApproval(page, sleeper.id);
    await invoke(page, "agents_approval_resolve", { task: sleeper.id, approval: approval.id, approved: true });
    crashTasks.push(sleeper.id);
    let dockerTask = null;
    if (managedBefore !== null && dockerImagePresent(DOCKER_IMAGE)) {
      await invoke(page, "execution_settings_update", { change: { kind: "dockerImage", image: DOCKER_IMAGE } });
      dockerTask = await startTask(
        page,
        {
          turns: [
            { steps: [{ toolUse: { id: "dsleep", name: "run_python", input: { code: "import time\ntime.sleep(600)" } } }] },
            { steps: [{ text: "done" }] },
          ],
        },
        request(project, { execution: "restricted", prompt: "Sleep in the sandbox" }),
      );
      crashTasks.push(dockerTask.id);
      dockerStarted = true;
    } else {
      notVerified("restricted-container-after-crash", `docker or local image ${DOCKER_IMAGE} unavailable`);
    }
    // A third task is left waiting for approval when the app dies.
    const waiting = await startTask(
      page,
      {
        turns: [
          { steps: [{ toolUse: { id: "wc", name: "create_file", input: { path: "pending.typ", content: "x\n" } } }] },
          { steps: [{ text: "done" }] },
        ],
      },
      request(project, { permission: "reviewEdits", prompt: "Pending approval at crash" }),
    );
    await waitApproval(page, waiting.id);
    crashTasks.push(waiting.id);
    await page.waitForTimeout(6000);
    const nativeBefore = processesWith(ORPHAN_MARKER);
    const managedDuring = dockerManaged() ?? [];
    note("crash-setup-native-process-running", nativeBefore.length > 0, nativeBefore);
    await writeFile(join(artifactsDir, "pre-crash.json"), JSON.stringify({ nativeBefore, managedDuring }, null, 2));
  } catch (error) {
    await app.close();
    throw error;
  }

  // Simulated crash: TerminateProcess on the app only.
  const exited = new Promise((resolve) => app.child.once("exit", resolve));
  hardKill(app.child.pid);
  await Promise.race([exited, new Promise((r) => setTimeout(r, 10_000))]);
  await new Promise((r) => setTimeout(r, 4000));
  const nativeAfter = processesWith(ORPHAN_MARKER);
  note("native-agent-process-killed-with-app", nativeAfter.length === 0, nativeAfter);
  const newContainers = (dockerManaged() ?? []).filter((n) => !(managedBefore ?? []).includes(n));
  if (dockerStarted) {
    note("restricted-container-stopped-with-app", newContainers.length === 0, newContainers);
  }

  // Relaunch over the same data directory.
  app = await launchApp({ env, dataDir });
  try {
    const { page } = app;
    await page.getByTestId("agents-panel").waitFor({ timeout: 15_000 });
    for (const id of crashTasks) {
      const d = await detail(page, id);
      const noticed = d.transcript.some((i) => i.kind === "notice" && /closed while this task was running/.test(i.message));
      note(`task-after-crash-${d.summary.title}`, d.summary.status.kind === "interrupted" && noticed, d.summary.status);
      const stuck = toolCalls(d).filter((c) => c.status === "running" || c.status === "awaitingApproval");
      note(
        `tool-calls-not-shown-live-after-crash-${d.summary.title}`,
        stuck.length === 0,
        stuck.map((c) => `${c.name}: ${c.status}`),
      );
    }
    // What the interrupted task view shows.
    const panel = page.getByTestId("agents-panel");
    await panel.getByTestId("agents-task-item").first().click().catch(() => {});
    await page.waitForSelector("[data-testid=agents-task-view]", { timeout: 10_000 }).catch(() => {});
    await page.locator("[data-testid=agents-tool-call]").first().click().catch(() => {});
    await page.waitForTimeout(400);
    await shot(page, "interrupted-task-view-light");
    await page.waitForTimeout(2000);
    const survivingContainers = (dockerManaged() ?? []).filter((n) => !(managedBefore ?? []).includes(n));
    if (dockerStarted) {
      note("restricted-container-gone-after-relaunch", survivingContainers.length === 0, survivingContainers);
    }
    await shot(page, "after-crash-relaunch-light");
    await invoke(page, "agents_provider_clear_key").catch(() => {});
  } finally {
    await app.close();
  }

  // Clean up anything the crash left behind.
  for (const line of processesWith(ORPHAN_MARKER)) {
    try {
      execFileSync("taskkill", ["/PID", line.split(" ")[0], "/T", "/F"], { stdio: "ignore" });
    } catch {}
  }
  for (const name of (dockerManaged() ?? []).filter((n) => !(managedBefore ?? []).includes(n))) {
    try {
      execFileSync("docker", ["kill", name], { stdio: "ignore" });
    } catch {}
  }
  const keyFiles = await filesContaining(dataDir, FAKE_KEY);
  note("app-data-has-no-key", keyFiles.length === 0, keyFiles);
  const projectKeyFiles = await filesContaining(project, FAKE_KEY);
  note("project-has-no-key", projectKeyFiles.length === 0, projectKeyFiles);

  await writeFile(join(artifactsDir, "observations.json"), JSON.stringify({ base, observations }, null, 2));
  const defects = observations.filter((o) => o.ok === false);
  if (defects.length) throw new Error(`${defects.length} DEFECT observation(s): ${defects.map((d) => d.id).join(", ")}`);
}
