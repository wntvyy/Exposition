// What the built-in agent is told about approvals and saved state, in the real
// app (debug build) with the scripted provider — development only. The
// scripted provider replaces only the model; the task loop, gate, approvals,
// project permissions, document service, execution service, and UI are real.
// It shows what a model receives, not how a live model words its replies.
//
// The situation of the 2026-11-15 walkthrough: Auto with Files: edit and
// Code: off, a script the agent creates, runs, edits, and runs again.
//   1. create_file writes the script to disk; the first run_command asks and
//      is approved "for this task" (one card).
//   2. The agent edits the script (editor text only) and runs it again: no
//      card; the program ran the SAVED file, and the result says so.
//   3. The user saves; a follow-up run sees the new text. The context update
//      sent before the follow-up states the allowance and each file's state.
//   4. The user revokes the allowance; the next run asks again and is
//      declined (recovery: nothing ran, the result carries no authorization).
// Evidence: test-results/e2e/agentctx-narration/evidence.json (+ screenshots):
// every task-context block and tool result exactly as sent to the provider.
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";

const invoke = (page, cmd, args) =>
  page.evaluate(([c, a]) => window.__TAURI_INTERNALS__.invoke(c, a), [cmd, args ?? {}]);

const tool = (id, name, input) => ({ toolUse: { id, name, input } });
const done = { steps: [{ text: "done" }] };
const run = (id) => tool(id, "run_command", { program: "python", args: ["scripts/check.py"], working_directory: "" });

async function waitDone(page, id, timeout = 240_000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    const task = (await invoke(page, "agents_task_list")).find((t) => t.id === id);
    if (task && !["queued", "running", "awaitingApproval", "awaitingAnswer"].includes(task.status.kind)) return task;
    await page.waitForTimeout(300);
  }
  throw new Error(`task ${id} did not finish`);
}

/** Every message of the task exactly as the provider receives it. */
function sentMessages(dataDir, task) {
  const db = new DatabaseSync(join(dataDir, "agents.sqlite3"), { readOnly: true });
  try {
    return db
      .prepare("SELECT role, content FROM transcript WHERE task_id = ? AND kind = 'message' ORDER BY seq")
      .all(task)
      .map((row) => ({ role: row.role, content: JSON.parse(row.content) }));
  } finally {
    db.close();
  }
}

const contextBlocks = (messages) =>
  messages
    .filter((m) => m.role === "user")
    .flatMap((m) => m.content)
    .filter((b) => b.type === "text" && b.text.startsWith("<task_context>"))
    .map((b) => b.text);

function resultOf(messages, id) {
  const block = messages.flatMap((m) => m.content).find((b) => b.type === "tool_result" && b.tool_use_id === id);
  if (!block) throw new Error(`no result for ${id}`);
  return { ...JSON.parse(block.content), isError: block.is_error === true };
}

export default async function agentctxNarration({ launchApp, artifactsDir, assert }) {
  const project = await mkdtemp(join(tmpdir(), "exposition-agentctx-"));
  await writeFile(join(project, "main.typ"), "= Notes\n\nOdd numbers sum to squares.\n");
  await mkdir(join(project, "scripts"), { recursive: true });
  const env = { EXPOSITION_CREDENTIAL_NAMESPACE: `e2e-agentctx-${Date.now()}` };
  const evidence = { project };
  const app = await launchApp({ env });
  try {
    const { page } = app;
    const info = await invoke(page, "app_info");
    assert.ok(info.debugBuild, "agentctx-narration needs a debug build (scripted provider)");
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.getByTestId("agents-panel").waitFor({ timeout: 15_000 });
    await invoke(page, "agents_provider_update", {
      settings: { provider: "scripted", model: "claude-opus-5-5", effort: "medium", consent: "scopeContent" },
    });
    const opened = await invoke(page, "workspace_open", { path: project });
    const root = opened.root;
    // Trust and the project's permissions are the user's choices (as in the walkthrough).
    await invoke(page, "trust_grant", { root: project });
    await invoke(page, "agents_permissions_set", { root, choice: { files: "edit", code: "off", setup: false } });

    await invoke(page, "agents_dev_load_script", {
      script: {
        turns: [
          { steps: [tool("n1", "create_file", { path: "scripts/check.py", content: "print('v1')\n" }), run("c1")] },
          { steps: [tool("r1", "read_file", { path: "scripts/check.py" })] },
          {
            steps: [
              tool("e1", "edit_document", {
                path: "scripts/check.py",
                base_revision: "$ref:r1/revision",
                edits: [{ old_text: "v1", new_text: "v2" }],
              }),
              run("c2"),
            ],
          },
          done,
          { steps: [run("c3")] },
          done,
          { steps: [run("c4")] },
          done,
        ],
      },
    });

    // --- 1 and 2: one card for the whole run.
    const panel = page.getByTestId("agents-panel");
    await panel.getByTestId("agents-new-task").click();
    await page.fill("[data-testid=agents-prompt]", "Write a script and run it");
    await panel.getByTestId("agents-folder").waitFor();
    // Approvals (Ask | Auto) is in the composer's Options sheet, Permissions tab.
    const sheet = page.getByRole("dialog", { name: "Options" });
    const permissionsTab = async () => {
      if ((await panel.getByTestId("agents-options").getAttribute("aria-expanded")) !== "true") {
        await panel.getByTestId("agents-options").click();
      }
      await sheet.getByRole("tab", { name: /^Permissions/ }).click();
      return page.getByTestId("agents-options-permissions");
    };
    const closeSheet = async () => {
      await page.keyboard.press("Escape");
      await sheet.waitFor({ state: "detached" });
    };
    await (await permissionsTab()).getByRole("radio", { name: "Auto" }).click();
    await page.waitForFunction(
      () => document.querySelector("[data-testid=agents-auto-profile]")?.dataset.granted === "true",
    );
    await closeSheet();
    await panel.getByTestId("agents-start").click();
    await page.waitForSelector("[data-testid=agents-task-view]");
    let task = (await invoke(page, "agents_task_list"))[0];
    await page.waitForSelector("[data-testid=agents-approval]", { timeout: 60_000 });
    await page.screenshot({ path: join(artifactsDir, "agentctx-first-run-asks.png") });
    await page.getByTestId("agents-approve-task").click();
    task = await waitDone(page, task.id);
    assert.equal(task.status.kind, "completed", JSON.stringify(task.status));
    assert.equal(await page.getByTestId("agents-approval").count(), 0, "no second card");
    // "Allowed for this task" with Revoke is under Options › Permissions; the tab counts it.
    const permissions = await permissionsTab();
    assert.match(await sheet.getByRole("tab", { name: /^Permissions/ }).innerText(), /1 allowed/);
    await permissions.getByTestId("agents-allowances").waitFor();
    assert.match(await permissions.getByTestId("agents-allowances").innerText(), /Allowed for this task:/);
    await page.screenshot({ path: join(artifactsDir, "agentctx-allowed-for-this-task.png") });
    await closeSheet();

    let messages = sentMessages(app.dataDir, task.id);
    let blocks = contextBlocks(messages);
    assert.equal(blocks.length, 1);
    evidence.firstContext = blocks[0];
    assert.match(blocks[0], /Approval mode: Auto \(the project's permissions: Files edit · Code off · Setup off/);
    assert.match(blocks[0], /Approved by the user for the rest of this task: nothing\n/);
    assert.match(blocks[0], /- File, graph, and workflow-draft changes: proceeds without a prompt \(the project's permissions\)\n/);
    assert.match(blocks[0], /- run_python, run_command: asks the user first \(they can approve once or for the rest of the task\)\n/);

    const n1 = resultOf(messages, "n1");
    const c1 = resultOf(messages, "c1");
    const e1 = resultOf(messages, "e1");
    const c2 = resultOf(messages, "c2");
    evidence.results = { n1, c1, e1, c2 };
    // create_file: on disk, saved; covered by the project's permissions.
    assert.equal(n1.unsavedChanges, false);
    assert.match(n1.note, /Written to disk \(saved\)/);
    assert.equal(n1.authorization, "No prompt: the project's permissions cover it (Auto).");
    assert.equal(await readFile(join(project, "scripts", "check.py"), "utf8"), "print('v1')\n");
    // First run: the one prompt, approved for the task.
    assert.match(c1.stdout, /v1/);
    assert.match(c1.authorization, /^The user approved this call in a prompt, for the rest of this task/);
    assert.equal(c1.unsavedNotSeen, undefined, "nothing was unsaved at the first run");
    // The edit is in the editor only; the disk keeps the saved text.
    assert.equal(e1.unsavedChanges, true);
    assert.match(e1.note, /not saved to disk/);
    assert.equal(await readFile(join(project, "scripts", "check.py"), "utf8"), "print('v1')\n");
    // Second run: no prompt (the allowance), and it ran the saved file — the result says so.
    assert.match(c2.authorization, /^No prompt: covered by what the user approved for the rest of this task \(native code/);
    assert.match(c2.stdout, /v1/, "the program ran the saved file, not the unsaved edit");
    assert.doesNotMatch(c2.stdout, /v2/);
    assert.deepEqual(c2.unsavedNotSeen, ["scripts/check.py"]);
    assert.match(c2.filesNote, /read files as saved on disk/);
    // The user sees the same result text in the transcript.
    const detail = await invoke(page, "agents_task_detail", { task: task.id });
    const shown = detail.transcript.find((i) => i.kind === "toolCall" && i.toolUseId === "c2");
    assert.match(shown.result, /No prompt: covered by what the user approved/);
    assert.deepEqual(shown.authorizedBy, { kind: "taskAllowance", kinds: ["nativeCode"] });

    // --- 3: the user saves; the follow-up run sees the edit, and the update says where things are.
    const before = contextBlocks(sentMessages(app.dataDir, task.id)).length;
    const documents = await invoke(page, "document_list");
    const script = documents.find((d) => d.path === "scripts/check.py");
    assert.ok(script && script.savedRevision !== script.revision, "the edit is unsaved in the editor");
    await invoke(page, "document_save", { document: script.id, overwrite: false });
    assert.equal(await readFile(join(project, "scripts", "check.py"), "utf8"), "print('v2')\n");
    await invoke(page, "agents_task_continue", { task: task.id, prompt: "Run it again", budgets: null });
    task = await waitDone(page, task.id);
    assert.equal(await page.getByTestId("agents-approval").count(), 0, "no card: the allowance still covers it");
    messages = sentMessages(app.dataDir, task.id);
    blocks = contextBlocks(messages);
    assert.equal(blocks.length, before + 1, "one context update per follow-up");
    evidence.updateAfterSave = blocks.at(-1);
    assert.match(blocks.at(-1), /Update before this follow-up/);
    assert.match(blocks.at(-1), /Approved by the user for the rest of this task: native code/);
    assert.match(
      blocks.at(-1),
      /- run_python, run_command: runs natively without a prompt \(approved by the user for the rest of this task\)\n/,
    );
    assert.match(
      blocks.at(-1),
      /- scripts\/check\.py \(created by you\): saved \(the editor and the file on disk are the same\); not reviewed by the user yet\n/,
    );
    assert.match(blocks.at(-1), /Open documents with unsaved changes: none\n/);
    const c3 = resultOf(messages, "c3");
    evidence.results.c3 = c3;
    assert.match(c3.stdout, /v2/, "after the save the program sees the edit");
    assert.equal(c3.unsavedNotSeen, undefined);
    assert.match(c3.authorization, /^No prompt: covered by what the user approved/);

    // --- 4: revoked allowance → the next run asks again; declined → nothing ran.
    await (await permissionsTab()).getByTestId("agents-allowance-revoke").first().click();
    await page.waitForFunction(() => document.querySelectorAll("[data-testid=agents-allowance]").length === 0);
    await closeSheet();
    await invoke(page, "agents_task_continue", { task: task.id, prompt: "And once more", budgets: null });
    await page.waitForSelector("[data-testid=agents-approval]", { timeout: 60_000 });
    await page.screenshot({ path: join(artifactsDir, "agentctx-asks-again-after-revoke.png") });
    await page.getByTestId("agents-decline").click();
    task = await waitDone(page, task.id);
    messages = sentMessages(app.dataDir, task.id);
    blocks = contextBlocks(messages);
    evidence.updateAfterRevoke = blocks.at(-1);
    assert.match(blocks.at(-1), /Approved by the user for the rest of this task: nothing\n/);
    assert.match(blocks.at(-1), /- run_python, run_command: asks the user first \(they can approve once or for the rest of the task\)\n/);
    const c4 = resultOf(messages, "c4");
    evidence.results.c4 = c4;
    assert.ok(c4.isError, "the declined call is an error result");
    assert.equal(c4.authorization, undefined, "a declined call was never authorized");
    assert.equal(c4.stdout, undefined, "nothing ran");

    // No context block is shown to the user as their own words.
    const final = await invoke(page, "agents_task_detail", { task: task.id });
    assert.ok(
      final.transcript.every((i) => i.kind !== "userText" || !i.text.includes("task_context")),
      "context blocks stay out of the transcript",
    );
    const errors = app.consoleMessages.filter((m) => m.startsWith("error:") || m.startsWith("pageerror:"));
    assert.deepEqual(errors, [], "no console errors");
  } finally {
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2));
    await app.close();
  }
}
