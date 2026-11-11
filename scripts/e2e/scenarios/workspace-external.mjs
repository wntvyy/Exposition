// External changes in the real app: a script modifies an open file on disk.
// A clean buffer reloads (an `external` transaction, undoable); a dirty buffer
// enters a conflict with explicit choices and neither side is lost. The tree
// follows files created and deleted outside the app.
import { readFile, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";

import {
  documentState,
  editorLocator,
  focusEditor,
  makeProject,
  openInEditor,
  openWorkspace,
  tabDirty,
  waitFor,
  waitForText,
} from "../workspace-helpers.mjs";

export default async function workspaceExternal({ launchApp, artifactsDir, assert, fixture }) {
  const project = await makeProject(fixture);
  const app = await launchApp({ dataDir: project.dataDir });
  const { page } = app;
  const shot = (name) => page.screenshot({ path: join(artifactsDir, `${name}.png`) });
  const target = join(project.root, "main.typ");
  try {
    await openWorkspace(page, project.root);
    const id = await openInEditor(page, "main.typ");
    const editor = editorLocator(page, "main.typ");

    // --- clean buffer: reload --------------------------------------------------
    await writeFile(target, "= Rewritten by a script\n\nNew body.\n");
    await waitForText(page, id, "= Rewritten by a script\n\nNew body.\n", "clean buffer reloaded");
    // Monaco renders spaces as no-break spaces.
    const shown = async () => (await editor.locator(".view-lines").innerText()).replaceAll(" ", " ");
    await waitFor(async () => (await shown()).includes("Rewritten by a script"), {
      message: "Monaco shows the reloaded text",
    });
    assert.equal(await tabDirty(page, "main.typ"), "false", "reloaded buffer is clean");
    await page.locator('[data-testid="notification"]', { hasText: "main.typ was reloaded" }).waitFor();
    await shot("01-reloaded");

    // --- dirty buffer: conflict, keep mine --------------------------------------
    await focusEditor(page, "main.typ", "end");
    await page.keyboard.type("My unsaved edit.");
    const mine = "= Rewritten by a script\n\nNew body.\nMy unsaved edit.";
    await waitForText(page, id, mine);
    await writeFile(target, "= Script again\n");
    const banner = editor.locator('[data-testid="conflict-banner"]');
    await banner.waitFor({ timeout: 10_000 });
    assert.equal(await banner.getAttribute("data-kind"), "modified");
    assert.equal((await documentState(page, id)).text, mine, "buffer untouched by the external change");
    assert.equal(await readFile(target, "utf8"), "= Script again\n", "disk untouched");
    await shot("02-conflict");
    await banner.getByRole("button", { name: "Keep My Changes" }).click();
    await banner.waitFor({ state: "detached" });
    await focusEditor(page, "main.typ", "end");
    await page.keyboard.press("Control+S");
    await waitFor(async () => (await readFile(target, "utf8")) === mine, { message: "kept version saved" });
    assert.equal(await tabDirty(page, "main.typ"), "false");

    // --- dirty buffer: conflict, reload from disk (mine stays in undo) ----------
    await page.keyboard.type(" Second edit.");
    await waitForText(page, id, `${mine} Second edit.`);
    await writeFile(target, "= Third version from disk\n");
    await banner.waitFor({ timeout: 10_000 });
    await banner.getByRole("button", { name: "Reload from Disk" }).click();
    await waitForText(page, id, "= Third version from disk\n", "reloaded from disk");
    await banner.waitFor({ state: "detached" });
    assert.equal(await tabDirty(page, "main.typ"), "false");
    await focusEditor(page, "main.typ", "end");
    await page.keyboard.press("Control+Z");
    await waitForText(page, id, `${mine} Second edit.`, "undo brings my version back");
    await shot("03-reload-then-undo");

    // --- deleted on disk: buffer kept and marked unsaved ------------------------
    await page.keyboard.press("Control+Y");
    await waitForText(page, id, "= Third version from disk\n");
    await rm(target);
    await banner.waitFor({ timeout: 10_000 });
    assert.equal(await banner.getAttribute("data-kind"), "deleted");
    await waitFor(async () => (await tabDirty(page, "main.typ")) === "true", { message: "deleted file buffer unsaved" });
    await page.locator('[data-testid="file-tree"] [data-path="main.typ"]').waitFor({ state: "detached" });
    await focusEditor(page, "main.typ", "end");
    await page.keyboard.press("Control+S");
    await waitFor(async () => (await readFile(target, "utf8").catch(() => null)) === "= Third version from disk\n", {
      message: "save recreates the deleted file",
    });
    await banner.waitFor({ state: "detached" });
    await page.locator('[data-testid="file-tree"] [data-path="main.typ"]').waitFor();

    // --- tree follows external creation ----------------------------------------
    await writeFile(join(project.root, "created-elsewhere.md"), "# new\n");
    await page.locator('[data-testid="file-tree"] [data-path="created-elsewhere.md"]').waitFor({ timeout: 10_000 });
    await shot("04-final");
  } catch (error) {
    await shot("failure").catch(() => {});
    throw error;
  } finally {
    await app.close();
  }
}
