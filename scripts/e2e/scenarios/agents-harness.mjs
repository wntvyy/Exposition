// Agent harness in the real app (debug build) with the scripted provider —
// development only. The scripted provider replaces only the model; the task
// loop, tools, document service transactions, persistence, review, workbench
// shell, and UI are the real ones. It cannot establish live model behavior.
//
// Covers: credentials dialog (configured / not configured, key never shown),
// a task editing an open document through a transaction while the user edits
// concurrently, streamed transcript, approval, Monaco review diff (panel and
// editor area), revert, cancellation, task history after a restart, no console
// errors, and reference screenshots in both themes. Screenshots go to
// test-results/e2e/agents-harness/ and, when EXPOSITION_DEV_HOME is set, to
// $EXPOSITION_DEV_HOME/ui-reference/agents/.
import { copyFile, mkdir, mkdtemp, readdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

const MAIN = "= Notes\nThe cat sat on the mat.\n";
const FAKE_KEY = "sk-ant-e2e-not-a-real-key-7f3a9c0d1e2b";

const invoke = (page, cmd, args) =>
  page.evaluate(([c, a]) => window.__TAURI_INTERNALS__.invoke(c, a), [cmd, args ?? {}]);

function consoleErrors(app) {
  return app.consoleMessages.filter((m) => m.startsWith("error:") || m.startsWith("pageerror:"));
}

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

async function selectOption(page, scope, label, option) {
  await scope.getByRole("button", { name: label }).click();
  await page.getByRole("option", { name: option, exact: true }).click();
}

async function documentText(page, path) {
  const docs = await invoke(page, "document_list");
  const doc = docs.find((d) => d.path === path);
  return doc ? invoke(page, "document_state", { document: doc.id }) : null;
}

async function waitForStatus(page, kind, timeout = 20_000) {
  await page.waitForFunction(
    (k) => document.querySelector("[data-testid=agents-task-status]")?.dataset.status === k,
    kind,
    { timeout },
  );
}

async function startTask(page, { prompt, folder, permission }) {
  const panel = page.getByTestId("agents-panel");
  await panel.getByTestId("agents-new-task").click();
  await page.fill("[data-testid=agents-prompt]", prompt);
  await panel.getByLabel("Project folder").fill(folder);
  await selectOption(page, panel, /Document changes/, permission);
  await panel.getByRole("button", { name: "Start task" }).click();
  await page.waitForSelector("[data-testid=agents-task-view]");
}

export default async function agentsHarness({ launchApp, artifactsDir, assert }) {
  const project = await mkdtemp(join(tmpdir(), "exposition-agents-project-"));
  await writeFile(join(project, "main.typ"), MAIN);
  const namespace = `e2e-${Date.now()}`;
  const env = { EXPOSITION_CREDENTIAL_NAMESPACE: namespace };
  const evidence = { project, namespace, steps: [] };
  const step = (name, data = {}) => evidence.steps.push({ name, ...data });
  const shots = [];
  const shot = async (page, name) => {
    const path = join(artifactsDir, `${name}.png`);
    await page.screenshot({ path });
    shots.push(path);
  };

  let app = await launchApp({ env });
  const dataDir = app.dataDir;
  try {
    const { page } = app;
    await page.setViewportSize({ width: 1440, height: 900 });
    // The Agent view is registered in the shell's auxiliary region.
    const panel = page.getByTestId("agents-panel");
    await panel.waitFor({ timeout: 15_000 });
    await setTheme(page, "light");
    await shot(page, "agents-empty-light");

    // --- Credentials: configured / not configured; the key is never revealed.
    await panel.getByRole("button", { name: "Agent provider settings" }).click();
    const dialog = page.getByRole("dialog", { name: "Agent provider" });
    await dialog.waitFor();
    const keyStatus = () => page.textContent("[data-testid=agents-key-status]");
    assert.match(await keyStatus(), /Not configured/);
    await dialog.getByLabel("API key").fill(FAKE_KEY);
    await dialog.getByRole("button", { name: "Save key" }).click();
    await page.waitForFunction(() => {
      const text = document.querySelector("[data-testid=agents-key-status]")?.textContent ?? "";
      return text.includes("Configured") && !text.includes("Not configured");
    });
    const leaks = await page.evaluate((key) => {
      const inputs = [...document.querySelectorAll("input, textarea")].some((el) => el.value.includes(key));
      return { html: document.documentElement.outerHTML.includes(key), inputs };
    }, FAKE_KEY);
    const status = await invoke(page, "agents_provider_status");
    assert.equal(status.configured, true);
    assert.ok(!JSON.stringify(status).includes(FAKE_KEY), "provider status must not contain the key");
    assert.deepEqual(leaks, { html: false, inputs: false }, "the key must not appear in the webview after saving");
    await page.waitForTimeout(200);
    await shot(page, "agents-settings-configured-light");
    step("key saved to OS credential storage; webview shows Configured only", { store: status.credentialStore });
    await dialog.getByRole("button", { name: "Remove" }).click();
    await page.waitForFunction(() =>
      (document.querySelector("[data-testid=agents-key-status]")?.textContent ?? "").includes("Not configured"),
    );
    step("key removed; Not configured");

    // Select the development-only scripted provider.
    await selectOption(page, dialog, /Provider/, "Scripted provider — development only");
    await dialog.getByRole("button", { name: "Save settings" }).click();
    await page.waitForSelector("[data-testid=agents-settings-message]");
    await dialog.getByRole("button", { name: "Close" }).click();
    await dialog.waitFor({ state: "detached" });
    assert.equal(await page.textContent("[data-testid=agents-provider-chip]"), "Scripted provider — development only");

    // --- Task A (read-only) opens main.typ in the document service.
    await invoke(page, "agents_dev_load_script", {
      script: {
        turns: [
          { steps: [{ toolUse: { id: "toolu_a", name: "read_file", input: { path: "main.typ" } } }] },
          { steps: [{ text: "main.typ has a title and one sentence." }] },
        ],
      },
    });
    await startTask(page, { prompt: "Summarize main.typ", folder: project, permission: "Read only" });
    await waitForStatus(page, "completed");
    let state = await documentText(page, "main.typ");
    assert.equal(state.text, MAIN);
    step("task A completed; main.typ open", { revision: state.info.revision });

    // The user edits the open document (unsaved).
    const docId = state.info.id;
    const userEdit = (base, start, text) =>
      invoke(page, "document_apply", {
        request: {
          id: `user-${Date.now()}`,
          origin: { kind: "source" },
          edits: [{ document: docId, baseRevision: base, patches: [{ range: { start, end: start }, text }] }],
          undo: { kind: "newGroup" },
          conflict: "reject",
          label: null,
          selection: null,
        },
      });
    await userEdit(state.info.revision, 0, "// draft\n");

    // --- Task B edits the open document; the user keeps typing during approval.
    await invoke(page, "agents_dev_load_script", {
      script: {
        turns: [
          {
            chunkDelayMs: 120,
            steps: [
              { thinking: "Read the current text before editing." },
              { text: "I will read main.typ and then make the sentence about the cat more precise." },
              { toolUse: { id: "toolu_read", name: "read_file", input: { path: "main.typ" } } },
            ],
          },
          {
            chunkDelayMs: 60,
            steps: [
              { text: "Replacing the sentence." },
              {
                toolUse: {
                  id: "toolu_edit",
                  name: "edit_document",
                  input: {
                    path: "main.typ",
                    base_revision: "$ref:toolu_read/revision",
                    edits: [{ old_text: "The cat sat on the mat.", new_text: "The cat sat quietly on the mat." }],
                  },
                },
              },
            ],
          },
          { chunkDelayMs: 60, steps: [{ text: "Done: one sentence in main.typ changed. Nothing was built or run." }] },
        ],
      },
    });
    await startTask(page, { prompt: "Make the cat sentence more precise", folder: project, permission: "Ask before each change" });
    const samples = [];
    for (let i = 0; i < 12; i++) {
      samples.push(
        await page.evaluate(() => document.querySelector("[data-testid=agents-draft]")?.textContent?.length ?? 0),
      );
      await page.waitForTimeout(120);
    }
    const growing = samples.filter((n, i) => i > 0 && n > samples[i - 1]).length;
    assert.ok(growing >= 3, `draft should grow while streaming (samples ${samples})`);
    step("transcript streamed", { draftLengths: samples });
    await page.waitForSelector("[data-testid=agents-approval]", { timeout: 20_000 });
    await page.waitForTimeout(200);
    await shot(page, "agents-approval-light");
    state = await documentText(page, "main.typ");
    await userEdit(state.info.revision, state.text.length, "// user keeps typing\n");
    await panel.getByRole("button", { name: "Approve" }).click();
    await waitForStatus(page, "completed");
    state = await documentText(page, "main.typ");
    assert.equal(state.text, "// draft\n= Notes\nThe cat sat quietly on the mat.\n// user keeps typing\n");
    const taskB = (await invoke(page, "agents_task_list"))[0];
    const detailB = await invoke(page, "agents_task_detail", { task: taskB.id });
    const editCall = detailB.transcript.find((i) => i.kind === "toolCall" && i.name === "edit_document");
    assert.equal(editCall.status, "succeeded");
    assert.ok(JSON.parse(editCall.result).rebased, "the agent edit was rebased over the concurrent user edit");
    await shot(page, "agents-task-completed-light");
    step("task B edited the open document through a transaction (rebased over a concurrent user edit)", {
      text: state.text,
      revision: state.info.revision,
    });

    // --- Review: Monaco diff in the panel and in the editor area, then revert.
    await panel.getByRole("button", { name: /Review changes/ }).click();
    await page.waitForSelector("[data-testid=agents-review-file][data-path='main.typ']");
    await page.waitForSelector("[data-testid=agents-panel] [data-testid=agents-diff] .monaco-diff-editor", { timeout: 20_000 });
    const diff = await invoke(page, "agents_review_diff", { task: taskB.id, path: "main.typ" });
    // The user's edit made during approval landed before the agent's edit, so
    // both sides include it; the diff is exactly the agent's change.
    assert.equal(diff.original, "// draft\n= Notes\nThe cat sat on the mat.\n// user keeps typing\n");
    assert.equal(diff.modified, "// draft\n= Notes\nThe cat sat quietly on the mat.\n// user keeps typing\n");
    await panel.getByRole("button", { name: "Open in editor area" }).click();
    await page.waitForSelector(".agents-editor [data-testid=agents-diff] .monaco-diff-editor", { timeout: 20_000 });
    await page.waitForTimeout(600);
    await shot(page, "agents-review-light");
    await setTheme(page, "dark");
    await shot(page, "agents-review-dark");
    await setTheme(page, "light");
    step("review shows the Monaco diff (panel and editor area)");
    await panel.getByTestId("agents-review-file").getByRole("button", { name: "Revert" }).click();
    await page.waitForSelector("[data-testid=agents-panel] [data-testid=agents-review-results]");
    state = await documentText(page, "main.typ");
    assert.equal(state.text, "// draft\n= Notes\nThe cat sat on the mat.\n// user keeps typing\n");
    step("revert restored the text and kept the user's later edit", { text: state.text });

    // --- Cancellation while the model is streaming.
    await invoke(page, "agents_dev_load_script", {
      script: {
        turns: [
          {
            chunkDelayMs: 50,
            steps: [{ text: "Working through a long answer that will be canceled." }, { waitForCancel: true }],
          },
        ],
      },
    });
    await startTask(page, { prompt: "A long task to cancel", folder: project, permission: "Read only" });
    await page.waitForFunction(() =>
      (document.querySelector("[data-testid=agents-draft]")?.textContent ?? "").includes("canceled"),
    );
    const draftText = await page.textContent("[data-testid=agents-draft]");
    assert.match(draftText, /Working through a long answer/, "the whole streamed text is shown, from the first word");
    await shot(page, "agents-running-light");
    await setTheme(page, "dark");
    await shot(page, "agents-running-dark");
    await setTheme(page, "light");
    const canceledAt = Date.now();
    await panel.getByRole("button", { name: "Cancel" }).click();
    await waitForStatus(page, "canceled", 10_000);
    const cancelMs = Date.now() - canceledAt;
    assert.ok(cancelMs < 5000, `cancel took ${cancelMs} ms`);
    await shot(page, "agents-canceled-light");
    step("canceled mid-stream", { cancelMs });

    // Settings and task list in the dark theme.
    await setTheme(page, "dark");
    await panel.getByRole("button", { name: "Tasks", exact: true }).click();
    await panel.getByTestId("agents-task-list").waitFor();
    await shot(page, "agents-tasks-dark");
    await panel.getByRole("button", { name: "Agent provider settings" }).click();
    await page.getByRole("dialog", { name: "Agent provider" }).waitFor();
    await page.waitForTimeout(200);
    await shot(page, "agents-settings-dark");
    await page.keyboard.press("Escape");
    await setTheme(page, "light");
    assert.deepEqual(consoleErrors(app), [], "no console errors");
  } finally {
    await invoke(app.page, "agents_provider_clear_key").catch(() => {});
    await app.close();
  }

  // --- Restart with the same data directory: history is still there.
  app = await launchApp({ env, dataDir });
  try {
    const { page } = app;
    const panel = page.getByTestId("agents-panel");
    await panel.waitFor({ timeout: 15_000 });
    await page.waitForSelector("[data-testid=agents-task-item]");
    const items = await page.$$eval("[data-testid=agents-task-item]", (els) => els.map((e) => e.textContent));
    assert.equal(items.length, 3, `three tasks persisted: ${items}`);
    assert.match(items.join("|"), /Task completed/);
    assert.match(items.join("|"), /Canceled/);
    const tasks = await invoke(page, "agents_task_list");
    const reviewed = tasks.find((t) => t.title.startsWith("Make the cat"));
    const review = await invoke(page, "agents_review", { task: reviewed.id });
    assert.equal(review.files[0].state, "reverted");
    await shot(page, "agents-tasks-after-restart-light");
    step("tasks and review state persisted across restart", { tasks: tasks.map((t) => [t.title, t.status.kind]) });
    assert.equal((await invoke(page, "agents_provider_status")).configured, false);
    assert.deepEqual(consoleErrors(app), [], "no console errors after restart");
  } finally {
    await app.close();
  }
  await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2));

  const devHome = process.env.EXPOSITION_DEV_HOME;
  if (devHome) {
    const target = join(devHome, "ui-reference", "agents");
    await mkdir(target, { recursive: true });
    for (const path of shots) await copyFile(path, join(target, path.split(/[\\/]/).pop()));
    console.log(`copied ${(await readdir(target)).length} screenshots to ${target}`);
  }
}
