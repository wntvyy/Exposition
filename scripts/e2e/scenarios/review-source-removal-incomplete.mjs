// Review (graph consumers) — "Delete from Source" when its list of uses
// cannot be complete, in the real app with the real graph and data side
// (disposable project, the Mathematics section):
//  1. A Lean declaration while another Lean file of the project cannot be
//     read (not UTF-8): the ONE decision says that not every use could be
//     checked and names the file by its project path; confirmed → removed
//     from the buffer as an unsaved edit, nothing written to disk.
//  2. With that file gone, a declaration nothing uses is removed without a
//     decision (a complete read raises no alarm).
//  3. A LaTeX lemma referred to from a LaTeX file the graph could not index
//     (Latin-1): the decision says that a file could not be indexed (its
//     reference is not in the graph); Cancel changes nothing.
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";
import { consoleErrors, decision, dismissNotifications, expectNoDialog, platformSkip, waitForNotification } from "../p1-helpers.mjs";
import { invoke, waitFor } from "../workspace-helpers.mjs";

const BASEL = "namespace LeanBasic\n\ntheorem basel : True := by\n  trivial\n\nend LeanBasic\n";
const KEEP = "namespace LeanBasic\n\ntheorem keep : True := trivial\n\nend LeanBasic\n";
const BROKEN = Buffer.from([0x74, 0x68, 0xff, 0xfe, 0x00, 0xc3, 0x28]);
const MAIN_TEX = String.raw`\documentclass{article}
\begin{document}

\begin{lemma}\label{lem:a}
A closed subset is compact.
\end{lemma}

\end{document}
`;
// Latin-1 (é as one byte): not UTF-8, so the graph cannot index it.
const OTHER_TEX = Buffer.from(String.raw`Café: see Lemma~\ref{lem:a}.` + "\n", "latin1");

async function entities(page) {
  return (await invoke(page, "graph_snapshot", { query: { kinds: null, text: null, around: null, includeInactive: true, limit: 500 } })).entities;
}

async function buffer(page, path) {
  const docs = await invoke(page, "document_list");
  const doc = docs.find((d) => d.path === path);
  if (!doc) return null;
  const state = await invoke(page, "document_state", { document: doc.id });
  return { text: state.text, dirty: state.info.savedRevision !== state.info.revision };
}

export default async function reviewSourceRemovalIncomplete({ launchApp, assert }) {
  const skip = platformSkip();
  if (skip) {
    console.log(`SKIPPED: review-source-removal-incomplete ${skip}`);
    return;
  }
  const base = await mkdtemp(join(tmpdir(), "xp-review-src-"));
  const root = join(base, "project");
  const dataDir = join(base, "data");
  await mkdir(join(root, "lean", "LeanBasic"), { recursive: true });
  await writeFile(join(root, "lean", "LeanBasic", "Basel.lean"), BASEL);
  await writeFile(join(root, "lean", "LeanBasic", "Keep.lean"), KEEP);
  await writeFile(join(root, "lean", "Broken.lean"), BROKEN);
  await writeFile(join(root, "main.tex"), MAIN_TEX);
  await writeFile(join(root, "other.tex"), OTHER_TEX);
  await mkdir(dataDir, { recursive: true });

  const app = await launchApp({ dataDir });
  const { page } = app;
  try {
    await page.setViewportSize({ width: 1440, height: 900 });
    await invoke(page, "workspace_open", { path: root });
    await waitFor(async () => (await invoke(page, "graph_status")).state.kind === "attached", { timeout: 20_000, message: "graph attached" });
    await waitFor(
      async () => {
        const all = await entities(page);
        return all.some((e) => e.key === "LeanBasic.basel") && all.some((e) => e.key === "LeanBasic.keep") && all.some((e) => e.kind === "lemma");
      },
      { timeout: 30_000, message: "both Lean declarations and the lemma extracted" },
    );
    const all = await entities(page);
    const basel = all.find((e) => e.key === "LeanBasic.basel");
    const keep = all.find((e) => e.key === "LeanBasic.keep");
    const lemma = all.find((e) => e.kind === "lemma");
    const deleteFromSource = async (entity) => {
      await runCommand(page, "Show Mathematics", /Graph: Show Mathematics/);
      const item = page.locator(`[data-entity-id="${entity.id}"], .react-flow__node[data-id="${entity.id}"]`).first();
      await item.waitFor({ timeout: 15_000 });
      await item.dispatchEvent("contextmenu", { bubbles: true, cancelable: true, button: 2, clientX: 600, clientY: 400 });
      await page.getByTestId("graph-context-menu").getByRole("menuitem", { name: /^Delete from Source/ }).click();
    };

    // 1. An unreadable Lean file: the decision says the list of uses is incomplete.
    await deleteFromSource(basel);
    await decision(page).waitFor({ timeout: 15_000 });
    const impacts = await decision(page).locator('[data-testid="op-impact"][data-kind="sourceDependents"]').allTextContents();
    assert.deepEqual(
      impacts,
      [
        "Not every use of “LeanBasic.basel” could be checked: 1 Lean file could not be read (lean/Broken.lean). Uses not listed here may break too; check the sources before you save.",
      ],
      "one consequence, naming the file by its project path",
    );
    assert.ok(!(await decision(page).textContent()).includes(base), "no absolute path in the decision");
    await decision(page).locator(".ui-dialog__footer button").last().click();
    await waitForNotification(page, "Removed “LeanBasic.basel” from its source");
    const lean = await buffer(page, "lean/LeanBasic/Basel.lean");
    assert.ok(lean.dirty && !lean.text.includes("theorem basel"), lean.text);
    assert.equal(await readFile(join(root, "lean", "LeanBasic", "Basel.lean"), "utf8"), BASEL, "nothing written to disk");
    assert.deepEqual(await readFile(join(root, "lean", "Broken.lean")), BROKEN, "the unreadable file is untouched");
    await dismissNotifications(page);

    // 2. Every Lean file readable and nothing uses the item: no decision.
    await rm(join(root, "lean", "Broken.lean"));
    await deleteFromSource(keep);
    await expectNoDialog(page, assert, 1500, "a complete list of uses with nothing in it asks nothing");
    await waitForNotification(page, "Removed “LeanBasic.keep” from its source");
    assert.equal(await decision(page).count(), 0, "no decision was shown");
    const kept = await buffer(page, "lean/LeanBasic/Keep.lean");
    assert.ok(kept.dirty && !kept.text.includes("theorem keep"), kept.text);
    assert.equal(await readFile(join(root, "lean", "LeanBasic", "Keep.lean"), "utf8"), KEEP, "nothing written to disk");
    await dismissNotifications(page);

    // 3. A LaTeX file the graph could not index refers to the lemma: said.
    assert.equal((await invoke(page, "graph_status")).coverage.failed, 1, "other.tex could not be indexed");
    await deleteFromSource(lemma);
    await decision(page).waitFor({ timeout: 15_000 });
    const latex = await decision(page).locator('[data-testid="op-impact"][data-kind="sourceDependents"]').allTextContents();
    assert.equal(latex.length, 1, JSON.stringify(latex));
    assert.match(latex[0], /^Not every use of “.+” could be checked: 1 file could not be indexed\. Uses not listed here may break too; check the sources before you save\.$/);
    await decision(page).getByRole("button", { name: "Cancel" }).click();
    await decision(page).waitFor({ state: "detached", timeout: 10_000 });
    assert.equal(await buffer(page, "main.tex"), null, "Cancel changes nothing");

    assert.deepEqual(consoleErrors(app), [], "no console errors");
  } finally {
    await app.close();
  }
}
