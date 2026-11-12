// Agent attention and review honesty in the real app (debug build, scripted
// provider — development only; no model-provider call is made).
//
// Covers:
// - A pending approval while the Agent view is hidden: the status bar item asks
//   for attention and opens the approval (keyboard), a persistent notification
//   with an "Open approval" action appears and is retired once answered.
// - The Monaco codicon font is bundled locally and the diff gutter glyphs use it.
// - Review follows content: the user's Ctrl+Z in the editor turns an agent
//   change into "Undone in editor" (nothing to revert); Ctrl+Y makes it pending.
// - A response cut off at the output limit stops the task honestly ("Stopped").
// - Removing the stored API key asks first, with the safe choice focused.
// - The live-agents scope check accepts a real application refusal and rejects
//   a model that merely declines.
// Screenshots (light and dark) go to test-results/e2e/agents-attention/.
import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { scopeRefusalEvidence } from "../agent-scope-check.mjs";
import { editorLocator, invoke, openInEditor, openWorkspace, waitFor } from "../workspace-helpers.mjs";

const MAIN = "= Notes\nThe cat sat on the mat.\n";
const FAKE_KEY = "sk-ant-e2e-attention-not-a-real-key-5b8e";

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

const tool = (id, name, input) => ({ toolUse: { id, name, input } });

function request(project, overrides = {}) {
  return {
    prompt: "Attention task",
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

async function waitStatus(page, id, kinds, timeout = 30_000) {
  return waitFor(
    async () => {
      const d = await invoke(page, "agents_task_detail", { task: id });
      return kinds.includes(d.summary.status.kind) ? d : null;
    },
    { timeout, interval: 150, message: `task ${id} status in ${kinds}` },
  );
}

/** From any Agent view page, open the newest task. */
async function showTask(panel) {
  const back = async (name) => {
    const button = panel.getByRole("button", { name, exact: true });
    if (await button.count()) await button.click();
  };
  await back("Back to task");
  await back("Back to tasks");
  await panel.getByTestId("agents-task-item").first().click();
  await panel.getByTestId("agents-task-view").waitFor({ timeout: 10_000 });
}

const approvalNotices = (page) =>
  page.$$eval("[data-testid=notification]", (els) =>
    els.map((e) => e.textContent ?? "").filter((t) => /approval/i.test(t)),
  );

export default async function agentsAttention({ launchApp, artifactsDir, assert }) {
  const project = await mkdtemp(join(tmpdir(), "exposition-attention-"));
  await writeFile(join(project, "main.typ"), MAIN);
  await writeFile(join(project, "secret.txt"), "OUT-OF-SCOPE-CONTENT-77\n");
  const env = { EXPOSITION_CREDENTIAL_NAMESPACE: `e2e-attention-${Date.now()}` };
  const app = await launchApp({ env });
  const info = await invoke(app.page, "app_info");
  if (!info.debugBuild) {
    // The scripted provider and the credential namespace exist only in debug
    // builds; never touch a release build's real credential entry.
    await app.close();
    console.log("SKIPPED: agents-attention needs a debug build");
    return;
  }
  const evidence = { project, steps: [] };
  const step = (name, data = {}) => evidence.steps.push({ name, ...data });
  const shot = (page, name) => page.screenshot({ path: join(artifactsDir, `${name}.png`) });
  try {
    const { page } = app;
    await page.setViewportSize({ width: 1440, height: 900 });
    const panel = page.getByTestId("agents-panel");
    await panel.waitFor({ timeout: 15_000 });
    await setTheme(page, "light");
    await openWorkspace(page, project);
    await invoke(page, "agents_provider_update", {
      settings: { provider: "scripted", model: "claude-opus-5-5", effort: "medium", consent: "scopeContent" },
    });

    // --- 1. Approval while the Agent view is hidden ---------------------------
    await page.locator("body").click({ position: { x: 700, y: 400 } });
    await page.keyboard.press("Control+Alt+B");
    await panel.waitFor({ state: "detached", timeout: 5000 }).catch(() => {});
    assert.ok(!(await panel.isVisible().catch(() => false)), "the Agent view is hidden");
    const created = await startTask(
      page,
      {
        turns: [
          { steps: [tool("c", "create_file", { path: "notes/new.typ", content: "= Draft\nAlpha\n" })] },
          { steps: [tool("r", "read_file", { path: "notes/new.typ" })] },
          {
            steps: [
              tool("e", "edit_document", {
                path: "notes/new.typ",
                base_revision: "$ref:r/revision",
                edits: [{ old_text: "Alpha", new_text: "Beta" }],
              }),
            ],
          },
          { steps: [{ text: "Created notes/new.typ and refined it." }] },
        ],
      },
      request(project, { permission: "reviewEdits", prompt: "Draft a note file" }),
    );
    await waitStatus(page, created.id, ["awaitingApproval"]);
    const statusButton = page.getByTestId("agents-status-approval");
    await statusButton.waitFor({ timeout: 5000 });
    assert.equal((await statusButton.textContent())?.trim(), "Agent: approval needed");
    assert.equal(await statusButton.locator("[data-status]").getAttribute("data-status"), "warning");
    await waitFor(async () => (await approvalNotices(page)).length === 1, { message: "approval notification" });
    const notice = page.locator("[data-testid=notification]", { hasText: "Agent needs your approval" });
    assert.match((await notice.textContent()) ?? "", /Create notes\/new\.typ/);
    await notice.getByRole("button", { name: "Open approval" }).waitFor();
    await page.waitForTimeout(400);
    await shot(page, "approval-status-and-notification-light");
    await setTheme(page, "dark");
    await shot(page, "approval-status-and-notification-dark");
    await setTheme(page, "light");
    // Still waiting after the theme switches (the notification never times out).
    assert.equal((await approvalNotices(page)).length, 1, "the approval notification persists");
    step("panel hidden: status item 'Agent: approval needed' (warning) and a persistent notification with Open approval");

    // Keyboard: focus the status item and press Enter -> the approval card has focus.
    await statusButton.focus();
    await page.keyboard.press("Enter");
    await panel.waitFor({ state: "visible", timeout: 5000 });
    await page.waitForFunction(() => document.activeElement?.getAttribute("data-testid") === "agents-approval", null, {
      timeout: 5000,
    });
    const cardName = await page.evaluate(() => document.activeElement?.getAttribute("aria-label"));
    assert.match(cardName ?? "", /^Approval needed: /);
    await waitFor(async () => (await approvalNotices(page)).length === 0, { message: "notification retired once the card is shown" });
    await page.waitForTimeout(200);
    await shot(page, "approval-card-focused-light");
    step("Enter on the status item opened the Agent view with focus on the approval card", { cardName });
    // Tab to Approve, Enter.
    await page.keyboard.press("Tab");
    assert.equal(await page.evaluate(() => document.activeElement?.textContent), "Approve");
    await page.keyboard.press("Enter");
    // The next approval arrives while the card is on screen: no notification.
    const second = await waitFor(
      async () => (await invoke(page, "agents_task_detail", { task: created.id })).approvals[0] ?? null,
      { message: "second approval" },
    );
    await page.waitForTimeout(500);
    assert.equal((await approvalNotices(page)).length, 0, "no notification for an approval already on screen");
    await invoke(page, "agents_approval_resolve", { task: created.id, approval: second.id, approved: true });
    await waitStatus(page, created.id, ["completed"]);
    await waitFor(async () => (await page.getByTestId("agents-status-approval").count()) === 0, {
      message: "status item no longer asks for attention",
    });
    step("approved by keyboard; status item and notifications cleared");

    // --- 2. Diff gutter icons (codicon font bundled locally) ------------------
    await panel.getByTestId("agents-open-review").click();
    await page.waitForSelector("[data-testid=agents-panel] [data-testid=agents-diff] .monaco-diff-editor", { timeout: 20_000 });
    await page.waitForTimeout(600);
    const fonts = await page.evaluate(async () => {
      await document.fonts.ready;
      const glyph = document.querySelector("[data-testid=agents-diff] .codicon");
      const face = [...document.fonts].find((f) => /codicon/i.test(f.family));
      if (face && face.status !== "loaded") await face.load();
      return {
        faces: [...document.fonts].map((f) => `${f.family}:${f.status}`),
        codiconLoaded: [...document.fonts].some((f) => /codicon/i.test(f.family) && f.status === "loaded"),
        glyphClass: glyph?.className ?? null,
        glyphFont: glyph ? getComputedStyle(glyph, "::before").fontFamily : null,
        checks: document.fonts.check('16px "codicon"'),
      };
    });
    assert.ok(fonts.codiconLoaded, `codicon font loaded: ${JSON.stringify(fonts)}`);
    assert.match(fonts.glyphFont ?? "", /codicon/, `diff glyph uses codicon: ${JSON.stringify(fonts)}`);
    const diffSection = page.locator("[data-testid=agents-panel] .agents-diff-section");
    await diffSection.screenshot({ path: join(artifactsDir, "diff-gutter-icons-light.png") });
    await setTheme(page, "dark");
    await page.waitForTimeout(300);
    await diffSection.screenshot({ path: join(artifactsDir, "diff-gutter-icons-dark.png") });
    await setTheme(page, "light");
    step("codicon font loaded from the bundle; diff gutter glyph uses it", fonts);

    // Revert the created-then-edited file: deleted.
    await panel.getByTestId("agents-revert-file").first().click();
    await page.waitForSelector("[data-testid=agents-review-results]", { timeout: 10_000 });
    const revertText = (await page.textContent("[data-testid=agents-review-results]")) ?? "";
    assert.match(revertText, /Deleted the file the agent created/);
    step("revert deleted the created-then-edited file", { revertText });

    // --- 3. Review follows content: Ctrl+Z / Ctrl+Y in the editor -------------
    const mainId = await openInEditor(page, "main.typ");
    const edit = await startTask(
      page,
      {
        turns: [
          { steps: [tool("r", "read_file", { path: "main.typ" })] },
          {
            steps: [
              tool("e", "edit_document", {
                path: "main.typ",
                base_revision: "$ref:r/revision",
                edits: [{ old_text: "cat", new_text: "dog" }],
              }),
            ],
          },
          { steps: [{ text: "Changed the animal." }] },
        ],
      },
      request(project, { prompt: "Change the animal" }),
    );
    await waitStatus(page, edit.id, ["completed"]);
    const textOf = async () => (await invoke(page, "document_state", { document: mainId })).text;
    assert.equal(await textOf(), "= Notes\nThe dog sat on the mat.\n");
    await showTask(panel);
    await panel.getByTestId("agents-open-review").click();
    const stateOf = () =>
      page.locator("[data-testid=agents-panel] [data-testid=agents-review-file][data-path='main.typ'] [data-testid=agents-review-state]");
    await waitFor(async () => (await stateOf().getAttribute("data-state")) === "pending", { message: "pending review" });
    await editorLocator(page, "main.typ").locator(".monaco-editor .view-lines").click();
    await page.keyboard.press("Control+Z");
    await waitFor(async () => (await textOf()) === MAIN, { message: "Ctrl+Z removed the agent's edit" });
    await waitFor(async () => (await stateOf().getAttribute("data-state")) === "undone", {
      timeout: 5000,
      message: "review shows the change as undone",
    });
    const undoneRow = page.locator("[data-testid=agents-panel] [data-testid=agents-review-file][data-path='main.typ']");
    assert.equal(await undoneRow.getByTestId("agents-revert-file").isDisabled(), true, "nothing to revert");
    const statusAfterUndo = await page.locator("[data-status-item='agents.status']").textContent();
    await page.waitForTimeout(200);
    await shot(page, "review-undone-light");
    await setTheme(page, "dark");
    await shot(page, "review-undone-dark");
    await setTheme(page, "light");
    const revertUndone = await invoke(page, "agents_review_revert", { task: edit.id, path: "main.typ" });
    assert.ok(revertUndone[0].ok && /already undone/.test(revertUndone[0].message), JSON.stringify(revertUndone));
    assert.equal(await textOf(), MAIN);
    step("Ctrl+Z in the editor: review shows 'Undone in editor', Revert disabled, IPC revert reports nothing to do", {
      statusBar: statusAfterUndo,
    });
    await editorLocator(page, "main.typ").locator(".monaco-editor .view-lines").click();
    await page.keyboard.press("Control+Y");
    await waitFor(async () => (await textOf()) === "= Notes\nThe dog sat on the mat.\n", { message: "Ctrl+Y restored it" });
    await waitFor(async () => (await stateOf().getAttribute("data-state")) === "pending", {
      timeout: 5000,
      message: "review shows the change pending again",
    });
    const revertRedone = await invoke(page, "agents_review_revert", { task: edit.id, path: "main.typ" });
    assert.ok(revertRedone[0].ok, `revert after undo+redo: ${JSON.stringify(revertRedone)}`);
    assert.equal(await textOf(), MAIN);
    step("Ctrl+Y: pending again; Revert after undo+redo succeeds (no false overlap)", { message: revertRedone[0].message });

    // --- 4. A response cut off at the output limit stops honestly -------------
    const cut = await startTask(
      page,
      {
        turns: [
          {
            steps: [{ text: "Writing the summary" }, tool("w", "create_file", { path: "summary.typ", content: "= Summary\n" })],
            stopReason: "max_tokens",
          },
        ],
      },
      request(project, { prompt: "Write a summary" }),
    );
    const cutDetail = await waitStatus(page, cut.id, ["stopped", "completed", "failed"]);
    assert.equal(cutDetail.summary.status.kind, "stopped", JSON.stringify(cutDetail.summary.status));
    const cutCall = cutDetail.transcript.find((t) => t.kind === "toolCall");
    assert.equal(cutCall.status, "notRun");
    await showTask(panel);
    await page.waitForSelector("[data-testid=agents-task-status][data-status=stopped]", { timeout: 10_000 });
    await page.waitForTimeout(300);
    await shot(page, "task-stopped-light");
    step("max_tokens mid tool_use: Stopped (response cut off), tool call not run", { status: cutDetail.summary.status });

    // --- 5. Removing the key asks first; Keep is focused ----------------------
    await panel.getByRole("button", { name: "Agent provider settings" }).click();
    const dialog = page.getByRole("dialog", { name: "Agent provider" });
    await dialog.waitFor();
    await dialog.getByLabel("API key").fill(FAKE_KEY);
    await dialog.getByTestId("agents-key-save").click();
    await page.waitForFunction(() => {
      const text = document.querySelector("[data-testid=agents-key-status]")?.textContent ?? "";
      return text.includes("Configured") && !text.includes("Not configured");
    });
    await dialog.getByTestId("agents-key-remove").click();
    const confirmRemove = page.getByRole("alertdialog", { name: "Remove the API key?" });
    await confirmRemove.waitFor({ timeout: 5000 });
    await page.waitForFunction(() => document.activeElement?.textContent === "Keep key", null, { timeout: 5000 });
    await page.waitForTimeout(250);
    await shot(page, "confirm-remove-key-light");
    // The theme command runs from the palette, which a modal blocks; switch the
    // theme attribute directly for the dark capture of the open dialog.
    await page.evaluate(() => (document.documentElement.dataset.theme = "dark"));
    await page.waitForTimeout(250);
    await shot(page, "confirm-remove-key-dark");
    await page.evaluate(() => (document.documentElement.dataset.theme = "light"));
    await page.keyboard.press("Escape");
    await confirmRemove.waitFor({ state: "detached", timeout: 5000 });
    let status = await invoke(page, "agents_provider_status");
    assert.equal(status.configured, true, "Escape keeps the key");
    await dialog.getByTestId("agents-key-remove").click();
    await confirmRemove.getByRole("button", { name: "Remove key" }).click();
    await page.waitForFunction(() =>
      (document.querySelector("[data-testid=agents-key-status]")?.textContent ?? "").includes("Not configured"),
    );
    status = await invoke(page, "agents_provider_status");
    assert.equal(status.configured, false);
    await page.keyboard.press("Escape");
    await dialog.waitFor({ state: "detached", timeout: 5000 });
    step("Remove asks (alertdialog, Keep focused); Escape keeps the key; Remove key removes it");

    // --- 6. The live scope check accepts only the application's refusal -------
    const scoped = await startTask(
      page,
      { turns: [{ steps: [tool("s", "read_file", { path: "secret.txt" })] }, { steps: [{ text: "It was refused." }] }] },
      request(project, { scope: { kind: "document", path: "main.typ" }, permission: "readOnly", prompt: "Read the secret" }),
    );
    const scopedDetail = await waitStatus(page, scoped.id, ["completed"]);
    const marker = { path: "secret.txt", markers: ["OUT-OF-SCOPE-CONTENT-77"] };
    const real = scopeRefusalEvidence(scopedDetail, marker);
    assert.ok(real.ok, `a real application refusal passes: ${JSON.stringify(real)}`);
    const declinedOnly = {
      ...scopedDetail,
      transcript: [{ kind: "assistantText", seq: 1, text: "That file is outside my scope, so I will not access it." }],
    };
    assert.equal(scopeRefusalEvidence(declinedOnly, marker).ok, false, "a model that only declines does not pass");
    const leaky = structuredClone(scopedDetail);
    const leakyCall = leaky.transcript.find((t) => t.kind === "toolCall");
    leakyCall.status = "succeeded";
    leakyCall.isError = false;
    leakyCall.result = JSON.stringify({ path: "secret.txt", text: "OUT-OF-SCOPE-CONTENT-77\n" });
    assert.equal(scopeRefusalEvidence(leaky, marker).ok, false, "a read that returned content does not pass");
    step("live-agents check D logic: real app refusal passes; model-only decline and a leaking read fail", {
      real: real.outcome,
      result: real.calls[0]?.result,
    });

    const errors = app.consoleMessages.filter((m) => m.startsWith("error:") || m.startsWith("pageerror:"));
    assert.deepEqual(errors, [], "no console errors");
  } finally {
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2)).catch(() => {});
    await app.close();
  }
}
