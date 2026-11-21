// Typing and explorer measurements in the real app.
//
// Typing: characters are typed into a small (2 KiB) and a large Markdown
// document (1 / 5 / 10 MiB by scale; headings and image references, so the
// outline and inline previews have work to do), with the Outline view hidden
// and then shown. Per keystroke: time from the key event to the end of the
// next frame, measured in the page. A separate pass counts the IPC traffic of
// typing: commands, request and response bytes, and `documents:event` bytes.
// (Counting adds work, so latency and bytes are never measured together.)
//
// Explorer: time to list and render a folder of 1k / 10k / 100k files, the
// rows and DOM nodes that produces, and the folder listings (count and
// bytes) caused by one new file, by a burst of 30 new files, and by a new
// file in a folder that is collapsed again.
//
//   node scripts/perf/app-bench.mjs editor-typing [--out <file.jsonl>]
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { EDITOR_SCALES, writeEditorProject } from "../fixtures-workspace.mjs";
import { editorLocator, invoke, openFromTree, waitFor } from "../../e2e/workspace-helpers.mjs";

const KEYS = 60;
const KEY_DELAY_MS = 50;

/** Record, per key event, the time until the frame after it has been produced. */
async function installKeyProbe(page) {
  await page.evaluate(() => {
    if (window.__perfKeys) return;
    window.__perfKeys = [];
    window.__perfLongTasks = [];
    document.addEventListener(
      "keydown",
      (event) => {
        if (event.key.length !== 1) return;
        const started = event.timeStamp;
        requestAnimationFrame(() => {
          const channel = new MessageChannel();
          channel.port1.onmessage = () => window.__perfKeys.push(performance.now() - started);
          channel.port2.postMessage(0);
        });
      },
      true,
    );
    try {
      new PerformanceObserver((list) => {
        for (const entry of list.getEntries()) window.__perfLongTasks.push(entry.duration);
      }).observe({ entryTypes: ["longtask"] });
    } catch {
      // Long tasks are not reported by this engine.
    }
  });
}

const takeKeys = (page) =>
  page.evaluate(() => {
    const out = { keys: window.__perfKeys.splice(0), longTasks: window.__perfLongTasks.splice(0) };
    return out;
  });

/** Count IPC commands (requests through the ipc custom protocol) and the events named in `eventNames`. */
async function installIpcCounter(page, eventNames) {
  await page.evaluate(async (names) => {
    if (window.__perfIpc) return;
    const stats = { commands: {}, events: {}, captured: 0 };
    window.__perfIpc = stats;
    const sizeOf = (body) => {
      if (body == null) return 0;
      if (typeof body === "string") return body.length;
      if (body instanceof ArrayBuffer) return body.byteLength;
      if (ArrayBuffer.isView(body)) return body.byteLength;
      return 0;
    };
    const originalFetch = window.fetch;
    window.fetch = function (input, init) {
      const url = typeof input === "string" ? input : (input?.url ?? "");
      const match = /^(?:https?:\/\/ipc\.localhost|ipc:\/\/localhost)\/([^?]+)/.exec(url);
      const promise = originalFetch.apply(this, arguments);
      if (!match) return promise;
      const name = decodeURIComponent(match[1]);
      const entry = (stats.commands[name] ??= { calls: 0, requestBytes: 0, responseBytes: 0 });
      entry.calls += 1;
      entry.requestBytes += sizeOf(init?.body);
      stats.captured += 1;
      return promise.then((response) => {
        response
          .clone()
          .arrayBuffer()
          .then((buffer) => (entry.responseBytes += buffer.byteLength))
          .catch(() => {});
        return response;
      });
    };
    const internals = window.__TAURI_INTERNALS__;
    for (const name of names) {
      const entry = (stats.events[name] = { count: 0, bytes: 0 });
      const handler = internals.transformCallback((event) => {
        entry.count += 1;
        entry.bytes += JSON.stringify(event.payload ?? null).length;
      });
      await internals.invoke("plugin:event|listen", { event: name, target: { kind: "Any" }, handler });
    }
  }, eventNames);
}

const resetIpc = (page) =>
  page.evaluate(() => {
    const stats = window.__perfIpc;
    stats.commands = {};
    stats.captured = 0;
    for (const entry of Object.values(stats.events)) {
      entry.count = 0;
      entry.bytes = 0;
    }
  });

const readIpc = (page) => page.evaluate(() => JSON.parse(JSON.stringify(window.__perfIpc)));

async function typeKeys(page, count) {
  const text = "abcdefghij".repeat(Math.ceil(count / 10)).slice(0, count);
  await page.keyboard.type(text, { delay: KEY_DELAY_MS });
}

async function focusEditorAt(page, path) {
  const editor = editorLocator(page, path);
  await editor.locator(".monaco-editor .view-lines").first().click();
  // The middle of the first screen: every edit shifts the rest of the text.
  await page.keyboard.press("Control+Home");
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("End");
}

function summarizeCommands(ipc) {
  const flat = {};
  for (const [name, entry] of Object.entries(ipc.commands)) {
    flat[`${name}.calls`] = entry.calls;
    flat[`${name}.requestBytes`] = entry.requestBytes;
    flat[`${name}.responseBytes`] = entry.responseBytes;
  }
  for (const [name, entry] of Object.entries(ipc.events)) {
    flat[`event.${name}.count`] = entry.count;
    flat[`event.${name}.bytes`] = entry.bytes;
  }
  return flat;
}

async function measureTyping(ctx, app, { path, label, bytes }) {
  const { page } = app;
  const openStarted = performance.now();
  await openFromTree(page, path);
  await editorLocator(page, path).locator(".monaco-editor .view-lines").first().waitFor({ timeout: 120_000 });
  const openMs = performance.now() - openStarted;
  await ctx.sleep(1_500);
  ctx.record({ bench: "app.editor.open_document", scale: { document: label, bytes }, samples_ms: [openMs] });

  const auxiliary = page.getByRole("tablist", { name: "Auxiliary panel views" });
  const initialTab = (await auxiliary.locator('[role="tab"][aria-selected="true"]').first().textContent())?.trim();
  for (const outline of [false, true]) {
    if (outline) {
      await auxiliary.getByRole("tab", { name: "Outline" }).click();
      await page.locator('[data-testid="outline-view"]').waitFor({ timeout: 30_000 });
      await waitFor(async () => (await page.locator('[data-testid="outline-view"]').getAttribute("data-count")) !== "", {
        timeout: 120_000,
        message: "outline loaded",
      });
    }
    await focusEditorAt(page, path);
    await typeKeys(page, 5); // warm-up
    await ctx.sleep(1_000);
    await takeKeys(page);
    await typeKeys(page, KEYS);
    await ctx.sleep(1_500);
    const { keys, longTasks } = await takeKeys(page);
    ctx.record({
      bench: "app.editor.keystroke_to_frame",
      scale: { document: label, bytes, outline: outline ? "shown" : "hidden" },
      samples_ms: keys,
      counters: { keys: keys.length, longTasks: longTasks.length, longTaskMs: Math.round(longTasks.reduce((a, b) => a + b, 0)) },
    });
  }
  const metrics = await ctx.metrics(app);
  ctx.record({ bench: "app.editor.after_typing", scale: { document: label, bytes }, counters: metrics });

  // IPC traffic of 20 typed characters and the idle second after them
  // (debounced consumers: outline, inline previews, recovery).
  await focusEditorAt(page, path);
  await resetIpc(page);
  await typeKeys(page, 20);
  await ctx.sleep(2_000);
  const ipc = await readIpc(page);
  ctx.record({
    bench: "app.editor.ipc_per_20_keys",
    scale: { document: label, bytes, outline: "shown" },
    counters: { captured: ipc.captured, ...summarizeCommands(ipc) },
  });
  // Save (the project is temporary), and go back to the auxiliary view that
  // was shown before, so the next document starts with the Outline hidden.
  await page.keyboard.press("Control+s");
  await ctx.sleep(1_000);
  if (initialTab && initialTab !== "Outline") await auxiliary.getByRole("tab", { name: initialTab }).first().click();
}

async function measureExplorer(ctx, app, project, sizes) {
  const { page } = app;
  const tree = page.locator('[data-testid="file-tree"]');
  const flatRow = tree.locator('[data-path="flat"]').first();
  await flatRow.waitFor();
  const rowsIn = () => page.evaluate(() => document.querySelectorAll('[data-testid="file-tree"] [data-path^="flat/"]').length);
  const before = await ctx.metrics(app, { collect: false });

  await resetIpc(page);
  const started = performance.now();
  await flatRow.locator(".ui-tree-item__chevron").first().click();
  await tree.locator(`[data-path="${project.firstFlat}"]`).first().waitFor({ timeout: 300_000 });
  const expandMs = performance.now() - started;
  await ctx.sleep(1_000);
  const after = await ctx.metrics(app, { collect: false });
  const listing = (await readIpc(page)).commands.workspace_list_dir ?? { calls: 0, responseBytes: 0 };
  ctx.record({
    bench: "app.explorer.expand_folder",
    scale: { files: sizes.flatFiles },
    samples_ms: [expandMs],
    counters: {
      rows: await rowsIn(),
      domNodesAdded: (after.domNodes ?? 0) - (before.domNodes ?? 0),
      listCalls: listing.calls,
      listResponseBytes: listing.responseBytes,
      jsHeapUsedMiB: after.jsHeapUsedMiB,
    },
  });

  // One new file in the listed folder.
  const listed = async (work) => {
    await resetIpc(page);
    const t0 = performance.now();
    const done = await work();
    const ms = performance.now() - t0;
    await ctx.sleep(1_500);
    const ipc = await readIpc(page);
    const calls = ipc.commands.workspace_list_dir ?? { calls: 0, responseBytes: 0 };
    return { ms: done === false ? null : ms, calls: calls.calls, bytes: calls.responseBytes, events: ipc.events["workspace:event"] };
  };
  const one = await listed(async () => {
    writeFileSync(join(project.root, "flat", "zz-new-one.txt"), "");
    await tree.locator('[data-path="flat/zz-new-one.txt"]').first().waitFor({ timeout: 300_000 });
  });
  ctx.record({
    bench: "app.explorer.one_new_file",
    scale: { files: sizes.flatFiles },
    samples_ms: [one.ms],
    counters: { listCalls: one.calls, listResponseBytes: one.bytes, workspaceEvents: one.events?.count ?? null },
  });

  // A build writing 30 files, one every 20 ms (several watcher batches).
  const burst = await listed(async () => {
    for (let i = 0; i < 30; i++) {
      writeFileSync(join(project.root, "flat", `zz-burst-${String(i).padStart(2, "0")}.txt`), "");
      await ctx.sleep(20);
    }
    await tree.locator('[data-path="flat/zz-burst-29.txt"]').first().waitFor({ timeout: 300_000 });
  });
  ctx.record({
    bench: "app.explorer.burst_30_files",
    scale: { files: sizes.flatFiles },
    samples_ms: [burst.ms],
    counters: { listCalls: burst.calls, listResponseBytes: burst.bytes, workspaceEvents: burst.events?.count ?? null },
  });

  // Collapsed again: its listing is kept; a change inside it.
  const collapseStarted = performance.now();
  await flatRow.locator(".ui-tree-item__chevron").first().click();
  await waitFor(async () => (await rowsIn()) === 0, { timeout: 300_000, message: "folder collapsed" });
  const collapseMs = performance.now() - collapseStarted;
  ctx.record({ bench: "app.explorer.collapse_folder", scale: { files: sizes.flatFiles }, samples_ms: [collapseMs] });
  const collapsed = await listed(async () => {
    writeFileSync(join(project.root, "flat", "zz-while-collapsed.txt"), "");
    return false;
  });
  ctx.record({
    bench: "app.explorer.new_file_in_collapsed_folder",
    scale: { files: sizes.flatFiles },
    counters: { listCalls: collapsed.calls, listResponseBytes: collapsed.bytes },
  });

  // A change elsewhere (the root) while the big listing is loaded but collapsed:
  // time until the new root row is shown.
  const rootChange = await listed(async () => {
    writeFileSync(join(project.root, "zz-root-new.txt"), "");
    await tree.locator('[data-path="zz-root-new.txt"]').first().waitFor({ timeout: 300_000 });
  });
  ctx.record({
    bench: "app.explorer.root_change_with_collapsed_listing",
    scale: { files: sizes.flatFiles },
    samples_ms: [rootChange.ms],
    counters: { listCalls: rootChange.calls, listResponseBytes: rootChange.bytes },
  });
}

export default async function editorTyping(ctx) {
  const sizes = EDITOR_SCALES[ctx.scale] ?? EDITOR_SCALES.small;
  const root = join(ctx.workDir, "project");
  mkdirSync(root, { recursive: true });
  const project = writeEditorProject(root, sizes);
  const app = await ctx.launchApp();
  try {
    const { page } = app;
    await page.waitForSelector("#root > *", { timeout: 30_000 });
    await installIpcCounter(page, ["documents:event", "workspace:event"]);
    await installKeyProbe(page);

    const started = performance.now();
    await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="main.typ"]').waitFor({ timeout: 60_000 });
    ctx.record({ bench: "app.explorer.open_project", scale: { treeFiles: sizes.treeFiles, flatFiles: sizes.flatFiles }, samples_ms: [performance.now() - started] });

    await measureTyping(ctx, app, { path: "small.md", label: "small", bytes: 2_000 });
    await measureTyping(ctx, app, { path: "large.md", label: "large", bytes: sizes.largeBytes });
    await measureExplorer(ctx, app, project, sizes);
  } finally {
    await app.close();
    rmSync(ctx.workDir, { recursive: true, force: true });
  }
}
