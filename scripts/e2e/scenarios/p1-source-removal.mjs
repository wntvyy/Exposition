// P1 follow-up — "Delete from Source" for graph items declared in a source,
// in the real app (disposable project; the Project graph CANVAS, the
// Mathematics section and the Inspector):
//  1. A theorem on the canvas: Delete (key) → ONE decision: its proof is
//     removed with it by default; confirm → the Editor shows paper.tex with
//     the theorem and its proof removed as an UNSAVED edit; the file on disk is
//     unchanged; "Undo Removal" brings the text back.
//  2. A lemma a proof refers to (\ref): the decision names the proof; Cancel
//     changes nothing.
//  3. A BibTeX entry cited by a proof (Inspector › More actions › Delete from
//     Source): confirmed, saved with Ctrl+S → the graph follows (the
//     reference is no longer active).
//  4. A Lean declaration (Mathematics, right click): the decision lists the
//     declaration in another file that mentions its name; confirmed → removed
//     from the buffer, nothing on disk.
//  5. Keys on items that cannot take them explain why: Delete on the project
//     node, Shift+Delete on a theorem ("use Delete from Source").
//  6. A labelled Typst theorem (Inspector): the decision says the reference
//     becomes undefined; the call AND its trailing label are removed in one
//     undoable edit; after saving, the surviving theorem keeps its own label
//     and the deleted one's label is defined nowhere.
import { cp, mkdtemp, mkdir, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";
import {
  consoleErrors,
  decision,
  dismissNotifications,
  platformSkip,
  pressNotificationAction,
  publishShots,
  settle,
  waitForNotification,
} from "../p1-helpers.mjs";
import { invoke, waitFor } from "../workspace-helpers.mjs";

const BASEL = "namespace LeanBasic\n\n/-- The Basel sum. -/\ntheorem basel : True := by\n  trivial\n\ntheorem keep : True := trivial\n\nend LeanBasic\n";
const USE = "import LeanBasic.Basel\n\nnamespace Other\n\ntheorem uses : True := LeanBasic.basel\n\nend Other\n";
// The review's fixture (XR-D-04): numbered theorem figures with labels after the calls.
const TYPST_REF = [
  "#set page(width: 150mm, height: 130mm, margin: 15mm)",
  '#let theorem(body) = figure(body, kind: "theorem", supplement: [Theorem], numbering: "1")',
  "#theorem[First surviving statement.] <keep-theorem>",
  "#theorem[Second target statement.] <delete-theorem>",
  "",
  "Reference to target: @delete-theorem.",
  "",
].join("\n");
const TYPST_AFTER = TYPST_REF.replace("#theorem[Second target statement.] <delete-theorem>\n", "");

async function entities(page) {
  return (await invoke(page, "graph_snapshot", { query: { kinds: null, text: null, around: null, includeInactive: true, limit: 500 } })).entities;
}

async function buffer(page, path) {
  const docs = await invoke(page, "document_list");
  const doc = docs.find((d) => d.path === path);
  if (!doc) return null;
  const state = await invoke(page, "document_state", { document: doc.id });
  return { text: state.text, dirty: state.info.savedRevision !== state.info.revision, id: doc.id };
}

export default async function p1SourceRemoval({ launchApp, artifactsDir, assert, fixture }) {
  const skip = platformSkip();
  if (skip) {
    console.log(`SKIPPED: p1-source-removal ${skip}`);
    return;
  }
  const base = await mkdtemp(join(tmpdir(), "xp-p1-src-"));
  const root = join(base, "project");
  const dataDir = join(base, "data");
  await cp(fixture("graph-basic"), root, { recursive: true });
  await mkdir(join(root, "lean", "LeanBasic"), { recursive: true });
  await writeFile(join(root, "lean", "LeanBasic", "Basel.lean"), BASEL);
  await writeFile(join(root, "lean", "LeanBasic", "Use.lean"), USE);
  await writeFile(join(root, "delete-ref.typ"), TYPST_REF);
  await mkdir(dataDir, { recursive: true });
  const paperOnDisk = await readFile(join(root, "paper.tex"), "utf8");
  const shots = [];
  const shot = async (page, name) => {
    const path = join(artifactsDir, `${name}.png`);
    await settle(page);
    await page.screenshot({ path });
    shots.push(path);
  };

  const app = await launchApp({ dataDir });
  const { page } = app;
  try {
    await page.setViewportSize({ width: 1440, height: 900 });
    await invoke(page, "workspace_open", { path: root });
    await waitFor(async () => (await invoke(page, "graph_status")).state.kind === "attached", { timeout: 20_000, message: "graph attached" });
    await waitFor(
      async () => {
        const all = await entities(page);
        return all.filter((e) => e.kind === "theorem").length >= 3 && all.some((e) => e.key === "LeanBasic.basel") && all.some((e) => e.key === "rudin");
      },
      { timeout: 30_000, message: "theorem, Lean declaration and bib entry extracted" },
    );
    const all = await entities(page);
    const theorem = all.find((e) => e.kind === "theorem" && /thm:main/.test(`${e.key} ${e.label}`));
    const lemma = all.find((e) => e.kind === "lemma");
    const rudin = all.find((e) => e.key === "rudin");
    const basel = all.find((e) => e.key === "LeanBasic.basel");
    const project = all.find((e) => e.kind === "project");

    await page.evaluate(() => localStorage.setItem("exposition.graph.mode.project", "canvas"));
    await runCommand(page, "Show Project Graph", /Graph: Show Project Graph/);
    const canvas = page.getByTestId("graph-canvas");
    await canvas.waitFor({ timeout: 15_000 });
    await page.waitForTimeout(800);
    const node = (id) => page.locator(`.react-flow__node[data-id="${id}"]`);
    const select = async (id) => {
      await node(id).focus();
      await page.keyboard.press("Enter");
      await waitFor(async () => (await node(id).getAttribute("class")).includes("selected"), { message: `node ${id} selected` });
    };

    // 5a. Delete on the project node explains instead of doing nothing.
    await select(project.id);
    await page.keyboard.press("Delete");
    const explained = await waitForNotification(page, /is not available$/);
    assert.match(explained.body, /The project itself is not deleted/);
    await dismissNotifications(page);
    // 5b. Shift+Delete on a theorem: removed from the source only as a reviewed edit.
    await select(theorem.id);
    await page.keyboard.press("Shift+Delete");
    const permanent = await waitForNotification(page, /^Delete Permanently is not available$/);
    assert.match(permanent.body, /use Delete from Source/);
    await dismissNotifications(page);

    // 1. The theorem: Delete → one decision (its proof), confirm → unsaved edit in the Editor.
    await select(theorem.id);
    await page.keyboard.press("Delete");
    await decision(page).waitFor({ timeout: 15_000 });
    assert.equal(await page.getByRole("alertdialog").count(), 1, "one decision");
    assert.match(await decision(page).textContent(), /Delete “Theorem thm:main” from its source/);
    const proofImpact = decision(page).locator('[data-testid="op-impact"][data-kind="sourceDependents"]');
    assert.match(await proofImpact.first().textContent(), /has a proof/);
    assert.equal(await decision(page).locator('[data-choice="proofs"]').getAttribute("data-value"), "remove", "the proof goes with it by default");
    await shot(page, "p1-source-removal-decision-light");
    await decision(page).locator(".ui-dialog__footer button").last().click();
    const removed = await waitForNotification(page, "Removed “Theorem thm:main” from its source");
    assert.match(removed.body, /with its proof as an unsaved edit/);
    assert.ok(removed.actions.includes("Show in Editor") && removed.actions.includes("Undo Removal"), JSON.stringify(removed.actions));
    const after = await buffer(page, "paper.tex");
    assert.ok(after && after.dirty, "paper.tex has an unsaved edit");
    assert.ok(!after.text.includes("\\begin{theorem}") && !after.text.includes("\\begin{proof}"), "theorem and proof removed from the buffer");
    assert.ok(after.text.includes("\\begin{lemma}"), "the lemma stays");
    assert.equal(await readFile(join(root, "paper.tex"), "utf8"), paperOnDisk, "nothing written to disk");
    await waitFor(async () => (await page.locator(".monaco-editor .view-lines").first().textContent().catch(() => "")).includes("begin{lemma}"), {
      message: "the Editor shows paper.tex",
    });
    await shot(page, "p1-source-removal-editor-light");
    await pressNotificationAction(page, "Removed “Theorem thm:main” from its source", "Undo Removal");
    await waitForNotification(page, "Removal undone");
    assert.equal((await buffer(page, "paper.tex")).text, paperOnDisk, "Undo Removal brings the text back");
    await dismissNotifications(page);

    // 2. The lemma a proof refers to: the decision names the proof; Cancel changes nothing.
    await runCommand(page, "Show Project Graph", /Graph: Show Project Graph/);
    await canvas.waitFor({ timeout: 15_000 });
    await select(lemma.id);
    await page.keyboard.press("Delete");
    await decision(page).waitFor({ timeout: 15_000 });
    assert.match(await decision(page).textContent(), /“Proof \(paper\.tex\)” references it/);
    await decision(page).getByRole("button", { name: "Cancel" }).click();
    await waitFor(async () => (await page.getByRole("alertdialog").count()) === 0, { message: "decision closed" });
    assert.equal((await buffer(page, "paper.tex")).text, paperOnDisk, "Cancel changes nothing");

    // 3. The BibTeX entry through the Inspector; saved → the graph follows.
    await select(rudin.id);
    const inspector = page.getByTestId("graph-entity-inspector");
    await inspector.waitFor({ timeout: 10_000 });
    await inspector.getByRole("button", { name: "More actions" }).click();
    await page.getByRole("menuitem", { name: /^Delete from Source/ }).click();
    await decision(page).waitFor({ timeout: 15_000 });
    assert.match(await decision(page).textContent(), /references it/);
    await decision(page).locator(".ui-dialog__footer button").last().click();
    await waitForNotification(page, /^Removed “Principles of Mathematical Analysis” from its source$/);
    const bib = await buffer(page, "refs.bib");
    assert.ok(bib.dirty && !bib.text.includes("@book{rudin"), "the entry is removed from the refs.bib buffer");
    await waitFor(async () => (await page.locator(".monaco-editor .view-lines").first().textContent().catch(() => "")) !== null, { message: "editor" });
    await page.locator(".monaco-editor textarea").first().focus();
    await page.keyboard.press("Control+S");
    await waitFor(async () => !(await buffer(page, "refs.bib")).dirty, { message: "refs.bib saved" });
    await waitFor(async () => (await entities(page)).find((e) => e.id === rudin.id)?.lifecycle !== "active", { timeout: 20_000, message: "the graph follows the save" });
    await dismissNotifications(page);

    // 4. A Lean declaration from Mathematics: a name mention in another file is listed.
    await runCommand(page, "Show Mathematics", /Graph: Show Mathematics/);
    const item = page.locator(`[data-entity-id="${basel.id}"], .react-flow__node[data-id="${basel.id}"]`).first();
    await item.waitFor({ timeout: 15_000 });
    await item.dispatchEvent("contextmenu", { bubbles: true, cancelable: true, button: 2, clientX: 600, clientY: 400 });
    await page.getByTestId("graph-context-menu").getByRole("menuitem", { name: /^Delete from Source/ }).click();
    await decision(page).waitFor({ timeout: 15_000 });
    const mention = await decision(page).locator('[data-testid="op-impact"][data-kind="sourceDependents"]').first().textContent();
    assert.match(mention, /mentioned by name in “Other\.uses” \(lean\/LeanBasic\/Use\.lean\)/);
    await shot(page, "p1-source-removal-lean-decision-light");
    await decision(page).locator(".ui-dialog__footer button").last().click();
    await waitForNotification(page, "Removed “LeanBasic.basel” from its source");
    const lean = await buffer(page, "lean/LeanBasic/Basel.lean");
    assert.ok(lean.dirty && !lean.text.includes("theorem basel") && !lean.text.includes("The Basel sum") && lean.text.includes("theorem keep"), lean.text);
    assert.equal(await readFile(join(root, "lean", "LeanBasic", "Basel.lean"), "utf8"), BASEL, "nothing written to disk");
    await dismissNotifications(page);

    // 6. A labelled Typst theorem (XR-D-04): its label goes with it in the
    //    same undoable edit, so the reference cannot attach to the theorem
    //    before it; after saving, the label is defined nowhere.
    const typstTheorem = (await entities(page)).find((e) => e.kind === "theorem" && (e.key === "delete-theorem" || e.label.includes("delete-theorem")));
    const keepTheorem = (await entities(page)).find((e) => e.kind === "theorem" && (e.key === "keep-theorem" || e.label.includes("keep-theorem")));
    assert.ok(typstTheorem && keepTheorem, "both Typst theorems are extracted");
    const deleteTypst = async () => {
      await runCommand(page, "Show Project Graph", /Graph: Show Project Graph/);
      await canvas.waitFor({ timeout: 15_000 });
      await select(typstTheorem.id);
      await inspector.waitFor({ timeout: 10_000 });
      await inspector.getByRole("button", { name: "More actions" }).click();
      await page.getByRole("menuitem", { name: /^Delete from Source/ }).click();
      await decision(page).waitFor({ timeout: 15_000 });
      const text = await decision(page).textContent();
      assert.match(text, /references it/, "the reference is named");
      assert.match(text, /shows as undefined/, "and said to become undefined");
      assert.match(text, /delete-ref\.typ, line 4/, "the item's line, label included");
      await decision(page).locator(".ui-dialog__footer button").last().click();
      await waitForNotification(page, /^Removed “.*delete-theorem.*” from its source$/);
    };
    await deleteTypst();
    await shot(page, "p1-source-removal-typst-light");
    const typst = await buffer(page, "delete-ref.typ");
    assert.equal(typst.text, TYPST_AFTER, "the call and its label are removed together; nothing else");
    assert.ok(typst.dirty, "an unsaved edit");
    assert.equal(await readFile(join(root, "delete-ref.typ"), "utf8"), TYPST_REF, "nothing written to disk");
    await pressNotificationAction(page, /^Removed “.*delete-theorem.*” from its source$/, "Undo Removal");
    await waitForNotification(page, "Removal undone");
    assert.equal((await buffer(page, "delete-ref.typ")).text, TYPST_REF, "one Undo brings back the call and its label");
    await dismissNotifications(page);
    await deleteTypst();
    await page.locator(".monaco-editor textarea").first().focus();
    await page.keyboard.press("Control+S");
    await waitFor(async () => !(await buffer(page, "delete-ref.typ")).dirty, { message: "delete-ref.typ saved" });
    assert.equal(await readFile(join(root, "delete-ref.typ"), "utf8"), TYPST_AFTER);
    await waitFor(async () => (await entities(page)).find((e) => e.id === typstTheorem.id)?.lifecycle !== "active", { timeout: 20_000, message: "the graph follows the save" });
    const kept = (await entities(page)).find((e) => e.id === keepTheorem.id);
    assert.equal(kept?.lifecycle, "active", "the surviving theorem keeps its identity");
    const inspected = await invoke(page, "graph_inspect_at_source", { request: { path: "delete-ref.typ", offset: null, revision: null } });
    const theorems = inspected.entities.filter((x) => x.entity.kind === "theorem").map((x) => x.binding.selector.name).filter(Boolean);
    assert.deepEqual(theorems, ["keep-theorem"], "the surviving theorem is still named by its own label only");

    assert.deepEqual(consoleErrors(app), [], "no console errors");
    await publishShots(shots);
  } finally {
    await app.close();
  }
}
