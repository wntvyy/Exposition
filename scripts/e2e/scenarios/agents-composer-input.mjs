// Agent composer input in the real app (debug build), on both Agent
// surfaces, against a local mock of the OpenAI API (127.0.0.1; no real
// provider and no real key: a namespaced development credential entry).
//
// Sidebar panel, new task:
//  1. Shift+Enter inserts a line break and sends nothing.
//  2. An IME composition (CDP Input.imeSetComposition) with a key event as
//     the input method reports it (keyCode 229) sends nothing; the committed
//     text is in the field. This is the composition path Chromium exposes to
//     automation, not a real installed input method.
//  3. A short paste from the OS clipboard is inserted as text; a large one
//     (150 lines, non-ASCII) becomes "pasted-text-1.txt" with the request
//     text unchanged and nothing sent; Undo removes it; pasted again it is
//     "pasted-text-2.txt".
//  4. The draft (text and attachments) is there again after leaving the
//     composer and coming back.
//  5. A real OS drop (OLE DoDragDrop, CF_HDROP: os-drag.ps1) of two files and
//     a folder onto the composer: the composer is highlighted during the
//     drag, the files are attached, the folder is refused with its reason,
//     nothing is imported into the project, nothing is sent.
//  6. Enter sends once: one request reaches the mock, carrying the typed
//     text, the pasted text exactly as the paste handler received it, and the
//     dropped files' content.
// Agents page, follow-up to that task:
//  7. Shift+Enter, a large paste, a real OS drop onto the transcript (the
//     page's composer is highlighted and takes it), then Enter: one more
//     request with the follow-up's text and attachments.
//
// Screens → test-results/e2e/agents-composer-input/ and, with
// EXPOSITION_DEV_HOME, $EXPOSITION_DEV_HOME/ui-reference/agents-composer-input/.
import { createServer } from "node:http";
import { copyFile, mkdir, mkdtemp, readdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { clientPoint, osDragFiles } from "../os-drag.mjs";
import { consoleErrors, notifications, ps, setOsClipboardText, setTheme, settle } from "../p1-helpers.mjs";
import { invoke, openWorkspace, waitFor } from "../workspace-helpers.mjs";

const FAKE_KEY = "sk-proj-e2e-not-a-real-key-7c3f1a9e2b55";
const ALPHA = "alpha: the first measurement is 4.2 kPa\n";
const DATA = "t,p\n0,4.2\n1,3.9\n";
const PAGE_NOTE = "# Page note\nDropped on the Agents page.\n";
/** 150 lines with non-ASCII text, tabs, and trailing spaces: more than 100 lines, fewer than 8,000 characters. */
const PASTED = Array.from({ length: 150 }, (_, i) => `\tlet x${i} = "héllo — 数据";  `).join("\n");

/** The mock OpenAI API: records every request; answers every streamed response with one sentence. */
async function startMock() {
  const seen = [];
  const server = createServer((req, res) => {
    const chunks = [];
    req.on("data", (c) => chunks.push(c));
    req.on("end", () => {
      const raw = Buffer.concat(chunks).toString("utf8");
      let body = null;
      try {
        body = raw ? JSON.parse(raw) : null;
      } catch {
        body = { unparsed: raw.length };
      }
      seen.push({ method: req.method, url: req.url, body });
      if (req.method === "GET" && req.url?.startsWith("/v1/models")) {
        res.writeHead(200, { "content-type": "application/json" });
        res.end(JSON.stringify({ object: "list", data: [{ id: "gpt-6.2-sol", object: "model", created: 1_760_000_000, owned_by: "system" }] }));
        return;
      }
      if (req.method === "POST" && req.url?.startsWith("/v1/responses")) {
        const model = body?.model ?? "unknown";
        const text = "Read.";
        const events = [
          { type: "response.created", response: { id: "resp_e2e", model, status: "in_progress" } },
          { type: "response.output_text.delta", output_index: 0, delta: text },
          {
            type: "response.completed",
            response: {
              id: "resp_e2e",
              model,
              status: "completed",
              output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text }] }],
              usage: { input_tokens: 1200, input_tokens_details: { cached_tokens: 0 }, output_tokens: 2 },
            },
          },
        ];
        res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
        res.end(events.map((e) => `event: ${e.type}\ndata: ${JSON.stringify(e)}\n\n`).join(""));
        return;
      }
      res.writeHead(404, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: { message: `no mock for ${req.method} ${req.url}` } }));
    });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  return { base: `http://127.0.0.1:${port}`, seen, close: () => new Promise((r) => server.close(r)) };
}

/** Put a file's text (UTF-8) on the OS clipboard as plain text. */
function setClipboardFromFile(path) {
  ps(`Set-Clipboard -Value ([System.IO.File]::ReadAllText('${path.replace(/'/g, "''")}', [System.Text.Encoding]::UTF8))`);
}

const responses = (mock) => mock.seen.filter((r) => r.method === "POST" && r.url?.startsWith("/v1/responses"));
/** The text parts of the last user message of a request. */
function lastUserTexts(request) {
  const user = [...request.body.input].reverse().find((item) => item.role === "user");
  return user.content.filter((p) => p.type === "input_text").map((p) => p.text);
}
const attachmentText = (name, mediaType, content) => `<attachment name="${name}" media_type="${mediaType}">\n${content}\n</attachment>`;

export default async function agentsComposerInput({ launchApp, artifactsDir, assert }) {
  if (process.platform !== "win32") {
    console.log("SKIPPED: agents-composer-input drives the Windows clipboard and a real OS drag and drop");
    return;
  }
  const mock = await startMock();
  const project = await mkdtemp(join(tmpdir(), "exposition-composer-input-project-"));
  await writeFile(join(project, "main.typ"), "= Notes\nThe cat sat on the mat.\n");
  // Dropped files live outside the project: attaching never imports them.
  const outside = await mkdtemp(join(tmpdir(), "exposition-composer-input-files-"));
  const alpha = join(outside, "drop-alpha.txt");
  const data = join(outside, "drop-data.csv");
  const folder = join(outside, "drop-folder");
  const pageNote = join(outside, "drop-page.md");
  const pasteFile = join(outside, "paste-source.txt");
  await writeFile(alpha, ALPHA);
  await writeFile(data, DATA);
  await mkdir(folder);
  await writeFile(join(folder, "inside.txt"), "inside the folder\n");
  await writeFile(pageNote, PAGE_NOTE);
  await writeFile(pasteFile, PASTED);
  const namespace = `e2e-${Date.now()}`;
  const env = { EXPOSITION_CREDENTIAL_NAMESPACE: namespace, EXPOSITION_DEV_OPENAI_BASE_URL: mock.base };
  const evidence = { project, namespace, mock: mock.base, steps: [], drops: [], notDriven: [] };
  const step = (name, detail = {}) => evidence.steps.push({ name, ...detail });
  const shots = [];
  const shot = async (page, name, locator) => {
    await settle(page);
    const path = join(artifactsDir, `${name}.png`);
    if (locator) await locator.screenshot({ path });
    else await page.screenshot({ path });
    shots.push(path);
  };

  const app = await launchApp({ env });
  const info = await invoke(app.page, "app_info");
  if (!info.debugBuild) {
    await app.close();
    await mock.close();
    throw new Error("agents-composer-input requires a debug build (it saves and clears a development OpenAI key)");
  }
  try {
    const { page } = app;
    const pid = app.child.pid;
    await page.setViewportSize({ width: 1440, height: 900 });
    await setTheme(page, "light");
    const panel = page.getByTestId("agents-panel");
    await panel.waitFor({ timeout: 15_000 });
    await openWorkspace(page, project);
    await invoke(page, "agents_provider_set_key", { key: FAKE_KEY, provider: "openai" });
    await invoke(page, "agents_provider_update", { settings: { provider: "openai", model: "gpt-6.2-sol", effort: "medium", consent: "scopeContent" } });
    // What the composer's paste handler receives, and the key events the field sees.
    await page.evaluate(() => {
      window.__pastes = [];
      window.__keys = [];
      document.addEventListener("paste", (e) => window.__pastes.push(e.clipboardData?.getData("text/plain") ?? ""), true);
      document.addEventListener("keydown", (e) => window.__keys.push({ key: e.key, keyCode: e.keyCode, isComposing: e.isComposing, shift: e.shiftKey }), true);
    });
    const taskCount = async () => (await invoke(page, "agents_task_list")).length;
    const chipNames = (scope) => scope.getByTestId("agents-attachment").locator(".ui-chip__label").allTextContents();

    // ------------------------------------------------------------------ sidebar panel
    await panel.getByTestId("agents-new-task").click();
    let composer = panel.getByTestId("agents-composer");
    await composer.waitFor();
    let field = composer.getByTestId("agents-prompt");
    assert.equal(
      await field.evaluate((el) => document.getElementById(el.getAttribute("aria-describedby") ?? "")?.textContent ?? ""),
      "Enter to send · Shift+Enter for a new line",
      "the field's keyboard hint is its accessible description",
    );

    // --- 1. Shift+Enter: a line break, nothing sent.
    await field.click();
    await page.keyboard.type("Compare the notes");
    await page.keyboard.press("Shift+Enter");
    await page.keyboard.type("with the data");
    assert.equal(await field.inputValue(), "Compare the notes\nwith the data");
    assert.equal(await taskCount(), 0, "Shift+Enter starts nothing");
    step("Shift+Enter inserted a line break and sent nothing");

    // --- 2. IME composition: the confirming key never sends.
    let ime = { driven: false };
    try {
      const cdp = await page.context().newCDPSession(page);
      await page.keyboard.type(" ");
      await cdp.send("Input.imeSetComposition", { text: "かな", selectionStart: 2, selectionEnd: 2 });
      await page.evaluate(() => (window.__keys = []));
      // The key event of an active input method: virtual key 229 ("Process").
      await cdp.send("Input.dispatchKeyEvent", { type: "rawKeyDown", key: "Process", code: "Enter", windowsVirtualKeyCode: 229, nativeVirtualKeyCode: 229 });
      await cdp.send("Input.dispatchKeyEvent", { type: "keyUp", key: "Process", code: "Enter", windowsVirtualKeyCode: 229, nativeVirtualKeyCode: 229 });
      const keys = await page.evaluate(() => window.__keys);
      await cdp.send("Input.insertText", { text: "かな" });
      await cdp.detach();
      ime = { driven: true, keys };
    } catch (error) {
      ime = { driven: false, error: String(error?.message ?? error) };
    }
    if (ime.driven) {
      assert.ok(
        ime.keys.some((k) => k.keyCode === 229 || k.isComposing),
        `the field saw the input method's key event: ${JSON.stringify(ime.keys)}`,
      );
      assert.equal(await taskCount(), 0, "the composition's key sends nothing");
      assert.equal(await field.inputValue(), "Compare the notes\nwith the data かな", "the committed composition is in the field");
    } else {
      evidence.notDriven.push(`IME composition over CDP: ${ime.error}`);
    }
    step("IME composition (CDP Input.imeSetComposition, key event with keyCode 229)", ime);

    // --- 3. Pastes from the OS clipboard.
    setOsClipboardText(" (short paste)");
    await page.keyboard.press("Control+V");
    await waitFor(async () => (await field.inputValue()).endsWith(" (short paste)"), { message: "a short paste is inserted as text" });
    assert.equal(await composer.getByTestId("agents-attachment").count(), 0, "a short paste attaches nothing");
    const typed = await field.inputValue();

    setClipboardFromFile(pasteFile);
    await page.keyboard.press("Control+V");
    await waitFor(async () => (await composer.getByTestId("agents-attachment").count()) === 1, { timeout: 15_000, message: "the large paste becomes an attachment" });
    assert.deepEqual(await chipNames(composer), ["pasted-text-1.txt"]);
    const chip = composer.getByTestId("agents-attachment").first();
    assert.equal(await chip.getAttribute("data-kind"), "text");
    assert.match(await chip.textContent(), /Text · \d+(\.\d)? KB/, "the chip shows the size");
    assert.equal(await field.inputValue(), typed, "the request text is unchanged by the large paste");
    const notice = composer.getByTestId("agents-paste-notice");
    assert.match(await notice.textContent(), /The pasted text was attached as pasted-text-1\.txt\./);
    assert.equal(await taskCount(), 0, "a paste sends nothing");
    const received = (await page.evaluate(() => window.__pastes)).at(-1);
    assert.equal(received.replace(/\r\n/g, "\n"), PASTED, "the paste handler received the clipboard text");
    await shot(page, "composer-input-pasted-panel-light", panel);
    // Undo, then paste again: a new attachment with the next name.
    await notice.getByTestId("agents-paste-undo").click();
    await waitFor(async () => (await composer.getByTestId("agents-attachment").count()) === 0, { message: "Undo removes the pasted attachment" });
    assert.equal(await field.inputValue(), typed);
    await field.click();
    await page.keyboard.press("Control+End");
    await page.keyboard.press("Control+V");
    await waitFor(async () => (await composer.getByTestId("agents-attachment").count()) === 1, { timeout: 15_000, message: "pasted again" });
    assert.deepEqual(await chipNames(composer), ["pasted-text-2.txt"]);
    step("short paste inline; large paste attached as pasted-text-1.txt; Undo; pasted again as pasted-text-2.txt", {
      receivedLength: received.length,
      receivedHasCarriageReturns: received.includes("\r"),
    });

    // --- 4. The draft is there again after leaving the composer.
    await panel.getByTestId("agents-show-tasks").click();
    await panel.getByTestId("agents-new-task").click();
    composer = panel.getByTestId("agents-composer");
    field = composer.getByTestId("agents-prompt");
    await composer.waitFor();
    assert.equal(await field.inputValue(), typed, "the draft text is restored");
    assert.deepEqual(await chipNames(composer), ["pasted-text-2.txt"], "the pasted attachment is restored");
    step("draft restored after leaving and reopening the new-task composer");

    // --- 5. A real OS drop of two files and a folder onto the composer.
    const projectBefore = (await readdir(project)).sort();
    let target = await clientPoint(page, field);
    const dropA = osDragFiles({ pid, x: target.x, y: target.y, paths: [alpha, data, folder], holdMs: 2500 });
    dropA.catch(() => {});
    const highlighted = await waitFor(() => panel.locator("[data-testid=agents-composer][data-drop-target]").count(), {
      timeout: 8000,
      message: "the composer is highlighted during the drag",
    }).catch(() => 0);
    if (highlighted) await shot(page, "composer-input-drop-target-panel-light", panel);
    const resultA = await dropA;
    evidence.drops.push({ drop: "drop-alpha.txt + drop-data.csv + drop-folder → panel composer", helper: resultA, highlighted: Boolean(highlighted) });
    assert.ok(resultA.onApp, `the drop point was on the app window (${JSON.stringify(resultA)})`);
    await waitFor(async () => (await composer.getByTestId("agents-attachment").count()) === 3, { timeout: 15_000, message: "the dropped files are attached" });
    assert.ok(highlighted, "the composer was highlighted during the drag");
    assert.deepEqual(await chipNames(composer), ["pasted-text-2.txt", "drop-alpha.txt", "drop-data.csv"]);
    const refused = await composer.getByTestId("agents-attachment-rejection").allTextContents();
    assert.deepEqual(refused, ["drop-folder: Folders cannot be attached. Attach the files inside it instead."], "the folder is refused with its reason");
    assert.equal(await panel.locator("[data-testid=agents-composer][data-drop-target]").count(), 0, "the highlight is gone after the drop");
    assert.equal(await page.locator("[data-drop-target]").count(), 0, "no other drop target is highlighted");
    assert.deepEqual((await readdir(project)).sort(), projectBefore, "nothing was imported into the project");
    assert.deepEqual(
      (await notifications(page)).filter((n) => /Import/i.test(n.title ?? "")),
      [],
      "no project import was started",
    );
    assert.ok(!(await composer.textContent()).includes(outside), "no absolute path is shown");
    assert.equal(await taskCount(), 0, "a drop starts nothing");
    assert.equal(responses(mock).length, 0, "no model request yet");
    await shot(page, "composer-input-dropped-panel-light", panel);
    step("OS drop onto the panel composer: highlight, two files attached, folder refused, nothing imported or sent", { refused });

    // --- 6. Enter sends once.
    await composer.getByRole("button", { name: "Dismiss" }).click();
    await field.click();
    await page.keyboard.press("Enter");
    await page.waitForSelector("[data-testid=agents-task-view]");
    await page.waitForFunction(() => document.querySelector("[data-testid=agents-task-status]")?.dataset.status === "completed", null, { timeout: 30_000 });
    assert.equal(await taskCount(), 1, "one task");
    assert.equal(responses(mock).length, 1, "one model request");
    const first = lastUserTexts(responses(mock)[0]);
    assert.ok(first.includes(typed), `the typed text with its line break: ${JSON.stringify(first.map((t) => t.slice(0, 40)))}`);
    assert.ok(first.includes(attachmentText("pasted-text-2.txt", "text/plain", received)), "the pasted text exactly as the paste handler received it");
    assert.ok(first.includes(attachmentText("drop-alpha.txt", "text/plain", ALPHA)), "the first dropped file's content");
    assert.ok(first.includes(attachmentText("drop-data.csv", "text/csv", DATA)), "the second dropped file's content");
    assert.ok(!first.some((t) => t.includes("inside the folder")), "nothing from inside the folder");
    const view = panel.getByTestId("agents-task-view");
    assert.deepEqual(await chipNames(view.getByTestId("agents-message-attachments")), ["pasted-text-2.txt", "drop-alpha.txt", "drop-data.csv"]);
    await shot(page, "composer-input-sent-panel-light", panel);
    step("Enter sent one request with the text, the pasted text, and the dropped files", { parts: first.length });

    // ------------------------------------------------------------------ Agents page
    const [task] = await invoke(page, "agents_task_list");
    await panel.getByTestId("agents-task-menu").click();
    await page.getByRole("menuitem", { name: "Open in Agents page" }).click();
    const agentsPage = page.getByTestId("agents-page");
    await agentsPage.locator(`[data-testid=agents-page-task-detail][data-task-id="${task.id}"]`).waitFor({ timeout: 10_000 });
    const pageComposer = agentsPage.getByTestId("agents-composer");
    const followUp = pageComposer.getByTestId("agents-follow-up-text");
    await followUp.click();
    await page.keyboard.type("Now the page note");
    await page.keyboard.press("Shift+Enter");
    await page.keyboard.type("and the paste again");
    assert.equal(await followUp.inputValue(), "Now the page note\nand the paste again");
    await page.keyboard.press("Control+V");
    await waitFor(async () => (await pageComposer.getByTestId("agents-attachment").count()) === 1, { timeout: 15_000, message: "the large paste on the Agents page" });
    assert.deepEqual(await chipNames(pageComposer), ["pasted-text-1.txt"], "each conversation numbers its own pastes");
    assert.equal(await followUp.inputValue(), "Now the page note\nand the paste again");
    // A real OS drop onto the transcript: the page's composer takes it.
    target = await clientPoint(page, agentsPage.getByTestId("agents-transcript"));
    const dropB = osDragFiles({ pid, x: target.x, y: target.y, paths: [pageNote], holdMs: 2500 });
    dropB.catch(() => {});
    const pageHighlighted = await waitFor(() => agentsPage.locator("[data-testid=agents-composer][data-drop-target]").count(), {
      timeout: 8000,
      message: "the page's composer is highlighted during the drag",
    }).catch(() => 0);
    if (pageHighlighted) await shot(page, "composer-input-drop-target-page-light");
    const resultB = await dropB;
    evidence.drops.push({ drop: "drop-page.md → Agents page transcript", helper: resultB, highlighted: Boolean(pageHighlighted) });
    assert.ok(resultB.onApp, `the drop point was on the app window (${JSON.stringify(resultB)})`);
    await waitFor(async () => (await pageComposer.getByTestId("agents-attachment").count()) === 2, { timeout: 15_000, message: "the dropped file is attached on the page" });
    assert.ok(pageHighlighted, "the page's composer was highlighted during the drag");
    assert.deepEqual(await chipNames(pageComposer), ["pasted-text-1.txt", "drop-page.md"]);
    assert.equal(await page.locator("[data-drop-target]").count(), 0, "the highlight is gone after the drop");
    assert.deepEqual((await readdir(project)).sort(), projectBefore, "nothing was imported into the project");
    assert.equal(responses(mock).length, 1, "a drop sends nothing");
    await shot(page, "composer-input-dropped-page-light");
    await followUp.click();
    await page.keyboard.press("Enter");
    await waitFor(async () => responses(mock).length === 2, { timeout: 30_000, message: "the follow-up's request" });
    await waitFor(async () => (await invoke(page, "agents_task_list"))[0].status.kind === "completed", { timeout: 30_000, message: "the follow-up completed" });
    await page.waitForTimeout(500);
    assert.equal(responses(mock).length, 2, "Enter sent the follow-up once");
    assert.equal(await taskCount(), 1, "the same task");
    const second = lastUserTexts(responses(mock)[1]);
    assert.ok(second.includes("Now the page note\nand the paste again"), "the follow-up text with its line break");
    assert.ok(second.includes(attachmentText("pasted-text-1.txt", "text/plain", received)), "the pasted text of the follow-up");
    assert.ok(second.includes(attachmentText("drop-page.md", "text/markdown", PAGE_NOTE)), "the file dropped on the page");
    assert.equal(await followUp.inputValue(), "", "the sent follow-up is cleared");
    await shot(page, "composer-input-sent-page-light");
    step("Agents page: Shift+Enter, large paste, OS drop onto the transcript, Enter sent one follow-up", { parts: second.length });

    assert.deepEqual(consoleErrors(app), [], "no console errors");
  } finally {
    await invoke(app.page, "agents_provider_clear_key", { provider: "openai" }).catch(() => {});
    await app.close();
    await mock.close();
  }
  await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2));

  const devHome = process.env.EXPOSITION_DEV_HOME;
  if (devHome) {
    const target = join(devHome, "ui-reference", "agents-composer-input");
    await mkdir(target, { recursive: true });
    for (const path of shots) await copyFile(path, join(target, path.split(/[\\/]/).pop()));
    console.log(`copied ${(await readdir(target)).length} screenshots to ${target}`);
  }
}
