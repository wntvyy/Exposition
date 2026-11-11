// Crash recovery in the real app: type unsaved text, hard-kill the process,
// relaunch with the same data directory, reopen the folder, and restore. The
// restored text comes back as a recovery transaction (undo returns to the disk
// version). When the file changed on disk after the edits, both versions are
// kept. Discard removes an entry without touching the file.
import { execFileSync } from "node:child_process";
import { readFile, readdir, writeFile } from "node:fs/promises";
import { join } from "node:path";

import {
  documentState,
  focusEditor,
  invoke,
  makeProject,
  openInEditor,
  openWorkspace,
  tabDirty,
  waitFor,
  waitForText,
} from "../workspace-helpers.mjs";

async function journals(dataDir) {
  const root = join(dataDir, "recovery");
  const out = [];
  for (const key of await readdir(root).catch(() => [])) {
    for (const name of await readdir(join(root, key))) out.push(name);
  }
  return out;
}

export default async function workspaceRecovery({ launchApp, artifactsDir, assert, fixture }) {
  const project = await makeProject(fixture);
  const mainPath = join(project.root, "main.typ");
  const notesPath = join(project.root, "notes.md");
  const readmePath = join(project.root, "README.txt");
  const mainOriginal = await readFile(mainPath, "utf8");
  const notesOriginal = await readFile(notesPath, "utf8");
  const readmeOriginal = await readFile(readmePath, "utf8");

  // --- session 1: unsaved edits, then a crash --------------------------------
  let app = await launchApp({ dataDir: project.dataDir });
  try {
    const { page } = app;
    await openWorkspace(page, project.root);
    const mainId = await openInEditor(page, "main.typ");
    await focusEditor(page, "main.typ", "end");
    await page.keyboard.type("Recovered paragraph.");
    await waitForText(page, mainId, `${mainOriginal}Recovered paragraph.`);
    const notesId = await openInEditor(page, "notes.md");
    await focusEditor(page, "notes.md", "end");
    await page.keyboard.type("- unsaved note");
    await waitForText(page, notesId, `${notesOriginal}- unsaved note`);
    const readmeId = await openInEditor(page, "README.txt");
    await focusEditor(page, "README.txt", "start");
    await page.keyboard.type("Discard me. ");
    await waitForText(page, readmeId, `Discard me. ${readmeOriginal}`);
    await waitFor(async () => (await journals(project.dataDir)).filter((n) => n.startsWith("live-")).length === 3, {
      message: "three live recovery journals",
    });
    await page.screenshot({ path: join(artifactsDir, "01-before-crash.png") });
  } catch (error) {
    await app.page.screenshot({ path: join(artifactsDir, "failure-session1.png") }).catch(() => {});
    await app.close();
    throw error;
  }
  // Hard kill: no save, no close, no shutdown handlers.
  execFileSync("taskkill", ["/PID", String(app.child.pid), "/T", "/F"], { stdio: "ignore" });
  await app.browser.close().catch(() => {});
  await waitFor(() => app.child.exitCode !== null || app.child.signalCode !== null, { message: "app exit" });
  assert.equal(await readFile(mainPath, "utf8"), mainOriginal, "nothing was saved");

  // Another program changes notes.md while Exposition is not running.
  await writeFile(notesPath, "# Notes changed elsewhere\n");

  // --- session 2: relaunch with the same data directory ----------------------
  app = await launchApp({ dataDir: project.dataDir });
  const { page } = app;
  const shot = (name) => page.screenshot({ path: join(artifactsDir, `${name}.png`) });
  try {
    const info = await openWorkspace(page, project.root);
    assert.deepEqual(info.recovery.map((e) => e.path).sort(), ["README.txt", "main.typ", "notes.md"]);
    const list = page.locator('[data-testid="recovery-list"]');
    await list.waitFor();
    const notesEntry = list.locator('[data-recovery-path="notes.md"]');
    assert.match(await notesEntry.innerText(), /changed on disk/);
    assert.doesNotMatch(await list.locator('[data-recovery-path="main.typ"]').innerText(), /changed on disk/);
    await shot("02-recovery-offered");

    // Restore main.typ: text back, disk untouched, undo returns to the disk text.
    await list.locator('[data-recovery-path="main.typ"]').getByRole("button", { name: "Restore" }).click();
    const editor = page.locator('[data-testid="source-editor"][data-path="main.typ"]');
    await editor.locator(".monaco-editor").waitFor();
    const mainId = await waitFor(() => editor.getAttribute("data-document-id"));
    await waitForText(page, mainId, `${mainOriginal}Recovered paragraph.`, "recovered text");
    await waitFor(async () => (await tabDirty(page, "main.typ")) === "true", { message: "restored buffer is unsaved" });
    assert.equal(await readFile(mainPath, "utf8"), mainOriginal, "restore does not write the file");
    await focusEditor(page, "main.typ", "end");
    await page.keyboard.press("Control+Z");
    await waitForText(page, mainId, mainOriginal, "undo returns to the disk version");
    await page.keyboard.press("Control+Y");
    await waitForText(page, mainId, `${mainOriginal}Recovered paragraph.`, "redo restores the recovered text");
    await shot("03-restored-main");

    // Restore notes.md although the disk changed: both versions survive.
    await notesEntry.getByRole("button", { name: "Restore" }).click();
    await page.locator('[data-testid="notification"]', { hasText: "Restored unsaved changes to notes.md" }).waitFor();
    assert.match(
      await page.locator('[data-testid="notification"]', { hasText: "Restored unsaved changes to notes.md" }).innerText(),
      /changed on disk/,
    );
    const notesEditor = page.locator('[data-testid="source-editor"][data-path="notes.md"]');
    await notesEditor.locator(".monaco-editor").waitFor();
    const notesId = await waitFor(() => notesEditor.getAttribute("data-document-id"));
    await waitForText(page, notesId, `${notesOriginal}- unsaved note`, "recovered notes text");
    assert.equal(await readFile(notesPath, "utf8"), "# Notes changed elsewhere\n", "disk version kept");
    await focusEditor(page, "notes.md", "end");
    await page.keyboard.press("Control+Z");
    await waitForText(page, notesId, "# Notes changed elsewhere\n", "undo shows the disk version");
    await page.keyboard.press("Control+Y");
    await shot("04-restored-notes");

    // Discard README.txt: entry and journal removed, file untouched.
    await list.locator('[data-recovery-path="README.txt"]').getByRole("button", { name: "Discard" }).click();
    await list.waitFor({ state: "detached" });
    assert.equal(await readFile(readmePath, "utf8"), readmeOriginal);
    assert.deepEqual(await invoke(page, "recovery_list"), []);
    const remaining = await journals(project.dataDir);
    assert.ok(remaining.every((n) => !n.startsWith("rec-")), `no recovery entries remain: ${remaining}`);
    // The restored buffers are journaled again in this session.
    await waitFor(async () => (await journals(project.dataDir)).filter((n) => n.startsWith("live-")).length === 2, {
      message: "restored buffers re-journaled",
    });

    // Saving clears the journal.
    const mainTab = page.locator('[data-testid="editor-tab"][data-key="file:main.typ"]');
    await mainTab.click();
    await focusEditor(page, "main.typ", "end");
    await page.keyboard.press("Control+S");
    await waitFor(async () => (await readFile(mainPath, "utf8")) === `${mainOriginal}Recovered paragraph.`, {
      message: "restored text saved",
    });
    await waitFor(async () => (await journals(project.dataDir)).filter((n) => n.startsWith("live-")).length === 1, {
      message: "journal removed after save",
    });
    const state = await documentState(page, mainId);
    assert.equal(state.info.savedRevision, state.info.revision);
    await shot("05-final");
  } catch (error) {
    await shot("failure").catch(() => {});
    throw error;
  } finally {
    await app.close();
  }
}
