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
import { existsSync } from "node:fs";
import { copyFile, cp, mkdir, mkdtemp, readdir, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { runCommand } from "../palette.mjs";

const MAIN = "= Notes\nThe cat sat on the mat.\n";
const FAKE_KEY = "sk-ant-e2e-not-a-real-key-7f3a9c0d1e2b";

const invoke = (page, cmd, args) =>
  page.evaluate(([c, a]) => window.__TAURI_INTERNALS__.invoke(c, a), [cmd, args ?? {}]);

function consoleErrors(app) {
  return app.consoleMessages.filter((m) => m.startsWith("error:") || m.startsWith("pageerror:"));
}

async function setTheme(page, theme) {
  await runCommand(page, `color theme ${theme}`, theme === "dark" ? /Color Theme: Dark/ : /Color Theme: Light/);
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

async function startTask(page, { prompt, permission }) {
  const panel = page.getByTestId("agents-panel");
  await panel.getByTestId("agents-new-task").click();
  await page.fill("[data-testid=agents-prompt]", prompt);
  // The task acts on the open project folder.
  await panel.getByTestId("agents-folder").waitFor();
  await selectOption(page, panel, /Document changes/, permission);
  await panel.getByRole("button", { name: "Start task" }).click();
  await page.waitForSelector("[data-testid=agents-task-view]");
}

const LEAN_TOOLCHAIN_DIR = "leanprover--lean4---v4.35.0";

export default async function agentsHarness({ launchApp, artifactsDir, assert, fixture }) {
  let leanRan = false;
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
  // Only debug builds honor EXPOSITION_CREDENTIAL_NAMESPACE; a release build
  // always uses the real credential entry. This scenario saves and clears a
  // test key, so it must never run against a release/installed build, where
  // it would overwrite or delete the user's actual API key.
  const info = await app.page.evaluate(() => window.__TAURI_INTERNALS__.invoke("app_info"));
  if (!info.debugBuild) {
    await app.close();
    throw new Error("agents-harness requires a debug build (it would modify the real credential entry of a release build)");
  }
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
    // Removing the key asks first; the safe choice (keep) has focus.
    await dialog.getByRole("button", { name: "Remove", exact: true }).click();
    const confirmRemove = page.getByRole("alertdialog", { name: "Remove the API key?" });
    await confirmRemove.waitFor();
    await page.waitForFunction(() => document.activeElement?.textContent === "Keep key");
    await confirmRemove.getByRole("button", { name: "Remove key" }).click();
    await page.waitForFunction(() =>
      (document.querySelector("[data-testid=agents-key-status]")?.textContent ?? "").includes("Not configured"),
    );
    step("key removed after confirmation (Keep focused by default); Not configured");

    // Open the project folder through the workspace service (as File ▸ Open Folder does).
    const opened = await invoke(page, "workspace_open", { path: project });
    step("project folder opened in the workspace", { root: opened.root });

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
    await startTask(page, { prompt: "Summarize main.typ", permission: "Read only" });
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
    await startTask(page, { prompt: "Make the cat sentence more precise", permission: "Ask before each change" });
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
    // While the edit waits for approval, every surface says so (nothing says "Running").
    await page.waitForFunction(() => {
      const panelEl = document.querySelector("[data-testid=agents-panel]");
      const task = panelEl?.querySelector("[data-testid=agents-task-status]")?.textContent;
      const draft = panelEl?.querySelector("[data-testid=agents-draft] .ui-status-pill")?.textContent;
      const tool = panelEl?.querySelector("[data-testid=agents-tool-call][data-tool=edit_document] summary .ui-status-pill")?.textContent;
      const bar = document.querySelector("[data-status-item='agents.status']")?.textContent;
      return (
        task === "Waiting for approval" &&
        draft === "Waiting for approval: edit_document" &&
        tool === "Waiting for approval" &&
        bar === "Agent: waiting for approval"
      );
    }, null, { timeout: 10_000 });
    step("approval pending: task, draft, tool call, and status bar all read 'Waiting for approval'");
    await page.waitForTimeout(200);
    await shot(page, "agents-approval-light");
    await setTheme(page, "dark");
    await shot(page, "agents-approval-dark");
    await setTheme(page, "light");
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

    // --- Real research run (agent-owned session) and real Typst build.
    // Trusting the folder is the user's explicit action (the trust dialog);
    // native Python additionally needs approval for each run.
    await invoke(page, "trust_grant", { root: project });
    const plot =
      "import matplotlib.pyplot as plt\nplt.plot([0, 1, 2, 3], [0, 1, 4, 9])\nplt.title('squares')\nplt.show()\nprint(6 * 7)";
    await invoke(page, "agents_dev_load_script", {
      script: {
        turns: [
          { steps: [{ text: "Computing the squares." }, { toolUse: { id: "py", name: "run_python", input: { code: plot } } }] },
          { steps: [{ text: "The run printed 42 and produced a figure." }] },
        ],
      },
    });
    await panel.getByTestId("agents-new-task").click();
    await page.fill("[data-testid=agents-prompt]", "Plot the squares");
    await selectOption(page, panel, /Commands and code/, "Ask before each command (runs natively)");
    await panel.getByRole("button", { name: "Start task" }).click();
    await page.waitForSelector("[data-testid=agents-approval]", { timeout: 30_000 });
    await shot(page, "agents-python-approval-light");
    await panel.getByRole("button", { name: "Approve" }).click();
    await waitForStatus(page, "completed", 180_000);
    const pyTask = (await invoke(page, "agents_task_list"))[0];
    const pyDetail = await invoke(page, "agents_task_detail", { task: pyTask.id });
    const pyCall = pyDetail.transcript.find((i) => i.kind === "toolCall" && i.name === "run_python");
    assert.equal(pyCall.status, "succeeded", pyCall.result);
    const pyResult = JSON.parse(pyCall.result);
    assert.equal(pyResult.stdout.trim(), "42");
    const figure = pyResult.artifacts.find((a) => a.kind === "figure");
    assert.ok(figure, `figure artifact: ${pyCall.result}`);
    const manifest = await invoke(page, "research_run", { runId: pyResult.run });
    assert.deepEqual(manifest.session.owner, { kind: "agent", taskId: pyTask.id }, "the run is owned by the agent task");
    step("run_python ran in an agent-owned research session", { run: pyResult.run, figure: figure.path });

    await invoke(page, "agents_dev_load_script", {
      script: {
        turns: [
          {
            steps: [
              {
                toolUse: {
                  id: "ins",
                  name: "insert_artifact",
                  input: {
                    path: "main.typ",
                    run_id: pyResult.run,
                    artifact_id: figure.id,
                    after_text: "",
                    caption: "Squares of small integers",
                    update_key: "",
                  },
                },
              },
            ],
          },
          { steps: [{ toolUse: { id: "bld", name: "build_document", input: { entry: "main.typ", engine: "auto" } } }] },
          { steps: [{ text: "Inserted the figure; main.typ builds." }] },
        ],
      },
    });
    await startTask(page, { prompt: "Insert the figure and build", permission: "Apply changes, review afterwards" });
    await waitForStatus(page, "completed", 60_000);
    const figTask = (await invoke(page, "agents_task_list"))[0];
    const figDetail = await invoke(page, "agents_task_detail", { task: figTask.id });
    // The agent's insertion is recorded in the project graph like a reviewed
    // promotion: the artifact retained, `main.typ usesAsset artifact`.
    const insertCall = figDetail.transcript.find((i) => i.kind === "toolCall" && i.name === "insert_artifact");
    const attached = JSON.parse(insertCall.result).graph?.attachment;
    assert.equal(attached?.state, "recorded", insertCall.result);
    const artifactEntity = await invoke(page, "graph_entity", { entity: attached.artifact });
    assert.ok(
      artifactEntity.incoming.some((r) => r.kind === "usesAsset" && r.from === attached.manuscript),
      "main.typ usesAsset the inserted artifact",
    );
    assert.ok(artifactEntity.evidence.some((e) => e.kind === "computational"), "computational evidence");
    const buildCall = figDetail.transcript.find((i) => i.kind === "toolCall" && i.name === "build_document");
    assert.equal(buildCall.status, "succeeded", buildCall.result);
    const buildResult = JSON.parse(buildCall.result);
    assert.equal(buildResult.status, "succeeded");
    assert.ok(buildResult.pages >= 1);
    state = await documentText(page, "main.typ");
    assert.match(state.text, /exposition:begin fig-/);
    assert.match(state.text, /Squares of small integers/);
    await shot(page, "agents-figure-built-light");
    step("insert_artifact attached the figure; build_document built main.typ", {
      build: buildResult.build,
      pages: buildResult.pages,
    });

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
    await startTask(page, { prompt: "A long task to cancel", permission: "Read only" });
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
    // --- Real Lean check (lean-basic fixture) in a second project.
    const elanHome = process.env.ELAN_HOME || join(homedir(), ".elan");
    if (existsSync(join(elanHome, "toolchains", LEAN_TOOLCHAIN_DIR, "bin"))) {
      const leanRoot = join(await mkdtemp(join(tmpdir(), "exposition-agents-lean-")), "lean-basic");
      await cp(fixture("lean-basic"), leanRoot, {
        recursive: true,
        filter: (src) => !/[\\/]\.lake([\\/]|$)/.test(src) && !src.endsWith("lake-manifest.json"),
      });
      await invoke(page, "workspace_open", { path: leanRoot });
      await invoke(page, "trust_grant", { root: leanRoot });
      await invoke(page, "agents_dev_load_script", {
        script: {
          turns: [
            {
              steps: [
                { text: "Checking both Lean files." },
                { toolUse: { id: "defs", name: "check_lean", input: { path: "LeanBasic/Defs.lean" } } },
                { toolUse: { id: "proofs", name: "check_lean", input: { path: "LeanBasic/Proofs.lean" } } },
              ],
            },
            { steps: [{ text: "Defs.lean is checked. Proofs.lean is not: it has an error and a sorry." }] },
          ],
        },
      });
      await panel.getByTestId("agents-new-task").click();
      await page.fill("[data-testid=agents-prompt]", "Check the Lean proofs");
      await selectOption(page, panel, /Commands and code/, "Ask before each command (runs natively)");
      await panel.getByRole("button", { name: "Start task" }).click();
      for (let i = 0; i < 2; i++) {
        await page.waitForSelector("[data-testid=agents-approval]", { timeout: 60_000 });
        await panel.getByRole("button", { name: "Approve" }).click();
        await page.waitForSelector("[data-testid=agents-approval]", { state: "detached", timeout: 10_000 });
      }
      await waitForStatus(page, "completed", 600_000);
      const leanTask = (await invoke(page, "agents_task_list"))[0];
      const leanDetail = await invoke(page, "agents_task_detail", { task: leanTask.id });
      const checks = leanDetail.transcript.filter((i) => i.kind === "toolCall" && i.name === "check_lean");
      const defs = JSON.parse(checks[0].result);
      const proofs = JSON.parse(checks[1].result);
      assert.equal(checks[0].status, "succeeded", checks[0].result);
      assert.equal(defs.checked, true);
      assert.equal(checks[1].status, "failed", "an unchecked file is a failed tool result");
      assert.equal(proofs.checked, false);
      assert.ok(proofs.sorries.length >= 1, checks[1].result);
      assert.ok(proofs.diagnostics.some((d) => d.severity === "error"), checks[1].result);
      await shot(page, "agents-lean-check-light");
      step("check_lean: Defs.lean checked, Proofs.lean not checked (error + sorry)", {
        defs: { checked: defs.checked, revision: defs.revision, toolchain: defs.toolchain },
        proofs: { checked: proofs.checked, sorries: proofs.sorries.length, errors: proofs.diagnostics.length },
      });
      leanRan = true;
    } else {
      console.log(`SKIPPED Lean step: toolchain ${LEAN_TOOLCHAIN_DIR} is not installed through elan`);
    }

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
    assert.equal(items.length, leanRan ? 6 : 5, `all tasks persisted: ${items}`);
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
