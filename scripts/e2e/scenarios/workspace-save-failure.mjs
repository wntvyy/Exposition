// Save failure in the real app: the file is made read-only on disk, Ctrl+S
// reports the error, the buffer stays dirty, and the original bytes are intact.
// After the attribute is cleared, the same buffer saves. Save All reports
// failures too, and an external modification is never silently overwritten.
import { chmod, readFile, readdir, writeFile } from "node:fs/promises";
import { join } from "node:path";

import {
  alertDialog,
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

export default async function workspaceSaveFailure({ launchApp, artifactsDir, assert, fixture }) {
  const project = await makeProject(fixture);
  const app = await launchApp({ dataDir: project.dataDir });
  const { page } = app;
  const shot = (name) => page.screenshot({ path: join(artifactsDir, `${name}.png`) });
  const target = join(project.root, "main.typ");
  try {
    await openWorkspace(page, project.root);
    const original = await readFile(target);
    const id = await openInEditor(page, "main.typ");
    await focusEditor(page, "main.typ", "end");
    await page.keyboard.type("Unsaved line.");
    const expected = `${original.toString("utf8")}Unsaved line.`;
    await waitForText(page, id, expected);

    // Read-only on disk (sets the Windows read-only attribute).
    await chmod(target, 0o444);
    await page.keyboard.press("Control+S");
    const banner = page.locator('[data-testid="save-error-banner"]');
    await banner.waitFor({ timeout: 10_000 });
    assert.match(await banner.innerText(), /read-only/i);
    await page.locator('[data-testid="notification"]', { hasText: "Could not save main.typ" }).waitFor();
    await shot("01-save-failed");
    assert.equal(await tabDirty(page, "main.typ"), "true", "buffer stays dirty");
    assert.deepEqual(await readFile(target), original, "original bytes intact");
    const state = await documentState(page, id);
    assert.equal(state.text, expected, "buffer text kept");
    assert.notEqual(state.info.savedRevision, state.info.revision);
    const leftovers = (await readdir(project.root)).filter((n) => n.includes(".exposition-save-"));
    assert.deepEqual(leftovers, [], "no temporary files left behind");

    // Save All reports the same failure.
    const all = await invoke(page, "document_save_all");
    assert.equal(all.failed.length, 1);
    assert.equal(all.failed[0].code, "file.readOnly");
    assert.deepEqual(await readFile(target), original);

    // Clear the attribute; retry from the banner succeeds.
    await chmod(target, 0o644);
    await banner.getByRole("button", { name: "Retry" }).click();
    await waitFor(async () => (await tabDirty(page, "main.typ")) === "false", { message: "saved after retry" });
    await banner.waitFor({ state: "detached" });
    assert.equal(await readFile(target, "utf8"), expected);

    // External modification since load: Ctrl+S asks before overwriting.
    await focusEditor(page, "main.typ", "end");
    await page.keyboard.type(" More.");
    await waitForText(page, id, `${expected} More.`);
    await invoke(page, "workspace_current"); // round trip so the edit is acknowledged
    // Write from "another program" and save before the watcher can react.
    await writeFile(target, "changed elsewhere\n");
    await page.keyboard.press("Control+S");
    const prompt = alertDialog(page);
    await prompt.waitFor();
    assert.match(await prompt.innerText(), /changed on disk/);
    await shot("02-overwrite-prompt");
    await prompt.getByRole("button", { name: "Cancel" }).click();
    assert.equal(await readFile(target, "utf8"), "changed elsewhere\n", "cancel never overwrites");
    await page.keyboard.press("Control+S");
    await prompt.getByRole("button", { name: "Overwrite" }).click();
    await waitFor(async () => (await readFile(target, "utf8")) === `${expected} More.`, { message: "explicit overwrite" });
  } catch (error) {
    await shot("failure").catch(() => {});
    throw error;
  } finally {
    await chmod(target, 0o644).catch(() => {});
    await app.close();
  }
}
