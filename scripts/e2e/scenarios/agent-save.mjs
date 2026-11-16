// An agent saves a file it edited (`save_document`), in the real app (debug
// build) with the scripted provider — development only. The scripted provider
// replaces only the model; the task loop, central gate, approval cards,
// document and workspace services, execution service, and UI are real. It
// shows what the application does with a save, not how a live model uses it.
//
// One Ask task ("Ask before each change", "Ask before each command") on a
// Python script that exists on disk:
//   1. The agent edits the script, saves it, and runs it. The save has its
//      own approval card (what changes on disk, whose changes it holds);
//      approved, the file on disk is the edited one and the run prints the
//      new output.
//   2. Follow-up: edit, save, run again, and the save is declined. The file
//      on disk is unchanged, the run prints the old output, and its result
//      names the file whose unsaved text it did not see.
//   3. Follow-up: the file is changed by another program while the editor
//      still holds the unsaved edit. The agent's save is refused without a
//      card (`agents.saveConflict`); the other program's version stays.
// Evidence: test-results/e2e/agent-save/evidence.json (+ screenshots): the
// approval card of the save and every tool result as sent to the provider.
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";

const invoke = (page, cmd, args) =>
  page.evaluate(([c, a]) => window.__TAURI_INTERNALS__.invoke(c, a), [cmd, args ?? {}]);

const SCRIPT = "scripts/check.py";
const tool = (id, name, input) => ({ toolUse: { id, name, input } });
const done = { steps: [{ text: "done" }] };
const read = (id) => tool(id, "read_file", { path: SCRIPT });
const edit = (id, from, oldText, newText) =>
  tool(id, "edit_document", {
    path: SCRIPT,
    base_revision: `$ref:${from}/revision`,
    edits: [{ old_text: oldText, new_text: newText }],
  });
const save = (id, from) => tool(id, "save_document", { path: SCRIPT, revision: `$ref:${from}/revision` });
const run = (id) => tool(id, "run_command", { program: "python", args: [SCRIPT], working_directory: "" });

async function waitDone(page, id, timeout = 240_000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    const task = (await invoke(page, "agents_task_list")).find((t) => t.id === id);
    if (task && !["queued", "running", "awaitingApproval"].includes(task.status.kind)) return task;
    await page.waitForTimeout(300);
  }
  throw new Error(`task ${id} did not finish`);
}

/** Wait for the approval card titled `title` (the previous card is gone by then). */
async function card(page, title) {
  await page.waitForFunction(
    (label) => document.querySelector("[data-testid=agents-approval]")?.getAttribute("aria-label") === label,
    `Approval needed: ${title}`,
    { timeout: 60_000 },
  );
  return page.getByTestId("agents-approval");
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

function resultOf(messages, id) {
  const block = messages.flatMap((m) => m.content).find((b) => b.type === "tool_result" && b.tool_use_id === id);
  if (!block) throw new Error(`no result for ${id}`);
  return { ...JSON.parse(block.content), isError: block.is_error === true };
}

async function callOf(page, task, id) {
  const detail = await invoke(page, "agents_task_detail", { task });
  return detail.transcript.find((i) => i.kind === "toolCall" && i.toolUseId === id);
}

export default async function agentSave({ launchApp, artifactsDir, assert }) {
  const project = await mkdtemp(join(tmpdir(), "exposition-agent-save-"));
  await writeFile(join(project, "main.typ"), "= Notes\n\nOdd numbers sum to squares.\n");
  await mkdir(join(project, "scripts"), { recursive: true });
  const onDisk = () => readFile(join(project, "scripts", "check.py"), "utf8");
  await writeFile(join(project, "scripts", "check.py"), "print('v1')\n");
  const env = { EXPOSITION_CREDENTIAL_NAMESPACE: `e2e-agent-save-${Date.now()}` };
  const evidence = { project, results: {} };
  const app = await launchApp({ env });
  try {
    const { page } = app;
    const info = await invoke(page, "app_info");
    assert.ok(info.debugBuild, "agent-save needs a debug build (scripted provider)");
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.getByTestId("agents-panel").waitFor({ timeout: 15_000 });
    await invoke(page, "agents_provider_update", {
      settings: { provider: "scripted", model: "claude-opus-5-5", effort: "medium", consent: "scopeContent" },
    });
    await invoke(page, "workspace_open", { path: project });
    // Trusting the folder is the user's own choice (native commands need it).
    await invoke(page, "trust_grant", { root: project });

    await invoke(page, "agents_dev_load_script", {
      script: {
        turns: [
          // 1: edit, save (approved), run.
          { steps: [read("r1")] },
          { steps: [edit("e1", "r1", "v1", "v2")] },
          { steps: [save("s1", "e1")] },
          { steps: [run("c1")] },
          done,
          // 2: edit, save (declined), run.
          { steps: [read("r2")] },
          { steps: [edit("e2", "r2", "v2", "v3")] },
          { steps: [save("s2", "e2")] },
          { steps: [run("c2")] },
          done,
          // 3: the file changed on disk meanwhile; the save is refused.
          { steps: [read("r3")] },
          { steps: [save("s3", "r3")] },
          done,
        ],
      },
    });

    // --- 1: the save asks with its own card; approved, the run sees the new text.
    const panel = page.getByTestId("agents-panel");
    await panel.getByTestId("agents-new-task").click();
    await page.fill("[data-testid=agents-prompt]", "Change the script to print v2 and run it");
    await panel.getByTestId("agents-folder").waitFor();
    await panel.getByRole("radio", { name: "Ask" }).click();
    await panel.getByRole("button", { name: /Document changes/ }).click();
    await page.getByRole("option", { name: "Ask before each change", exact: true }).click();
    await panel.getByRole("button", { name: /Commands and code/ }).click();
    await page.getByRole("option", { name: "Ask before each command (runs natively)", exact: true }).click();
    await panel.getByRole("button", { name: "Start task" }).click();
    await page.waitForSelector("[data-testid=agents-task-view]");
    let task = (await invoke(page, "agents_task_list"))[0];

    await card(page, `Apply edits to ${SCRIPT}`);
    await page.getByTestId("agents-approve").click();
    assert.equal(await onDisk(), "print('v1')\n", "the edit alone saves nothing");

    const saveCard = await card(page, `Save ${SCRIPT}`);
    const payload = await page.getByTestId("agents-approval-payload").innerText();
    evidence.saveCard = { title: await saveCard.getAttribute("aria-label"), payload };
    assert.match(payload, /^Save scripts\/check\.py to disk: revision \d+ of the editor text \(12 bytes, sha256 [0-9a-f]{64}\)/);
    assert.match(payload, /Every unsaved change in this file was made by this task\./);
    assert.match(payload, /- print\('v1'\)\n\+ print\('v2'\)/, "the card shows what changes on disk");
    assert.equal(await page.getByTestId("agents-approve-task").count(), 1, "a save is a repeatable file change");
    assert.match(await page.getByTestId("agents-approval-scope").innerText(), /\S/);
    await page.screenshot({ path: join(artifactsDir, "agent-save-approval.png") });
    assert.equal(await onDisk(), "print('v1')\n", "nothing is written while the card waits");
    await page.getByTestId("agents-approve").click();

    await card(page, "Run python");
    assert.equal(await onDisk(), "print('v2')\n", "the approved save reached the disk before the run");
    await page.getByTestId("agents-approve").click();
    task = await waitDone(page, task.id);
    assert.equal(task.status.kind, "completed", JSON.stringify(task.status));

    let messages = sentMessages(app.dataDir, task.id);
    const e1 = resultOf(messages, "e1");
    const s1 = resultOf(messages, "s1");
    const c1 = resultOf(messages, "c1");
    evidence.results = { e1, s1, c1 };
    assert.match(e1.note, /not saved to disk/);
    assert.match(e1.note, /save_document saves it/);
    assert.equal(s1.isError, false);
    assert.equal(s1.saved, true);
    assert.equal(s1.path, SCRIPT);
    assert.equal(s1.revision, e1.revision);
    assert.equal(s1.bytes, 12);
    assert.match(s1.contentHash, /^[0-9a-f]{64}$/);
    assert.equal(s1.unsavedChanges, false);
    assert.equal(s1.otherChanges, "none");
    assert.match(s1.note, /Saved to disk with the editor's own save/);
    assert.match(s1.authorization, /The user approved this call in a prompt, once/);
    assert.match(c1.stdout, /v2/, "the program ran the saved, edited script");
    assert.equal(c1.unsavedNotSeen, undefined);
    // The editor knows the file is saved (no unsaved dot), and the row says how it was authorized.
    const documents = await invoke(page, "document_list");
    const open = documents.find((d) => d.path === SCRIPT);
    assert.ok(open && open.savedRevision === open.revision, "the document is saved in the editor");
    const row = await callOf(page, task.id, "s1");
    assert.equal(row.status, "succeeded");
    assert.deepEqual(row.authorizedBy, { kind: "approved", forTask: false });
    const saveRow = panel.locator("[data-testid=agents-tool-call][data-tool=save_document]").first();
    await saveRow.waitFor();
    assert.match(await saveRow.innerText(), /scripts\/check\.py/);
    assert.match(await saveRow.getByTestId("agents-tool-authorization").innerText(), /Approved by you/);
    await page.screenshot({ path: join(artifactsDir, "agent-save-tool-row.png") });

    // --- 2: declined — the file is unchanged and the run reads the old script.
    await invoke(page, "agents_task_continue", { task: task.id, prompt: "Now print v3", budgets: null });
    await card(page, `Apply edits to ${SCRIPT}`);
    await page.getByTestId("agents-approve").click();
    await card(page, `Save ${SCRIPT}`);
    await page.getByTestId("agents-decline").click();
    await card(page, "Run python");
    await page.getByTestId("agents-approve").click();
    task = await waitDone(page, task.id);
    assert.equal(await onDisk(), "print('v2')\n", "a declined save leaves the file as it was");
    messages = sentMessages(app.dataDir, task.id);
    const s2 = resultOf(messages, "s2");
    const c2 = resultOf(messages, "c2");
    Object.assign(evidence.results, { s2, c2 });
    assert.ok(s2.isError, "the declined save is an error result");
    assert.match(s2.error, /declined/);
    assert.equal(s2.authorization, undefined, "a declined call was never authorized");
    assert.equal((await callOf(page, task.id, "s2")).status, "declined");
    assert.match(c2.stdout, /v2/);
    assert.doesNotMatch(c2.stdout, /v3/);
    assert.deepEqual(c2.unsavedNotSeen, [SCRIPT]);
    assert.match(c2.filesNote, /save_document saves one of them/);
    const unsaved = (await invoke(page, "document_list")).find((d) => d.path === SCRIPT);
    assert.notEqual(unsaved.savedRevision, unsaved.revision, "the edit stays in the editor, unsaved");

    // --- 3: another program changes the file; the agent's save is refused, with no card.
    await writeFile(join(project, "scripts", "check.py"), "print('from another program')\n");
    await invoke(page, "agents_task_continue", { task: task.id, prompt: "Save it now", budgets: null });
    task = await waitDone(page, task.id);
    assert.equal(await page.getByTestId("agents-approval").count(), 0, "a conflict is refused, not asked");
    messages = sentMessages(app.dataDir, task.id);
    const s3 = resultOf(messages, "s3");
    evidence.results.s3 = s3;
    assert.ok(s3.isError);
    assert.equal(s3.code, "agents.saveConflict");
    assert.equal(s3.saved, false);
    assert.match(s3.error, /Nothing was saved and the file on disk was not changed\. Only the user resolves this/);
    assert.equal(s3.authorization, undefined);
    assert.equal(await onDisk(), "print('from another program')\n", "the other program's version is not overwritten");
    const kept = (await invoke(page, "document_list")).find((d) => d.path === SCRIPT);
    assert.notEqual(kept.savedRevision, kept.revision, "the editor keeps the unsaved text for the user to resolve");

    const errors = app.consoleMessages.filter((m) => m.startsWith("error:") || m.startsWith("pageerror:"));
    assert.deepEqual(errors, [], "no console errors");
  } finally {
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2));
    await app.close();
  }
}
