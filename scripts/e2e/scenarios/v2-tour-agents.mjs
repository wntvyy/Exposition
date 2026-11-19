// V2 tour of the agent surfaces (debug build, scripted provider — development
// only; it replaces only the model: the task loop, persistence, transcript,
// approvals, questions, attachments, and UI are the real ones; no
// model-provider call is made).
//
// Populated states, each in all four themes (Light, Dark, Ocean, Sage):
// - the Agents page with several tasks (long title, every headline status),
//   a long response with prose, a Python fence, inline and display math, a
//   list and a table, and the user's message with three attachments;
// - a failed task: a failed tool call (its turn starts expanded) and the
//   "Task failed" banner with the provider's message;
// - a structured question card with Needs you (page and narrow panel);
// - the Agent panel at a normal (~480 px) and a narrow (~360 px) width with
//   an approval card and its complete payload (an invisible character shown
//   as ⟨U+…⟩), keyboard focus on the card after a Needs you jump, and two
//   pasted image attachments in the composer;
// - the Options sheet's Model tab with a long model name (the per-message
//   choice and the saved default);
// - the Agent view's task list.
//
// Shots go to test-results/e2e/v2-tour-agents/ and, when EXPOSITION_DEV_HOME
// is set, to $EXPOSITION_DEV_HOME/ui-reference/v2/agents/.
import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { applyTheme, shootThemes } from "../theme-helpers.mjs";
import { invoke, openWorkspace, waitFor } from "../workspace-helpers.mjs";

const AREA = "agents";

const MAIN = [
  "= Results",
  "",
  "The corrected calibration of 2026-03-14 gives the rate constants in Table 2.",
  "The fit uses the Arrhenius form with a common pre-exponential factor.",
  "Residuals are below two percent at every temperature.",
  "",
].join("\n");

const LONG_TITLE =
  "Fit the Arrhenius parameters for Table 2 from the corrected calibration and report the activation energy with its uncertainty";

const FENCE = [
  "import numpy as np",
  "from scipy.optimize import curve_fit",
  "",
  "R = 8.314  # J/(mol K)",
  "T, k = np.loadtxt(\"data/rate_constants_2026-03-14_calibrated.csv\", delimiter=\",\", skiprows=1, unpack=True)",
  "",
  "def arrhenius(T, A, Ea):",
  "    return A * np.exp(-Ea / (R * T))",
  "",
  "(A, Ea), cov = curve_fit(arrhenius, T, k, p0=(1e10, 5e4))",
  "print(f\"Ea = {Ea / 1000:.1f} ± {np.sqrt(cov[1, 1]) / 1000:.1f} kJ/mol\")",
].join("\n");

const RESPONSE = [
  "The corrected calibration changes Table 2 only slightly. The rate constants follow the Arrhenius form",
  "",
  "$$",
  String.raw`k(T) = A \exp\!\left(-\frac{E_a}{R T}\right), \qquad \ln k = \ln A - \frac{E_a}{R}\,\frac{1}{T}`,
  "$$",
  "",
  String.raw`so a straight line in $1/T$ gives $E_a$ from the slope. A nonlinear fit on $k$ itself weights the high-temperature points less, which matters here because their relative error is about **2 %**.`,
  "",
  "The fit I ran on the calibrated data:",
  "",
  "```python",
  FENCE,
  "```",
  "",
  "What changes in the manuscript:",
  "",
  String.raw`- $E_a = 53.4 \pm 0.6$ kJ/mol (was $54.1 \pm 0.9$);`,
  String.raw`- $A = (2.31 \pm 0.08) \times 10^{10}\ \mathrm{s^{-1}}$;`,
  "- the residual plot stays below two percent at every temperature.",
  "",
  "| $T$ (K) | $k$ measured (s⁻¹) | $k$ fitted (s⁻¹) | Residual |",
  "|---:|---:|---:|---:|",
  "| 298.15 | 0.0102 | 0.0101 | 1.0 % |",
  "| 308.15 | 0.0204 | 0.0207 | −1.5 % |",
  "| 318.15 | 0.0401 | 0.0398 | 0.7 % |",
  "| 328.15 | 0.0746 | 0.0751 | −0.7 % |",
].join("\n");

const Q_UNIT = "Which temperature unit should Table 2 use?";

const disabled = (locator) =>
  locator.evaluate((el) => el.hasAttribute("disabled") || el.getAttribute("data-disabled") === "true" || el.getAttribute("aria-disabled") === "true");

async function waitStatus(page, id, kinds, timeout = 30_000) {
  return waitFor(
    async () => {
      const d = await invoke(page, "agents_task_detail", { task: id });
      return kinds.includes(d.summary.status.kind) ? d : null;
    },
    { timeout, interval: 150, message: `task ${id} status in ${kinds}` },
  );
}

/** Base64 PNG bytes drawn on a canvas in the page (a small bar chart). */
async function pngBase64(page, { width, height, bars }) {
  return page.evaluate(
    ([w, h, b]) => {
      const canvas = document.createElement("canvas");
      canvas.width = w;
      canvas.height = h;
      const ctx = canvas.getContext("2d");
      ctx.fillStyle = "#f4f6fa";
      ctx.fillRect(0, 0, w, h);
      ctx.fillStyle = "#3a6ea5";
      b.forEach((v, i) => ctx.fillRect(10 + i * 24, h - v, 16, v));
      return canvas.toDataURL("image/png").split(",")[1];
    },
    [width, height, bars],
  );
}

/** Paste images into a composer field the way the OS clipboard does (a paste event with image files). */
async function pasteImages(field, images) {
  await field.evaluate(async (el, list) => {
    const dt = new DataTransfer();
    for (const { name, data } of list) {
      const bytes = Uint8Array.from(atob(data), (c) => c.charCodeAt(0));
      dt.items.add(new File([bytes], name, { type: "image/png" }));
    }
    el.dispatchEvent(new ClipboardEvent("paste", { clipboardData: dt, bubbles: true, cancelable: true }));
  }, images);
}

export default async function v2TourAgents({ launchApp, artifactsDir, assert }) {
  const project = await mkdtemp(join(tmpdir(), "exposition-v2-agents-"));
  await writeFile(join(project, "main.typ"), MAIN);
  const env = { EXPOSITION_CREDENTIAL_NAMESPACE: `e2e-v2-agents-${Date.now()}` };
  const app = await launchApp({ env });
  const info = await invoke(app.page, "app_info");
  if (!info.debugBuild) {
    await app.close();
    throw new Error("v2-tour-agents needs a debug build (scripted provider)");
  }
  const evidence = { project, steps: [], shots: [] };
  const step = (name, data = {}) => {
    evidence.steps.push({ name, ...data });
    console.log(`ok: ${name}${Object.keys(data).length ? ` ${JSON.stringify(data)}` : ""}`);
  };
  // Task notifications (approval, question, failure) would cover the views: dismiss them before each shot.
  const clearToasts = async (page) => {
    const buttons = page.getByRole("button", { name: /^Dismiss notification/ });
    for (let i = 0; i < 20 && (await buttons.count()) > 0; i++) await buttons.first().click();
    await page.waitForTimeout(150);
  };
  const shoot = async (name, prepare) => {
    const paths = await shootThemes(app.page, {
      outDir: artifactsDir,
      area: AREA,
      name,
      prepare: async (page, theme) => {
        await clearToasts(page);
        if (prepare) await prepare(page, theme);
      },
    });
    evidence.shots.push(...paths);
  };
  try {
    const { page } = app;
    await page.setViewportSize({ width: 1440, height: 900 });
    const panel = page.getByTestId("agents-panel");
    await panel.waitFor({ timeout: 15_000 });
    await applyTheme(page, "light");
    const opened = await openWorkspace(page, project);
    const root = opened.root ?? project;
    await invoke(page, "agents_provider_update", {
      settings: { provider: "scripted", model: "claude-opus-5-5", effort: "medium", consent: "scopeContent" },
    });
    const pagesBar = page.locator('[aria-label="Pages"]');
    const gotoPage = async (name) => {
      await pagesBar.getByRole("button", { name, exact: true }).click();
      await page.waitForTimeout(150);
    };
    const create = (prompt, extra = {}) =>
      invoke(page, "agents_task_create", {
        request: {
          prompt,
          workspaceRoot: root,
          scope: { kind: "workspace" },
          permission: "reviewEdits",
          execution: "disabled",
          consent: "scopeContent",
          budgets: null,
          ...extra,
        },
      });

    // ---------------------------------------------------------------- tasks
    // 1. A completed task with a long title, three attachments, a read, and a long response.
    const ingest = async (name, mediaType, data) => {
      const outcome = await invoke(page, "agents_attachments_add_bytes", { name, mediaType, data });
      assert.equal(outcome.kind, "added", `attachment ${name}: ${JSON.stringify(outcome)}`);
      return outcome.attachment.id;
    };
    const csv = Buffer.from("T,k\n298.15,0.0102\n308.15,0.0204\n318.15,0.0401\n328.15,0.0746\n").toString("base64");
    const notes = Buffer.from("# Lab notebook, 2026-03-14\n\nCalibration corrected; thermocouple offset -0.4 K.\n").toString("base64");
    const attachments = [
      await ingest("rate_constants_2026-03-14_calibrated_with_thermocouple_offset.csv", "text/csv", csv),
      await ingest("lab-notebook-2026-03-14.md", "text/markdown", notes),
      await ingest("residuals.png", "image/png", await pngBase64(page, { width: 120, height: 80, bars: [30, 50, 65, 40] })),
    ];
    await invoke(page, "agents_dev_load_script", {
      script: {
        turns: [
          {
            steps: [
              { text: "I will read the results section first." },
              { toolUse: { id: "toolu_read", name: "read_file", input: { path: "main.typ" } } },
            ],
          },
          { steps: [{ text: RESPONSE }] },
        ],
      },
    });
    const completed = await create(LONG_TITLE, { permission: "readOnly", attachments });
    await waitStatus(page, completed.id, ["completed"]);
    step("completed task with three attachments and a long response");

    // 2. A failed task: a failed read (the turn starts expanded), then a provider error.
    await invoke(page, "agents_dev_load_script", {
      script: {
        turns: [
          {
            steps: [
              { text: "Reading the raw calibration export." },
              { toolUse: { id: "toolu_missing", name: "read_file", input: { path: "data/raw_export_2026-03-14.csv" } } },
            ],
          },
          { steps: [{ error: { kind: "invalid_request_error", message: "prompt is too long: 212480 tokens > 200000 maximum" } }] },
        ],
      },
    });
    const failed = await create("Compare the raw calibration export with Table 2", { permission: "readOnly" });
    await waitStatus(page, failed.id, ["failed"], 60_000);
    step("failed task (failed tool call, provider error)");

    // 3. A structured question.
    await invoke(page, "agents_dev_load_script", {
      script: {
        turns: [
          {
            steps: [
              { text: "Before I change Table 2 I need one decision from you." },
              {
                toolUse: {
                  id: "ask",
                  name: "ask_user",
                  input: {
                    questions: [
                      {
                        prompt: Q_UNIT,
                        explanation: "The fit uses Kelvin; the lab notebook records Celsius.",
                        options: [
                          { id: "kelvin", label: "Kelvin", consequence: "Matches the fit and the figure axis", recommended: true },
                          { id: "celsius", label: "Celsius", consequence: "Matches the lab notebook", recommended: false },
                        ],
                      },
                    ],
                    allowCustom: true,
                  },
                },
              },
            ],
          },
          { steps: [{ text: "Thanks." }] },
        ],
      },
    });
    const question = await create("Tidy Table 2 of the Arrhenius fit");
    await waitStatus(page, question.id, ["awaitingAnswer"]);
    step("question task waits for the answer");

    // 4. An approval with a multi-line payload (one zero-width space in the new text).
    await invoke(page, "agents_dev_load_script", {
      script: {
        turns: [
          {
            steps: [
              { text: "I will read main.typ and then update the results paragraph." },
              { toolUse: { id: "toolu_read", name: "read_file", input: { path: "main.typ" } } },
            ],
          },
          {
            steps: [
              { text: "Replacing the two sentences about the fit." },
              {
                toolUse: {
                  id: "toolu_edit",
                  name: "edit_document",
                  input: {
                    path: "main.typ",
                    base_revision: "$ref:toolu_read/revision",
                    edits: [
                      {
                        old_text: "The fit uses the Arrhenius form with a common pre-exponential factor.",
                        new_text:
                          "The fit uses the Arrhenius form with a common pre-exponential factor,\n$A = (2.31 plus.minus 0.08) times 10^10 \"s\"^(-1)$, and gives\n$E_a = 53.4 plus.minus 0.6 \"kJ/mol\"$ (previously 54.1​ plus.minus 0.9).",
                      },
                      {
                        old_text: "Residuals are below two percent at every temperature.",
                        new_text: "Residuals stay below two percent at every temperature (@fig:residuals).",
                      },
                    ],
                  },
                },
              },
            ],
          },
          { steps: [{ text: "Done." }] },
        ],
      },
    });
    const approval = await create("Update the results paragraph with the new fit");
    await waitStatus(page, approval.id, ["awaitingApproval"]);
    step("approval task waits for approval");

    // ---------------------------------------------------------------- Agents page
    await gotoPage("Agents");
    const agentsPage = page.getByTestId("agents-page");
    await agentsPage.waitFor();
    const openOnPage = async (id) => {
      await agentsPage.locator(`[data-testid=agents-page-task][data-task-id="${id}"]`).click();
      await agentsPage.locator(`[data-testid=agents-page-task-detail][data-task-id="${id}"]`).waitFor();
      await page.waitForTimeout(200);
    };
    assert.equal(await agentsPage.getByTestId("agents-page-task").count(), 4, "four tasks on the page");

    await openOnPage(completed.id);
    const response = agentsPage.getByTestId("agents-assistant-text").last();
    await response.locator("[data-math=display] .katex-display").waitFor();
    await response.locator(".ui-md-tok-keyword").first().waitFor({ timeout: 10_000 });
    assert.equal(await agentsPage.getByTestId("agents-message-attachments").getByTestId("agents-attachment").count(), 3);
    const transcript = agentsPage.getByTestId("agents-transcript");
    await shoot("page-response-top", async () => {
      await transcript.evaluate((el) => (el.scrollTop = 0));
      await page.waitForTimeout(100);
    });
    await shoot("page-response-code", async () => {
      // Scroll the transcript (not the fence sideways) so the fence's header is near the top.
      const header = response.locator(".ui-md-code__header").first();
      await transcript.evaluate((log, top) => (log.scrollTop += top - log.getBoundingClientRect().top - 120), (await header.boundingBox()).y);
      await page.waitForTimeout(100);
    });
    step("Agents page: task list, long title, attachments, long response with code and math");

    await openOnPage(failed.id);
    await transcript.locator(".ui-banner__title", { hasText: "Task failed" }).waitFor();
    assert.equal(await agentsPage.locator("[data-testid=agents-turn][data-failed]").count() >= 1, true, "a turn marked failed");
    await shoot("page-failed", async () => {
      await transcript.evaluate((el) => (el.scrollTop = el.scrollHeight));
      await page.waitForTimeout(100);
    });
    step("Agents page: failed task with the failed tool call and the error banner");

    await openOnPage(question.id);
    await agentsPage.getByTestId("agents-question").waitFor();
    assert.equal(await agentsPage.getByTestId("agents-needs-question").count(), 1);
    await shoot("page-question", async () => {
      await agentsPage.getByTestId("agents-question").evaluate((el) => el.scrollIntoView({ block: "center" }));
      await page.waitForTimeout(100);
    });
    step("Agents page: question card and Needs you");

    // ---------------------------------------------------------------- Agent panel
    await openOnPage(approval.id);
    await agentsPage.getByTestId("agents-show-in-panel").click();
    await panel.getByTestId("agents-task-view").waitFor();
    await panel.getByTestId("agents-approval").waitFor();
    const handle = page.getByRole("separator", { name: "Resize auxiliary panel" });
    const resizeTo = async (target) => {
      for (let i = 0; i < 3; i++) {
        const width = await panel.evaluate((el) => el.getBoundingClientRect().width);
        if (Math.abs(width - target) <= 12) return Math.round(width);
        const box = await handle.boundingBox();
        await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
        await page.mouse.down();
        await page.mouse.move(box.x + box.width / 2 - (target - width), box.y + box.height / 2, { steps: 8 });
        await page.mouse.up();
        await page.waitForTimeout(300);
      }
      return Math.round(await panel.evaluate((el) => el.getBoundingClientRect().width));
    };
    const normal = await resizeTo(480);
    assert.ok(normal >= 440 && normal <= 520, `normal Agent panel width ${normal}`);

    // The complete payload, with the invisible character made visible.
    const payload = panel.getByTestId("agents-approval-payload");
    const payloadText = await payload.textContent();
    assert.ok(payloadText.includes("Residuals stay below two percent"), "the payload names every edit");
    evidence.payloadHasMarker = /⟨U\+200B⟩/.test(payloadText);
    assert.ok(await panel.getByTestId("agents-approve-task").isVisible(), "Approve for this task is offered");
    assert.ok(await panel.getByTestId("agents-approval-scope").isVisible(), "its scope sentence is visible");

    // Two pasted images in the follow-up composer.
    const field = panel.getByTestId("agents-follow-up-text");
    const images = [
      { name: "image.png", data: await pngBase64(page, { width: 140, height: 90, bars: [20, 60, 45, 70, 30] }) },
      { name: "image.png", data: await pngBase64(page, { width: 140, height: 90, bars: [70, 35, 55, 25, 65] }) },
    ];
    await pasteImages(field, images);
    await waitFor(async () => (await panel.getByTestId("agents-draft-attachment-list").getByTestId("agents-attachment").count()) === 2, {
      timeout: 15_000,
      message: "two pasted images attached",
    });
    await field.fill("Also mention the thermocouple offset in the caption.");

    // Keyboard: Needs you → Enter moves focus to the approval card.
    // The jump centers the card; `block` then shows its start (banner) or its end (the decision).
    const focusApproval = (block) => async () => {
      const link = panel.getByTestId("agents-needs-approval");
      await link.focus();
      await page.keyboard.press("Enter");
      await page.waitForFunction(() => document.activeElement?.getAttribute("data-testid") === "agents-approval", null, { timeout: 5000 });
      await panel.getByTestId("agents-approval").evaluate((el, b) => el.scrollIntoView({ block: b }), block);
      await page.waitForTimeout(100);
    };
    await shoot("panel-approval", focusApproval("start"));
    step("Agent panel (normal width): approval card with the complete payload, focus after Needs you, two pasted attachments", {
      width: normal,
      payloadHasMarker: evidence.payloadHasMarker,
    });

    const narrow = await resizeTo(360);
    assert.ok(narrow >= 340 && narrow <= 380, `narrow Agent panel width ${narrow}`);
    const noSideScroll = async () => {
      const overflow = await panel.evaluate((el) => {
        const scroll = el.querySelector("[data-testid=agents-transcript]");
        return { panel: el.scrollWidth - el.clientWidth, transcript: scroll ? scroll.scrollWidth - scroll.clientWidth : 0 };
      });
      assert.ok(overflow.panel <= 1 && overflow.transcript <= 1, `no horizontal scroll in the narrow panel: ${JSON.stringify(overflow)}`);
    };
    await noSideScroll();
    await shoot("panel-approval-narrow", async () => {
      await payload.evaluate((el) => el.scrollIntoView({ block: "start" }));
      await page.waitForTimeout(100);
    });
    await shoot("panel-approval-narrow-actions", focusApproval("end"));
    step("Agent panel (narrow): approval payload and actions", { width: narrow });

    // The question in the narrow panel.
    await panel.getByTestId("agents-show-tasks").click();
    await panel.locator(`[data-testid=agents-task-item][data-task-id="${question.id}"]`).click();
    await panel.getByTestId("agents-question").waitFor();
    await noSideScroll();
    await shoot("panel-question-narrow", async () => {
      await panel.getByTestId("agents-question").evaluate((el) => el.scrollIntoView({ block: "start" }));
      await page.waitForTimeout(100);
    });
    step("Agent panel (narrow): question card");

    // The task list in the panel.
    await panel.getByTestId("agents-show-tasks").click();
    await panel.getByTestId("agents-task-list").waitFor();
    await shoot("panel-tasks-narrow");

    // ---------------------------------------------------------------- Options sheet: long model name
    const LONG_MODEL = "claude-opus-5-5-research-preview-20261101-extended-context-structured-outputs";
    await invoke(page, "agents_provider_update", {
      settings: { provider: "anthropic", model: LONG_MODEL, effort: "high", consent: "scopeContent" },
    });
    await resizeTo(480);
    await panel.locator(`[data-testid=agents-task-item][data-task-id="${approval.id}"]`).click();
    await panel.getByTestId("agents-task-view").waitFor();
    const options = panel.getByTestId("agents-options");
    const sheet = page.getByRole("dialog", { name: "Options" });
    const openSheet = async () => {
      if (!(await sheet.isVisible().catch(() => false))) {
        await options.click();
        await sheet.waitFor();
      }
      await sheet.getByRole("tab", { name: /^Model/ }).click();
      await sheet.getByTestId("model-groups").waitFor();
      await page.waitForTimeout(150);
    };
    await openSheet();
    const longOption = sheet.locator(`[data-testid=model-option][data-model-id="${LONG_MODEL}"]`);
    if ((await longOption.count()) > 0 && !(await disabled(longOption.first()))) {
      await longOption.first().click();
      await waitFor(async () => ((await options.textContent()) ?? "").includes(LONG_MODEL), { message: "the long model in the Options summary" });
      evidence.longModelChosen = true;
    } else {
      evidence.longModelChosen = false;
    }
    await openSheet();
    // The sheet stays within the panel: no horizontal overflow from the long name.
    await shoot("panel-options-long-model", openSheet);
    const sheetOverflow = await sheet.evaluate((el) => {
      const box = el.getBoundingClientRect();
      const wide = [...el.querySelectorAll("*")]
        .filter((n) => n.getBoundingClientRect().right > box.right + 1)
        .slice(0, 8)
        .map((n) => `${n.tagName.toLowerCase()}.${String(n.className).split(" ").join(".")}[${n.getAttribute("data-testid") ?? ""}] ${Math.round(n.getBoundingClientRect().right - box.right)}`);
      return { overflow: el.scrollWidth - el.clientWidth, wide };
    });
    evidence.sheetOverflow = sheetOverflow;
    assert.ok(sheetOverflow.overflow <= 1, `the Options sheet does not scroll sideways (${JSON.stringify(sheetOverflow)})`);
    step("Options sheet, Model tab, long model name", { longModelChosen: evidence.longModelChosen });
    await page.keyboard.press("Escape");

    // The new-task composer with the unconfigured provider (an error the user can act on).
    await gotoPage("Agents");
    await agentsPage.getByTestId("agents-page-new-task").first().click();
    await agentsPage.getByTestId("agents-page-compose").waitFor();
    await shoot("page-new-task-unconfigured");
    step("Agents page: new task with the provider not configured");

    await applyTheme(page, "light");
    const errors = app.consoleMessages.filter((m) => m.startsWith("error:") || m.startsWith("pageerror:"));
    assert.deepEqual(errors, [], "no console errors");
  } finally {
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2)).catch(() => {});
    await app.close();
  }
}
