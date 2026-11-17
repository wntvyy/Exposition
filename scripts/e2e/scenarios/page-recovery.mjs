// Recovery from page-view states in the real app (ST, Codex review R1):
//  1. XR-A-01 — an untrusted LaTeX project's build status ("main.tex:
//     workspace not trusted") does not survive leaving the project: Close
//     Folder → Recent → an unrelated project → open beta.txt shows no build
//     status and no Problems from the previous folder.
//  2. XR-B-01 — in a Typst document whose latest build failed while Page view
//     shows the last good pages, one click on a source diagnostic past those
//     pages (an unclosed delimiter appended at the end) shows the source:
//     the editor switches to Split with the Monaco caret on the error line
//     and keyboard focus in the source. A diagnostic the pages can show keeps
//     Page mode (page caret placed, no switch).
// No TeX is needed (the LaTeX build is refused before any engine runs).
// Screenshots: test-results/e2e/page-recovery/.
import { cp, mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";
import { invoke, openFromTree, waitFor } from "../workspace-helpers.mjs";

export default async function pageRecovery({ launchApp, artifactsDir, assert, fixture }) {
  const base = await mkdtemp(join(tmpdir(), "xp-page-recovery-"));
  const latex = join(base, "untrusted-paper");
  const plain = join(base, "notes");
  const typst = join(base, "typst-paper");
  await cp(fixture("latex-basic"), latex, { recursive: true });
  await cp(fixture("typst-basic"), typst, { recursive: true });
  await mkdir(plain, { recursive: true });
  await writeFile(join(plain, "beta.txt"), "Beta notes.\n");
  const app = await launchApp();
  const { page } = app;
  const evidence = [];
  const shot = (name) => page.screenshot({ path: join(artifactsDir, `${name}.png`) });
  const modeButton = (label) => page.locator(".ui-segmented__item", { hasText: label }).first();
  const buildStatus = page.locator('[data-testid="build-status"]');

  try {
    await page.setViewportSize({ width: 1440, height: 900 });

    // --- 1. XR-A-01: a previous project's build status ---------------------------------
    // The unrelated project is in Recent.
    await invoke(page, "workspace_open", { path: plain });
    await page.locator('[data-testid="file-tree"] [data-path="beta.txt"]').waitFor({ timeout: 10_000 });
    await invoke(page, "workspace_open", { path: latex });
    await page.locator('[data-testid="file-tree"] [data-path="main.tex"]').waitFor({ timeout: 10_000 });
    await openFromTree(page, "main.tex");
    await modeButton("Page").waitFor({ timeout: 10_000 });
    await modeButton("Page").click();
    const trust = page.getByRole("alertdialog", { name: "Trust this workspace?" });
    await trust.waitFor({ timeout: 15_000 });
    await trust.getByRole("button", { name: /Don.t trust/ }).click();
    await trust.waitFor({ state: "detached" });
    await page.locator('[data-testid="build-status"][data-status="untrusted"]').waitFor({ timeout: 10_000 });
    assert.match(await buildStatus.innerText(), /main\.tex: workspace not trusted/);
    await shot("01-untrusted-latex");

    await runCommand(page, "Close Folder", /File: Close Folder/);
    const recent = page.locator('[data-testid="recent-project"]').filter({ hasText: "notes" }).first();
    await recent.waitFor({ timeout: 10_000 });
    await recent.getByRole("button").first().click();
    await page.locator('[data-testid="file-tree"] [data-path="beta.txt"]').waitFor({ timeout: 10_000 });
    await openFromTree(page, "beta.txt");
    await page.locator('[data-testid="source-editor"][data-path="beta.txt"] .monaco-editor').waitFor({ timeout: 10_000 });
    await page.waitForTimeout(500);
    assert.equal(await buildStatus.count(), 0, `no build status from the previous folder: ${await buildStatus.allInnerTexts()}`);
    await page.getByRole("tab", { name: "Problems" }).click();
    await page.getByTestId("problems-panel").waitFor({ timeout: 5_000 });
    assert.equal(await page.locator('[data-testid="problem"]').count(), 0, "no Problems from the previous folder");
    await shot("02-switched-project-no-stale-status");
    evidence.push("XR-A-01: after Close Folder → Recent → beta.txt the status bar shows no main.tex status and Problems is empty");

    // --- 2. XR-B-01: a diagnostic the last good pages cannot show ------------------------
    await invoke(page, "workspace_open", { path: typst });
    await page.locator('[data-testid="file-tree"] [data-path="main.typ"]').waitFor({ timeout: 10_000 });
    await openFromTree(page, "main.typ");
    await modeButton("Page").waitFor({ timeout: 10_000 });
    await modeButton("Page").click();
    const editor = page.locator('[data-testid="page-editor"]');
    await editor.waitFor({ timeout: 10_000 });
    await page.waitForFunction(() => {
      const el = document.querySelector('[data-testid="page-editor"]');
      const img = document.querySelector('[data-testid="page"] img');
      return el?.dataset.stale === "false" && el.dataset.build && img?.complete;
    }, null, { timeout: 30_000 });
    const doc = (await invoke(page, "document_list")).find((d) => d.path === "main.typ").id;
    const apply = async (id, patches) => {
      const current = await invoke(page, "document_state", { document: doc });
      await invoke(page, "document_apply", {
        request: {
          id,
          origin: { kind: "source" },
          edits: [{ document: doc, baseRevision: current.info.revision, patches: patches(current.text) }],
          undo: { kind: "newGroup" },
          conflict: "reject",
          label: null,
          selection: null,
        },
      });
    };
    const failed = (value) =>
      page.waitForFunction((v) => document.querySelector('[data-testid="page-editor"]')?.dataset.failed === v, value, { timeout: 15_000 });
    await page.getByRole("tab", { name: "Problems" }).click();

    // (a) A diagnostic the shown pages contain keeps Page mode: the escaped
    // "\#tag" becomes code ("unknown variable: tag") in text the last good
    // pages show; this page view has followed every edit since that build.
    await apply("e2e-break-shown-text", (text) => {
      const escape = text.indexOf("\\#tag");
      assert.ok(escape > 0, "fixture text");
      return [{ range: { start: escape, end: escape + 1 }, text: "" }];
    });
    await failed("true");
    const inside = page.locator('[data-testid="problem"][data-source="build"]').filter({ hasText: /tag/ }).first();
    await inside.waitFor({ timeout: 15_000 });
    const insideText = await inside.innerText();
    await inside.click();
    await page.locator('[data-testid="page-caret"]').first().waitFor({ timeout: 5_000 });
    await page.waitForTimeout(800);
    assert.equal(await page.getByTestId("typst-split").count(), 0, `a position on the shown pages keeps Page mode (${insideText})`);
    await shot("03-diagnostic-on-page-keeps-page-mode");
    evidence.push(`XR-B-01: a diagnostic inside the shown pages ("${insideText.replace(/\s+/g, " ")}") keeps Page mode with the page caret`);
    // Fixed again: a good build.
    await apply("e2e-fix-shown-text", (text) => {
      const at = text.indexOf("#tag");
      return [{ range: { start: at, end: at }, text: "\\" }];
    });
    await failed("false");

    // (b) The reviewer's case: an unclosed delimiter appended after the last good pages.
    await apply("e2e-unclosed", (text) => [{ range: { start: text.length, end: text.length }, text: "\n#let broken = (\n" }]);
    await failed("true");
    // The pages shown now are the last good build (read after the failure).
    const goodBuild = await editor.getAttribute("data-build");
    assert.ok(goodBuild && (await page.locator('[data-testid="page"]').count()) > 0, "the last good pages stay visible");
    const problem = page.locator('[data-testid="problem"][data-source="build"]').filter({ hasText: /unclosed/i }).first();
    await problem.waitFor({ timeout: 10_000 });
    const location = await waitFor(async () => {
      const text = await problem.locator(".problems__location").innerText();
      return /main\.typ:\d+:\d+/.test(text) ? text : null;
    }, { message: "diagnostic position" });
    const errorLine = Number(/main\.typ:(\d+):/.exec(location)[1]);
    assert.equal(await page.getByTestId("typst-split").count(), 0, "Page mode before the click");
    await shot("04-failed-build-page-mode");

    // One click: the source is shown beside the pages, caret on the error line, focused.
    await problem.click();
    await page.getByTestId("typst-split").waitFor({ timeout: 10_000 });
    const source = page.locator('[data-testid="source-editor"][data-path="main.typ"]');
    await source.locator(".monaco-editor").waitFor({ timeout: 10_000 });
    const activeLine = await waitFor(async () => {
      const text = await source.locator(".line-numbers.active-line-number").first().innerText().catch(() => "");
      return Number(text) === errorLine ? text : null;
    }, { timeout: 10_000, message: `Monaco caret on line ${errorLine}` });
    const focusedInSource = await page.evaluate(
      () => !!document.activeElement?.closest('[data-testid="source-editor"][data-path="main.typ"]'),
    );
    assert.ok(focusedInSource, "keyboard focus is in the source editor");
    // The Split's page view attaches anew and shows the same last good pages.
    await waitFor(async () => (await editor.getAttribute("data-build")) === goodBuild, {
      timeout: 10_000,
      message: "the last good pages shown beside the source",
    });
    await page.waitForTimeout(300);
    await shot("05-diagnostic-shown-in-split");
    evidence.push(`XR-B-01: Problems click on "${location}" switched Page → Split, Monaco active line ${activeLine}, focus in source`);

    const errors = app.consoleMessages.filter((m) => m.startsWith("error:") || m.startsWith("pageerror:"));
    assert.deepEqual(errors, [], "no console errors");
  } finally {
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2)).catch(() => {});
    await app.close();
  }
}
