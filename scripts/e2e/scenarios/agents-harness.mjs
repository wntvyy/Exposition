// Agent harness in the real app (debug build) with the scripted provider —
// development only. The scripted provider replaces only the model; the task
// loop, tools, document service transactions, persistence, review, and UI are
// the real ones. It cannot establish live model behavior.
//
// Covers: credentials dialog (configured / not configured, key never shown),
// a task editing an open document through a transaction while the user edits
// concurrently, streamed transcript, approval, Monaco review diff, revert,
// cancellation, and task history after an app restart.
import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

const MAIN = "= Notes\nThe cat sat on the mat.\n";
const FAKE_KEY = "sk-ant-e2e-not-a-real-key-7f3a9c0d1e2b";

const invoke = (page, cmd, args) =>
  page.evaluate(([c, a]) => window.__TAURI_INTERNALS__.invoke(c, a), [cmd, args ?? {}]);

async function mountPanel(page) {
  await page.waitForFunction(() => Boolean(window.__expositionAgentsDev), null, { timeout: 15_000 });
  await page.evaluate(() => window.__expositionAgentsDev.mountPanel());
  await page.waitForSelector("[data-testid=agents-panel]");
}

async function documentText(page, path) {
  const docs = await invoke(page, "document_list");
  const doc = docs.find((d) => d.path === path);
  if (!doc) return null;
  return invoke(page, "document_state", { document: doc.id });
}

async function waitForStatus(page, label, timeout = 20_000) {
  await page.waitForFunction(
    (l) => document.querySelector("[data-testid=agents-task-status]")?.textContent === l,
    label,
    { timeout },
  );
}

async function startTask(page, { prompt, folder, permission }) {
  await page.click("[data-testid=agents-new-task]");
  await page.fill("[data-testid=agents-prompt]", prompt);
  await page.fill("[data-testid=agents-folder]", folder);
  await page.selectOption("[data-testid=agents-permission]", permission);
  await page.click("[data-testid=agents-start]");
  await page.waitForSelector("[data-testid=agents-task-view]");
}

export default async function agentsHarness({ launchApp, artifactsDir, assert }) {
  const project = await mkdtemp(join(tmpdir(), "exposition-agents-project-"));
  await writeFile(join(project, "main.typ"), MAIN);
  const namespace = `e2e-${Date.now()}`;
  const env = { EXPOSITION_CREDENTIAL_NAMESPACE: namespace };
  const evidence = { project, namespace, steps: [] };
  const step = (name, data = {}) => evidence.steps.push({ name, ...data });

  let app = await launchApp({ env });
  const dataDir = app.dataDir;
  try {
    const { page } = app;
    await mountPanel(page);

    // --- Credentials: configured / not configured, the key is never revealed.
    await page.click("[data-testid=agents-open-settings]");
    await page.waitForSelector("[data-testid=agents-settings][open]");
    const keyStatus = () => page.textContent("[data-testid=agents-key-status]");
    assert.match(await keyStatus(), /Not configured/);
    await page.fill("[data-testid=agents-key-input]", FAKE_KEY);
    await page.click("[data-testid=agents-key-save]");
    await page.waitForFunction(
      () => /Configured/.test(document.querySelector("[data-testid=agents-key-status]")?.textContent ?? "") &&
        !/Not configured/.test(document.querySelector("[data-testid=agents-key-status]")?.textContent ?? ""),
    );
    const leaks = await page.evaluate((key) => {
      const inputs = [...document.querySelectorAll("input, textarea")].some((el) => el.value.includes(key));
      return { html: document.documentElement.outerHTML.includes(key), inputs };
    }, FAKE_KEY);
    const status = await invoke(page, "agents_provider_status");
    assert.equal(status.configured, true);
    assert.ok(!JSON.stringify(status).includes(FAKE_KEY), "provider status must not contain the key");
    assert.deepEqual(leaks, { html: false, inputs: false }, "the key must not appear in the webview after saving");
    await page.screenshot({ path: join(artifactsDir, "01-key-configured.png") });
    step("key saved; webview shows Configured only", { credentialStore: status.credentialStore });
    await page.click("[data-testid=agents-key-remove]");
    await page.waitForFunction(() =>
      /Not configured/.test(document.querySelector("[data-testid=agents-key-status]")?.textContent ?? ""),
    );
    step("key removed; Not configured");

    // Select the development-only scripted provider.
    await page.selectOption("[data-testid=agents-provider-select]", "scripted");
    await page.click("[data-testid=agents-settings-save]");
    await page.waitForSelector("[data-testid=agents-settings-message]");
    await page.click("[data-testid=agents-settings-close]");
    assert.equal(
      await page.textContent("[data-testid=agents-provider-chip]"),
      "Scripted provider — development only",
    );

    // --- Task A (read-only) opens main.typ in the document service.
    await invoke(page, "agents_dev_load_script", {
      script: {
        turns: [
          { steps: [{ toolUse: { id: "toolu_a", name: "read_file", input: { path: "main.typ" } } }] },
          { steps: [{ text: "main.typ has a title and one sentence." }] },
        ],
      },
    });
    await startTask(page, { prompt: "Summarize main.typ", folder: project, permission: "readOnly" });
    await waitForStatus(page, "Completed");
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
    await startTask(page, { prompt: "Make the cat sentence more precise", folder: project, permission: "reviewEdits" });
    // The transcript streams: the draft grows while the turn is generated.
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
    await page.screenshot({ path: join(artifactsDir, "02-approval.png") });
    state = await documentText(page, "main.typ");
    await userEdit(state.info.revision, state.text.length, "// user keeps typing\n");
    await page.click("[data-testid=agents-approve]");
    await waitForStatus(page, "Completed");
    state = await documentText(page, "main.typ");
    assert.equal(state.text, "// draft\n= Notes\nThe cat sat quietly on the mat.\n// user keeps typing\n");
    const tasks = await invoke(page, "agents_task_list");
    const taskB = tasks[0];
    const detailB = await invoke(page, "agents_task_detail", { task: taskB.id });
    const editCall = detailB.transcript.find((i) => i.kind === "toolCall" && i.name === "edit_document");
    assert.equal(editCall.status, "succeeded");
    assert.ok(JSON.parse(editCall.result).rebased, "the agent edit was rebased over the concurrent user edit");
    await page.screenshot({ path: join(artifactsDir, "03-task-completed.png") });
    step("task B edited the open document through a transaction (rebased over a concurrent user edit)", {
      text: state.text,
      revision: state.info.revision,
    });

    // --- Review: Monaco diff of the task's change, then revert.
    await page.click("[data-testid=agents-open-review]");
    await page.waitForSelector("[data-testid=agents-review-file][data-path='main.typ']");
    await page.waitForSelector("[data-testid=agents-diff] .monaco-diff-editor", { timeout: 20_000 });
    const diff = await invoke(page, "agents_review_diff", { task: taskB.id, path: "main.typ" });
    // The user's edit made during approval landed before the agent's edit, so
    // both sides include it; the diff is exactly the agent's change.
    assert.equal(diff.original, "// draft\n= Notes\nThe cat sat on the mat.\n// user keeps typing\n");
    assert.equal(diff.modified, "// draft\n= Notes\nThe cat sat quietly on the mat.\n// user keeps typing\n");
    await page.waitForTimeout(500);
    await page.screenshot({ path: join(artifactsDir, "04-review-diff.png") });
    step("review shows the Monaco diff");
    await page.click("[data-testid=agents-revert-file]");
    await page.waitForSelector("[data-testid=agents-review-results]");
    state = await documentText(page, "main.typ");
    assert.equal(state.text, "// draft\n= Notes\nThe cat sat on the mat.\n// user keeps typing\n");
    await page.screenshot({ path: join(artifactsDir, "05-reverted.png") });
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
    await startTask(page, { prompt: "A long task to cancel", folder: project, permission: "readOnly" });
    await page.waitForFunction(() => (document.querySelector("[data-testid=agents-draft]")?.textContent ?? "").includes("canceled"));
    const canceledAt = Date.now();
    await page.click("[data-testid=agents-cancel]");
    await waitForStatus(page, "Canceled", 10_000);
    const cancelMs = Date.now() - canceledAt;
    assert.ok(cancelMs < 5000, `cancel took ${cancelMs} ms`);
    await page.screenshot({ path: join(artifactsDir, "06-canceled.png") });
    step("canceled mid-stream", { cancelMs });
  } finally {
    await invoke(app.page, "agents_provider_clear_key").catch(() => {});
    await app.close();
  }

  // --- Restart with the same data directory: history is still there.
  app = await launchApp({ env, dataDir });
  try {
    const { page } = app;
    await mountPanel(page);
    await page.waitForSelector("[data-testid=agents-task-item]");
    const items = await page.$$eval("[data-testid=agents-task-item]", (els) => els.map((e) => e.textContent));
    assert.equal(items.length, 3, `three tasks persisted: ${items}`);
    assert.match(items.join("|"), /Completed/);
    assert.match(items.join("|"), /Canceled/);
    const tasks = await invoke(page, "agents_task_list");
    const reviewed = tasks.find((t) => t.title.startsWith("Make the cat"));
    const review = await invoke(page, "agents_review", { task: reviewed.id });
    assert.equal(review.files[0].state, "reverted");
    await page.screenshot({ path: join(artifactsDir, "07-after-restart.png") });
    step("tasks and review state persisted across restart", { tasks: tasks.map((t) => [t.title, t.status.kind]) });
    const status = await invoke(page, "agents_provider_status");
    assert.equal(status.configured, false);
  } finally {
    await app.close();
  }
  await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2));
}
