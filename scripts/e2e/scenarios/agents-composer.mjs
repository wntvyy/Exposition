// Agent composer with OpenAI (API key) against a local mock of the OpenAI API,
// in the real app (debug build). The mock runs in this scenario's process on
// 127.0.0.1 and implements GET /v1/models and POST /v1/responses (streamed);
// debug builds accept only loopback URLs in EXPOSITION_DEV_OPENAI_BASE_URL.
// No real provider is called and no real key is used (a namespaced
// development credential entry). It cannot establish live OpenAI behavior.
//
// Covers: saving an OpenAI key in Settings › Model providers (its own
// connection; the key never reaches the webview), the grouped model menu
// populated from the connection's catalog (badges, exact IDs), choosing the
// default (Settings › Defaults for new tasks) and a per-task model (the
// composer's Options › Model), attaching two files through the backend's native file
// dialog (UI Automation), pasting a real screenshot from the OS clipboard
// (System.Windows.Forms.Clipboard.SetImage) and typing text after it, sending,
// the request the mock received (input_image with the PNG bytes, the text, the
// attached files' content, the chosen model, the bearer key), the transcript's
// attachment rows, and the same rows after reopening the app. Screenshots go
// to test-results/e2e/agents-composer/ and, when EXPOSITION_DEV_HOME is set,
// to $EXPOSITION_DEV_HOME/ui-reference/agents-composer/.
import { execFileSync } from "node:child_process";
import { createServer } from "node:http";
import { copyFile, mkdir, mkdtemp, readdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { withNativeDialog } from "../native-dialog.mjs";
import { runCommand } from "../palette.mjs";
import { consoleErrors, setTheme, settle } from "../p1-helpers.mjs";
import { invoke, openWorkspace, waitFor } from "../workspace-helpers.mjs";

const MAIN = "= Notes\nThe cat sat on the mat.\n";
const FAKE_KEY = "sk-proj-e2e-not-a-real-key-5b2e9c1d7a40";
const TYPED = "Compare the screenshot with both notes.";
const ALPHA = "alpha: the first measurement is 4.2 kPa\n";
const BETA = "beta: the second measurement is 3.9 kPa\n";
const CREATED = 1_760_000_000;

/** The mock OpenAI API: records every request; answers models and one streamed response. */
async function startMock() {
  const seen = [];
  const models = [
    { id: "gpt-6.2-sol", object: "model", created: CREATED + 30, owned_by: "system" },
    { id: "gpt-6.2-luna", object: "model", created: CREATED + 20, owned_by: "system" },
    { id: "ft:gpt-6.2-sol:exposition-e2e:composer-check:a1b2c3", object: "model", created: CREATED + 10, owned_by: "user" },
    { id: "text-embedding-3-large", object: "model", created: CREATED, owned_by: "system" },
  ];
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
      seen.push({ method: req.method, url: req.url, authorization: req.headers.authorization ?? null, project: req.headers["openai-project"] ?? null, body });
      if (req.method === "GET" && req.url?.startsWith("/v1/models")) {
        res.writeHead(200, { "content-type": "application/json" });
        res.end(JSON.stringify({ object: "list", data: models }));
        return;
      }
      if (req.method === "POST" && req.url?.startsWith("/v1/responses")) {
        const model = body?.model ?? "unknown";
        const text = "I compared the screenshot with both notes.";
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
              usage: { input_tokens: 1200, input_tokens_details: { cached_tokens: 0 }, output_tokens: 12 },
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

/** Put a real bitmap (a small drawn screenshot) on the Windows clipboard. */
function setClipboardImage() {
  const script = [
    "Add-Type -AssemblyName System.Windows.Forms",
    "Add-Type -AssemblyName System.Drawing",
    "$bmp = New-Object System.Drawing.Bitmap 160, 90",
    "$g = [System.Drawing.Graphics]::FromImage($bmp)",
    "$g.Clear([System.Drawing.Color]::FromArgb(255, 240, 244, 250))",
    "$g.FillRectangle([System.Drawing.Brushes]::SteelBlue, 12, 50, 24, 30)",
    "$g.FillRectangle([System.Drawing.Brushes]::SteelBlue, 48, 30, 24, 50)",
    "$g.FillRectangle([System.Drawing.Brushes]::SteelBlue, 84, 15, 24, 65)",
    "$g.Dispose()",
    "[System.Windows.Forms.Clipboard]::SetImage($bmp)",
    "if (-not [System.Windows.Forms.Clipboard]::ContainsImage()) { throw 'clipboard has no image' }",
    "Write-Output ok",
  ].join("; ");
  return execFileSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-STA", "-Command", script], { encoding: "utf8" }).trim();
}

/** Model ids of a provider's group in a model menu, in order. */
const groupOptions = (menu, provider) => menu.locator(`[data-testid=model-group][data-provider=${provider}] [data-testid=model-option]`);

export default async function agentsComposer({ launchApp, artifactsDir, assert }) {
  if (process.platform !== "win32") {
    console.log("SKIPPED: agents-composer drives the Windows file dialog and clipboard");
    return;
  }
  const mock = await startMock();
  const project = await mkdtemp(join(tmpdir(), "exposition-composer-project-"));
  await writeFile(join(project, "main.typ"), MAIN);
  // Attached files live outside the project: attaching never imports them.
  // No extension, so the dialog lists them by their full name.
  const outside = await mkdtemp(join(tmpdir(), "exposition-composer-files-"));
  const alpha = join(outside, "notes-alpha");
  const beta = join(outside, "notes-beta");
  await writeFile(alpha, ALPHA);
  await writeFile(beta, BETA);
  const namespace = `e2e-${Date.now()}`;
  const env = { EXPOSITION_CREDENTIAL_NAMESPACE: namespace, EXPOSITION_DEV_OPENAI_BASE_URL: mock.base };
  const evidence = { project, namespace, mock: mock.base, steps: [] };
  const step = (name, data = {}) => evidence.steps.push({ name, ...data });
  const shots = [];
  const shot = async (page, name, locator) => {
    await settle(page);
    const path = join(artifactsDir, `${name}.png`);
    if (locator) await locator.screenshot({ path });
    else await page.screenshot({ path });
    shots.push(path);
  };

  let app = await launchApp({ env });
  const dataDir = app.dataDir;
  const info = await invoke(app.page, "app_info");
  if (!info.debugBuild) {
    // The mock endpoint and the credential namespace exist only in debug
    // builds; never touch a release build's real credential entries.
    await app.close();
    await mock.close();
    throw new Error("agents-composer requires a debug build (it saves and clears an OpenAI key)");
  }
  let taskId = null;
  try {
    const { page } = app;
    const pid = app.child.pid;
    await page.setViewportSize({ width: 1440, height: 900 });
    await setTheme(page, "light");
    const panel = page.getByTestId("agents-panel");
    await panel.waitFor({ timeout: 15_000 });
    await openWorkspace(page, project);

    // --- OpenAI key on its own connection (Settings › Model providers); never in the webview.
    await runCommand(page, "Agent Provider Settings", /Agent Provider Settings/);
    const providers = page.getByTestId("settings-providers");
    await providers.waitFor();
    const openai = providers.getByTestId("settings-provider-openai");
    assert.match(await openai.getByTestId("agents-key-status").textContent(), /Not configured/);
    await openai.getByTestId("agents-key-input").locator("input").fill(FAKE_KEY);
    await openai.getByTestId("agents-key-save").click();
    await waitFor(async () => /OpenAI API key saved/.test((await openai.getByTestId("agents-settings-message").textContent().catch(() => "")) ?? ""), {
      message: "OpenAI key saved",
    });
    const status = await invoke(page, "agents_provider_status");
    const connections = Object.fromEntries(status.connections.map((c) => [c.provider, c.configured]));
    assert.equal(connections.openai, true, "the OpenAI connection is configured");
    assert.equal(connections.anthropic, false, "the Anthropic connection is untouched");
    assert.match(await providers.getByTestId("settings-provider-anthropic").getByTestId("agents-key-status").textContent(), /Not configured/);
    assert.ok(!JSON.stringify(status).includes(FAKE_KEY), "provider status never contains the key");
    const leaks = await page.evaluate((key) => document.documentElement.outerHTML.includes(key), FAKE_KEY);
    assert.equal(leaks, false, "the key is not in the webview after saving");
    step("OpenAI key saved to its own credential entry", { connections });

    // --- The default: OpenAI, from the populated, grouped model menu.
    const defaults = providers.getByTestId("agents-default");
    await defaults.getByTestId("agents-default-model").click();
    const menu = page.getByTestId("agents-default-menu");
    await menu.waitFor();
    const list = menu.getByRole("listbox", { name: "OpenAI models" });
    await list.waitFor({ timeout: 15_000 });
    const ids = await groupOptions(menu, "openai").evaluateAll((els) => els.map((e) => e.getAttribute("data-model-id")));
    assert.deepEqual(ids.slice(0, 2), ["gpt-6.2-sol", "gpt-6.2-luna"], `supported models first: ${ids}`);
    assert.ok(ids.includes("text-embedding-3-large"), "an unsupported model is listed (not selectable)");
    assert.equal(await list.locator('[data-model-id="text-embedding-3-large"]').getAttribute("aria-disabled"), "true");
    assert.match(await list.locator('[data-model-id^="ft:"]').textContent(), /Unverified/);
    // The connection's suggestion (the newest model Exposition supports) is marked, not chosen silently.
    assert.match(await list.locator('[data-model-id="gpt-6.2-sol"]').textContent(), /Suggested/);
    assert.doesNotMatch(await list.locator('[data-model-id="gpt-6.2-luna"]').textContent(), /Suggested/);
    await shot(page, "agents-composer-picker-light");
    await list.locator('[data-model-id="gpt-6.2-sol"]').click();
    await waitFor(async () => (await defaults.getByTestId("agents-settings-message").textContent().catch(() => "")) === "Default model saved.", {
      message: "default saved on choice",
    });
    const saved = await invoke(page, "agents_provider_status");
    assert.equal(saved.settings.provider, "openai");
    assert.equal(saved.settings.model, "gpt-6.2-sol");
    if (await menu.isVisible().catch(() => false)) {
      await page.keyboard.press("Escape");
      await menu.waitFor({ state: "detached" });
    }
    assert.match(await panel.getByTestId("agents-provider-chip").textContent(), /^OpenAI · gpt-6\.2-sol$/);
    const modelRequests = mock.seen.filter((r) => r.url?.startsWith("/v1/models"));
    assert.ok(modelRequests.length >= 1 && modelRequests.every((r) => r.authorization === `Bearer ${FAKE_KEY}`), "catalog fetched with the OpenAI key");
    step("default chosen from the populated menu", { ids, modelRequests: modelRequests.length });

    // --- Composer: a per-task model, two files, a pasted screenshot, then text.
    await panel.getByTestId("agents-new-task").click();
    const composer = panel.getByTestId("agents-composer");
    await composer.waitFor();
    // The Options summary names the inherited default first.
    const options = composer.getByTestId("agents-options");
    assert.match(await options.textContent(), /^Options: gpt-6\.2-sol · /);
    await options.click();
    const sheet = page.getByRole("dialog", { name: "Options" });
    await sheet.waitFor();
    await sheet.getByRole("tab", { name: /^Model/ }).click();
    const taskList = sheet.getByRole("listbox", { name: "OpenAI models" });
    await taskList.waitFor({ timeout: 15_000 });
    await taskList.locator('[data-model-id="gpt-6.2-luna"]').click();
    await waitFor(async () => /^Options: gpt-6\.2-luna · /.test((await options.textContent()) ?? ""), { message: "per-task model in the summary" });
    if (await sheet.isVisible().catch(() => false)) {
      await page.keyboard.press("Escape");
      await sheet.waitFor({ state: "detached" });
    }
    assert.equal((await invoke(page, "agents_provider_status")).settings.model, "gpt-6.2-sol", "the per-task choice leaves the default alone");

    const dialogs = [];
    for (const file of [alpha, beta]) {
      const before = await composer.getByTestId("agents-attachment").count();
      const report = await withNativeDialog({ title: "Attach files", path: file, pid }, () => composer.getByRole("button", { name: "Attach files…" }).click());
      dialogs.push(report);
      await waitFor(async () => (await composer.getByTestId("agents-attachment").count()) === before + 1, {
        timeout: 15_000,
        message: `attachment row for ${file}`,
      });
    }
    const names = await composer.getByTestId("agents-attachment").locator(".ui-chip__label").allTextContents();
    assert.deepEqual(names, ["notes-alpha", "notes-beta"]);
    step("two files attached through the backend's native dialog", { dialogs });

    assert.equal(setClipboardImage(), "ok");
    const field = composer.getByTestId("agents-prompt");
    await field.click();
    await page.keyboard.press("Control+V");
    await waitFor(async () => (await composer.getByTestId("agents-attachment").count()) === 3, {
      timeout: 15_000,
      message: "the pasted screenshot becomes an attachment (OS clipboard paste)",
    });
    const pasted = composer.getByTestId("agents-attachment").nth(2);
    assert.equal(await pasted.locator(".ui-chip__label").textContent(), "Pasted image 1.png");
    assert.match(await pasted.textContent(), /PNG image · .* · 160×90/);
    await pasted.getByTestId("agents-attachment-thumb").waitFor();
    // Pasting the image inserted no text; typing afterwards goes to the field.
    assert.equal(await field.inputValue(), "");
    await page.keyboard.type(TYPED);
    assert.equal(await field.inputValue(), TYPED);
    await shot(page, "agents-composer-light");
    await shot(page, "agents-composer-panel-light", panel);
    step("screenshot pasted from the OS clipboard, then text typed");

    await composer.getByTestId("agents-start").click();
    await page.waitForSelector("[data-testid=agents-task-view]");
    await page.waitForFunction(() => document.querySelector("[data-testid=agents-task-status]")?.dataset.status === "completed", null, {
      timeout: 30_000,
    });

    // --- What the provider received.
    const responses = mock.seen.filter((r) => r.method === "POST" && r.url?.startsWith("/v1/responses"));
    assert.equal(responses.length, 1, "one model request");
    const request = responses[0];
    assert.equal(request.authorization, `Bearer ${FAKE_KEY}`);
    assert.equal(request.body.model, "gpt-6.2-luna", "the per-task model reaches the adapter");
    const input = request.body.input;
    const user = [...input].reverse().find((item) => item.role === "user" || Array.isArray(item.content));
    const parts = user.content;
    const imagePart = parts.find((p) => p.type === "input_image");
    assert.ok(imagePart, `an input_image part: ${parts.map((p) => p.type)}`);
    assert.match(imagePart.image_url, /^data:image\/png;base64,iVBORw0KGgo/, "the screenshot's PNG bytes");
    const texts = parts.filter((p) => p.type === "input_text").map((p) => p.text);
    assert.ok(texts.some((t) => t.includes(TYPED)), "the typed text");
    assert.ok(texts.some((t) => t.includes('<attachment name="notes-alpha"') && t.includes(ALPHA.trim())), "the first file's content");
    assert.ok(texts.some((t) => t.includes('<attachment name="notes-beta"') && t.includes(BETA.trim())), "the second file's content");
    step("the mock received the image, the text, and both files", {
      model: request.body.model,
      parts: parts.map((p) => p.type),
      imageBytes: imagePart.image_url.length,
    });

    // --- The transcript shows the message's attachments.
    const view = panel.getByTestId("agents-task-view");
    const sent = view.getByTestId("agents-message-attachments").getByTestId("agents-attachment");
    assert.deepEqual(await sent.locator(".ui-chip__label").allTextContents(), ["notes-alpha", "notes-beta", "Pasted image 1.png"]);
    await sent.nth(2).getByTestId("agents-attachment-thumb").waitFor();
    // The composer's draft was cleared of what was sent.
    const tasks = await invoke(page, "agents_task_list");
    taskId = tasks[0]?.id ?? null;
    await shot(page, "agents-composer-sent-light");
    assert.deepEqual(consoleErrors(app), [], "no console errors");
  } finally {
    await app.close();
  }

  // --- Reopen: the message keeps its attachments.
  app = await launchApp({ env, dataDir });
  try {
    const { page } = app;
    await page.setViewportSize({ width: 1440, height: 900 });
    await setTheme(page, "light");
    const panel = page.getByTestId("agents-panel");
    await panel.waitFor({ timeout: 15_000 });
    await openWorkspace(page, project);
    await panel.getByTestId("agents-task-item").first().click();
    const view = panel.getByTestId("agents-task-view");
    await view.waitFor();
    const rows = view.getByTestId("agents-message-attachments").getByTestId("agents-attachment");
    await waitFor(async () => (await rows.count()) === 3, { message: "attachments after reopening" });
    assert.deepEqual(await rows.locator(".ui-chip__label").allTextContents(), ["notes-alpha", "notes-beta", "Pasted image 1.png"]);
    assert.equal(await rows.nth(0).getAttribute("data-missing"), null);
    await rows.nth(2).getByTestId("agents-attachment-thumb").waitFor();
    await shot(page, "agents-composer-reopened-light");
    step("attachments shown after reopening", { taskId });
    assert.deepEqual(consoleErrors(app), [], "no console errors after reopening");
  } finally {
    await invoke(app.page, "agents_provider_clear_key", { provider: "openai" }).catch(() => {});
    await app.close();
    await mock.close();
  }
  await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2));

  const devHome = process.env.EXPOSITION_DEV_HOME;
  if (devHome) {
    const target = join(devHome, "ui-reference", "agents-composer");
    await mkdir(target, { recursive: true });
    for (const path of shots) await copyFile(path, join(target, path.split(/[\\/]/).pop()));
    console.log(`copied ${(await readdir(target)).length} screenshots to ${target}`);
  }
}
