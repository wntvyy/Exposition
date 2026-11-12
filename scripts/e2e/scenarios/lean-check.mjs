// Lean end to end in the real app, against the real Lean server (`lake serve`
// of the pinned toolchain): open the lean-basic fixture project, see that an
// untrusted workspace does not run Lean, trust it, open a Lean file and see
// Errors + goals at the cursor, fix the proof in Monaco (unsaved buffer) and see
// Incomplete (sorry) distinguished from Checked, use a Unicode abbreviation,
// jump to a definition in another file, and capture the infoview in both themes.
//
// Requires Lean through elan with leanprover/lean4:v4.35.0 installed; without it
// the scenario reports SKIPPED unless EXPOSITION_REQUIRE_LEAN=1.
import { existsSync } from "node:fs";
import { copyFile, cp, mkdir, mkdtemp, readFile, readdir, rm } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";

import { alertDialog, documentState, editorLocator, invoke, openInEditor, waitFor } from "../workspace-helpers.mjs";
import { runCommand } from "../palette.mjs";

const TOOLCHAIN_DIR = "leanprover--lean4---v4.35.0";
const PROOFS = "LeanBasic/Proofs.lean";

/** Screen box of the `occurrence`-th `needle` in a Monaco view line (spaces render as NBSP). */
async function textBox(line, needle, occurrence = 0) {
  return line.evaluate(
    (el, [n, k]) => {
      const nodes = [];
      const walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
      while (walker.nextNode()) nodes.push(walker.currentNode);
      const text = nodes.map((t) => t.data).join("").replace(/\u00a0/g, " ");
      let at = -1;
      for (let i = 0; i <= k; i++) at = text.indexOf(n, at + 1);
      if (at < 0) return null;
      const locate = (offset) => {
        for (const node of nodes) {
          if (offset <= node.data.length) return [node, offset];
          offset -= node.data.length;
        }
        return [nodes.at(-1), nodes.at(-1).data.length];
      };
      const range = document.createRange();
      range.setStart(...locate(at));
      range.setEnd(...locate(at + n.length));
      const r = range.getBoundingClientRect();
      return { x: r.x, y: r.y, width: r.width, height: r.height };
    },
    [needle, occurrence],
  );
}

/** Hover `target` (a locator or a screen box) in a Monaco editor; returns the visible hover's text containing `expected`. */
async function hoverText(page, target, expected, timeout = 30000) {
  const box = typeof target.boundingBox === "function" ? await target.boundingBox() : target;
  if (!box) throw new Error("hover target is not visible");
  const deadline = Date.now() + timeout;
  for (let dx = 0; Date.now() < deadline; dx = (dx + 1) % 3) {
    // Re-enter the target now and then: Monaco shows a hover after the pointer rests.
    await page.mouse.move(box.x + 1, box.y - 30);
    await page.mouse.move(box.x + box.width / 2 + dx, box.y + box.height / 2);
    const hover = page.locator(".monaco-hover:visible").filter({ hasText: expected });
    if (await hover.first().waitFor({ state: "visible", timeout: 3000 }).then(() => true, () => false)) {
      return hover.first().innerText();
    }
  }
  const shown = await page.locator(".monaco-hover:visible").allInnerTexts();
  throw new Error(`no hover with ${expected} (visible hovers: ${JSON.stringify(shown).slice(0, 300)})`);
}

async function dismissHover(page) {
  await page.mouse.move(5, 5);
  await page.keyboard.press("Escape");
  await page.locator(".monaco-hover:visible").first().waitFor({ state: "hidden", timeout: 5000 }).catch(() => {});
}

async function setTheme(page, assert, theme) {
  await runCommand(page, `color theme ${theme}`, theme === "dark" ? /Color Theme: Dark/ : /Color Theme: Light/);
  await waitFor(async () => (await page.evaluate(() => document.documentElement.dataset.theme)) === theme, {
    message: `${theme} theme`,
  });
}

export default async function leanCheck({ launchApp, artifactsDir, assert, fixture }) {
  const elanHome = process.env.ELAN_HOME || join(homedir(), ".elan");
  if (!existsSync(join(elanHome, "toolchains", TOOLCHAIN_DIR, "bin"))) {
    const message = `SKIPPED: Lean toolchain ${TOOLCHAIN_DIR} is not installed through elan`;
    if (process.env.EXPOSITION_REQUIRE_LEAN === "1") throw new Error(message);
    console.log(message);
    return;
  }
  const base = await mkdtemp(join(tmpdir(), "xp-lean-"));
  const root = join(base, "project");
  const dataDir = join(base, "data");
  await cp(fixture("lean-basic"), root, {
    recursive: true,
    filter: (src) => !/[\\/]\.lake([\\/]|$)/.test(src) && !src.endsWith("lake-manifest.json"),
  });
  await mkdir(dataDir, { recursive: true });
  const original = await readFile(join(root, "LeanBasic", "Proofs.lean"), "utf8");

  const app = await launchApp({ dataDir });
  const { page } = app;
  const shots = [];
  const shot = async (name) => {
    const path = join(artifactsDir, `${name}.png`);
    await page.screenshot({ path });
    shots.push(path);
  };
  const bothThemes = async (name) => {
    await shot(`${name}-light`);
    await setTheme(page, assert, "dark");
    await shot(`${name}-dark`);
    await setTheme(page, assert, "light");
  };
  try {
    await page.setViewportSize({ width: 1440, height: 900 });
    await setTheme(page, assert, "light");

    // --- open the project: untrusted, so Lean does not run ---------------------
    await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="lakefile.toml"]').waitFor({ timeout: 15000 });
    await runCommand(page, "show lean infoview", /Show Lean Infoview/);
    const docId = await openInEditor(page, PROOFS);
    const panel = page.getByTestId("lean-panel");
    const status = () => panel.getAttribute("data-status");
    await waitFor(async () => (await status()) === "untrusted", { timeout: 20000, message: "untrusted status" });
    assert.match(await page.getByTestId("lean-status").innerText(), /Untrusted/);
    const jobs = await invoke(page, "job_list");
    assert.ok(jobs.every((j) => j.kind !== "interactive"), "no Lean server ran in the untrusted workspace");
    await bothThemes("01-untrusted");

    // --- trust → Lean checks the buffer: errors and goals --------------------------
    await panel.getByRole("button", { name: "Trust workspace…" }).click();
    const dialog = alertDialog(page);
    await dialog.getByRole("button", { name: "Trust workspace", exact: true }).click();
    await waitFor(async () => (await status()) === "errors", { timeout: 300_000, interval: 250, message: "errors" });
    assert.match(await page.getByTestId("lean-status").innerText(), /1 error/);
    assert.match(await page.getByTestId("lean-status-sentence").innerText(), /revision 1/);
    assert.match(await page.getByTestId("lean-status-item").innerText(), /Lean: 1 error/);
    // The shared Problems view lists the Lean error with its source.
    await page.getByRole("tab", { name: /Problems/ }).click();
    const leanProblem = page.locator('[data-testid="problem"][data-source="Lean"]');
    await leanProblem.first().waitFor({ timeout: 10000 });
    assert.equal(await leanProblem.first().getAttribute("data-severity"), "error");
    assert.match(await leanProblem.first().innerText(), /Lean/);
    assert.match(await leanProblem.first().innerText(), /LeanBasic\/Proofs\.lean:\d+:\d+/);
    await waitFor(async () => /Lean \(version 4\.35\.0/.test(await page.getByTestId("lean-toolchain").innerText()), {
      timeout: 60000,
      message: "toolchain version",
    });
    assert.match(await page.getByTestId("lean-toolchain").innerText(), /leanprover\/lean4:v4\.35\.0/);
    const diagnostics = page.getByTestId("lean-diagnostics");
    assert.match(await diagnostics.innerText(), /unsolved goals/);
    assert.match(await diagnostics.innerText(), /declaration uses `sorry`/);

    const editor = editorLocator(page, PROOFS);
    // Cursor at the end of the `skip` line: the unsolved goal.
    await editor.locator(".view-line", { hasText: /^\s*skip\s*$/ }).click();
    const goals = page.getByTestId("lean-goals");
    await waitFor(async () => (await goals.innerText()).includes("⊢ a + b = b + a"), {
      timeout: 60000,
      message: "goal at skip",
    });
    assert.match(await goals.innerText(), /a b : Nat/);
    await bothThemes("02-errors-goals");

    // Goals follow the cursor: inside the first proof.
    await editor.locator(".view-line", { hasText: /^\s*unfold\sdouble\s*$/ }).click();
    await page.keyboard.press("Home");
    await waitFor(async () => (await goals.innerText()).includes("⊢ double (a + b) = double a + double b"), {
      timeout: 60000,
      message: "goal at unfold",
    });

    // --- Monaco hovers (editor contributions are bundled) -------------------------------
    // A diagnostic squiggle shows its message.
    const squiggle = editor.locator(".squiggly-error").first();
    await squiggle.waitFor({ timeout: 30000 });
    const diagnosticHover = await hoverText(page, squiggle, /unsolved goals/);
    assert.match(diagnosticHover, /unsolved goals/, "error squiggle hover");
    await dismissHover(page);
    // A term shows Lean's hover: `double` in `... : double (a + b) = ...`.
    const term = await textBox(editor.locator(".view-line", { hasText: /theorem\sdouble_add/ }), ": double (", 0);
    const termHover = await hoverText(page, { ...term, x: term.x + term.width * 0.4, width: term.width * 0.4 }, /Twice a natural number/);
    assert.match(termHover, /double/, "Lean hover over a term");
    await shot("02b-hover-light");
    await dismissHover(page);

    // --- fix the error in Monaco: Incomplete (sorry), not Checked ---------------------
    // Select the tactic on its line (End, then Shift+Home selects to the indentation).
    await editor.locator(".view-line", { hasText: /^\s*skip\s*$/ }).click();
    await page.keyboard.press("End");
    await page.keyboard.press("Shift+Home");
    await page.keyboard.type("omega");
    await waitFor(async () => (await documentState(page, docId)).text.includes("b + a := by\n  omega"), {
      message: "buffer edited",
    });
    await waitFor(async () => (await status()) === "checkedWithSorry", { timeout: 120000, message: "incomplete" });
    assert.match(await page.getByTestId("lean-status").innerText(), /Incomplete \(sorry\)/);
    assert.match(await page.getByTestId("lean-sorries").innerText(), /add_zero'/);
    assert.equal(await page.locator(".exp-lean-sorry").count() > 0, true, "sorry token highlighted in the editor");
    assert.equal(
      await readFile(join(root, "LeanBasic", "Proofs.lean"), "utf8"),
      original,
      "Lean checked the unsaved buffer; the file on disk is unchanged",
    );
    await editor.locator(".view-line", { hasText: /^\s*sorry\s*$/ }).click();
    await bothThemes("03-incomplete-sorry");

    // --- Unicode abbreviation input -----------------------------------------------------
    await editor.locator(".view-line", { hasText: /^end\sLeanBasic/ }).click();
    await page.keyboard.press("End");
    await page.keyboard.press("Enter");
    await page.keyboard.type("-- \\forall x \\in s, x \\to y");
    await page.keyboard.press("Escape");
    await waitFor(async () => (await documentState(page, docId)).text.includes("-- ∀ x ∈ s, x → y"), {
      message: "abbreviations expanded",
    });

    // --- complete the proof: Checked ----------------------------------------------------
    await editor.locator(".view-line", { hasText: /^\s*sorry\s*$/ }).click();
    await page.keyboard.press("End");
    await page.keyboard.press("Shift+Home");
    await page.keyboard.type("simp");
    await waitFor(async () => (await status()) === "checked", { timeout: 120000, message: "checked" });
    const revision = (await documentState(page, docId)).info.revision;
    assert.match(await page.getByTestId("lean-status").innerText(), /Checked/);
    assert.match(await page.getByTestId("lean-status").innerText(), new RegExp(`rev ${revision}`));
    await bothThemes("04-checked");

    // The agent seam agrees, for the exact revision.
    const report = await invoke(page, "lean_check", {
      request: { target: { kind: "document", document: docId }, goalsAt: [], timeoutMs: 60000 },
    });
    assert.equal(report.checked, true);
    assert.equal(report.revision, revision);

    // --- a link in a hover is copied, never opened ------------------------------------------
    {
      const state = await documentState(page, docId);
      const at = state.text.indexOf("end LeanBasic");
      await invoke(page, "document_apply", {
        request: {
          id: "e2e-linked-doc",
          origin: { kind: "source" },
          edits: [{
            document: docId,
            baseRevision: state.info.revision,
            patches: [{ range: { start: at, end: at }, text: ["/-- See [the guide](https://example.com/lean-guide). -/", "def linkedDoc : Nat := 1", "", ""].join("\n") }],
          }],
          undo: { kind: "newGroup" },
          conflict: "reject",
          label: null,
          selection: null,
        },
      });
      await waitFor(async () => (await status()) === "checked" && (await documentState(page, docId)).info.revision === state.info.revision + 1, {
        timeout: 120000,
        message: "checked after adding linkedDoc",
      });
      const name = await textBox(editor.locator(".view-line", { hasText: /^def\slinkedDoc/ }), "linkedDoc");
      await hoverText(page, name, /the guide/);
      const pagesBefore = page.context().pages().length;
      const urlBefore = page.url();
      await page.locator(".monaco-hover:visible a", { hasText: "the guide" }).first().click();
      const note = page.getByRole("region", { name: "Notifications" });
      await note.getByText(/Link copied|Link not opened/).first().waitFor({ timeout: 10000 });
      assert.match(await note.innerText(), /example\.com\/lean-guide/);
      await page.waitForTimeout(500);
      assert.equal(page.url(), urlBefore, "the webview did not navigate");
      assert.equal(page.context().pages().length, pagesBefore, "no window was opened");
      await dismissHover(page);
    }

    // --- definition in another file -------------------------------------------------------
    // Caret inside `double` in `... : double (a + b) = ...` (column 36 of the line).
    await editor.locator(".view-line", { hasText: /theorem\sdouble_add/ }).click();
    await page.keyboard.press("Home");
    for (let i = 0; i < 35; i++) await page.keyboard.press("ArrowRight");
    await runCommand(page, "lean go to definition", /Go to Definition/);
    const defs = editorLocator(page, "LeanBasic/Defs.lean");
    await defs.locator(".monaco-editor").waitFor({ timeout: 15000 });
    await waitFor(async () => (await status()) === "checked", { timeout: 120000, message: "Defs.lean checked" });
    assert.match(await page.getByTestId("lean-panel").innerText(), /Defs\.lean/);
    await shot("05-definition-light");

    const errors = app.consoleMessages.filter((m) => m.startsWith("error") || m.startsWith("pageerror"));
    assert.deepEqual(errors, [], "no console errors");
  } catch (error) {
    await shot("failure").catch(() => {});
    throw error;
  } finally {
    await app.close();
  }

  const devHome = process.env.EXPOSITION_DEV_HOME;
  if (devHome) {
    const target = join(devHome, "ui-reference", "lean");
    await mkdir(target, { recursive: true });
    for (const stale of (await readdir(target)).filter((f) => f.endsWith(".png"))) await rm(join(target, stale));
    for (const path of shots) await copyFile(path, join(target, path.split(/[\\/]/).pop()));
    console.log(`copied ${shots.length} screenshots to ${target}`);
  }
}
