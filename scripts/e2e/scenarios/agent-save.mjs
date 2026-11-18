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
//   4. Follow-up (needs Docker with the local image python:3.13-slim, never
//      pulled; reported as not run without it): a data file that an enabled
//      workflow trigger watches. The save card names the trigger's workflow,
//      the result carries the same list, and the trigger still fires: one
//      run starts after its quiet period, as for a user's save. A trigger
//      cannot watch a script (code never runs without review), so the
//      watched file is the workflow's data input.
// Evidence: test-results/e2e/agent-save/evidence.json (+ screenshots): the
// approval card of the save and every tool result as sent to the provider.
import { execFileSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";

const invoke = (page, cmd, args) =>
  page.evaluate(([c, a]) => window.__TAURI_INTERNALS__.invoke(c, a), [cmd, args ?? {}]);

const SCRIPT = "scripts/check.py";
const DATA = "data/obs.csv";
const IMAGE = "python:3.13-slim";
const WATCHED = "Watched count";
const tool = (id, name, input) => ({ toolUse: { id, name, input } });
const done = { steps: [{ text: "done" }] };
const read = (id, path = SCRIPT) => tool(id, "read_file", { path });
const edit = (id, from, oldText, newText, path = SCRIPT) =>
  tool(id, "edit_document", {
    path,
    base_revision: `$ref:${from}/revision`,
    edits: [{ old_text: oldText, new_text: newText }],
  });
const save = (id, from, path = SCRIPT) => tool(id, "save_document", { path, revision: `$ref:${from}/revision` });
const run = (id) => tool(id, "run_command", { program: "python", args: [SCRIPT], working_directory: "" });

function dockerAvailable() {
  try {
    execFileSync("docker", ["image", "inspect", IMAGE], { stdio: "ignore", timeout: 60_000 });
    return true;
  } catch {
    return false;
  }
}

/** A restricted batch workflow over a script (code) and a data file (the input a trigger may watch). */
function watchedWorkflow() {
  const input = (id, label, portType, path) => ({
    id,
    label,
    portType,
    source: { kind: "resource", path, version: { kind: "atRunStart" } },
  });
  return {
    schema: "exposition.workflow/1",
    title: WATCHED,
    description: "",
    preset: null,
    inputs: [input("script", "Python script", "codeSource", "count.py"), input("data", "Observations", "dataset", DATA)],
    steps: [
      {
        id: "run",
        label: "Run",
        operation: { id: "research.pythonBatch", version: 1 },
        parameters: { timeoutSeconds: { kind: "integer", value: 300 } },
        payload: null,
      },
    ],
    connections: [
      { kind: "data", from: { kind: "input", input: "script" }, to: { step: "run", port: "script" } },
      { kind: "data", from: { kind: "input", input: "data" }, to: { step: "run", port: "data" } },
    ],
    outcomes: [{ id: "out", kind: "transformedDataset", label: "Result", from: { step: "run", port: "dataset" }, optional: false }],
    ceiling: { execution: "restricted", modelRequests: false, retainResults: false, proposeChanges: false },
  };
}

async function waitFor(check, message, timeout = 120_000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    const value = await check();
    if (value) return value;
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error(`timed out: ${message}`);
}

async function waitDone(page, id, timeout = 240_000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    const task = (await invoke(page, "agents_task_list")).find((t) => t.id === id);
    if (task && !["queued", "running", "awaitingApproval", "awaitingAnswer"].includes(task.status.kind)) return task;
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
  await mkdir(join(project, "data"), { recursive: true });
  await writeFile(join(project, "data", "obs.csv"), "x,y\n0,1\n1,3\n");
  await writeFile(
    join(project, "count.py"),
    'import csv, os\nrows = list(csv.DictReader(open("data/obs.csv")))\n' +
      'with open(os.path.join(os.environ["EXPOSITION_OUTPUT_DIR"], "dataset.csv"), "w") as f:\n' +
      '    f.write("rows\\n%d\\n" % len(rows))\n',
  );
  const docker = dockerAvailable();
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
          // 4: a data file an enabled trigger watches.
          { steps: [read("r4", DATA)] },
          { steps: [edit("e4", "r4", "1,3", "1,4", DATA)] },
          { steps: [save("s4", "e4", DATA)] },
          done,
        ],
      },
    });

    // --- 1: the save asks with its own card; approved, the run sees the new text.
    const panel = page.getByTestId("agents-panel");
    await panel.getByTestId("agents-new-task").click();
    await page.fill("[data-testid=agents-prompt]", "Change the script to print v2 and run it");
    await panel.getByTestId("agents-folder").waitFor();
    // Design C: approvals, document changes, and commands are in Options › Permissions.
    await panel.getByTestId("agents-options").click();
    const sheet = page.getByRole("dialog", { name: "Options" });
    await sheet.waitFor();
    await sheet.getByRole("tab", { name: /^Permissions/ }).click();
    await sheet.getByRole("radio", { name: "Ask" }).click();
    await sheet.getByRole("button", { name: /Document changes/ }).click();
    await page.getByRole("option", { name: "Ask before each change", exact: true }).click();
    await sheet.getByRole("button", { name: /Commands and code/ }).click();
    await page.getByRole("option", { name: "Ask before each command (runs natively)", exact: true }).click();
    // The list closes with an exit animation; Escape during it closes only the list.
    await page.waitForFunction(() => !document.querySelector(".ui-select__popover"));
    await page.keyboard.press("Escape");
    await sheet.waitFor({ state: "detached" });
    await panel.getByTestId("agents-start").click();
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
    // No enabled trigger watches the script: the card and the result say nothing more.
    assert.doesNotMatch(payload, /trigger/i);
    assert.equal(s1.triggersKnown, true);
    assert.deepEqual(s1.triggers, []);
    assert.equal(s1.triggersNote, undefined);
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
    // Routine tool rows show under an expanded turn header (design C).
    const toggles = panel.getByTestId("agents-turn-toggle");
    for (let i = 0; i < (await toggles.count()); i++) {
      if ((await toggles.nth(i).getAttribute("aria-expanded")) !== "true") await toggles.nth(i).click();
    }
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

    // --- 4: a file an enabled trigger watches — the card names it, and the trigger still fires.
    if (docker) {
      await invoke(page, "execution_settings_update", { change: { kind: "dockerImage", image: IMAGE } });
      const workflow = await invoke(page, "workflow_save", {
        request: { id: null, expectedRevision: null, definition: watchedWorkflow() },
      });
      const review = await invoke(page, "workflow_trigger_plan", {
        request: { workflow: workflow.id, kind: { kind: "inputsChanged", inputs: ["data"] } },
      });
      assert.deepEqual(review.blockers, [], JSON.stringify(review.blockers));
      const settings = { debounceSeconds: 3, maxRunsPerHour: 12, maxRunsPerDay: 100, allowNative: false, maxModelRequestsPerDay: 0 };
      const trigger = await invoke(page, "workflow_trigger_enable", {
        request: { plan: review.plan.plan, standingHash: review.standingHash, settings },
      });
      await waitFor(
        async () => (await invoke(page, "workflow_trigger_list", { workflow: workflow.id }))[0]?.listening,
        "the trigger listens",
      );
      assert.deepEqual(await invoke(page, "workflow_runs", { workflow: workflow.id }), [], "enabling runs nothing");

      await invoke(page, "agents_task_continue", { task: task.id, prompt: "Correct the second observation", budgets: null });
      await card(page, `Apply edits to ${DATA}`);
      await page.getByTestId("agents-approve").click();
      await card(page, `Save ${DATA}`);
      const watchedPayload = await page.getByTestId("agents-approval-payload").innerText();
      evidence.watchedSaveCard = watchedPayload;
      assert.ok(
        watchedPayload.includes(
          "An enabled workflow trigger watches this file:\n" +
            `- “${WATCHED}”: saving this file can start a run of this workflow, within the trigger's own limits.`,
        ),
        watchedPayload,
      );
      assert.equal(await page.getByTestId("agents-approve-task").count(), 1, "still an ordinary, repeatable file change");
      await page.screenshot({ path: join(artifactsDir, "agent-save-watched-approval.png") });
      assert.deepEqual(await invoke(page, "workflow_runs", { workflow: workflow.id }), [], "nothing runs while the card waits");
      await page.getByTestId("agents-approve").click();
      task = await waitDone(page, task.id);
      assert.equal(await readFile(join(project, "data", "obs.csv"), "utf8"), "x,y\n0,1\n1,4\n");
      const s4 = resultOf(sentMessages(app.dataDir, task.id), "s4");
      evidence.results.s4 = s4;
      assert.equal(s4.saved, true);
      assert.equal(s4.triggersKnown, true);
      assert.deepEqual(s4.triggers, [{ workflow: WATCHED, startsOnSave: true }]);
      assert.match(s4.triggersNote, /^An enabled workflow trigger watches the file you saved:\n- “Watched count”: this save can start a run/);
      assert.match(s4.triggersNote, /Tell the user\.$/);
      // The trigger behaves as for a user's save: one run after the quiet period.
      const fired = await waitFor(async () => {
        const runs = await invoke(page, "workflow_runs", { workflow: workflow.id });
        if (runs.length === 0) return false;
        const run = await invoke(page, "workflow_run_get", { run: runs[0].id });
        return run.state !== "running" && { run, count: runs.length };
      }, "the trigger's run ended", 240_000);
      assert.equal(fired.count, 1, "exactly one run");
      assert.equal(fired.run.state, "succeeded", `${fired.run.code}: ${fired.run.message}`);
      evidence.triggered = { trigger: trigger.id, run: fired.run.id, state: fired.run.state };
      await invoke(page, "workflow_trigger_disable", { trigger: trigger.id });
    } else {
      evidence.notRun = [`4 (watched file): Docker image ${IMAGE} not available`];
      console.log(`agent-save: case 4 (a watched file) NOT RUN: Docker image ${IMAGE} is not available`);
    }

    const errors = app.consoleMessages.filter((m) => m.startsWith("error:") || m.startsWith("pageerror:"));
    assert.deepEqual(errors, [], "no console errors");
  } finally {
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2));
    await app.close();
  }
}
