// Shared helpers for the visual equation editor scenarios (equation-source,
// equation-page): one temporary project per language from
// tests/fixtures/equations/<language>, opening its document in a mode, waiting
// for pages that cover the current revision, driving the Equation panel, and
// placing carets in Monaco and on the pages.
import { cp, mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { openFromTree } from "./workspace-helpers.mjs";
import { waitForAsync } from "./wait.mjs";
import { runCommand } from "./palette.mjs";

/** Document and fixture subfolder per language (under the `equations` fixture). */
export const LANGUAGES = {
  latex: { path: "main.tex", folder: "latex" },
  typst: { path: "main.typ", folder: "typst" },
  markdown: { path: "main.md", folder: "markdown" },
};

/** Remove all whitespace (serializers may space operators differently). */
export const squash = (text) => text.replace(/\s+/g, "");

/**
 * A fresh temporary project for `language` from `fixtureRoot` (the scenario's
 * `fixture("equations")`); LaTeX: with the chosen engine as default target.
 */
export async function makeEquationProject(fixtureRoot, language, engine = "pdfLatex") {
  const root = join(await mkdtemp(join(tmpdir(), `exposition-eq-${language}-`)), "project");
  await cp(join(fixtureRoot, LANGUAGES[language].folder), root, { recursive: true });
  if (language === "latex") {
    await mkdir(join(root, ".exposition"), { recursive: true });
    await writeFile(
      join(root, ".exposition", "project.json"),
      JSON.stringify({
        version: 1,
        entry: "main.tex",
        targets: [
          { name: "pdf", format: "latex", engine: "pdfLatex", entry: "main.tex" },
          { name: "lua", format: "latex", engine: "luaLatex", entry: "main.tex" },
        ],
        defaultTarget: engine === "luaLatex" ? "lua" : "pdf",
      }),
    );
  }
  return root;
}

/** Everything a scenario needs for one open document. */
export function equationSession(page, path) {
  const invoke = (cmd, args = {}) => page.evaluate(([c, a]) => window.__TAURI_INTERNALS__.invoke(c, a), [cmd, args]);
  const docId = async () => (await invoke("document_list")).find((d) => d.path === path).id;
  const state = async () => invoke("document_state", { document: await docId() });
  const text = async () => (await state()).text;
  const revision = async () => (await state()).info.revision;
  const pageEditor = page.locator('[data-testid="page-editor"]');
  const view = page.locator('[data-testid="equation-view"]');
  const mathEditor = page.locator('[data-testid="equation-editor"]');

  /** Wait until the document text satisfies `predicate`. */
  const waitText = async (predicate, what, timeout = 10000) => {
    const deadline = Date.now() + timeout;
    let last = "";
    while (!predicate((last = await text()))) {
      if (Date.now() > deadline) throw new Error(`source never satisfied: ${what}\n---\n${last}`);
      await new Promise((r) => setTimeout(r, 50));
    }
    return last;
  };

  /** Wait until the shown pages were built from the current revision and the build succeeded. */
  const settled = async (label, timeout = 60000) => {
    await waitForAsync(
      page,
      async (p) => {
        const el = document.querySelector('[data-testid="page-editor"]');
        const img = document.querySelector('[data-testid="page"] img');
        if (!(el?.dataset.stale === "false" && el.dataset.build && img?.complete)) return false;
        const docs = await window.__TAURI_INTERNALS__.invoke("document_list");
        const d = docs.find((x) => x.path === p);
        return !!d && el.dataset.buildRevision === String(d.revision);
      },
      path,
      { timeout, message: `pages settled (${label})` },
    );
    if ((await pageEditor.getAttribute("data-failed")) === "true") throw new Error(`build failed (${label})`);
    return pageEditor.getAttribute("data-build");
  };

  /** Rendered text of a source range on the shown pages. */
  const rendered = async (start, end) =>
    (await invoke("page_selection", { build: await pageEditor.getAttribute("data-build"), path, start, end })).text;

  /** Screen point of a source offset on the shown pages (scrolled into view). */
  const point = async (offset, affinity = "downstream", dx = 0) => {
    const g = await invoke("page_caret", { build: await pageEditor.getAttribute("data-build"), path, offset, affinity });
    if (!g) throw new Error(`no page geometry for offset ${offset}`);
    const el = page.locator(`[data-testid="page"][data-page="${g.page}"]`);
    await el.evaluate(
      (pageEl, [top, x]) => {
        const scroller = pageEl.closest('[data-testid="page-scroller"]');
        const scale = pageEl.getBoundingClientRect().width / Number(pageEl.dataset.widthPt);
        const box = scroller.getBoundingClientRect();
        scroller.scrollTop += pageEl.getBoundingClientRect().top + top * scale - (box.top + box.height / 2);
        scroller.scrollLeft += pageEl.getBoundingClientRect().left + x * scale - (box.left + box.width / 2);
      },
      [g.top, g.x + dx],
    );
    const box = await el.boundingBox();
    const scale = box.width / Number(await el.getAttribute("data-width-pt"));
    return { x: box.x + (g.x + dx) * scale, y: box.y + ((g.top + g.bottom) / 2) * scale, mode: g.mode };
  };

  /**
   * Click on the page at a source offset and wait for the caret there (or,
   * with `mode`, for any caret in that mode: "Math" inside an equation).
   */
  const clickPageAt = async (offset, { affinity = "downstream", dx = 0.3, mode = null } = {}) => {
    const p = await point(offset, affinity, dx);
    await page.mouse.click(p.x, p.y);
    if (mode) {
      await page.locator('[data-testid="page-mode"]', { hasText: mode }).waitFor({ timeout: 10000 });
      return;
    }
    await page.waitForFunction(
      (o) => document.querySelector('[data-testid="page-caret"]')?.getAttribute("data-offset") === String(o),
      offset,
      { timeout: 10000 },
    );
  };

  /** Put the Monaco caret on the line containing `marker`, at its end or `column` (0-based) characters in. */
  const monacoCaret = async (marker, column = null) => {
    const line = page.locator('[data-testid="source-editor"] .view-line', { hasText: marker }).first();
    await line.click();
    if (column === null) {
      await page.keyboard.press("End");
    } else {
      await page.keyboard.press("Home");
      // Home goes to the first non-blank character; a second Home to column 1 when indented.
      const lineText = (await line.textContent()).replace(/ /g, " ");
      if (/^\s/.test(lineText)) await page.keyboard.press("Home");
      for (let i = 0; i < column; i++) await page.keyboard.press("ArrowRight");
    }
  };

  /** Wait for a draft of `kind` ("insert" | "edit") in the panel, focused. */
  const draft = async (kind) => {
    await page.locator(`[data-testid="equation-view"][data-state="draft"][data-kind="${kind}"]`).waitFor({ timeout: 10000 });
    await page.waitForFunction(() => document.activeElement?.getAttribute("data-testid") === "equation-editor", null, {
      timeout: 5000,
    });
  };

  /** Type into the structural editor (each character a key press). */
  const mathType = async (keys) => {
    await mathEditor.focus();
    await page.keyboard.type(keys);
  };

  const template = (label) => page.locator('[data-testid="equation-templates"]').getByRole("button", { name: label, exact: true }).click();

  const apply = async () => {
    await page.getByTestId("equation-apply").click();
    await page.locator('[data-testid="equation-view"][data-state="idle"]').waitFor({ timeout: 10000 });
  };

  /** Show the Equation panel through the palette. */
  const showPanel = async () => {
    await runCommand(page, "Show Equation Panel", /Show Equation Panel/);
    await view.waitFor({ timeout: 10000 });
  };

  /** Search the symbol browser and return the first option's accessible name. */
  const search = async (query) => {
    const field = page.getByRole("searchbox", { name: "Search symbols" });
    await field.fill(query);
    const first = page.locator('[data-testid="equation-symbol-grid"] [role="option"]').first();
    await page.waitForFunction(
      (q) => document.querySelector('[data-testid="equation-symbol-grid"]')?.getAttribute("aria-label") === `Symbols matching ${q}`,
      query,
      { timeout: 5000 },
    );
    return first.getAttribute("aria-label");
  };

  return {
    invoke,
    docId,
    state,
    text,
    revision,
    waitText,
    settled,
    rendered,
    point,
    clickPageAt,
    monacoCaret,
    draft,
    mathType,
    template,
    apply,
    showPanel,
    search,
    view,
    mathEditor,
    pageEditor,
  };
}

/**
 * Open `path` from the file tree and switch to `mode` ("Source" | "Page" |
 * "Split"); a LaTeX project's first page view asks for workspace trust
 * (`trustPrompt: false` once it was granted).
 */
export async function openDocumentIn(page, path, mode, { trustPrompt = true } = {}) {
  await page.locator(`[data-testid="file-tree"] [data-path="${path}"]`).waitFor({ timeout: 10000 });
  await openFromTree(page, path);
  const button = page.locator(".ui-segmented__item", { hasText: mode }).first();
  await button.waitFor({ timeout: 10000 });
  await button.click();
  if (mode === "Source") {
    await page.locator('[data-testid="source-editor"] .monaco-editor').waitFor({ timeout: 10000 });
    return;
  }
  await page.locator('[data-testid="page-editor"]').waitFor({ timeout: 10000 });
  if (path.endsWith(".tex") && trustPrompt) {
    const dialog = page.getByRole("alertdialog", { name: "Trust this workspace?" });
    await dialog.waitFor({ timeout: 15000 });
    await dialog.getByRole("button", { name: "Trust workspace", exact: true }).click();
    await dialog.waitFor({ state: "detached" });
  }
}

/** The structural patterns each language's serializer must produce (whitespace removed). */
export const PATTERNS = {
  // "+1" typed at the end of `E = m c^2` (LaTeX: the \label inside the
  // equation is the row's last, opaque item, so "+1" follows it).
  energyPlusOne: /E=mc\^\{?2\}?(\\label\{eq:energy\})?\+1/,
  nestedFraction: {
    latex: /\\frac\{1\}\{\\sqrt\{x\^(\{2\}|2)\}\}/,
    markdown: /\\frac\{1\}\{\\sqrt\{x\^(\{2\}|2)\}\}/,
    typst: /(frac\(1,sqrt\(x\^(\(2\)|2)\)\)|\(?1\)?\/\(?sqrt\(x\^(\(2\)|2)\)\)?)/,
  },
  leq: { latex: /\\leq?(?![a-zA-Z])/, markdown: /\\leq?(?![a-zA-Z])/, typst: /(lt\.eq|<=|≤)/ },
  alpha: { latex: /\\alpha/, markdown: /\\alpha/, typst: /alpha|α/ },
  matrix: { latex: /\\begin\{[pbv]?matrix\}/, markdown: /\\begin\{[pbv]?matrix\}/, typst: /mat\(/ },
  cases: { latex: /\\begin\{cases\}/, markdown: /\\begin\{cases\}/, typst: /cases\(/ },
};
