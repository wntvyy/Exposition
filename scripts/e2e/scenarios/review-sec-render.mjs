// M6 security review (area 2: content rendering) in the real debug app.
//
// Hostile markup reaches the UI through every real path that shows untrusted
// text or media: Python kernel display/stream/error outputs (HTML tables, SVG
// figures, JavaScript/Markdown/LaTeX/widget mimetypes, OSC-8 links), Typst /
// Markdown / LaTeX diagnostics in Problems and the page view, a hostile file
// name, the TeX job log, Lean messages and a Lean hover rendered by Monaco
// (docstring with images, http/command/javascript links and raw HTML), Monaco
// link detection in the source editor, and agent transcript / approval / diff
// text from the debug-only scripted provider. CSP probes are re-run.
//
// For every vector it checks: no script ran (window.__xsr, document.title, an
// IPC side effect), no request reached a local HTTP listener, no live DOM
// element/attribute that can run script or fetch, no navigation of the main
// webview, and no new window.
//
// Run by name only: `npm run e2e -- review-sec-render` (debug build). Prints
// `ok:` / `DEFECT:` / `INFO:` lines; findings in
// test-results/e2e/review-sec-render/findings.json. Fails if any DEFECT (or a
// phase could not run: `SETUP-FAIL:`).
import { createServer } from "node:http";
import { existsSync } from "node:fs";
import { cp, mkdir, mkdtemp, readdir, readFile, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";
import { waitForAsync } from "../wait.mjs";
import { invoke, openInEditor, waitFor } from "../workspace-helpers.mjs";

const NAME = "review-sec-render";
const LEAN_TOOLCHAIN_DIR = "leanprover--lean4---v4.35.0";

export default async function reviewSecRender({ launchApp, artifactsDir, fixture }) {
  if (!process.argv.slice(2).includes(NAME)) {
    console.log(`skip ${NAME} (run by name)`);
    return;
  }

  const findings = [];
  const record = (kind, id, detail) => {
    findings.push({ kind, id, detail });
    const text = typeof detail === "string" ? detail : JSON.stringify(detail);
    console.log(`${kind}: ${id}${text ? ` — ${text}` : ""}`);
  };
  const ok = (id, detail = "") => record("ok", id, detail);
  const defect = (id, detail) => record("DEFECT", id, detail);
  const info = (id, detail) => record("INFO", id, detail);
  const check = (cond, id, detail) => (cond ? ok(id, detail) : defect(id, detail));

  // ---------------------------------------------------------------- listener
  const hits = [];
  const server = createServer((req, res) => {
    hits.push({ url: req.url, method: req.method, at: Date.now() });
    res.writeHead(200, { "content-type": "text/plain", "access-control-allow-origin": "*" });
    res.end("probe");
  });
  // WebSocket handshakes arrive as 'upgrade', not 'request'.
  server.on("upgrade", (req, socket) => {
    hits.push({ url: req.url, method: "UPGRADE", at: Date.now() });
    socket.destroy();
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  const ORIGIN = `http://127.0.0.1:${server.address().port}`;
  const hitsFor = (prefix) => hits.filter((h) => h.url.includes(prefix)).map((h) => h.url);

  // Script payload: any execution appends its tag to window.__xsr, changes the
  // title, and closes the workspace through IPC (observable from Node).
  const P = (tag) =>
    `window.__xsr=(window.__xsr||'')+'${tag};';document.title='pwned-${tag}';window.__TAURI_INTERNALS__.invoke('workspace_close')`;

  // ---------------------------------------------------------------- project
  const base = await mkdtemp(join(tmpdir(), "xp-review-sec-render-"));
  const root = join(base, "project");
  const leanAvailable = existsSync(join(process.env.ELAN_HOME || join(homedir(), ".elan"), "toolchains", LEAN_TOOLCHAIN_DIR, "bin"));
  if (leanAvailable) {
    await cp(fixture("lean-basic"), root, {
      recursive: true,
      filter: (src) => !/[\\/]\.lake([\\/]|$)/.test(src) && !src.endsWith("lake-manifest.json"),
    });
  } else {
    await mkdir(root, { recursive: true });
  }
  const payloads = {
    table_html:
      `<base href="${ORIGIN}/k-base/"><meta http-equiv="refresh" content="0;url=${ORIGIN}/k-refresh">` +
      `<link rel="stylesheet" href="${ORIGIN}/k-link.css"><link rel="prefetch" href="${ORIGIN}/k-prefetch">` +
      `<style>@import url(${ORIGIN}/k-import.css); td{background:url(${ORIGIN}/k-style-bg.png)}</style>` +
      `<table><tr><th>h&lt;b&gt;x</th><th>second</th></tr>` +
      `<tr><td><img src="${ORIGIN}/k-table-img.png" onerror="${P("table-img")}">cell1</td>` +
      `<td><a href="javascript:${P("table-js-href")}">jslink</a> <a href="${ORIGIN}/k-anchor">anchor</a></td></tr>` +
      `<tr><td><form action="${ORIGIN}/k-form"><button formaction="${ORIGIN}/k-formaction">go</button></form>f</td>` +
      `<td><svg><image href="${ORIGIN}/k-inline-svg-image.png"/></svg>&lt;script&gt;literal</td></tr></table>` +
      `<script>${P("table-script")}</script><iframe src="${ORIGIN}/k-iframe"></iframe>` +
      `<object data="${ORIGIN}/k-object"></object><embed src="${ORIGIN}/k-embed"><video src="${ORIGIN}/k-video"></video>`,
    plain_html: `<img src=x onerror="${P("plain-html")}"><b>bold</b><iframe srcdoc="<script>${P("srcdoc")}</script>"></iframe>`,
    svg:
      `<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="80" height="80" onload="${P("svg-onload")}">` +
      `<script>${P("svg-script")}</script>` +
      `<style>@import url(${ORIGIN}/k-svg-import.css); rect.b{fill:url(${ORIGIN}/k-svg-fill.svg#g)}</style>` +
      `<image href="${ORIGIN}/k-svg-image.png" width="10" height="10"/>` +
      `<image xlink:href="${ORIGIN}/k-svg-xlink.png" x="10" width="10" height="10"/>` +
      `<use href="${ORIGIN}/k-svg-use.svg#a"/>` +
      `<foreignObject width="50" height="50"><body xmlns="http://www.w3.org/1999/xhtml"><img src="${ORIGIN}/k-svg-fo-img.png"/>` +
      `<script>${P("svg-fo-script")}</script></body></foreignObject>` +
      `<a href="javascript:${P("svg-js-href")}"><rect width="80" height="80" fill="teal"/></a><rect class="b" width="5" height="5"/></svg>`,
    js: P("application-javascript"),
    md: `[x](javascript:${P("md-mime")}) ![i](${ORIGIN}/k-md-mime.png) <img src="${ORIGIN}/k-md-mime-raw.png" onerror="${P("md-mime-raw")}">`,
    latex: `$x$ \\href{${ORIGIN}/k-latex-mime}{l} <img src=x onerror="${P("latex-mime")}">`,
    widget_text: `<img src=x onerror="${P("widget")}">widget-plain`,
    iframe_html: `<iframe srcdoc="<script>${P("iframe-mime")}</script>" src="${ORIGIN}/k-iframe-mime"></iframe>`,
    stream:
      `STREAM<img src=x onerror="${P("stream")}"><script>${P("stream-script")}</script>` +
      `\u001b]8;;${ORIGIN}/k-osc8\u001b\\osc8-link\u001b]8;;\u001b\\ \u202Eevil\u202C &amp; done`,
    error: `ERR<img src=x onerror="${P("error")}"><script>${P("error-script")}</script>`,
  };
  await writeFile(join(root, "payloads.json"), JSON.stringify(payloads), "utf8");
  await writeFile(
    join(root, "main.typ"),
    [
      "= Title",
      "Hello.",
      `#link("javascript:${P("typst-js-link").replaceAll("'", "")}")[js link]`,
      `#link("${ORIGIN}/k-typst-link")[http link]`,
      `#panic("<img src=x onerror=${P("typst-panic").replaceAll("'", "")}><script>alert(1)</script>")`,
      "",
    ].join("\n"),
    "utf8",
  );
  const hostileName = "x&lt;img src=x onerror=alert(1)&gt;.typ";
  await writeFile(join(root, hostileName), `#panic("named-file")\n`, "utf8");
  await writeFile(
    join(root, "notes.md"),
    [
      "# Notes",
      "",
      `<img src="${ORIGIN}/k-md-raw-img.png" onerror="${P("md-raw")}"><script>${P("md-script")}</script>`,
      "",
      `![remote](${ORIGIN}/k-md-image.png)`,
      "",
      "![unc](//127.0.0.1/share/k-md-unc.png)",
      "",
      "![file](file:///C:/Windows/win.ini)",
      "",
      `[jslink](javascript:${P("md-link")}) and [web](${ORIGIN}/k-md-link).`,
      "",
      `Editor link: ${ORIGIN}/k-editor-link here.`,
      "",
    ].join("\n"),
    "utf8",
  );
  await writeFile(
    join(root, "paper.tex"),
    [
      "\\documentclass{article}",
      "\\begin{document}",
      "Hello.",
      "\\errmessage{TEXERR<img src=x onerror=document.title=1><script>alert(1)</script>}",
      "\\end{document}",
      "",
    ].join("\n"),
    "utf8",
  );
  await writeFile(join(root, "hostile.txt"), `TOOLRESULT<img src=x onerror="${P("tool-result")}"><script>${P("tool-script")}</script>\n`, "utf8");
  const leanHoverMd =
    `HOVERDOC ![probe](${ORIGIN}/k-lean-hover-img.png) [probe link](${ORIGIN}/k-lean-hover-link) ` +
    `[cmd link](command:editor.action.selectAll) [js link](javascript:${P("lean-js-link")}) ` +
    `<img src="${ORIGIN}/k-lean-hover-raw.png" onerror="${P("lean-hover-raw")}"> <script>${P("lean-hover-script")}</script>`;
  const leanString = (s) => s.replaceAll("\\", "\\\\").replaceAll('"', '\\"');
  if (leanAvailable) {
    await writeFile(
      join(root, "LeanBasic", "Hostile.lean"),
      [
        `/-- ${leanHoverMd} -/`,
        "def hostileDef : Nat := 1",
        "",
        `#eval IO.println "${leanString(`LEANMSG<img src=x onerror="${P("lean-msg")}"><script>${P("lean-script")}</script>`)}"`,
        "",
        `#eval (throw (IO.userError "${leanString(`LEANERR<b>bold</b><img src=x onerror="${P("lean-err")}">`)}") : IO Unit)`,
        "",
      ].join("\n"),
      "utf8",
    );
  }

  const namespace = `review-sec-render-${Date.now()}`;
  const app = await launchApp({ env: { EXPOSITION_CREDENTIAL_NAMESPACE: namespace } });
  const { page, browser } = app;
  const appUrl = page.url();
  const navigations = [];
  page.on("framenavigated", (frame) => {
    if (frame === page.mainFrame()) navigations.push(frame.url());
  });
  const pageCount = () => browser.contexts().reduce((n, c) => n + c.pages().length, 0);
  const pagesAtStart = pageCount();
  const shot = (name) => page.screenshot({ path: join(artifactsDir, `${name}.png`) }).catch(() => {});

  const info0 = await invoke(page, "app_info");
  if (!info0.debugBuild) {
    await app.close();
    server.close();
    throw new Error(`${NAME} requires a debug build (scripted provider, credential namespace)`);
  }

  // Live-DOM watchdog: any element/attribute able to run script or fetch.
  await page.evaluate(() => {
    const w = { bad: [], remote: [] };
    window.__watch = w;
    const FORBID = new Set(["SCRIPT", "IFRAME", "FRAME", "OBJECT", "EMBED", "BASE", "META", "PORTAL", "LINK"]);
    const URL_ATTRS = /^(href|src|xlink:href|action|formaction|data|srcdoc|poster|background|srcset)$/i;
    const inspect = (el) => {
      if (!(el instanceof Element) || el.closest("head")) return;
      for (const e of [el, ...el.querySelectorAll("*")]) {
        if (e.closest("head")) continue;
        const tag = e.tagName.toUpperCase();
        if (FORBID.has(tag)) w.bad.push(`<${tag.toLowerCase()}> ${e.outerHTML.slice(0, 160)}`);
        for (const a of Array.from(e.attributes)) {
          if (/^on/i.test(a.name)) w.bad.push(`${tag.toLowerCase()}[${a.name}=${a.value.slice(0, 80)}]`);
          if (URL_ATTRS.test(a.name)) {
            const v = a.value.trim().toLowerCase();
            if (v.startsWith("javascript:") || v.startsWith("vbscript:") || a.name.toLowerCase() === "srcdoc")
              w.bad.push(`${tag.toLowerCase()}[${a.name}=${a.value.slice(0, 80)}]`);
            else if (/^(https?:|file:|\\\\|\/\/)/.test(v)) w.remote.push(`${tag.toLowerCase()}[${a.name}=${a.value.slice(0, 120)}]`);
          }
        }
      }
    };
    new MutationObserver((mutations) => {
      for (const m of mutations) {
        if (m.type === "attributes") inspect(m.target);
        for (const n of m.addedNodes) inspect(n);
      }
    }).observe(document.documentElement, { subtree: true, childList: true, attributes: true });
    // window.open calls (they are denied by the runtime; recorded for attribution).
    const open = window.open.bind(window);
    window.__opens = [];
    window.open = (...args) => {
      window.__opens.push(String(args[0]));
      return open(...args);
    };
  });

  /** Common post-conditions after one vector. */
  const sweep = async (phase, hitPrefix) => {
    const state = await page.evaluate(() => ({
      xsr: window.__xsr ?? null,
      title: document.title,
      bad: window.__watch.bad.splice(0),
      remote: window.__watch.remote.splice(0),
      opens: window.__opens.splice(0),
      url: location.href,
    }));
    const ws = await invoke(page, "workspace_current").catch((e) => ({ error: String(e) }));
    check(state.xsr === null && !state.title.startsWith("pwned"), `${phase}: no script ran`, `xsr=${state.xsr} title=${state.title}`);
    check(ws && ws.root, `${phase}: no IPC side effect (workspace still open)`, ws ? "" : "workspace was closed");
    check(state.bad.length === 0, `${phase}: no active element/attribute in the live DOM`, state.bad.slice(0, 8).join(" | "));
    if (state.remote.length) info(`${phase}: remote URLs present in live DOM (CSP must block them)`, state.remote.slice(0, 8));
    const got = hitsFor(hitPrefix);
    check(got.length === 0, `${phase}: no request reached the local listener`, got.slice(0, 10).join(", "));
    check(state.url === appUrl && navigations.every((u) => u === appUrl), `${phase}: main webview stayed on the app`, `${state.url} ${navigations.join(",")}`);
    check(pageCount() === pagesAtStart, `${phase}: no new window opened`, `pages ${pageCount()} vs ${pagesAtStart}; window.open calls ${JSON.stringify(state.opens)}`);
  };

  const setupFail = (phase, error) => record("SETUP-FAIL", phase, String(error?.stack ?? error).slice(0, 600));
  const phase = async (name, fn) => {
    try {
      await fn();
    } catch (error) {
      setupFail(name, error);
      await shot(`fail-${name}`);
    }
  };

  try {
    await page.setViewportSize({ width: 1440, height: 900 });
    const opened = await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="main.typ"]').waitFor({ timeout: 15_000 });
    await invoke(page, "trust_grant", { root: opened.root });

    // ============================================================ CSP probes
    await phase("csp", async () => {
      // Positive controls: the listener and the DOM watchdog do detect.
      await fetch(`${ORIGIN}/control-node`).catch(() => {});
      const watchdogControl = await page.evaluate(async () => {
        const d = document.createElement("div");
        d.setAttribute("onclick", "x()");
        document.body.appendChild(d);
        await new Promise((r) => setTimeout(r, 100));
        d.remove();
        const seen = window.__watch.bad.splice(0).length;
        return seen;
      });
      check(hits.some((h) => h.url === "/control-node") && watchdogControl > 0, "controls: listener and DOM watchdog detect", { hits: hits.length, watchdogControl });
      hits.length = 0;
      const result = await page.evaluate(async (origin) => {
        const out = {};
        const s = document.createElement("script");
        s.textContent = "window.__cspInline = 1";
        document.body.appendChild(s);
        const d = document.createElement("script");
        d.src = "data:text/javascript,window.__cspData=1";
        document.body.appendChild(d);
        const b = document.createElement("script");
        b.src = URL.createObjectURL(new Blob(["window.__cspBlob=1"], { type: "text/javascript" }));
        document.body.appendChild(b);
        const r = document.createElement("script");
        r.src = `${origin}/k-csp-script.js`;
        document.body.appendChild(r);
        const img = new Image();
        img.src = `${origin}/k-csp-img.png`;
        const div = document.createElement("div");
        div.style.backgroundImage = `url(${origin}/k-csp-style.png)`;
        div.style.width = "10px";
        div.style.height = "10px";
        document.body.appendChild(div);
        const style = document.createElement("style");
        style.textContent = `@import url(${origin}/k-csp-import.css); @font-face{font-family:probe;src:url(${origin}/k-csp-font.woff)} .probe-font{font-family:probe}`;
        document.head.appendChild(style);
        const span = document.createElement("span");
        span.className = "probe-font";
        span.textContent = "font";
        document.body.appendChild(span);
        try {
          await fetch(`${origin}/k-csp-fetch`);
          out.fetch = "allowed";
        } catch (e) {
          out.fetch = `blocked: ${e.message}`;
        }
        try {
          const ws = new WebSocket(`ws://${origin.slice(7)}/k-csp-ws`);
          await new Promise((res) => {
            ws.onerror = res;
            ws.onopen = res;
            setTimeout(res, 1000);
          });
          out.ws = ws.readyState;
        } catch (e) {
          out.ws = `blocked: ${e.message}`;
        }
        const f = document.createElement("iframe");
        f.src = `${origin}/k-csp-frame`;
        document.body.appendChild(f);
        await new Promise((res) => setTimeout(res, 1500));
        out.inline = window.__cspInline ?? null;
        out.data = window.__cspData ?? null;
        out.blob = window.__cspBlob ?? null;
        for (const el of [s, d, b, r, div, span, f]) el.remove();
        style.remove();
        return out;
      }, ORIGIN);
      check(result.inline === null && result.data === null && result.blob === null, "csp: inline/data:/blob: scripts blocked", result);
      check(result.fetch.startsWith("blocked"), "csp: fetch to http blocked", result.fetch);
      // Clear the watchdog of the probe's own elements.
      await page.evaluate(() => {
        window.__watch.bad.length = 0;
        window.__watch.remote.length = 0;
      });
      // eval / new Function / setTimeout(string) from a real (non-CDP) event.
      await page.evaluate(() => {
        const btn = document.createElement("button");
        btn.id = "xp-eval-probe";
        btn.textContent = "eval probe";
        btn.style.cssText = "position:fixed;left:2px;top:2px;z-index:99999";
        btn.addEventListener("click", () => {
          const r = {};
          try {
            eval("window.__evalRan = 1");
            r.eval = "ran";
          } catch (e) {
            r.eval = `blocked: ${e.name}`;
          }
          try {
            new Function("window.__fnRan = 1")();
            r.fn = "ran";
          } catch (e) {
            r.fn = `blocked: ${e.name}`;
          }
          try {
            window.open("http://127.0.0.1:1/never", "_blank");
          } catch {}
          window.__evalProbe = r;
        });
        document.body.appendChild(btn);
      });
      await page.click("#xp-eval-probe");
      const evalProbe = await waitForAsync(page, () => window.__evalProbe ?? null, null, { timeout: 5000 });
      await page.evaluate(() => document.getElementById("xp-eval-probe")?.remove());
      check(evalProbe.eval.startsWith("blocked") && evalProbe.fn.startsWith("blocked"), "csp: eval/new Function blocked", evalProbe);
      await page.waitForTimeout(500);
      await page.evaluate(() => window.__opens.splice(0));
      check(pageCount() === pagesAtStart, "runtime: window.open from a user gesture opens no window", `pages ${pageCount()}`);
      const got = hitsFor("/k-csp");
      check(got.length === 0, "csp: no script/img/style/font/frame/fetch/ws request reached the listener", got.join(", "));
    });

    // ============================================================ research
    await phase("research", async () => {
      await runCommand(page, "Show Research", /Show Research/);
      await page.getByTestId("research-panel").waitFor({ timeout: 15_000 });
      await page.getByRole("button", { name: "Start session" }).click();
      await page.locator('[data-testid="research-session-status"][data-state="idle"]').waitFor({ timeout: 120_000 });
      const output = () => page.getByTestId("research-output");
      const runInPanel = async (code) => {
        const previous = (await output().count()) ? await output().getAttribute("data-run-id") : null;
        const area = page.getByTestId("research-code");
        await area.fill(code);
        await area.focus();
        await page.keyboard.press("Control+Enter");
        const id = await waitFor(
          async () => {
            const cur = (await output().count()) ? await output().getAttribute("data-run-id") : null;
            return cur && cur !== previous ? cur : null;
          },
          { timeout: 30_000, message: "a new research run" },
        );
        await waitFor(async () => !["running", "queued"].includes(await output().getAttribute("data-status")), {
          timeout: 120_000,
          message: `run ${id} to finish`,
        });
        return id;
      };
      const code = [
        "import json, sys",
        "from IPython.display import HTML, SVG, display",
        "P = json.load(open('payloads.json', encoding='utf-8'))",
        "display(HTML(P['table_html']))",
        "display(HTML(P['plain_html']))",
        "display(SVG(P['svg']))",
        "display({'application/javascript': P['js'], 'text/plain': 'js-plain'}, raw=True)",
        "display({'text/markdown': P['md'], 'text/plain': 'md-plain'}, raw=True)",
        "display({'text/latex': P['latex'], 'text/plain': 'latex-plain'}, raw=True)",
        "display({'application/vnd.jupyter.widget-view+json': {'model_id': 'x', 'version_major': 2, 'version_minor': 0}, 'text/plain': P['widget_text']}, raw=True)",
        "display({'text/html': P['iframe_html'], 'text/plain': 'iframe-plain'}, raw=True)",
        "print(P['stream'])",
        "sys.stderr.write(P['stream'] + '\\n')",
        "print('RUN-DONE')",
      ].join("\n");
      const runId = await runInPanel(code);
      await output().getByTestId("research-table").locator("td").first().waitFor({ timeout: 30_000 });
      await output().getByTestId("research-figure-image").first().waitFor({ timeout: 30_000 });
      await page.waitForTimeout(2000);
      const view = await page.evaluate(() => {
        const out = document.querySelector('[data-testid="research-output"]');
        return {
          cells: [...out.querySelectorAll('[data-testid="research-table"] th, [data-testid="research-table"] td')].map((c) => c.textContent),
          figures: [...out.querySelectorAll('[data-testid="research-figure-image"]')].map((i) => ({
            tag: i.tagName,
            scheme: i.src.split(":")[0],
          })),
          streams: [...out.querySelectorAll('[data-testid="research-stream"]')].map((p) => p.textContent),
          results: [...out.querySelectorAll('[data-testid="research-result"]')].map((p) => p.textContent),
          liveImgs: [...out.querySelectorAll("img")].map((i) => i.getAttribute("src")?.slice(0, 40)),
        };
      });
      info("research: rendered view", view);
      check(view.cells.includes("h<b>x") && view.cells.includes("cell1"), "research: HTML table shown as text cells (entities decoded once, not as markup)", view.cells);
      check(view.figures.length > 0 && view.figures.every((f) => f.tag === "IMG" && f.scheme === "blob"), "research: SVG figure shown only as <img> (blob:)", view.figures);
      check(view.streams.some((t) => t.includes("STREAM<img src=x")), "research: stream markup shown literally", view.streams.map((t) => t.slice(0, 60)));
      check(view.liveImgs.every((s) => s && s.startsWith("blob:")), "research: no kernel-supplied <img> in the live DOM", view.liveImgs);
      // Interact with the outputs like a user: click the figure and the table.
      await output().getByTestId("research-figure-image").first().click();
      await output().getByTestId("research-table").locator("td").nth(1).click();
      await page.waitForTimeout(1000);
      await shot("research-outputs");
      await sweep("research display/stream", "/k-");

      // Error output.
      await runInPanel("import json\nP = json.load(open('payloads.json', encoding='utf-8'))\nraise ValueError(P['error'])");
      await page.getByTestId("research-error-output").first().waitFor({ timeout: 30_000 });
      const errText = await page.getByTestId("research-error-output").first().innerText();
      check(errText.includes("ERR<img src=x"), "research: error name/value/traceback shown literally", errText.slice(0, 120));
      await sweep("research error", "/k-");
      info("research run", runId);
    });

    // ============================================================ Typst / Markdown / LaTeX diagnostics
    const modeButton = (label) => page.locator(".ui-segmented__item:visible", { hasText: label }).first();
    const problemsText = async () => {
      await page.getByRole("tab", { name: "Problems" }).click();
      await page.getByTestId("problems-panel").waitFor({ timeout: 10_000 });
      return page.getByTestId("problems-panel").innerText();
    };
    const near = (text, needle, n = 160) => {
      const i = text.indexOf(needle);
      return i < 0 ? `(not found: ${needle})` : text.slice(i, i + n);
    };
    await phase("typst", async () => {
      await openInEditor(page, "main.typ");
      await modeButton("Split").click();
      await waitFor(async () => (await problemsText()).includes("img src=x onerror"), { timeout: 60_000, message: "Typst panic in Problems" });
      const text = await problemsText();
      check(text.includes("<img src=x onerror="), "typst: panic message with markup shown literally in Problems", near(text, "panicked with: <img"));
      await page.waitForTimeout(1000);
      await shot("typst-problems");
      await sweep("typst diagnostics/page", "/k-typst");
    });
    await phase("hostile-file-name", async () => {
      const row = page.locator(`[data-testid="file-tree"] [data-path="${hostileName}"]`).first();
      await row.waitFor({ timeout: 10_000 });
      const shown = await row.innerText();
      check(shown.includes("&lt;img"), "file name: entity text shown literally in the tree", shown);
      await openInEditor(page, hostileName);
      await modeButton("Split").click();
      await waitFor(async () => (await problemsText()).includes("named-file"), { timeout: 60_000, message: "named-file panic in Problems" });
      const text = await problemsText();
      check(text.includes("x&lt;img src=x onerror=alert(1)&gt;.typ") && !text.includes("x<img src=x onerror=alert(1)>.typ"), "file name: path in Problems shown literally (not decoded)", near(text, "x&lt;img"));
      const tabTitle = await page.locator(`[data-editor-key="file:${hostileName}"]`).first().innerText();
      check(tabTitle.includes("&lt;img"), "file name: tab title literal", tabTitle);
      await sweep("hostile file name", "/k-");
    });
    await phase("markdown", async () => {
      await openInEditor(page, "notes.md");
      await modeButton("Split").click();
      await waitFor(async () => (await problemsText()).includes("cannot be used"), { timeout: 60_000, message: "Markdown image diagnostics" });
      const text = await problemsText();
      check(near(text, "k-md-image.png", 120).includes("not loaded"), "markdown: remote image refused with a diagnostic", near(text, "k-md-image.png", 120));
      check(near(text, "file:///C:/Windows/win.ini", 120).includes("not loaded"), "markdown: file: image refused with a diagnostic", near(text, "file:///C:", 120));
      info("markdown: UNC-style image //127.0.0.1/share", near(text, "k-md-unc", 160));
      check(text.includes("raw HTML is not interpreted"), "markdown: raw HTML printed verbatim (diagnostic)", near(text, "raw HTML", 100));
      await page.waitForTimeout(1500);
      await shot("markdown-split");
      await sweep("markdown page/diagnostics", "/k-md");
      // Monaco link detection: Ctrl+click an http URL in the source text.
      const pos = await page.evaluate((needle) => {
        const root = [...document.querySelectorAll('[data-testid="source-editor"]')].find((e) => e.dataset.path === "notes.md");
        for (const line of root?.querySelectorAll(".view-line") ?? []) {
          const walker = document.createTreeWalker(line, NodeFilter.SHOW_TEXT);
          let n;
          while ((n = walker.nextNode())) {
            const i = n.data.indexOf(needle);
            if (i >= 0) {
              const r = document.createRange();
              r.setStart(n, i);
              r.setEnd(n, i + needle.length);
              const b = r.getBoundingClientRect();
              return { x: b.x + b.width / 2, y: b.y + b.height / 2 };
            }
          }
        }
        return null;
      }, "k-editor-link");
      if (pos) {
        await page.keyboard.down("Control");
        await page.mouse.click(pos.x, pos.y);
        await page.keyboard.up("Control");
        await page.waitForTimeout(1500);
        const opens = await page.evaluate(() => window.__opens.slice());
        info("editor link ctrl+click: window.open calls", opens);
        await sweep("editor link ctrl+click", "/k-editor-link");
      } else {
        info("editor link", "link text not visible in the editor viewport; skipped");
      }
    });
    await phase("latex", async () => {
      const jobsBefore = new Set((await invoke(page, "job_list")).map((j) => j.info.id));
      await openInEditor(page, "paper.tex");
      await modeButton("Split").click();
      await waitFor(async () => (await problemsText()).includes("TEXERR"), { timeout: 120_000, message: "LaTeX errmessage in Problems" });
      const text = await problemsText();
      check(text.includes("TEXERR<img src=x"), "latex: diagnostic markup shown literally in Problems", near(text, "TEXERR"));
      await sweep("latex diagnostics", "/k-");
      // The TeX job log in the Jobs panel.
      let job = null;
      for (const j of (await invoke(page, "job_list")).filter((j) => !jobsBefore.has(j.info.id))) {
        const out = await invoke(page, "job_output", { job: j.info.id, after: null, maxBytes: 1024 * 1024 }).catch(() => null);
        if (out && JSON.stringify(out).includes("TEXERR")) job = j;
      }
      info("jobs: job whose output carries the TeX error", job ? job.info.label : null);
      if (job) {
        await page.getByRole("tab", { name: "Jobs" }).click();
        await page.locator(`li.exp-jobs__row[data-job-id="${job.info.id}"]`).first().click();
        await page.getByTestId("job-output").waitFor({ timeout: 10_000 });
        await waitFor(async () => (await page.getByTestId("job-output").innerText()).includes("TEXERR"), { timeout: 15_000, message: "TeX log in Jobs" });
        const log = await page.getByTestId("job-output").innerText();
        check(log.includes("TEXERR<img src=x"), "jobs: TeX log markup shown literally", log.slice(log.indexOf("TEXERR"), log.indexOf("TEXERR") + 80));
        await shot("jobs-tex-log");
        await sweep("jobs log", "/k-");
      } else {
        info("jobs", "no LaTeX job found to inspect");
      }
    });

    // ============================================================ Lean
    await phase("lean", async () => {
      if (!leanAvailable) {
        info("lean", `SKIPPED: ${LEAN_TOOLCHAIN_DIR} not installed`);
        return;
      }
      await runCommand(page, "show lean infoview", /Show Lean Infoview/);
      const docId = await openInEditor(page, "LeanBasic/Hostile.lean");
      const report = await invoke(page, "lean_check", {
        request: { target: { kind: "document", document: docId }, goalsAt: [], timeoutMs: 240_000 },
      });
      const messages = report.diagnostics.map((d) => d.message);
      info("lean: messages", messages.map((m) => m.slice(0, 100)));
      await page.getByTestId("lean-panel").waitFor();
      await waitFor(async () => (await page.getByTestId("lean-panel").innerText()).includes("LEANMSG"), { timeout: 30_000, message: "Lean message in the infoview" });
      const panelText = await page.getByTestId("lean-panel").innerText();
      check(panelText.includes("LEANMSG<img src=x") && panelText.includes("LEANERR<b>bold</b>"), "lean: messages shown literally in the infoview", panelText.slice(panelText.indexOf("LEANMSG"), panelText.indexOf("LEANMSG") + 80));
      const problems = await problemsText();
      check(problems.includes("LEANMSG<img") && problems.includes("LEANERR<b>bold</b>"), "lean: messages literal in Problems", near(problems, "LEANMSG"));
      await sweep("lean messages", "/k-");
      // The server's hover for the documented definition (what Monaco renders).
      const state = await invoke(page, "document_state", { document: docId });
      const offset = state.text.indexOf("def hostileDef") + 6;
      const ipcHover = await invoke(page, "lean_hover", { document: docId, revision: state.info.revision, offset }).catch((e) => ({ error: String(e) }));
      info("lean hover: server markdown", JSON.stringify(ipcHover).slice(0, 400));
      // Hover over the documented definition: Monaco renders the docstring.
      await page.locator('[data-testid="source-editor"][data-path="LeanBasic/Hostile.lean"] .view-lines').click();
      const hover = await page.evaluate((needle) => {
        const root = [...document.querySelectorAll('[data-testid="source-editor"]')].find((e) => e.dataset.path === "LeanBasic/Hostile.lean");
        for (const line of root?.querySelectorAll(".view-line") ?? []) {
          if (!line.textContent.replace(/\u00a0/g, " ").startsWith("def hostileDef")) continue;
          const walker = document.createTreeWalker(line, NodeFilter.SHOW_TEXT);
          let n;
          while ((n = walker.nextNode())) {
            const i = n.data.indexOf(needle);
            if (i >= 0) {
              const r = document.createRange();
              r.setStart(n, i);
              r.setEnd(n, i + needle.length);
              const b = r.getBoundingClientRect();
              return { x: b.x + b.width / 2, y: b.y + b.height / 2 };
            }
          }
        }
        return null;
      }, "hostileDef");
      if (!hover) throw new Error("hostileDef not visible in the editor");
      const hoverBox = page.locator(".monaco-hover", { hasText: "HOVERDOC" }).first();
      for (let attempt = 0; attempt < 4 && !(await hoverBox.isVisible().catch(() => false)); attempt++) {
        await page.mouse.move(hover.x - 20, hover.y + 40);
        await page.mouse.move(hover.x, hover.y, { steps: 4 });
        await page.waitForTimeout(1500);
        if (await hoverBox.isVisible().catch(() => false)) break;
        // Keyboard: caret on the word, then "Show Hover" (Ctrl+K Ctrl+I).
        await page.mouse.click(hover.x, hover.y);
        await page.keyboard.press("Control+K");
        await page.keyboard.press("Control+I");
        await page.waitForTimeout(1500);
      }
      info("lean hover: visible hover widgets", await page.locator(".monaco-hover").evaluateAll((els) => els.map((e) => e.innerText.slice(0, 120))));
      if (!(await hoverBox.isVisible().catch(() => false))) {
        // The bundle the debug app embeds (dist/ from the same checkout).
        const assets = join(import.meta.dirname, "..", "..", "..", "dist", "assets");
        const entry = (await readdir(assets).catch(() => [])).find((f) => /^index-.*\.js$/.test(f));
        const text = entry ? await readFile(join(assets, entry), "utf8") : "";
        const bundled = { entry, contentHover: text.includes("editor.contrib.contentHover"), linkDetector: text.includes("editor.linkDetector") };
        info("lean hover: no hover widget appears; Monaco contributions in the bundle", bundled);
        await sweep("lean hover (not rendered)", "/k-lean");
        return;
      }
      await page.waitForTimeout(1500);
      const hoverView = await hoverBox.evaluate((el) => ({
        text: el.innerText.slice(0, 300),
        imgs: [...el.querySelectorAll("img")].map((i) => i.getAttribute("src")),
        links: [...el.querySelectorAll("a")].map((a) => ({ href: a.getAttribute("href"), dataHref: a.dataset.href ?? null, text: a.textContent })),
        scripts: el.querySelectorAll("script, iframe, object, embed").length,
      }));
      info("lean hover: rendered", hoverView);
      await shot("lean-hover");
      check(hoverView.scripts === 0, "lean hover: no script/iframe/object elements", hoverView.scripts);
      check(!hoverView.links.some((l) => /^(command|javascript):/i.test(l.dataHref ?? l.href ?? "")), "lean hover: command:/javascript: links stripped (isTrusted: false)", hoverView.links);
      check(hitsFor("/k-lean-hover-img").length === 0 && hitsFor("/k-lean-hover-raw").length === 0, "lean hover: remote image not fetched (CSP img-src)", hitsFor("/k-lean"));
      // Click the http link like a user.
      const link = hoverBox.locator("a", { hasText: "probe link" }).first();
      if (await link.count()) {
        await link.click();
        await page.waitForTimeout(1500);
        const opens = await page.evaluate(() => window.__opens.slice());
        info("lean hover link click: window.open calls", opens);
      }
      await page.mouse.move(5, 5);
      await sweep("lean hover", "/k-lean");
    });

    // ============================================================ Agents transcript
    await phase("agents", async () => {
      const status = await invoke(page, "agents_provider_status");
      if (!status.scriptedAvailable) throw new Error("scripted provider not available");
      await runCommand(page, "Show Agent Tasks", /Show Agent Tasks/);
      const panel = page.getByTestId("agents-panel");
      await panel.waitFor({ timeout: 15_000 });
      const selectOption = async (scope, label, option) => {
        await scope.getByRole("button", { name: label }).click();
        await page.getByRole("option", { name: option, exact: true }).click();
      };
      // Select the development-only scripted provider in the provider dialog.
      await panel.getByRole("button", { name: "Agent provider settings" }).click();
      const dialog = page.getByRole("dialog", { name: "Agent provider" });
      await dialog.waitFor();
      await selectOption(dialog, /Provider/, "Scripted provider — development only");
      await dialog.getByRole("button", { name: "Save settings" }).click();
      await page.waitForSelector("[data-testid=agents-settings-message]");
      await dialog.getByRole("button", { name: "Close" }).click();
      await dialog.waitFor({ state: "detached" });
      const startTask = async (prompt, permission) => {
        await panel.getByTestId("agents-new-task").click();
        await page.fill("[data-testid=agents-prompt]", prompt);
        await panel.getByTestId("agents-folder").waitFor();
        await selectOption(panel, /Document changes/, permission);
        await panel.getByRole("button", { name: "Start task" }).click();
        await page.waitForSelector("[data-testid=agents-task-view]");
      };
      const waitForStatus = (kind, timeout = 30_000) =>
        waitFor(async () => (await page.getAttribute("[data-testid=agents-task-status]", "data-status")) === kind, {
          timeout,
          message: `agent status ${kind}`,
        });
      await invoke(page, "agents_dev_load_script", {
        script: {
          turns: [
            {
              steps: [
                { thinking: `THINK<img src=x onerror="${P("agent-thinking")}">` },
                { text: `ASSIST<img src=x onerror="${P("agent-text")}"><script>${P("agent-script")}</script>[l](javascript:${P("agent-md")}) ![i](${ORIGIN}/k-agent-img.png)` },
                { toolUse: { id: "toolu_read", name: "read_file", input: { path: "hostile.txt" } } },
                { toolUse: { id: "toolu_bad", name: "read_file", input: { path: `BADPATH<img src=x onerror="${P("agent-input")}">.txt` } } },
              ],
            },
            { steps: [{ text: `FINAL<svg onload="${P("agent-final")}"></svg>` }] },
          ],
        },
      });
      await startTask(`PROMPT<img src=x onerror="${P("agent-prompt")}">`, "Read only");
      await waitForStatus("completed");
      await page.waitForTimeout(500);
      const notes = await page.getByTestId("notification").allInnerTexts();
      check(notes.some((t) => t.includes("PROMPT<img src=x")), "notifications: task title with markup shown literally", notes.map((t) => t.slice(0, 80)));
      // Expand tool call details.
      for (const s of await page.locator("[data-testid=agents-task-view] details > summary").all()) await s.click().catch(() => {});
      await page.waitForTimeout(500);
      const transcript = await page.getByTestId("agents-task-view").innerText();
      check(
        transcript.includes("ASSIST<img src=x") && transcript.includes("PROMPT<img src=x") && transcript.includes("THINK<img src=x") &&
          transcript.includes("TOOLRESULT<img src=x") && transcript.includes("BADPATH<img src=x"),
        "agents: prompt, reasoning, assistant text, tool input and tool result shown literally",
        ["ASSIST", "PROMPT", "THINK", "TOOLRESULT", "BADPATH", "FINAL"].map((k) => `${k}:${transcript.includes(`${k}<`)}`).join(" "),
      );
      await shot("agents-transcript");
      await sweep("agent transcript", "/k-agent");

      // Approval card + review diff with hostile edit text.
      await invoke(page, "agents_dev_load_script", {
        script: {
          turns: [
            { steps: [{ toolUse: { id: "toolu_r2", name: "read_file", input: { path: "main.typ" } } }] },
            {
              steps: [
                {
                  toolUse: {
                    id: "toolu_e2",
                    name: "edit_document",
                    input: {
                      path: "main.typ",
                      base_revision: "$ref:toolu_r2/revision",
                      edits: [{ old_text: "Hello.", new_text: `EDIT<img src=x onerror="${P("agent-edit")}"><script>${P("agent-edit-script")}</script>` }],
                    },
                  },
                },
              ],
            },
            { steps: [{ text: "done" }] },
          ],
        },
      });
      await startTask("Edit main", "Ask before each change");
      await page.waitForSelector("[data-testid=agents-approval]", { timeout: 30_000 });
      const approvalText = await page.getByTestId("agents-approval").innerText();
      check(approvalText.includes("EDIT<img src=x"), "agents: approval detail shows the markup literally", approvalText.slice(0, 200));
      await sweep("agent approval card", "/k-agent");
      await panel.getByRole("button", { name: "Approve" }).click();
      await waitForStatus("completed");
      // Open the review (Monaco diff) if offered.
      const review = panel.getByRole("button", { name: /Review/ }).first();
      if (await review.count()) {
        await review.click().catch(() => {});
        await page.waitForTimeout(1500);
      }
      await shot("agents-review");
      await sweep("agent review/diff", "/k-agent");
    });
  } finally {
    await page.evaluate(() => window.__watch).then((w) => info("final watchdog", w)).catch(() => {});
    await shot("final");
    await app.close();
    server.close();
    info("all listener hits", hits.map((h) => h.url));
    await writeFile(join(artifactsDir, "findings.json"), JSON.stringify({ origin: ORIGIN, root, findings, hits }, null, 2));
  }

  const defects = findings.filter((f) => f.kind === "DEFECT");
  const setup = findings.filter((f) => f.kind === "SETUP-FAIL");
  console.log(`${NAME}: ${findings.filter((f) => f.kind === "ok").length} ok, ${defects.length} DEFECT, ${setup.length} SETUP-FAIL`);
  if (defects.length || setup.length) {
    throw new Error(`${defects.length} DEFECT, ${setup.length} SETUP-FAIL: ${[...defects, ...setup].map((d) => d.id).join("; ")}`);
  }
}
