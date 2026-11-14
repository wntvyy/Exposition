// P1 project operations — Research files in the Research explorer (spec §5
// row 1 "including Research files and multi-selection"), disposable project:
//  1. Two research files selected in the Research page's tree (Ctrl+click),
//     Delete: no dialog, both leave the research space, both are in the REAL
//     Windows Recycle Bin, the listing and the graph follow.
//  2. Copy a source, select the Data group, Paste: a copy lands in Data with
//     the same bytes; the source is unchanged; the menu named the destination.
//  3. Duplicate (Ctrl+D) a source beside itself.
// Nothing runs: no research session is started by any of this.
import { mkdir, mkdtemp, readFile, readdir, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";
import { consoleErrors, dismissNotifications, expectNoDialog, publishShots, recycleBinItemsFrom, settle, waitForNotification } from "../p1-helpers.mjs";
import { invoke, waitFor } from "../workspace-helpers.mjs";

export default async function p1Research({ launchApp, artifactsDir, assert }) {
  const base = await mkdtemp(join(tmpdir(), "xp-p1-research-"));
  const root = join(base, "project");
  const dataDir = join(base, "data");
  await mkdir(root, { recursive: true });
  await mkdir(dataDir, { recursive: true });
  await writeFile(join(root, "main.typ"), "= Notes\n");
  const evidence = { real: [], notes: [] };
  const shots = [];
  const app = await launchApp({ dataDir });
  const { page } = app;
  const shot = async (name) => {
    const path = join(artifactsDir, `${name}.png`);
    await settle(page);
    await page.screenshot({ path });
    shots.push(path);
  };
  try {
    await page.setViewportSize({ width: 1440, height: 900 });
    await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="main.typ"]').waitFor({ timeout: 15_000 });
    await waitFor(async () => (await invoke(page, "graph_status")).state.kind === "attached", { timeout: 20_000, message: "graph attached" });
    const space = await invoke(page, "research_space_enable");
    const files = {};
    for (const [group, name, text] of [
      ["sources", "fit.py", "print('fit')\n"],
      ["sources", "plot.py", "print('plot')\n"],
      ["notes", "method.md", "# Method\n"],
      ["data", "keep.csv", "x\n1\n"],
    ]) {
      const created = await invoke(page, "research_space_create_file", { request: { group, name } });
      await writeFile(join(root, ...created.path.split("/")), text);
      files[name] = created.path;
    }
    const sessionsBefore = (await invoke(page, "research_sessions")).length;
    await runCommand(page, "Show Research", /View: Show Research/);
    const tree = page.getByTestId("research-tree");
    await tree.waitFor({ timeout: 15_000 });
    const row = (path) => tree.locator(`[data-path="${path}"]`).first();
    // Select exactly these rows with Ctrl+click (toggles), without opening anything.
    const selectOnly = async (paths) => {
      const selectedNow = async () => tree.locator('[aria-selected="true"]').evaluateAll((rows) => rows.map((r) => r.getAttribute("data-path") ?? r.getAttribute("data-item-key")));
      for (const p of await selectedNow()) if (!paths.includes(p)) await tree.locator(`[data-path="${p}"], [data-item-key="${p}"]`).first().locator(".ui-tree-item__title").click({ modifiers: ["Control"] });
      for (const p of paths) if (!(await selectedNow()).includes(p)) await row(p).locator(".ui-tree-item__title").click({ modifiers: ["Control"] });
      await waitFor(async () => { const now = await selectedNow(); return now.length === paths.length && paths.every((p) => now.includes(p)); }, { message: `research selection ${paths.join(", ")}` });
      await row(paths.at(-1)).focus();
    };
    await row(files["method.md"]).waitFor({ timeout: 15_000 });

    // --- 1. multi-selection delete -------------------------------------------------------
    await selectOnly([files["plot.py"], files["method.md"]]);
    await page.keyboard.press("Delete");
    await expectNoDialog(page, assert, 800, "unused research files are deleted at once");
    await waitForNotification(page, /^Moved 2 items to the (Recycle Bin|Trash)$/);
    await waitFor(async () => !existsSync(join(root, ...files["plot.py"].split("/"))) && !existsSync(join(root, ...files["method.md"].split("/"))), { message: "research files deleted" });
    await waitFor(async () => (await row(files["plot.py"]).count()) === 0, { message: "listing updated" });
    const binned = [
      ...recycleBinItemsFrom(join(root, ...space.root.split("/"), "sources")),
      ...recycleBinItemsFrom(join(root, ...space.root.split("/"), "notes")),
    ].map((i) => i.name);
    assert.ok(binned.some((n) => n.startsWith("plot")) && binned.some((n) => n.startsWith("method")), `real Recycle Bin holds both (${binned.join(", ")})`);
    evidence.real.push("research files deleted from the Research explorer are in the Windows Recycle Bin");
    const snap = await invoke(page, "graph_snapshot", { query: { kinds: null, text: "plot.py", around: null, includeInactive: true, limit: 20 } });
    assert.ok(!snap.entities.some((e) => e.key === files["plot.py"] && e.lifecycle === "active"), "graph has no active resource for it");
    await shot("p1-research-delete-light");
    await dismissNotifications(page);

    // --- 2. copy a source, paste into Data -----------------------------------------------
    await selectOnly([files["fit.py"]]);
    await page.keyboard.press("Control+C");
    await waitForNotification(page, /^Copied “fit\.py”/);
    const dataGroup = tree.locator('[data-item-key="group:data"]').first();
    await dataGroup.click({ button: "right" });
    const menu = page.getByTestId("research-context-menu");
    await menu.waitFor();
    const pasteItem = menu.getByRole("menuitem", { name: /^Paste into .*\/data\/$/ });
    assert.ok(await pasteItem.isVisible(), "the menu names the Data folder as the destination");
    await shot("p1-research-paste-menu-light");
    await pasteItem.click();
    const dataDirAbs = join(root, ...space.root.split("/"), "data");
    await waitFor(async () => (await readdir(dataDirAbs)).includes("fit.py"), { message: "fit.py pasted into Data" });
    assert.equal(await readFile(join(dataDirAbs, "fit.py"), "utf8"), "print('fit')\n", "same bytes");
    assert.equal(await readFile(join(root, ...files["fit.py"].split("/")), "utf8"), "print('fit')\n", "source unchanged");
    await dismissNotifications(page);

    // --- 3. duplicate beside itself ---------------------------------------------------------
    await selectOnly([files["fit.py"]]);
    await page.keyboard.press("Control+D");
    const sourcesDir = join(root, ...space.root.split("/"), "sources");
    await waitFor(async () => (await readdir(sourcesDir)).filter((n) => /^fit.*\.py$/.test(n)).length === 2, { message: "duplicate beside the source" });
    evidence.notes.push(`sources after duplicate: ${(await readdir(sourcesDir)).join(", ")}`);

    assert.equal((await invoke(page, "research_sessions")).length, sessionsBefore, "no research session was started");
    assert.deepEqual(consoleErrors(app), [], "no console errors");
  } finally {
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2)).catch(() => {});
    await app.close();
  }
  await publishShots(shots);
}
