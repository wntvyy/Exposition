// Closing the app window with unsaved changes asks Save / Don't Save / Cancel.
// Cancel keeps the app running; Don't Save closes it and discards the recovery
// journal, so the next session offers nothing to restore and the file is untouched.
// The close request is a real WM_CLOSE (taskkill without /F), not a process kill.
import { execFileSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import { join } from "node:path";

import { focusEditor, invoke, makeProject, openInEditor, openWorkspace, waitFor, waitForText } from "../workspace-helpers.mjs";

function requestClose(pid) {
  try {
    execFileSync("taskkill", ["/PID", String(pid)], { stdio: "ignore" });
  } catch {
    // taskkill reports failure when the window vetoes the close; that is expected.
  }
}

export default async function workspaceWindowClose({ launchApp, artifactsDir, assert, fixture }) {
  if (process.platform !== "win32") return;
  const project = await makeProject(fixture);
  const mainPath = join(project.root, "main.typ");
  const original = await readFile(mainPath, "utf8");
  let app = await launchApp({ dataDir: project.dataDir });
  try {
    const { page } = app;
    await openWorkspace(page, project.root);
    const id = await openInEditor(page, "main.typ");
    await focusEditor(page, "main.typ", "end");
    await page.keyboard.type("Not saved.");
    await waitForText(page, id, `${original}Not saved.`);

    requestClose(app.child.pid);
    const prompt = page.locator('[data-testid="prompt"]');
    await prompt.waitFor({ timeout: 10_000 });
    assert.match(await prompt.innerText(), /Save changes to main\.typ\?/);
    await page.screenshot({ path: join(artifactsDir, "01-close-prompt.png") });
    await prompt.locator('[data-prompt-button="cancel"]').click();
    await prompt.waitFor({ state: "detached" });
    assert.equal(app.child.exitCode, null, "cancel keeps the app open");
    assert.equal((await invoke(page, "document_state", { document: id })).text, `${original}Not saved.`);

    requestClose(app.child.pid);
    await prompt.waitFor({ timeout: 10_000 });
    await prompt.locator('[data-prompt-button="discard"]').click();
    await waitFor(() => app.child.exitCode !== null, { message: "app exited after Don't Save" });
  } catch (error) {
    await app.page.screenshot({ path: join(artifactsDir, "failure.png") }).catch(() => {});
    throw error;
  } finally {
    await app.close();
  }
  assert.equal(await readFile(mainPath, "utf8"), original, "file untouched");

  app = await launchApp({ dataDir: project.dataDir });
  try {
    const info = await openWorkspace(app.page, project.root);
    assert.deepEqual(info.recovery, [], "discarded changes are not offered for recovery");
  } finally {
    await app.close();
  }
}
