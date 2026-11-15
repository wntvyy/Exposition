// A1 lane P: Ask / Auto approval modes, project permissions, and Lean edits as
// execution, in the real app (debug build) with the scripted provider —
// development only. The scripted provider replaces only the model; the task
// loop, central gate, approvals, project permissions, research sessions, and
// UI are the real ones. It cannot establish live model behavior.
//
// Steps (each records every approval card it saw, by title):
//   a. Ask: a native Python run asks; "Approve for this task" → the next native
//      run does not ask.
//   b. Auto, native profile (chosen in the composer): two runs and a file
//      creation, no card.
//   c. Auto, restricted profile without a container image: a native-only Lean
//      check asks once; Python is refused (never asked around, never native).
//   g. Auto without native code: a Lean-file edit asks (Lean elaborates it).
//   d. Auto: a deletion with consequences (unsaved edits) asks once, never "for
//      this task".
//   e. Auto: an out-of-scope change is refused, no card.
//   f. Revoking the project's permissions in Settings › Agents while an Auto
//      task runs Python stops it; its research session stops too.
// Evidence: test-results/e2e/a1-permissions/evidence.json (+ screenshots); the
// screenshots are also copied to A1_SHOTS_DIR when it is set.
import { copyFile, mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { runCommand } from "../palette.mjs";

const invoke = (page, cmd, args) =>
  page.evaluate(([c, a]) => window.__TAURI_INTERNALS__.invoke(c, a), [cmd, args ?? {}]);

function consoleErrors(app) {
  return app.consoleMessages.filter((m) => m.startsWith("error:") || m.startsWith("pageerror:"));
}

async function selectOption(page, scope, label, option) {
  await scope.getByRole("button", { name: label }).click();
  await page.getByRole("option", { name: option, exact: true }).click();
}

/** Record every approval card that appears (its title), from now on. */
async function watchCards(page) {
  await page.evaluate(() => {
    window.__a1Cards = [];
    window.__a1Seen = new WeakSet();
    if (window.__a1Observer) window.__a1Observer.disconnect();
    const scan = () => {
      for (const card of document.querySelectorAll("[data-testid=agents-approval]")) {
        if (window.__a1Seen.has(card)) continue;
        window.__a1Seen.add(card);
        window.__a1Cards.push({
          title: (card.getAttribute("aria-label") ?? "").replace(/^Approval needed: /, ""),
          forTask: card.dataset.repeatable === "true",
        });
      }
    };
    window.__a1Observer = new MutationObserver(scan);
    window.__a1Observer.observe(document.body, { childList: true, subtree: true });
    scan();
  });
}

const cardsSeen = (page) => page.evaluate(() => window.__a1Cards ?? []);

async function newestTask(page) {
  return (await invoke(page, "agents_task_list"))[0];
}

async function waitDone(page, id, timeout = 240_000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    const task = (await invoke(page, "agents_task_list")).find((t) => t.id === id);
    if (task && !["queued", "running", "awaitingApproval"].includes(task.status.kind)) return task;
    await page.waitForTimeout(300);
  }
  throw new Error(`task ${id} did not finish`);
}

async function toolCalls(page, id) {
  const detail = await invoke(page, "agents_task_detail", { task: id });
  return detail.transcript.filter((i) => i.kind === "toolCall");
}

/** Approvals the persisted record shows (approved or declined calls). */
const recordedApprovals = (calls) =>
  calls.filter((c) => c.status === "declined" || c.authorizedBy?.kind === "approved").map((c) => c.name);

/** Open the composer in the Agent view and fill it. */
async function compose(page, { prompt, mode, permission, execution, scope, document }) {
  const panel = page.getByTestId("agents-panel");
  await panel.getByTestId("agents-new-task").click();
  await page.fill("[data-testid=agents-prompt]", prompt);
  await panel.getByTestId("agents-folder").waitFor();
  if (scope) await selectOption(page, panel, /Scope/, scope);
  if (document) await panel.getByLabel("Document (relative to the project folder)").fill(document);
  await panel.getByRole("radio", { name: mode }).click();
  if (mode === "Ask") {
    if (permission) await selectOption(page, panel, /Document changes/, permission);
    if (execution) await selectOption(page, panel, /Commands and code/, execution);
  } else {
    await panel.getByTestId("agents-auto-profile").waitFor();
  }
  return panel;
}

async function start(page, panel) {
  await panel.getByRole("button", { name: "Start task" }).click();
  await page.waitForSelector("[data-testid=agents-task-view]");
  return newestTask(page);
}

const tool = (id, name, input) => ({ toolUse: { id, name, input } });
const done = { steps: [{ text: "done" }] };

export default async function a1Permissions({ launchApp, artifactsDir, assert }) {
  const project = await mkdtemp(join(tmpdir(), "exposition-a1-permissions-"));
  await writeFile(join(project, "main.typ"), "= Notes\n\nThe squares grow quickly.\n");
  await writeFile(join(project, "Main.lean"), "theorem t : 1 = 1 := rfl\n");
  await mkdir(join(project, "notes"), { recursive: true });
  await writeFile(join(project, "notes", "a.typ"), "= A\n");
  const env = { EXPOSITION_CREDENTIAL_NAMESPACE: `e2e-a1p-${Date.now()}` };
  const evidence = { project, steps: [] };
  const shots = [];
  const shot = async (page, name) => {
    const path = join(artifactsDir, `${name}.png`);
    await page.waitForTimeout(250);
    await page.screenshot({ path });
    shots.push(path);
  };
  const record = async (page, name, id, extra = {}) => {
    const calls = id ? await toolCalls(page, id) : [];
    const entry = {
      name,
      cards: (await cardsSeen(page)).map((c) => c.title),
      recordedApprovals: recordedApprovals(calls),
      calls: calls.map((c) => ({ tool: c.name, status: c.status, authorizedBy: c.authorizedBy })),
      ...extra,
    };
    evidence.steps.push(entry);
    console.log(`[a1-permissions] ${name}: cards ${JSON.stringify(entry.cards)}`);
    return entry;
  };

  const app = await launchApp({ env });
  try {
    const { page } = app;
    const info = await invoke(page, "app_info");
    assert.ok(info.debugBuild, "a1-permissions needs a debug build (scripted provider)");
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.getByTestId("agents-panel").waitFor({ timeout: 15_000 });
    await invoke(page, "agents_provider_update", {
      settings: { provider: "scripted", model: "claude-opus-5-5", effort: "medium", consent: "scopeContent" },
    });
    const opened = await invoke(page, "workspace_open", { path: project });
    const root = opened.root;
    // Trusting the folder is the user's explicit action (the trust dialog).
    await invoke(page, "trust_grant", { root: project });
    await watchCards(page);

    // --- a. Ask: approve the first native run for the task; the next one does not ask.
    await invoke(page, "agents_dev_load_script", {
      script: {
        turns: [
          { steps: [tool("p1", "run_python", { code: "print(6 * 7)" })] },
          { steps: [tool("p2", "run_python", { code: "print(7 * 8)" })] },
          done,
        ],
      },
    });
    let panel = await compose(page, {
      prompt: "Compute two products",
      mode: "Ask",
      permission: "Apply changes, review afterwards",
      execution: "Ask before each command (runs natively)",
    });
    await shot(page, "a1-composer-ask-light");
    let task = await start(page, panel);
    await page.waitForSelector("[data-testid=agents-approval]", { timeout: 60_000 });
    assert.equal(await page.getByTestId("agents-approve-task").count(), 1, "Approve for this task is offered");
    await shot(page, "a1-approval-for-task-light");
    await page.getByTestId("agents-approve-task").click();
    task = await waitDone(page, task.id);
    assert.equal(task.status.kind, "completed", JSON.stringify(task.status));
    let calls = await toolCalls(page, task.id);
    assert.deepEqual(
      calls.map((c) => [c.status, c.authorizedBy]),
      [
        ["succeeded", { kind: "approved", forTask: true }],
        ["succeeded", { kind: "taskAllowance", kinds: ["nativeCode"] }],
      ],
    );
    await page.getByTestId("agents-allowances").waitFor();
    await shot(page, "a1-task-allowance-light");
    let step = await record(page, "a. Ask: native Python ×2, first approved for the task", task.id);
    assert.equal(step.cards.length, 1, "exactly one card");

    // --- b. Auto, native profile chosen in the composer: no card.
    await watchCards(page);
    await invoke(page, "agents_dev_load_script", {
      script: {
        turns: [
          { steps: [tool("p1", "run_python", { code: "print(1 + 1)" }), tool("p2", "run_python", { code: "print(2 + 2)" })] },
          { steps: [tool("c1", "create_file", { path: "notes/result.typ", content: "= Result\n4\n" })] },
          done,
        ],
      },
    });
    panel = await compose(page, { prompt: "Compute and write the result", mode: "Auto" });
    assert.equal(await page.getByTestId("agents-auto-profile").getAttribute("data-granted"), "false");
    assert.ok(await page.getByTestId("agents-start").isDisabled(), "Auto needs the project's permissions first");
    await shot(page, "a1-composer-auto-profile-light");
    await page.getByTestId("agents-permissions-change").click();
    await selectOption(page, panel, /Code/, "Native on this computer");
    await page.getByTestId("agents-permissions-environment").getByText(/not a sandbox/).waitFor();
    await shot(page, "a1-composer-auto-native-light");
    await page.getByTestId("agents-permissions-use").click();
    await page.waitForFunction(
      () => document.querySelector("[data-testid=agents-auto-profile]")?.dataset.granted === "true",
    );
    task = await start(page, panel);
    task = await waitDone(page, task.id);
    assert.equal(task.status.kind, "completed", JSON.stringify(task.status));
    assert.ok(existsSync(join(project, "notes", "result.typ")));
    calls = await toolCalls(page, task.id);
    assert.ok(calls.every((c) => c.status === "succeeded" && c.authorizedBy?.kind === "projectPermissions"), JSON.stringify(calls));
    step = await record(page, "b. Auto (native profile): Python ×2 + create_file", task.id, {
      permissions: (await invoke(page, "agents_permissions_get", { root })).permissions,
    });
    assert.equal(step.cards.length, 0, "no card in Auto within the profile");

    // --- c. Auto, restricted profile (no container image configured).
    await invoke(page, "agents_permissions_set", { root, choice: { files: "edit", code: "restricted", setup: false } });
    await watchCards(page);
    await invoke(page, "agents_dev_load_script", {
      script: {
        turns: [
          { steps: [tool("l1", "check_lean", { path: "Main.lean" })] },
          { steps: [tool("p1", "run_python", { code: "print(3)" })] },
          done,
        ],
      },
    });
    panel = await compose(page, { prompt: "Check Lean, then compute", mode: "Auto" });
    await page.waitForFunction(() => document.querySelector("[data-testid=agents-auto-profile]")?.dataset.granted === "true");
    task = await start(page, panel);
    await page.waitForSelector("[data-testid=agents-approval]", { timeout: 30_000 });
    await page.getByTestId("agents-decline").click();
    task = await waitDone(page, task.id);
    calls = await toolCalls(page, task.id);
    assert.equal(calls[0].status, "declined");
    assert.match(calls[1].result, /agents\.restrictedUnavailable/, calls[1].result);
    step = await record(page, "c. Auto (restricted, no image): check_lean asks once (declined); run_python refused", task.id);
    assert.deepEqual(step.cards, ["Check Main.lean with Lean"]);

    // --- g. Auto without native code: a Lean-file edit asks (Lean elaborates it).
    await watchCards(page);
    await invoke(page, "agents_dev_load_script", {
      script: {
        turns: [
          { steps: [tool("r", "read_file", { path: "Main.lean" })] },
          {
            steps: [
              tool("e", "edit_document", {
                path: "Main.lean",
                base_revision: "$ref:r/revision",
                edits: [{ old_text: "rfl", new_text: "rfl\n#eval IO.println \"hi\"" }],
              }),
            ],
          },
          done,
        ],
      },
    });
    panel = await compose(page, { prompt: "Add an eval to Main.lean", mode: "Auto" });
    await page.waitForFunction(() => document.querySelector("[data-testid=agents-auto-profile]")?.dataset.granted === "true");
    task = await start(page, panel);
    await page.waitForSelector("[data-testid=agents-approval]", { timeout: 30_000 });
    const leanPayload = await page.getByTestId("agents-approval-payload").innerText();
    assert.match(leanPayload, /Lean elaborates the text of an open Lean file natively/);
    await shot(page, "a1-lean-edit-approval-light");
    await page.getByTestId("agents-decline").click();
    task = await waitDone(page, task.id);
    step = await record(page, "g. Auto (code restricted): Lean-file edit asks (declined)", task.id);
    assert.deepEqual(step.cards, ["Apply edits to Main.lean"]);

    // --- d. Auto: a deletion with consequences (unsaved edits) asks once, never for the task.
    const doc = await invoke(page, "document_open", { path: "notes/a.typ" });
    await invoke(page, "document_apply", {
      request: {
        id: `user-${Date.now()}`,
        origin: { kind: "source" },
        edits: [{ document: doc.state.info.id, baseRevision: doc.state.info.revision, patches: [{ range: { start: 0, end: 0 }, text: "// unsaved\n" }] }],
        undo: { kind: "newGroup" },
        conflict: "reject",
        label: null,
        selection: null,
      },
    });
    await watchCards(page);
    await invoke(page, "agents_dev_load_script", {
      script: { turns: [{ steps: [tool("d", "delete_paths", { paths: ["notes/a.typ"] })] }, done] },
    });
    panel = await compose(page, { prompt: "Delete notes/a.typ", mode: "Auto" });
    await page.waitForFunction(() => document.querySelector("[data-testid=agents-auto-profile]")?.dataset.granted === "true");
    task = await start(page, panel);
    await page.waitForSelector("[data-testid=agents-approval]", { timeout: 30_000 });
    assert.equal(await page.getByTestId("agents-approve-task").count(), 0, "consequences are never approved for the task");
    await page.getByTestId("agents-decline").click();
    task = await waitDone(page, task.id);
    assert.ok(existsSync(join(project, "notes", "a.typ")), "declined: nothing deleted");
    step = await record(page, "d. Auto: delete with consequences asks once (declined)", task.id);
    assert.equal(step.cards.length, 1);
    assert.match(step.cards[0], /^Move 1 item to the Recycle Bin/);

    // --- e. Auto: out of scope is refused without a card.
    await watchCards(page);
    await invoke(page, "agents_dev_load_script", {
      script: { turns: [{ steps: [tool("x", "create_file", { path: "notes/outside.typ", content: "x" })] }, done] },
    });
    panel = await compose(page, { prompt: "Write outside the scope", mode: "Auto", scope: "One document", document: "main.typ" });
    await page.waitForFunction(() => document.querySelector("[data-testid=agents-auto-profile]")?.dataset.granted === "true");
    task = await start(page, panel);
    task = await waitDone(page, task.id);
    calls = await toolCalls(page, task.id);
    assert.equal(calls[0].status, "failed");
    assert.match(calls[0].result, /agents\.outOfScope/);
    assert.ok(!existsSync(join(project, "notes", "outside.typ")));
    step = await record(page, "e. Auto: out-of-scope create refused", task.id);
    assert.equal(step.cards.length, 0);

    // --- f. Revoke in Settings › Agents while an Auto task runs Python.
    await invoke(page, "agents_permissions_set", { root, choice: { files: "edit", code: "native", setup: false } });
    await watchCards(page);
    await invoke(page, "agents_dev_load_script", {
      script: {
        turns: [
          { steps: [tool("slow", "run_python", { code: "import time\ntime.sleep(120)\nprint('finished')" })] },
          { steps: [tool("after", "create_file", { path: "notes/after.typ", content: "x" })] },
          done,
        ],
      },
    });
    panel = await compose(page, { prompt: "A long computation", mode: "Auto" });
    await page.waitForFunction(() => document.querySelector("[data-testid=agents-auto-profile]")?.dataset.granted === "true");
    task = await start(page, panel);
    const running = task.id;
    const deadline = Date.now() + 120_000;
    for (;;) {
      const sessions = await invoke(page, "research_sessions");
      const mine = sessions.find((s) => s.owner?.kind === "agent" && s.owner.taskId === running && s.state.kind === "busy");
      if (mine) break;
      if (Date.now() > deadline) throw new Error("the agent research session never became busy");
      await page.waitForTimeout(300);
    }
    await runCommand(page, "open settings", /Preferences: Open Settings/);
    await page.getByRole("tab", { name: "Agents", exact: true }).click();
    const row = page.getByTestId("settings-project-permission").filter({ hasText: "Code: native" });
    await row.waitFor();
    await shot(page, "a1-settings-agents-light");
    await row.getByTestId("settings-permissions-revoke").click();
    task = await waitDone(page, running, 30_000);
    assert.equal(task.status.kind, "canceled");
    const detail = await invoke(page, "agents_task_detail", { task: running });
    const notice = detail.transcript.find((i) => i.kind === "notice" && i.message.startsWith("Project permissions changed"));
    assert.ok(notice, "a notice says why");
    assert.match(notice.message, /revoked/);
    assert.match(notice.message, /not undone/);
    calls = await toolCalls(page, running);
    assert.equal(calls[0].status, "canceled", JSON.stringify(calls[0]));
    assert.ok(!existsSync(join(project, "notes", "after.typ")), "nothing ran after the stop");
    const alive = (await invoke(page, "research_sessions")).filter(
      (s) => s.owner?.kind === "agent" && s.owner.taskId === running && !["stopped", "detached", "crashed"].includes(s.state.kind),
    );
    assert.deepEqual(alive, [], "the agent's research session stopped");
    assert.equal(await page.getByTestId("settings-project-permissions-empty").count(), 1);
    await shot(page, "a1-settings-agents-revoked-light");
    step = await record(page, "f. Revoked in Settings › Agents while Python ran: task stopped", running, {
      notice: notice.message,
    });
    assert.equal(step.cards.length, 0);

    assert.deepEqual(consoleErrors(app), [], "no console errors");
  } finally {
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2));
    await app.close();
  }
  const target = process.env.A1_SHOTS_DIR;
  if (target) {
    await mkdir(target, { recursive: true });
    for (const path of shots) await copyFile(path, join(target, path.split(/[\\/]/).pop()));
    console.log(`copied ${shots.length} screenshots to ${target}`);
  }
}
