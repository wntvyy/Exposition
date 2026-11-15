// LIVE check of what the built-in agent tells the user about approvals, saved
// state, and where things are in the app (opt-in: `npm run e2e --
// live-agentctx`). Costs money: it uses the API key saved in the app's
// provider settings; the task has small hard budgets. A short version of the
// 2026-11-15 walkthrough in the same settings (Auto, Files: edit, Code: off):
//   1. Write a script and run it: the first native run asks (approved for the
//      task).
//   2. Follow-up: change the script and run it again. No card may appear; the
//      agent is asked to say beforehand whether a prompt will appear and
//      afterwards which version ran and what is saved.
//   3. Follow-up without tools: how the user runs the script, sees runs, and
//      saves or keeps the changes.
// The model's sentences are recorded verbatim; the assertions cover what the
// application did (cards, authorizations) and the clearest narration errors of
// the walkthrough. Evidence: test-results/e2e/live-agentctx/evidence.json.
import { mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

const invoke = (page, cmd, args) =>
  page.evaluate(([c, a]) => window.__TAURI_INTERNALS__.invoke(c, a), [cmd, args ?? {}]);

const BUDGET = { turns: 18, toolCalls: 18, commands: 6, outputTokens: 30000, minutes: 10 };
const terminal = (status) => !["queued", "running", "awaitingApproval"].includes(status.kind);

/** Wait for the task to end, answering each card "for this task"; the cards' titles. */
async function finish(page, id, artifactsDir, label, timeout = 600_000) {
  const deadline = Date.now() + timeout;
  const cards = [];
  for (;;) {
    const card = page.getByTestId("agents-approval").first();
    if (await card.count()) {
      cards.push((await card.getAttribute("aria-label")) ?? "");
      await page.screenshot({ path: join(artifactsDir, `${label}-card-${cards.length}.png`) });
      const forTask = card.getByTestId("agents-approve-task");
      if (await forTask.count()) await forTask.click();
      else await card.getByTestId("agents-approve").click();
      await card.waitFor({ state: "detached", timeout: 30_000 }).catch(() => {});
    }
    const task = (await invoke(page, "agents_task_list")).find((t) => t.id === id);
    if (task && terminal(task.status)) return { task, cards };
    if (Date.now() > deadline) throw new Error(`task ${id} did not finish`);
    await page.waitForTimeout(500);
  }
}

/** The transcript from item `from` on: the agent's text and its tool calls. */
async function since(page, id, from) {
  const detail = await invoke(page, "agents_task_detail", { task: id });
  const items = detail.transcript.slice(from);
  return {
    next: detail.transcript.length,
    text: items.filter((i) => i.kind === "assistantText").map((i) => i.text),
    calls: items
      .filter((i) => i.kind === "toolCall")
      .map((c) => ({ tool: c.name, status: c.status, authorizedBy: c.authorizedBy, result: (c.result ?? "").slice(0, 600) })),
  };
}

/** Sentences that announce an approval prompt. */
const promisesPrompt = (texts) =>
  texts
    .join("\n")
    .split(/(?<=[.!?])\s+|\n+/)
    .filter((s) => /\b(you(?:'ll| will| should)|should (?:see|get|appear)|will (?:see|get|appear|ask))\b/i.test(s))
    .filter((s) => /\b(approv|prompt|permission card|your ok)\b/i.test(s))
    .filter((s) => !/\b(no|not|without|won't|will not|shouldn't|don't) \b/i.test(s));

export default async function liveAgentctx({ launchApp, artifactsDir, assert }) {
  const root = await mkdtemp(join(tmpdir(), "exposition-live-agentctx-"));
  await writeFile(join(root, "main.typ"), "= Odd sums\n\nThe sum of the first $n$ odd numbers is $n^2$.\n");
  await mkdir(join(root, "scripts"), { recursive: true });
  const evidence = { root, steps: [] };
  const app = await launchApp();
  const { page } = app;
  try {
    await page.setViewportSize({ width: 1500, height: 980 });
    const provider = await invoke(page, "agents_provider_status");
    assert.ok(provider.configured, "the model provider has an API key");
    assert.notEqual(provider.settings.provider, "scripted", "live run uses the real provider");
    evidence.model = provider.settings.model;
    await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"]').waitFor({ timeout: 15_000 });
    const workspace = await invoke(page, "workspace_current");
    await invoke(page, "trust_grant", { root: workspace.root });
    await invoke(page, "agents_permissions_set", {
      root: workspace.root,
      choice: { files: "edit", code: "off", setup: false },
    });

    // ---- 1. Write and run: one card, approved for the task.
    const panel = page.getByTestId("agents-panel");
    await panel.getByTestId("agents-new-task").click();
    await page.fill(
      "[data-testid=agents-prompt]",
      "Write a short Python script scripts/check_odd_sums.py that checks, for n = 1..200, that the sum of the first n " +
        "odd numbers equals n squared and prints one summary line, then run it with run_command (python " +
        "scripts/check_odd_sums.py). Before each tool call, tell me in one sentence what is about to happen, " +
        "including whether I will be asked to approve it. Keep everything else brief.",
    );
    await panel.getByTestId("agents-folder").waitFor();
    await panel.getByRole("radio", { name: "Auto" }).click();
    await page.waitForFunction(
      () => document.querySelector("[data-testid=agents-auto-profile]")?.dataset.granted === "true",
    );
    await panel.getByRole("button", { name: /^Budgets/ }).click();
    const fill = (label, value) => panel.getByLabel(label, { exact: true }).fill(String(value));
    await fill("Model turns", BUDGET.turns);
    await fill("Tool calls", BUDGET.toolCalls);
    await fill("Commands", BUDGET.commands);
    await fill("Output tokens", BUDGET.outputTokens);
    await fill("Minutes", BUDGET.minutes);
    await panel.getByRole("button", { name: "Start task" }).click();
    await page.waitForSelector("[data-testid=agents-task-view]");
    const id = (await invoke(page, "agents_task_list"))[0].id;
    let { task, cards } = await finish(page, id, artifactsDir, "1");
    let step = { name: "1. write and run", status: task.status, cards, ...(await since(page, id, 0)) };
    let next = step.next;
    evidence.steps.push(step);
    console.log(`[live-agentctx] 1: cards ${JSON.stringify(cards)}\n${step.text.join("\n---\n")}`);
    assert.equal(task.status.kind, "completed", JSON.stringify(task.status));
    assert.equal(cards.length, 1, `one card for the first native run (saw ${JSON.stringify(cards)})`);
    assert.ok(step.calls.some((c) => c.authorizedBy?.kind === "approved" && c.authorizedBy.forTask));

    // ---- 2. Change and rerun: no card; narration about the prompt and the saved state.
    await invoke(page, "agents_task_continue", {
      task: id,
      prompt:
        "Now change the script to check n = 1..500 and run it again the same way. Before running, tell me in one " +
        "sentence whether I will be asked to approve anything. Afterwards tell me exactly which version of the script " +
        "ran, whether your change is saved to disk, and what, if anything, I need to do.",
      budgets: null,
    });
    ({ task, cards } = await finish(page, id, artifactsDir, "2"));
    step = { name: "2. change and rerun", status: task.status, cards, ...(await since(page, id, next)) };
    next = step.next;
    step.promisedPrompts = promisesPrompt(step.text);
    evidence.steps.push(step);
    console.log(`[live-agentctx] 2: cards ${JSON.stringify(cards)}\n${step.text.join("\n---\n")}`);
    assert.equal(task.status.kind, "completed", JSON.stringify(task.status));
    assert.equal(cards.length, 0, "the allowance covers the second run: no card");
    assert.deepEqual(step.promisedPrompts, [], "the agent announced a prompt that did not appear");

    // ---- 3. Navigation, without tools.
    await invoke(page, "agents_task_continue", {
      task: id,
      prompt:
        "Without using any tools: how would I run this script myself inside Exposition, where do I see the runs " +
        "afterwards, and how do I save or keep the changes you made? Be brief and concrete.",
      budgets: null,
    });
    ({ task, cards } = await finish(page, id, artifactsDir, "3"));
    step = { name: "3. how do I do it myself", status: task.status, cards, ...(await since(page, id, next)) };
    evidence.steps.push(step);
    console.log(`[live-agentctx] 3:\n${step.text.join("\n---\n")}`);
    assert.equal(task.status.kind, "completed", JSON.stringify(task.status));
    const answer = step.text.join("\n");
    assert.match(answer, /Research/, "names the Research panel or page");
    assert.match(answer, /Run File in Research Session|Run file|\bRun\b/, "names an in-app run control");
    assert.match(answer, /Ctrl\+S|File: Save|Save All/, "says how to save");
    assert.match(answer, /\bKeep\b/, "names Keep for the review");
    await page.screenshot({ path: join(artifactsDir, "3-done.png") });
    evidence.usage = task.usage;
  } finally {
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2));
    await app.close();
  }
}
