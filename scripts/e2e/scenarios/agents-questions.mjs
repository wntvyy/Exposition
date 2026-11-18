// Structured questions from an agent in the real app (debug build, scripted
// provider — development only; no model-provider call is made). The scripted
// provider replaces only the model: `ask_user`, the question request, Chat
// about it (one bounded discussion turn in the task's worker with
// `revise_question`), answering, persistence, and the UI are the real ones.
//
// Covers:
// 1. A script calls `ask_user` with two questions (one Recommended option).
// 2. The question card renders at the call; the task status reads "Waiting
//    for your answer"; the "Needs you" link jumps to the card (focus).
//    While the question waits, Send is disabled for an ordinary follow-up.
// 3. Chat about it focuses the composer with the About chip and keeps the
//    draft text; sending a discussion yields the scripted reply and a
//    `revise_question` that adds an option, marked New on the card; the
//    unrelated draft comes back afterwards.
// 4. Send answer is enabled only once every question has an answer; the
//    answer resumes the task, which completes; a repeated identical answer is
//    accepted without effect and a different one is refused.
// 5. The Agents page shows the same answered request once (no duplicate).
//
// Figures are not covered here: a figure reference is recorded only from a
// native `run_python` run's manifest, which needs a Python installation, so
// it is checked where a real Python run already happens (agents-harness: the
// figure beside its call) and in the FigureCard unit tests.
//
// Screenshots (light and dark) go to test-results/e2e/agents-questions/ and,
// when EXPOSITION_DEV_HOME is set, to $EXPOSITION_DEV_HOME/ui-reference/agents-questions/.
import { copyFile, mkdir, mkdtemp, readdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";
import { invoke, invokeError, openWorkspace, waitFor } from "../workspace-helpers.mjs";

const MAIN = "= Arrhenius fit\nTable 2 lists the rate constants.\n";

const Q1 = "Which temperature unit should Table 2 use?";
const Q2 = "Report the activation energy in kJ/mol or eV?";
const NEW_OPTION = "Kelvin, with °C in the caption";
const DRAFT = "Draft note: mention the calibration run.";
const DISCUSSION = "Could the table keep Kelvin and still mention Celsius?";
const REPLY = "Kelvin keeps Table 2 consistent with the fit. I added an option that keeps Kelvin and names Celsius in the caption.";

const unitQuestion = (extra = []) => ({
  prompt: Q1,
  explanation: "The fit uses Kelvin; the lab notebook records Celsius.",
  options: [
    { id: "kelvin", label: "Kelvin", consequence: "Matches the fit and the figure axis", recommended: true },
    { id: "celsius", label: "Celsius", consequence: "Matches the lab notebook", recommended: false },
    ...extra,
  ],
});

const energyQuestion = {
  prompt: Q2,
  explanation: "",
  options: [
    { id: "kjmol", label: "kJ/mol", consequence: "Consistent with Section 2", recommended: false },
    { id: "ev", label: "eV", consequence: "", recommended: false },
  ],
};

/**
 * The model's side. A script turn serves one model request of the task, in
 * order: (1) the question; (2–3) the discussion — a reply with
 * `revise_question` naming the request (its id comes from the held
 * `ask_user` result through `$ref`), then the reply after the tool result;
 * (4) the turn after the answer.
 */
const SCRIPT = {
  turns: [
    {
      steps: [
        { text: "Before I change Table 2 I need two decisions from you." },
        { toolUse: { id: "ask", name: "ask_user", input: { questions: [unitQuestion(), energyQuestion], allowCustom: true } } },
      ],
    },
    {
      steps: [
        { text: REPLY },
        {
          toolUse: {
            id: "revise",
            name: "revise_question",
            input: {
              request: "$ref:ask/request",
              questions: [
                unitQuestion([{ id: "kelvin_caption", label: NEW_OPTION, consequence: "Keeps the table narrow", recommended: false }]),
                energyQuestion,
              ],
            },
          },
        },
      ],
    },
    { steps: [{ text: "The question now includes that option; answer when you are ready." }] },
    { steps: [{ text: "Using Kelvin with °C in the caption and kJ/mol. Nothing else changes in Table 2." }] },
  ],
};

async function setTheme(page, theme) {
  await runCommand(page, `color theme ${theme}`, theme === "dark" ? /Color Theme: Dark/ : /Color Theme: Light/);
  await page.waitForFunction((t) => document.documentElement.dataset.theme === t, theme);
  await page.waitForTimeout(250);
}

async function detailOf(page, id) {
  return invoke(page, "agents_task_detail", { task: id });
}

async function waitStatus(page, id, kinds, timeout = 30_000) {
  return waitFor(
    async () => {
      const d = await detailOf(page, id);
      return kinds.includes(d.summary.status.kind) ? d : null;
    },
    { timeout, interval: 150, message: `task ${id} status in ${kinds}` },
  );
}

const disabled = (locator) =>
  locator.evaluate((el) => el.hasAttribute("disabled") || el.getAttribute("data-disabled") === "true" || el.getAttribute("aria-disabled") === "true");

export default async function agentsQuestions({ launchApp, artifactsDir, assert }) {
  const project = await mkdtemp(join(tmpdir(), "exposition-questions-"));
  await writeFile(join(project, "main.typ"), MAIN);
  const env = { EXPOSITION_CREDENTIAL_NAMESPACE: `e2e-questions-${Date.now()}` };
  const app = await launchApp({ env });
  const info = await invoke(app.page, "app_info");
  if (!info.debugBuild) {
    // The scripted provider exists only in debug builds.
    await app.close();
    console.log("SKIPPED: agents-questions needs a debug build");
    return;
  }
  const evidence = { project, steps: [] };
  const step = (name, data = {}) => evidence.steps.push({ name, ...data });
  const shots = [];
  const shot = async (page, name) => {
    const path = join(artifactsDir, `${name}.png`);
    await page.screenshot({ path });
    shots.push(path);
  };
  const bothThemes = async (page, name) => {
    await page.waitForTimeout(200);
    await shot(page, `${name}-light`);
    await setTheme(page, "dark");
    await shot(page, `${name}-dark`);
    await setTheme(page, "light");
  };
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

    // --- 1. The agent asks two questions ------------------------------------
    await invoke(page, "agents_dev_load_script", { script: SCRIPT });
    const created = await invoke(page, "agents_task_create", {
      request: {
        prompt: "Tidy Table 2 of the Arrhenius fit",
        workspaceRoot: project,
        scope: { kind: "workspace" },
        permission: "reviewEdits",
        execution: "disabled",
        consent: "scopeContent",
        budgets: null,
      },
    });
    const asked = await waitStatus(page, created.id, ["awaitingAnswer"]);
    assert.equal(asked.questions?.length, 1, "one question request");
    const request = asked.questions[0];
    assert.equal(request.toolUseId, "ask");
    assert.equal(request.state.kind, "pending");
    assert.deepEqual(
      request.questions.map((q) => [q.id, q.prompt, q.options.filter((o) => o.recommended).map((o) => o.id)]),
      [
        ["q1", Q1, ["kelvin"]],
        ["q2", Q2, []],
      ],
    );
    step("ask_user recorded a pending request with two questions; the task waits for the answer", {
      request: request.id,
      revision: request.revision,
    });

    // --- 2. The card, the status, and the Needs-you jump ---------------------
    await page.evaluate(() => document.querySelector("[data-testid=agents-show-tasks]")?.click());
    await panel.getByTestId("agents-task-item").first().click();
    await panel.getByTestId("agents-task-view").waitFor({ timeout: 10_000 });
    const card = panel.locator(`[data-testid=agents-question][data-question="${request.id}"]`);
    await card.waitFor({ timeout: 10_000 });
    assert.equal(await panel.getByTestId("agents-question").count(), 1, "one card for one request");
    assert.equal(await card.getAttribute("data-open"), "true");
    assert.equal(await card.getAttribute("aria-label"), Q1);
    assert.ok(await card.getByText("Needs your answer", { exact: true }).isVisible(), "the info banner names the card");
    assert.ok(await card.getByText("0 of 2 answered", { exact: true }).isVisible());
    assert.equal(await card.getByText("Recommended", { exact: true }).count(), 1, "one Recommended option");
    assert.equal(await card.locator("input[type=radio]:checked").count(), 0, "nothing is preselected");
    // The card is at the ask_user call, inside the conversation, and visible
    // although the turn's routine activity is collapsed.
    assert.equal(await panel.locator("[data-needs-you='question:ask'] [data-testid=agents-question]").count(), 1);
    assert.equal(await panel.locator("[data-testid=agents-transcript] [data-testid=agents-question]").count(), 1);
    const taskStatus = panel.getByTestId("agents-task-status");
    assert.equal(await taskStatus.getAttribute("data-status"), "awaitingAnswer");
    assert.equal((await taskStatus.textContent())?.trim(), "Waiting for your answer");
    // An ordinary follow-up cannot be sent while the question waits.
    const field = panel.getByTestId("agents-follow-up-text");
    await field.fill(DRAFT);
    const send = panel.getByTestId("agents-continue");
    assert.ok(await disabled(send), "Send is disabled while a question waits");
    assert.equal(await panel.getByTestId("agents-send-waits").textContent(), "Answer the question above, or use Chat about it.");
    // Needs you: a jump link with the short prompt; it focuses the card.
    const needs = panel.getByTestId("agents-needs-question");
    assert.equal(await needs.count(), 1);
    assert.equal(await needs.getAttribute("aria-label"), `Needs your answer: ${Q1}`);
    await field.focus();
    await needs.click();
    await page.waitForFunction(
      (id) => document.activeElement?.getAttribute("data-testid") === "agents-question" && document.activeElement.getAttribute("data-question") === id,
      request.id,
      { timeout: 5000 },
    );
    await bothThemes(page, "question-card");
    step("card at the call (Needs your answer, Recommended, nothing chosen); status 'Waiting for your answer'; Needs-you jump focuses the card; Send disabled");

    // --- 3. Chat about it ----------------------------------------------------
    await card.getByTestId("agents-question-chat").click();
    const about = panel.getByTestId("agents-about");
    await about.waitFor({ timeout: 5000 });
    assert.equal((await about.textContent())?.trim(), `About: ${Q1}`);
    assert.equal(await about.getAttribute("data-tone"), "accent");
    await page.waitForFunction(() => document.activeElement?.getAttribute("data-testid") === "agents-follow-up-text", null, {
      timeout: 5000,
    });
    assert.equal(await field.inputValue(), DRAFT, "Chat about it keeps the draft text");
    assert.equal(await panel.getByTestId("agents-send-waits").count(), 0);
    assert.ok(!(await disabled(send)), "Send takes the discussion message");
    await shot(page, "question-chat-about-light");
    await field.fill(DISCUSSION);
    await send.click();
    // The discussion turn: the scripted reply, then the revision with a new option.
    const discussed = await waitFor(
      async () => {
        const d = await detailOf(page, created.id);
        const r = d.questions?.find((q) => q.id === request.id);
        return d.summary.status.kind === "awaitingAnswer" && r?.state.kind === "pending" && r.revision > request.revision ? d : null;
      },
      { timeout: 30_000, interval: 150, message: "the discussion revised the question and ended" },
    );
    const revised = discussed.questions.find((q) => q.id === request.id);
    assert.deepEqual(
      revised.questions[0].options.map((o) => o.id),
      ["kelvin", "celsius", "kelvin_caption"],
      "revise_question kept the existing ids and added one",
    );
    const discussionMessage = discussed.transcript.find((t) => t.kind === "userText" && t.text === DISCUSSION);
    assert.equal(discussionMessage?.question, request.id, "the discussion message is about the request");
    await panel.locator("[data-testid=agents-assistant-text]", { hasText: "I added an option" }).waitFor({ timeout: 10_000 });
    await waitFor(async () => (await card.getAttribute("data-revision")) === String(revised.revision), { message: "card at the new revision" });
    const marks = card.getByTestId("agents-question-new");
    assert.equal(await marks.count(), 1, "one option marked New");
    assert.match(
      (await marks.first().evaluate((el) => el.closest(".ui-option-list__option")?.textContent ?? "")) ?? "",
      new RegExp(NEW_OPTION),
    );
    await waitFor(async () => (await about.count()) === 0, { message: "the About chip is gone after sending" });
    assert.equal(await field.inputValue(), DRAFT, "the unrelated draft comes back after the discussion");
    assert.ok(await disabled(send), "Send is disabled again while the question waits");
    await bothThemes(page, "question-discussed");
    step("Chat about it: About chip, focus, draft kept; discussion reply and revise_question (revision bumped, new option marked New)", {
      revision: revised.revision,
    });

    // --- 4. Answer ------------------------------------------------------------
    const sendAnswer = card.getByTestId("agents-question-send");
    assert.ok(await disabled(sendAnswer), "Send answer needs an answer to every question");
    await card.locator("[data-question-id=q1]").getByText(NEW_OPTION, { exact: true }).click();
    assert.ok(await card.getByText("1 of 2 answered", { exact: true }).isVisible());
    assert.ok(await disabled(sendAnswer), "one of two answered is not enough");
    await card.locator("[data-question-id=q2]").getByText("kJ/mol", { exact: true }).click();
    assert.ok(await card.getByText("2 of 2 answered", { exact: true }).isVisible());
    assert.ok(!(await disabled(sendAnswer)), "complete answers can be sent");
    await sendAnswer.click();
    const done = await waitStatus(page, created.id, ["completed"]);
    const answered = done.questions.find((q) => q.id === request.id);
    assert.equal(answered.state.kind, "answered");
    const answers = [
      { questionId: "q1", value: { kind: "option", optionId: "kelvin_caption" } },
      { questionId: "q2", value: { kind: "option", optionId: "kjmol" } },
    ];
    assert.deepEqual(
      [...answered.state.answers].sort((a, b) => a.questionId.localeCompare(b.questionId)),
      answers,
      "the recorded answers",
    );
    assert.equal(done.questions.length, 1, "no new request");
    await waitFor(async () => (await card.getAttribute("data-state")) === "answered", { message: "card answered" });
    assert.deepEqual(await card.getByTestId("agents-question-answered").allTextContents(), [`Answered: ${NEW_OPTION}`, "Answered: kJ/mol"]);
    assert.equal(await card.getByRole("button").count(), 0, "an answered card has no controls");
    assert.equal(await panel.getByTestId("agents-needs-question").count(), 0, "nothing needs you any more");
    assert.equal(await taskStatus.getAttribute("data-status"), "completed");
    // The backend is idempotent for the same answer and refuses a different one.
    await invoke(page, "agents_question_answer", { task: created.id, request: request.id, revision: answered.revision, answers });
    const refused = await invokeError(page, "agents_question_answer", {
      task: created.id,
      request: request.id,
      revision: answered.revision,
      answers: [answers[0], { questionId: "q2", value: { kind: "option", optionId: "ev" } }],
    });
    assert.equal(refused.error?.code, "agents.questionResolved", JSON.stringify(refused));
    await bothThemes(page, "question-answered");
    step("Send answer enabled only when complete; the task resumed and completed; same answer again is a no-op, a different one is refused", {
      refused: refused.error?.code,
    });

    // --- 5. The Agents page: the same answered state, once ---------------------
    await panel.getByTestId("agents-task-menu").click();
    await page.getByRole("menuitem", { name: "Open in Agents page" }).click();
    const agentsPage = page.getByTestId("agents-page");
    await agentsPage.waitFor({ timeout: 10_000 });
    await agentsPage.locator(`[data-testid=agents-page-task-detail][data-task-id="${created.id}"]`).waitFor({ timeout: 10_000 });
    const pageCards = agentsPage.getByTestId("agents-question");
    await pageCards.first().waitFor({ timeout: 10_000 });
    assert.equal(await pageCards.count(), 1, "one card on the page");
    assert.equal(await pageCards.first().getAttribute("data-state"), "answered");
    assert.deepEqual(await pageCards.first().getByTestId("agents-question-answered").allTextContents(), [
      `Answered: ${NEW_OPTION}`,
      "Answered: kJ/mol",
    ]);
    assert.equal(await agentsPage.getByText(DISCUSSION, { exact: true }).count(), 1, "the discussion message once");
    assert.equal(await agentsPage.getByTestId("agents-page-status").getAttribute("data-status"), "completed");
    await bothThemes(page, "question-agents-page");
    step("Agents page shows the same answered request once, and the discussion message once");

    const errors = app.consoleMessages.filter((m) => m.startsWith("error:") || m.startsWith("pageerror:"));
    assert.deepEqual(errors, [], "no console errors");
  } finally {
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2)).catch(() => {});
    await app.close();
  }

  const devHome = process.env.EXPOSITION_DEV_HOME;
  if (devHome) {
    const target = join(devHome, "ui-reference", "agents-questions");
    await mkdir(target, { recursive: true });
    for (const path of shots) await copyFile(path, join(target, path.split(/[\\/]/).pop()));
    console.log(`copied ${(await readdir(target)).length} screenshots to ${target}`);
  }
}
