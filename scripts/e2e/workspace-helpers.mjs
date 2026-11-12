// Shared helpers for the workspace end-to-end scenarios (scenarios/workspace-*.mjs).
// Fixtures are always copied to a fresh temporary project before anything mutates them.
import { execFileSync } from "node:child_process";
import { cp, mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

/**
 * Create `<tmp>/project` from tests/fixtures/workspace-basic (+ encoding/), hidden
 * build/VCS folders, and a junction `escape` pointing to `<tmp>/outside`.
 */
export async function makeProject(fixture) {
  const base = await mkdtemp(join(tmpdir(), "xp-ws-"));
  const root = join(base, "project");
  const outside = join(base, "outside");
  const dataDir = join(base, "data");
  await cp(fixture("workspace-basic"), root, { recursive: true });
  await cp(fixture("encoding"), join(root, "encoding"), { recursive: true });
  for (const dir of [".git", ".lake/build", "node_modules/pkg", "target/debug"]) await mkdir(join(root, dir), { recursive: true });
  await mkdir(outside, { recursive: true });
  await mkdir(dataDir, { recursive: true });
  await writeFile(join(root, ".git", "HEAD"), "ref: refs/heads/main\n");
  await writeFile(join(root, "node_modules", "pkg", "index.js"), "module.exports = 1;\n");
  await writeFile(join(outside, "secret.txt"), "outside the workspace\n");
  if (process.platform === "win32") {
    execFileSync("cmd", ["/c", "mklink", "/J", join(root, "escape"), outside], { stdio: "ignore" });
  }
  return { base, root, outside, dataDir };
}

/** Invoke an app command; a rejection becomes an Error carrying the command's error payload. */
export async function invoke(page, command, args = {}) {
  const outcome = await page.evaluate(
    ([c, a]) =>
      window.__TAURI_INTERNALS__.invoke(c, a).then(
        (ok) => ({ ok }),
        (error) => ({ error }),
      ),
    [command, args],
  );
  if ("error" in outcome) {
    const detail = typeof outcome.error === "string" ? outcome.error : JSON.stringify(outcome.error);
    const error = new Error(`${command} failed: ${detail}`);
    error.payload = outcome.error;
    throw error;
  }
  return outcome.ok;
}

/** Invoke a command that must fail; returns the rejection payload. */
export async function invokeError(page, command, args = {}) {
  return page.evaluate(
    ([c, a]) =>
      window.__TAURI_INTERNALS__.invoke(c, a).then(
        (value) => ({ unexpectedSuccess: value }),
        (error) => ({ error }),
      ),
    [command, args],
  );
}

export async function waitFor(check, { timeout = 10_000, interval = 100, message = "condition" } = {}) {
  const deadline = Date.now() + timeout;
  let last;
  while (Date.now() < deadline) {
    try {
      last = await check();
      if (last) return last;
    } catch (error) {
      last = error;
    }
    await new Promise((r) => setTimeout(r, interval));
  }
  throw new Error(`timed out waiting for ${message} (last: ${last instanceof Error ? last.message : JSON.stringify(last)})`);
}

export async function openWorkspace(page, root) {
  const info = await invoke(page, "workspace_open", { path: root });
  await page.locator('[data-testid="file-tree"]').waitFor({ timeout: 10_000 });
  await page.locator('[data-testid="file-tree"] [data-path="main.typ"]').waitFor({ timeout: 10_000 });
  return info;
}

/** Expand the folders above `path` and press its row (opening a file). */
export async function openFromTree(page, path) {
  const parts = path.split("/");
  for (let i = 1; i < parts.length; i++) {
    const dir = parts.slice(0, i).join("/");
    const row = page.locator(`[data-testid="file-tree"] [data-path="${dir}"]`).first();
    await row.waitFor();
    if ((await row.getAttribute("aria-expanded")) !== "true") {
      await row.locator(".ui-tree-item__chevron").first().click();
    }
  }
  const row = page.locator(`[data-testid="file-tree"] [data-path="${path}"]`).first();
  await row.waitFor();
  await row.locator(".ui-tree-item__title").first().click();
}

export function editorLocator(page, path) {
  return page.locator(`[data-testid="source-editor"][data-path="${path}"]`);
}

/** Open a file from the tree and wait for its Monaco editor; returns the document id. */
export async function openInEditor(page, path) {
  await openFromTree(page, path);
  const editor = editorLocator(page, path);
  await editor.locator(".monaco-editor").waitFor({ timeout: 10_000 });
  return waitFor(() => editor.getAttribute("data-document-id"), { message: `document id for ${path}` });
}

/** Focus the editor of `path` and move the caret (`end` | `start`). */
export async function focusEditor(page, path, where = "end") {
  const editor = editorLocator(page, path);
  await editor.locator(".monaco-editor .view-lines").click();
  await page.keyboard.press(where === "end" ? "Control+End" : "Control+Home");
}

export async function documentState(page, id) {
  return invoke(page, "document_state", { document: id });
}

/** Wait until the document service has applied every pending edit and the text matches. */
export async function waitForText(page, id, expected, message = "document text") {
  return waitFor(async () => (await documentState(page, id)).text === expected, { message });
}

export function tabLocator(page, path) {
  return page.locator(`[data-editor-key="file:${path}"]`).first();
}

/** "true" when the editor tab shows unsaved changes, else "false". */
export async function tabDirty(page, path) {
  return (await tabLocator(page, path).getAttribute("data-dirty")) === "true" ? "true" : "false";
}

/** Close an editor tab with its close affordance. */
export async function closeTab(page, path) {
  await tabLocator(page, path).locator(".ui-editor-tab__close").click();
}

/** The unsaved-changes / confirmation alert dialog. */
export function alertDialog(page) {
  return page.getByRole("alertdialog");
}

/**
 * Read the Monaco model behind a source editor (by workspace path) through the
 * SourceEditor's React state (its loaded DocumentHandle). Adapted from the R-m1
 * review helpers. Returns { text, revision, pending, resyncing, holding } or null.
 */
export function monacoState(page, path) {
  return page.evaluate((p) => {
    const el = [...document.querySelectorAll('[data-testid="source-editor"]')].find((e) => e.dataset.path === p);
    if (!el) return null;
    const key = Object.keys(el).find((k) => k.startsWith("__reactFiber$"));
    let fiber = key ? el[key] : null;
    while (fiber) {
      let hook = fiber.memoizedState;
      while (hook && typeof hook === "object" && "next" in hook) {
        const s = hook.memoizedState;
        if (s && s.status === "ready" && s.handle?.model) {
          const h = s.handle;
          return {
            text: h.model.getValue(),
            revision: h.sync.revision,
            pending: h.sync.pendingCount,
            resyncing: h.sync.isResyncing,
            holding: h.sync.isHolding,
          };
        }
        hook = hook.next;
      }
      fiber = fiber.return;
    }
    return null;
  }, path);
}

/** Wait until the editor of `path` mirrors the service exactly; returns the text. */
export async function waitForMirror(page, path, id, message = "Monaco mirrors the document") {
  let last;
  await waitFor(
    async () => {
      const view = await monacoState(page, path);
      const service = await documentState(page, id);
      last = { view: view?.text, service: service.text };
      return (
        view &&
        view.pending === 0 &&
        !view.resyncing &&
        !view.holding &&
        view.text === service.text &&
        view.revision === service.info.revision
      );
    },
    { message: `${message} (${JSON.stringify(last)})` },
  );
  return (await documentState(page, id)).text;
}
