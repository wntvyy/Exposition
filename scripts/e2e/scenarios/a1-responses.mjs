// A1 agent responses in the real app (debug build, scripted provider —
// development only; it replaces only the model: the task loop, persistence,
// transcript, renderer, and shell are the real ones).
//
// 1. Treatments: one representative response (prose with emphasis and a link,
//    inline and display math, a Python fence, a list, a table) streamed into a
//    task, shown in the three response treatments under review (A band,
//    B inset, C document) on the Agents page (1440 px) and in the narrow Agent
//    panel (~400 px), light and dark. Screenshots go to test-results and, when
//    A1_TREATMENTS_DIR is set, there too.
// 2. Behavior: streaming renders without errors; follow-the-bottom scrolling
//    does not pull a reader who scrolled up; Copy on a fence and on the
//    message puts the exact source on the OS clipboard; a hostile response
//    (raw HTML, script, event handlers, iframes, javascript:/data:/file:/
//    tauri: links in several spellings, remote and data: images, KaTeX
//    \href/\includegraphics) renders inert — no script runs, no request
//    reaches a local listener, no navigation, no element that can load or
//    navigate; an http link opens only through app_open_external (recorded,
//    not launched); Run in Research with no session fills the Research input
//    and starts nothing; the narrow panel has no horizontal page scroll.
import { execFileSync } from "node:child_process";
import { createServer } from "node:http";
import { copyFile, mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";

const VARIANTS = [
  { id: "band", letter: "A" },
  { id: "inset", letter: "B" },
  { id: "document", letter: "C" },
];
const VARIANT_KEY = "exposition.debug.responseVariant";

// Same text as SAMPLE_RESPONSE in src/ui/reference/UiReference.tsx.
const FENCE = [
  "import math",
  "",
  "partial = sum(1 / n**2 for n in range(1, 10_001))",
  "print(partial, math.pi**2 / 6)  # 1.6448340718 1.6449340668",
].join("\n");
const SAMPLE = [
  "The series converges because the terms decay like $1/n^2$; see the [Basel problem](https://en.wikipedia.org/wiki/Basel_problem) for the closed form. In short, **the sum is finite** and *the partial sums increase monotonically*.",
  "",
  "$$",
  String.raw`\sum_{n=1}^{\infty} \frac{1}{n^2} = \frac{\pi^2}{6}`,
  "$$",
  "",
  "A quick numerical check in Python:",
  "",
  "```python",
  FENCE,
  "```",
  "",
  "What the check shows:",
  "",
  String.raw`- the partial sum after $10^4$ terms is within $10^{-4}$ of $\pi^2/6$;`,
  "- the error shrinks like $1/N$;",
  "- no term is negative, so the order of summation does not matter.",
  "",
  "| $N$ | Partial sum | Error |",
  "|---:|---:|---:|",
  "| 10 | 1.549768 | 0.095166 |",
  "| 100 | 1.634984 | 0.009950 |",
  "| 10 000 | 1.644834 | 0.000100 |",
].join("\n");

const invoke = (page, cmd, args) =>
  page.evaluate(([c, a]) => window.__TAURI_INTERNALS__.invoke(c, a), [cmd, args ?? {}]);

function consoleErrors(app) {
  return app.consoleMessages.filter((m) => m.startsWith("error:") || m.startsWith("pageerror:"));
}

async function setTheme(page, theme) {
  await runCommand(page, `color theme ${theme}`, theme === "dark" ? /Color Theme: Dark/ : /Color Theme: Light/);
  await page.waitForFunction((t) => document.documentElement.dataset.theme === t, theme);
  await page.waitForTimeout(250);
}

async function setVariant(page, variant) {
  await page.evaluate(
    ([key, value]) => {
      localStorage.setItem(key, value);
      window.dispatchEvent(new StorageEvent("storage", { key }));
    },
    [VARIANT_KEY, variant],
  );
  await page.waitForFunction(
    (v) => {
      const msgs = [...document.querySelectorAll("[data-testid=agents-assistant-text]")];
      return msgs.length > 0 && msgs.every((m) => m.querySelector(`.ui-response[data-variant="${v}"]`));
    },
    variant,
  );
  await page.waitForTimeout(200);
}

async function createTask(page, root, prompt) {
  return invoke(page, "agents_task_create", {
    request: { prompt, workspaceRoot: root, scope: { kind: "workspace" }, permission: "readOnly", execution: "disabled", consent: "scopeContent", budgets: null },
  });
}

async function waitDone(page, id, timeout = 60_000) {
  const deadline = Date.now() + timeout;
  for (;;) {
    const task = (await invoke(page, "agents_task_list")).find((t) => t.id === id);
    if (task && !["queued", "running", "awaitingApproval"].includes(task.status.kind)) return task;
    if (Date.now() > deadline) throw new Error(`task ${id} did not finish: ${JSON.stringify(task?.status)}`);
    await page.waitForTimeout(150);
  }
}

/** The OS clipboard's text (Windows), with CRLF folded to LF (the clipboard stores CRLF). */
function osClipboardText() {
  if (process.platform !== "win32") return null;
  const out = execFileSync("powershell", ["-NoProfile", "-NonInteractive", "-Command", "[Console]::OutputEncoding=[Text.Encoding]::UTF8; Get-Clipboard -Raw"], {
    encoding: "utf8",
  });
  return out.replace(/\r\n/g, "\n").replace(/\n$/, "");
}

export default async function a1Responses({ launchApp, artifactsDir, assert }) {
  const project = await mkdtemp(join(tmpdir(), "exposition-a1-responses-"));
  await writeFile(join(project, "main.typ"), "= Notes\n\nThe Basel series.\n");
  const namespace = `e2e-${Date.now()}`;
  const evidence = { project, steps: [], shots: [] };
  const step = (name, data = {}) => {
    evidence.steps.push({ name, ...data });
    console.log(`ok: ${name}${Object.keys(data).length ? ` ${JSON.stringify(data)}` : ""}`);
  };
  const treatmentsDir = process.env.A1_TREATMENTS_DIR;
  if (treatmentsDir) await mkdir(treatmentsDir, { recursive: true });
  const shot = async (target, name, options = {}) => {
    const path = join(artifactsDir, `${name}.png`);
    await target.screenshot({ path, ...options });
    evidence.shots.push(path);
    if (treatmentsDir) await copyFile(path, join(treatmentsDir, `${name}.png`));
  };

  // A local listener: any request from rendered content would land here.
  const hits = [];
  const server = createServer((req, res) => {
    hits.push(`${req.method} ${req.url}`);
    res.writeHead(200, { "content-type": "image/png", "access-control-allow-origin": "*" });
    res.end();
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const probe = `http://127.0.0.1:${server.address().port}`;

  const app = await launchApp({ env: { EXPOSITION_CREDENTIAL_NAMESPACE: namespace } });
  const info = await app.page.evaluate(() => window.__TAURI_INTERNALS__.invoke("app_info"));
  if (!info.debugBuild) {
    await app.close();
    server.close();
    throw new Error("a1-responses requires a debug build (scripted provider, response treatment switch)");
  }
  try {
    const { page } = app;
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.getByTestId("agents-panel").waitFor({ timeout: 15_000 });
    await setTheme(page, "light");
    await invoke(page, "agents_provider_update", {
      settings: { provider: "scripted", model: "claude-opus-5-5", effort: "medium", consent: "scopeContent" },
    });
    const opened = await invoke(page, "workspace_open", { path: project });
    const pagesBar = page.locator('[aria-label="Pages"]');
    const gotoPage = async (name) => {
      await pagesBar.getByRole("button", { name, exact: true }).click();
      await page.waitForTimeout(150);
    };
    const agentsPage = page.getByTestId("agents-page");

    // ---------------------------------------------------------------- 1. stream the representative response
    await invoke(page, "agents_dev_load_script", { script: { turns: [{ chunkDelayMs: 12, steps: [{ text: SAMPLE }] }] } });
    await gotoPage("Agents");
    await agentsPage.waitFor();
    const sampleTask = await createTask(page, opened.root, "Check that the Basel series converges");
    await agentsPage.locator(`[data-testid=agents-page-task][data-task-id="${sampleTask.id}"]`).click();
    // While streaming: the draft grows and renders without throwing; an open fence or math block shows as text.
    const samples = [];
    for (let i = 0; i < 25; i++) {
      samples.push(
        await page.evaluate(() => {
          const draft = document.querySelector("[data-testid=agents-page-task-detail] [data-testid=agents-draft]");
          return draft
            ? { len: draft.textContent.length, openCode: !!draft.querySelector("[data-open]"), math: draft.querySelectorAll(".katex").length }
            : null;
        }),
      );
      await page.waitForTimeout(100);
    }
    const seen = samples.filter(Boolean);
    assert.ok(seen.length >= 3, `the draft streamed (${JSON.stringify(samples)})`);
    assert.ok(seen.some((s, i) => i > 0 && s.len > seen[i - 1].len), "the draft grew while streaming");
    const done = await waitDone(page, sampleTask.id);
    assert.equal(done.status.kind, "completed", JSON.stringify(done.status));
    const response = agentsPage.getByTestId("agents-assistant-text").last();
    await response.locator("[data-math=display] .katex-display").waitFor();
    await response.locator(".ui-md-tok-keyword").first().waitFor({ timeout: 10_000 });
    const structure = await response.evaluate((el) => ({
      link: el.querySelector("[data-external-link]")?.textContent,
      linkHref: el.querySelector("a[href]") !== null,
      address: el.querySelector(".ui-md-link__address")?.textContent,
      inlineMath: el.querySelectorAll("[data-math=inline] .katex").length,
      displayMath: el.querySelectorAll("[data-math=display] .katex-display").length,
      mathml: el.querySelectorAll(".katex math").length,
      fence: el.querySelector("[data-testid=md-code-text]")?.textContent,
      run: el.querySelector("[data-testid=md-code-run]")?.textContent,
      items: el.querySelectorAll("li").length,
      rows: el.querySelectorAll("tbody tr").length,
      strong: el.querySelector("strong")?.textContent,
      em: el.querySelector("em")?.textContent,
    }));
    assert.deepEqual(
      { ...structure, mathml: structure.mathml >= 7 },
      {
        link: "Basel problem",
        linkHref: false,
        address: "https://en.wikipedia.org/wiki/Basel_problem",
        inlineMath: 6,
        displayMath: 1,
        mathml: true,
        fence: FENCE,
        run: "Run in Research",
        items: 3,
        rows: 3,
        strong: "the sum is finite",
        em: "the partial sums increase monotonically",
      },
    );
    step("representative response streamed and rendered (link + address, 6 inline / 1 display formulas with MathML, highlighted fence, list, table)", {
      draftSamples: seen.length,
    });

    // ---------------------------------------------------------------- 2. treatment screenshots: Agents page
    const fullResponse = async (name) => {
      // The transcript box is limited to 55vh; a tall window and no height limit, for this picture only.
      await page.setViewportSize({ width: 1440, height: 1900 });
      await page.addStyleTag({ content: ".agents-transcript { max-height: none !important; }" }).then((h) => h.evaluate((el) => (el.id = "a1-shot-style")));
      await page.waitForTimeout(250);
      await shot(response, name);
      await page.evaluate(() => document.getElementById("a1-shot-style")?.remove());
      await page.setViewportSize({ width: 1440, height: 1100 });
    };
    await page.setViewportSize({ width: 1440, height: 1100 });
    for (const theme of ["light", "dark"]) {
      await setTheme(page, theme);
      for (const { id, letter } of VARIANTS) {
        await setVariant(page, id);
        await agentsPage.getByTestId("agents-transcript").evaluate((el) => (el.scrollTop = 0));
        await page.waitForTimeout(150);
        await shot(page, `${letter}-${id}-agents-page-${theme}`);
        await fullResponse(`${letter}-${id}-agents-page-${theme}-response`);
      }
    }
    step("treatment screenshots on the Agents page (1440 px), light and dark");

    // ---------------------------------------------------------------- 3. treatment screenshots: Agent panel (narrow)
    await setTheme(page, "light");
    await page.setViewportSize({ width: 1440, height: 900 });
    await agentsPage.getByTestId("agents-show-in-panel").click();
    const panel = page.getByTestId("agents-panel");
    await panel.getByTestId("agents-task-view").waitFor();
    // Widen the auxiliary panel from its 320 px default to ~400 px by dragging its handle.
    const handle = page.getByRole("separator", { name: "Resize auxiliary panel" });
    const box = await handle.boundingBox();
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width / 2 - 80, box.y + box.height / 2, { steps: 8 });
    await page.mouse.up();
    await page.waitForTimeout(300);
    const panelWidth = await panel.evaluate((el) => Math.round(el.getBoundingClientRect().width));
    assert.ok(panelWidth >= 340 && panelWidth <= 460, `Agent panel width ${panelWidth}`);
    const panelResponse = panel.getByTestId("agents-assistant-text").last();
    await panelResponse.locator("[data-math=display] .katex-display").waitFor();
    for (const theme of ["light", "dark"]) {
      await setTheme(page, theme);
      for (const { id, letter } of VARIANTS) {
        await setVariant(page, id);
        await panel.getByTestId("agents-transcript").evaluate((el) => (el.scrollTop = 0));
        await page.waitForTimeout(150);
        await shot(page, `${letter}-${id}-agent-panel-${theme}`);
        // The whole response: a tall window and no transcript height limit, for this picture only.
        await page.setViewportSize({ width: 1440, height: 1900 });
        await page.addStyleTag({ content: ".agents-transcript { max-height: none !important; }" }).then((h) => h.evaluate((el) => (el.id = "a1-shot-style")));
        await page.waitForTimeout(250);
        await shot(panelResponse, `${letter}-${id}-agent-panel-${theme}-response`);
        await page.evaluate(() => document.getElementById("a1-shot-style")?.remove());
        await page.setViewportSize({ width: 1440, height: 900 });
      }
    }
    step("treatment screenshots in the Agent panel", { panelWidth });

    // Narrow layout: no horizontal page scroll; wide content scrolls inside its own block.
    const overflow = await page.evaluate(() => {
      const panelEl = document.querySelector("[data-testid=agents-panel]");
      const scroller = panelEl.closest(".wb-view-container__body") ?? panelEl.parentElement;
      return {
        page: document.documentElement.scrollWidth - document.documentElement.clientWidth,
        panel: scroller.scrollWidth - scroller.clientWidth,
        code: [...panelEl.querySelectorAll(".ui-md-code__body")].map((el) => el.scrollWidth > el.clientWidth),
      };
    });
    assert.equal(overflow.page, 0, `no horizontal page scroll: ${JSON.stringify(overflow)}`);
    assert.ok(overflow.panel <= 1, `no horizontal scroll in the Agent panel: ${JSON.stringify(overflow)}`);
    step("narrow panel: no horizontal page or panel scroll; the code block scrolls inside itself", overflow);
    await setTheme(page, "light");
    await setVariant(page, "inset");

    // ---------------------------------------------------------------- 4. copy is exact
    if (process.platform === "win32") {
      await panelResponse.getByTestId("md-code-copy").click();
      await page.waitForFunction(() => document.querySelector("[data-testid=agents-panel] [data-testid=md-code-copy]")?.dataset.copyState === "copied");
      assert.equal(osClipboardText(), FENCE, "fence Copy puts exactly the fence text on the clipboard");
      await panelResponse.getByTestId("response-copy").click();
      await page.waitForFunction(() => document.querySelector("[data-testid=agents-panel] [data-testid=response-copy]")?.dataset.copyState === "copied");
      assert.equal(osClipboardText(), SAMPLE, "message Copy puts exactly the original Markdown on the clipboard");
      step("Copy (fence and message) puts the exact source on the OS clipboard");
    }

    // ---------------------------------------------------------------- 5. Run in Research without a session
    const sessionsBefore = await invoke(page, "research_sessions");
    await panelResponse.getByTestId("md-code-run").click();
    const researchCode = page.getByTestId("research-code");
    await researchCode.waitFor({ timeout: 10_000 });
    await page.waitForFunction((code) => document.querySelector("[data-testid=research-code]")?.value === code, FENCE);
    await page.waitForTimeout(500);
    const sessionsAfter = await invoke(page, "research_sessions");
    assert.equal(sessionsAfter.length, sessionsBefore.length, "Run in Research without a session starts nothing");
    step("Run in Research with no session: the code is in the Research input, the panel is shown, nothing started", {
      sessions: sessionsAfter.length,
    });

    // ---------------------------------------------------------------- 6. hostile response renders inert
    // Record app_open_external instead of launching the system browser. Tauri's
    // invoke is read-only, but its IPC transport is a fetch to
    // http://ipc.localhost/<command>: answer that one command here and pass
    // every other call through unchanged.
    await page.evaluate(() => {
      const original = window.fetch.bind(window);
      window.__opened = [];
      window.fetch = (input, init) => {
        const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
        if (/^https?:\/\/ipc\.localhost\/app_open_external(\?|$)/.test(url)) {
          let args = null;
          try {
            args = JSON.parse(init?.body ?? "null");
          } catch {}
          window.__opened.push(args?.url ?? null);
          return Promise.resolve(
            new Response("null", { status: 200, headers: { "Tauri-Response": "ok", "Content-Type": "application/json" } }),
          );
        }
        return original(input, init);
      };
    });
    // Probe with an address the Rust side refuses anyway, so nothing could open if the recorder were bypassed.
    const probeCall = await page.evaluate(async () => {
      try {
        await window.__TAURI_INTERNALS__.invoke("app_open_external", { url: "javascript:probe" });
        return { ok: true, opened: window.__opened.splice(0) };
      } catch (error) {
        return { ok: false, error: String(error?.message ?? error) };
      }
    });
    assert.deepEqual(probeCall, { ok: true, opened: ["javascript:probe"] }, "app_open_external calls are recorded (no browser is launched by this scenario)");
    const hostile = [
      "Hostile content follows.",
      "",
      "<script>window.__xsr = 'script'</script>",
      `<img src="${probe}/img-onerror" onerror="window.__xsr = 'onerror'">`,
      `<iframe src="${probe}/iframe"></iframe>`,
      `<a href="javascript:window.__xsr='raw-anchor'">raw anchor</a> <b onmouseover="window.__xsr='hover'">bold</b>`,
      "",
      "[js](javascript:window.__xsr='js') [JS](JaVaScRiPt:window.__xsr='JS') [ent](&#106;avascript:window.__xsr='ent')",
      "[tab](java\tscript:window.__xsr='tab') [vb](vbscript:msgbox) [data](data:text/html,<script>window.__xsr='data'</script>)",
      `[file](file:///C:/Windows/win.ini) [tauri](tauri://localhost/index.html) [ipc](ipc://localhost/app_info) [rel](notes/a.md) [frag](#top)`,
      `[ref link][r] and <${probe}/autolink> and [creds](https://user:pw@example.org/)`,
      "",
      `[r]: javascript:window.__xsr='ref'`,
      "",
      `![remote](${probe}/remote.png) ![data](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==)`,
      "",
      String.raw`Math: $\href{javascript:window.__xsr='katex'}{x}$ and $\includegraphics{` + probe + String.raw`/katex.png}$ and $\htmlClass{x}{y}$`,
      "",
      `[safe](${probe}/link-click)`,
    ].join("\n");
    await invoke(page, "agents_dev_load_script", { script: { turns: [{ chunkDelayMs: 2, steps: [{ text: hostile }] }] } });
    await gotoPage("Agents");
    const hostileTask = await createTask(page, opened.root, "Show hostile content");
    await agentsPage.locator(`[data-testid=agents-page-task][data-task-id="${hostileTask.id}"]`).click();
    await waitDone(page, hostileTask.id);
    const hostileMsg = agentsPage.getByTestId("agents-assistant-text").last();
    await hostileMsg.getByText("Hostile content follows.").waitFor();
    const urlBefore = page.url();
    // Click every inert link and the external one; press Enter on the external one.
    for (const el of await hostileMsg.locator("[data-inert-link]").all()) await el.click();
    await hostileMsg.locator("[data-external-link]", { hasText: "safe" }).click();
    await page.waitForTimeout(1500);
    const inert = await hostileMsg.evaluate((root) => {
      const bad = [];
      for (const el of root.querySelectorAll("*")) {
        const tag = el.tagName.toLowerCase();
        if (["script", "iframe", "img", "object", "embed", "video", "audio", "link", "style", "form", "base", "meta"].includes(tag)) bad.push(`<${tag}>`);
        for (const attr of el.getAttributeNames()) {
          if (attr.startsWith("on") || ["href", "src", "srcset", "target", "action", "formaction", "srcdoc", "xlink:href"].includes(attr)) bad.push(`${tag}[${attr}]`);
        }
      }
      return {
        bad,
        xsr: window.__xsr ?? null,
        images: root.querySelectorAll("[data-testid=md-image]").length,
        openButtons: root.querySelectorAll("[data-testid=md-image] button").length,
        inertLinks: root.querySelectorAll("[data-inert-link]").length,
        externalLinks: [...root.querySelectorAll("[data-external-link]")].map((a) => a.textContent),
        rawText: root.textContent.includes("<script>window.__xsr = 'script'</script>"),
        opened: window.__opened,
      };
    });
    assert.deepEqual(inert.bad, [], "no element or attribute that can run script, load, or navigate");
    assert.equal(inert.xsr, null, "no script ran");
    assert.equal(inert.images, 2, "both images render as placeholders");
    assert.equal(inert.openButtons, 1, "only the http image offers Open");
    assert.ok(inert.inertLinks >= 11, `unsafe links render as inert text (${inert.inertLinks})`);
    assert.deepEqual(inert.externalLinks, [`${probe}/autolink`, "safe"], "only http(s) links are links");
    assert.ok(inert.rawText, "raw HTML is shown as the literal text the model wrote");
    assert.deepEqual(inert.opened, [`${probe}/link-click`], "app_open_external received only the clicked http link");
    assert.equal(page.url(), urlBefore, "the window did not navigate");
    assert.deepEqual(hits, [], "no request reached the local listener");
    await hostileMsg.locator("[data-external-link]", { hasText: "safe" }).focus();
    await page.keyboard.press("Enter");
    await page.waitForTimeout(200);
    assert.deepEqual(await page.evaluate(() => window.__opened), [`${probe}/link-click`, `${probe}/link-click`], "Enter opens the link too");
    await shot(page, "hostile-agents-page-light");
    step("hostile response inert: no script, no request, no navigation, no live element; links open only via app_open_external", {
      inertLinks: inert.inertLinks,
      images: inert.images,
    });

    // ---------------------------------------------------------------- 7. follow the bottom only when at the bottom
    const long = Array.from({ length: 120 }, (_, i) => `Line ${i + 1} of a long answer with enough words to wrap.`).join("\n\n");
    await invoke(page, "agents_dev_load_script", { script: { turns: [{ chunkDelayMs: 8, steps: [{ text: long }] }] } });
    const longTask = await createTask(page, opened.root, "A long answer");
    await agentsPage.locator(`[data-testid=agents-page-task][data-task-id="${longTask.id}"]`).click();
    const log = agentsPage.getByTestId("agents-transcript");
    await page.waitForFunction(() => {
      const el = document.querySelector("[data-testid=agents-page-task-detail] [data-testid=agents-transcript]");
      return el && el.scrollHeight > el.clientHeight + 200;
    }, null, { timeout: 30_000 });
    const following = await log.evaluate((el) => el.scrollHeight - el.scrollTop - el.clientHeight);
    await log.evaluate((el) => {
      el.scrollTop = 0;
      el.dispatchEvent(new Event("scroll"));
    });
    const heightBefore = await log.evaluate((el) => el.scrollHeight);
    await page.waitForTimeout(1200);
    const reading = await log.evaluate((el) => ({ top: el.scrollTop, height: el.scrollHeight }));
    assert.ok(following < 40, `follows new output while at the bottom (${following}px from the end)`);
    assert.ok(reading.height > heightBefore, "output kept arriving");
    assert.ok(reading.top < 40, `a reader who scrolled up stays put (scrollTop ${reading.top})`);
    await waitDone(page, longTask.id);
    step("streaming follows the bottom only while the reader is there", { following, reading });

    const errors = consoleErrors(app);
    assert.deepEqual(errors, [], "no console errors");
    evidence.hits = hits;
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2));
  } finally {
    await app.close();
    server.close();
  }
}
