// Graph page in the real app (M7 lane D):
//  1. Ordinary use with the Graph page never opened: open a project, edit and
//     save a file; the Graph page is not mounted.
//  2. Graph canvas: colored rectangles, move a node (its connections stay
//     attached: edge endpoints follow), pan, zoom, wait for the layout to save.
//  3. Relaunch: positions and viewport restored; semantic relations unchanged
//     by layout changes.
//  4. Typed relationship authoring on the canvas: a valid relation is reviewed
//     and applied; an invalid one is refused with the backend's reason and
//     nothing changes.
//  5. Keyboard alternative (table), Mathematics dependencies + verification
//     table (dependencies, proof target, no formal evidence, formal dependency
//     edges unavailable), Editor dependency inspector at a Monaco caret.
//  6. Page navigation (Mod+1/Mod+4, aria-current), F6 regions on the Graph page,
//     narrow window, both themes. Reference screenshots go to
//     test-results/e2e/graph-canvas/ and, with EXPOSITION_DEV_HOME set, to
//     $EXPOSITION_DEV_HOME/ui-reference/graph/.
import { execFileSync } from "node:child_process";
import { copyFile, cp, mkdir, mkdtemp, readFile, readdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";
import { editorLocator, invoke, invokeError, openFromTree, waitFor, waitForText } from "../workspace-helpers.mjs";

const WIDE = { width: 1440, height: 900 };

async function closeGracefully(app) {
  const exited = new Promise((resolve) => {
    if (app.child.exitCode !== null) resolve();
    else app.child.once("exit", resolve);
  });
  try {
    if (process.platform === "win32") execFileSync("taskkill", ["/PID", String(app.child.pid)], { stdio: "ignore" });
    else app.child.kill("SIGTERM");
  } catch {
    // already exiting
  }
  await Promise.race([exited, new Promise((resolve) => setTimeout(resolve, 15000))]);
  await app.close();
}

async function settle(page) {
  await page
    .waitForFunction(
      () =>
        !document.querySelector("[data-entering], [data-exiting]") &&
        document.getAnimations().every((a) => a.playState !== "running" || a.effect?.getTiming().iterations === Infinity),
      null,
      { timeout: 5000 },
    )
    .catch(() => {});
}

async function setViewport(page, size) {
  await page.setViewportSize(size);
  await page.waitForFunction((w) => window.innerWidth === w, size.width);
  await page.waitForTimeout(150);
}

function consoleErrors(app) {
  return app.consoleMessages.filter((m) => m.startsWith("error:") || m.startsWith("pageerror:"));
}

async function openProject(page, root) {
  await invoke(page, "workspace_open", { path: root });
  await page.locator('[data-testid="file-tree"] [data-path="paper.tex"]').waitFor({ timeout: 15000 });
  await waitFor(async () => (await invoke(page, "graph_status")).state.kind === "attached", { timeout: 15000, message: "graph attached" });
}

/** Propose and apply typed operations through the backend (as the user). */
async function change(page, operations, summary = "e2e setup") {
  const status = await invoke(page, "graph_status");
  const proposal = await invoke(page, "graph_propose_change", { request: { baseRevision: status.revision, operations, summary } });
  if (!proposal.valid) throw new Error(`proposal invalid: ${JSON.stringify(proposal.diagnostics)}`);
  const outcome = await invoke(page, "graph_apply_change", { request: { proposal: proposal.id, expectedRevision: proposal.baseRevision } });
  if (outcome.kind !== "applied") throw new Error(`apply ${outcome.kind}: ${JSON.stringify(outcome.diagnostics)}`);
  return outcome;
}

const create = (tempId, kind, label) => ({ op: "createEntity", tempId, kind, roles: [], label, key: null, attributes: {}, binding: null });
const rel = (kind, from, to) => ({
  op: "addRelation",
  kind,
  from: from.startsWith("t:") ? { kind: "temp", tempId: from.slice(2) } : { kind: "id", id: from },
  to: to.startsWith("t:") ? { kind: "temp", tempId: to.slice(2) } : { kind: "id", id: to },
  evidenceKind: null,
  evidence: [],
  note: null,
});

async function semanticRelations(page) {
  const snap = await invoke(page, "graph_snapshot", {
    query: { kinds: null, text: null, around: null, includeInactive: true, limit: 2000 },
  });
  return snap.relations.map((r) => `${r.kind}:${r.from}>${r.to}:${r.status}`).sort();
}

const node = (page, id) => page.locator(`[data-testid="graph-canvas"] .react-flow__node[data-id="${id}"]`);
const edgePath = (page, id) => page.locator(`[data-testid="graph-canvas"] .react-flow__edge[data-id="${id}"] path.react-flow__edge-path`).first();

function endpoints(d) {
  const numbers = d.match(/-?\d+(\.\d+)?/g).map(Number);
  return { source: numbers.slice(0, 2), target: numbers.slice(-2) };
}

async function nodeTransform(page, id) {
  return node(page, id).evaluate((el) => {
    const m = /translate\((-?[\d.]+)px,\s*(-?[\d.]+)px\)/.exec(el.style.transform);
    return m ? { x: Number(m[1]), y: Number(m[2]) } : null;
  });
}

async function viewportTransform(page, testId = "graph-canvas") {
  return page.locator(`[data-testid="${testId}"] .react-flow__viewport`).evaluate((el) => {
    const m = /translate\((-?[\d.]+)px,\s*(-?[\d.]+)px\)\s*scale\(([\d.]+)\)/.exec(el.style.transform);
    return m ? { x: Number(m[1]), y: Number(m[2]), zoom: Number(m[3]) } : null;
  });
}

async function drag(page, from, to, steps = 12) {
  await page.mouse.move(from.x, from.y);
  await page.mouse.down();
  for (let i = 1; i <= steps; i++) {
    await page.mouse.move(from.x + ((to.x - from.x) * i) / steps, from.y + ((to.y - from.y) * i) / steps);
  }
  await page.mouse.up();
}

async function center(locator) {
  const box = await locator.boundingBox();
  return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
}

async function waitSaved(page) {
  await waitFor(async () => (await page.getByTestId("graph-save-state").first().getAttribute("data-state")) === "saved", {
    timeout: 10000,
    message: "layout saved",
  });
}

async function projectView(page) {
  const views = await invoke(page, "graph_views");
  const summary = views.find((v) => v.scope.kind === "project");
  return summary ? invoke(page, "graph_view_get", { view: summary.id }) : null;
}

async function showGraph(page) {
  await page.locator('[data-page-button="graph"]').click();
  await page.getByTestId("graph-page").waitFor({ timeout: 10000 });
}

/** Dismiss visible notifications (they can cover canvas nodes in the bottom-right corner). */
async function dismissNotifications(page) {
  for (const button of await page.getByRole("button", { name: /^Dismiss notification/ }).all()) await button.click().catch(() => {});
}

async function connect(page, fromId, toId) {
  await dismissNotifications(page);
  const from = node(page, fromId);
  await from.hover();
  const handle = from.locator(".xp-graph-node__source");
  await handle.waitFor({ state: "visible" });
  const start = await center(handle);
  const end = await center(node(page, toId));
  await drag(page, start, end, 16);
}

/** Choose an option of a labelled Select inside `scope`. */
async function chooseOption(page, scope, label, option) {
  const field = scope.locator(".ui-select", { has: page.locator(".ui-field__label", { hasText: label }) });
  await field.getByRole("button").click();
  const listbox = page.getByRole("listbox");
  await listbox.waitFor();
  await listbox.getByRole("option", { name: option }).click();
  await listbox.waitFor({ state: "detached" });
}

export default async function graphCanvas({ launchApp, artifactsDir, assert, fixture }) {
  const shots = [];
  const shot = async (page, name) => {
    await settle(page);
    const path = join(artifactsDir, `${name}.png`);
    await page.screenshot({ path });
    shots.push(path);
  };
  const base = await mkdtemp(join(tmpdir(), "xp-graph-"));
  const root = join(base, "project");
  const dataDir = join(base, "data");
  await cp(fixture("graph-basic"), root, { recursive: true });
  await mkdir(dataDir, { recursive: true });
  const paperText = (await readFile(join(root, "paper.tex"), "utf8")).replace(/\r\n/g, "\n");

  const ids = {};
  let relationsBefore;
  let savedPositions;
  let savedViewport;
  const thmStart = paperText.indexOf("\\begin{theorem}");
  const thmEnd = paperText.indexOf("\\end{theorem}") + "\\end{theorem}".length;

  // ===== Session 1 ==========================================================
  let app = await launchApp({ dataDir });
  try {
    const { page } = app;
    await setViewport(page, WIDE);
    await page.getByTestId("workbench").waitFor({ timeout: 15000 });
    await runCommand(page, "color theme light", /Color Theme: Light/);

    // --- 1. Ordinary use with Graph never opened ---------------------------------
    await openProject(page, root);
    await openFromTree(page, "notes.md");
    const notes = editorLocator(page, "notes.md");
    await notes.locator(".monaco-editor").waitFor({ timeout: 15000 });
    const docId = await waitFor(() => notes.getAttribute("data-document-id"), { message: "notes document id" });
    await notes.locator(".view-lines").click();
    await page.keyboard.press("Control+End");
    await page.keyboard.type("Edited without the graph.");
    const original = (await readFile(join(root, "notes.md"), "utf8")).replace(/\r\n/g, "\n");
    await waitForText(page, docId, `${original}Edited without the graph.`);
    await page.keyboard.press("Control+S");
    await waitFor(async () => (await readFile(join(root, "notes.md"), "utf8")).includes("Edited without the graph."), { message: "notes saved" });
    assert.equal(await page.locator('[data-page="graph"]').count(), 0, "the Graph page was never mounted during ordinary use");
    const railGraph = page.locator('[data-page-button="graph"]');
    assert.equal(await railGraph.getAttribute("aria-current"), null);
    assert.equal(await page.locator('[data-page-button="editor"]').getAttribute("aria-current"), "page");

    // --- Seed typed entities and relations through the backend ---------------------
    const seeded = await change(page, [
      create("thm", "theorem", "Image of a compact set"),
      create("lem", "lemma", "Closed subsets of compacts"),
      create("def", "definition", "Compactness"),
      create("hyp", "assumption", "Hypothesis H"),
      create("prf", "proof", "Proof of the image theorem"),
      create("col", "collection", "Reading list"),
      rel("dependsOn", "t:thm", "t:lem"),
      rel("assumes", "t:thm", "t:hyp"),
      rel("dependsOn", "t:lem", "t:def"),
      rel("proofOf", "t:prf", "t:thm"),
      rel("memberOf", "t:lem", "t:col"),
    ]);
    for (const { tempId, id } of seeded.created) ids[tempId] = id;
    await change(page, [
      { op: "bindSource", entity: { kind: "id", id: ids.thm }, binding: { path: "paper.tex", selector: { kind: "region" }, range: { start: thmStart, end: thmEnd } } },
    ]);
    const dependsEdge = (await invoke(page, "graph_entity", { entity: ids.thm })).outgoing.find((r) => r.kind === "dependsOn" && r.to === ids.lem);
    ids.dependsEdge = dependsEdge.id;

    // --- 6a. Navigation: Mod+4 shows Graph (focus moves in), Mod+1 returns -----------
    await page.keyboard.press("Control+4");
    await page.getByTestId("graph-page").waitFor({ timeout: 10000 });
    assert.equal(await railGraph.getAttribute("aria-current"), "page", "Graph is the current page");
    await waitFor(() => page.evaluate(() => Boolean(document.activeElement?.closest('[data-page="graph"]'))), { message: "focus moved into the Graph page" });

    // --- 2. Canvas ------------------------------------------------------------------
    const canvas = page.getByTestId("graph-canvas");
    await canvas.waitFor({ timeout: 15000 });
    for (const key of ["thm", "lem", "def", "hyp", "prf", "col"]) await node(page, ids[key]).waitFor({ timeout: 15000 });
    await waitSaved(page); // initial placement of unpositioned items is saved
    const lemCard = node(page, ids.lem).locator(".ui-node-card");
    assert.equal(await lemCard.getAttribute("data-category"), "math", "lemma is a math-colored rectangle");
    assert.match(await lemCard.innerText(), /Lemma/i, "type label is text on the node");
    const lemStyle = await lemCard.evaluate((el) => ({ bg: getComputedStyle(el).backgroundColor, radius: getComputedStyle(el).borderRadius }));
    assert.notEqual(lemStyle.bg, "rgba(0, 0, 0, 0)", "node has a colored fill");
    await shot(page, "graph-canvas-light");

    // Move a node: its edges stay attached (endpoints follow the node).
    await edgePath(page, ids.dependsEdge).waitFor({ state: "attached", timeout: 10000 });
    const pathBefore = endpoints(await edgePath(page, ids.dependsEdge).getAttribute("d"));
    const posBefore = await nodeTransform(page, ids.lem);
    const lemCenter = await center(lemCard);
    await drag(page, lemCenter, { x: lemCenter.x + 160, y: lemCenter.y + 110 });
    await waitFor(async () => {
      const p = await nodeTransform(page, ids.lem);
      return p && (Math.abs(p.x - posBefore.x) > 50 || Math.abs(p.y - posBefore.y) > 50);
    }, { message: "lemma node moved" });
    const posAfter = await nodeTransform(page, ids.lem);
    const pathAfter = endpoints(await edgePath(page, ids.dependsEdge).getAttribute("d"));
    assert.notDeepEqual(pathAfter.target, pathBefore.target, `the dependency edge's lemma end followed the node (${pathBefore.target} -> ${pathAfter.target})`);
    // The edge end lies on the moved node's border (attached, not left behind).
    const zoomNow = (await viewportTransform(page)).zoom;
    const lemBox = await lemCard.boundingBox();
    const vp = await viewportTransform(page);
    const canvasBox = await canvas.locator(".react-flow").boundingBox();
    const endScreen = { x: canvasBox.x + vp.x + pathAfter.target[0] * zoomNow, y: canvasBox.y + vp.y + pathAfter.target[1] * zoomNow };
    const tolerance = 4;
    assert.ok(
      endScreen.x >= lemBox.x - tolerance && endScreen.x <= lemBox.x + lemBox.width + tolerance && endScreen.y >= lemBox.y - tolerance && endScreen.y <= lemBox.y + lemBox.height + tolerance,
      `edge end ${JSON.stringify(endScreen)} is on the moved node ${JSON.stringify(lemBox)}`,
    );
    await shot(page, "graph-node-moved-light");

    // Pan by dragging the background, then zoom with the button and the wheel.
    const vpBefore = await viewportTransform(page);
    const paneBox = await canvas.locator(".react-flow").boundingBox();
    const empty = { x: paneBox.x + 30, y: paneBox.y + paneBox.height - 30 };
    const hit = await page.evaluate(({ x, y }) => document.elementFromPoint(x, y)?.className?.toString() ?? "", empty);
    assert.match(hit, /react-flow__pane/, `pan starts on the background (${hit})`);
    await drag(page, empty, { x: empty.x + 120, y: empty.y - 60 });
    const vpPanned = await viewportTransform(page);
    assert.ok(Math.abs(vpPanned.x - vpBefore.x - 120) < 3 && Math.abs(vpPanned.y - vpBefore.y + 60) < 3, `background drag pans (${JSON.stringify(vpBefore)} -> ${JSON.stringify(vpPanned)})`);
    await canvas.getByRole("button", { name: "Zoom in" }).click();
    await waitFor(async () => (await viewportTransform(page)).zoom > vpPanned.zoom + 0.01, { message: "zoom in button" });
    const zoomed = await viewportTransform(page);
    await page.mouse.move(paneBox.x + paneBox.width / 2, paneBox.y + paneBox.height / 2);
    await page.mouse.wheel(0, 240);
    await waitFor(async () => (await viewportTransform(page)).zoom < zoomed.zoom - 0.01, { message: "wheel zooms out" });
    await page.waitForTimeout(400);
    await waitSaved(page);
    savedViewport = await viewportTransform(page);
    const view = await projectView(page);
    const savedLem = view.nodes.find((n) => n.entity === ids.lem);
    assert.ok(Math.abs(savedLem.x - posAfter.x) <= 1 && Math.abs(savedLem.y - posAfter.y) <= 1, `moved position saved (${JSON.stringify(savedLem)} vs ${JSON.stringify(posAfter)})`);
    assert.ok(Math.abs(view.viewport.zoom - savedViewport.zoom) < 0.002, `viewport saved (${JSON.stringify(view.viewport)} vs ${JSON.stringify(savedViewport)})`);
    savedPositions = Object.fromEntries(view.nodes.map((n) => [n.entity, { x: n.x, y: n.y }]));
    relationsBefore = await semanticRelations(page);

    // Inspector for a selected node (typed relationships, actions).
    await node(page, ids.thm).click();
    const inspector = page.getByTestId("graph-entity-inspector");
    await inspector.waitFor();
    assert.equal(await inspector.getAttribute("data-entity-id"), ids.thm);
    await inspector.getByText("Mathematical dependencies").first().waitFor();
    await inspector.getByRole("button", { name: "What needs review if this changes?" }).click();
    await page.getByTestId("graph-impact-item").first().waitFor({ timeout: 10000 });
    await shot(page, "graph-inspector-light");

    // F6 cycles the Graph page's regions.
    const regions = [];
    for (let i = 0; i < 7; i++) {
      await page.keyboard.press("F6");
      regions.push(await page.evaluate(() => document.activeElement?.closest("[data-region]")?.getAttribute("data-region")));
    }
    for (const region of ["activitybar", "graph-header", "graph-toolbar", "graph-main", "graph-inspector", "statusbar"]) {
      assert.ok(regions.includes(region), `F6 reaches ${region} (visited ${regions.join(", ")})`);
    }

    assert.deepEqual(consoleErrors(app), [], "no console errors in session 1");
  } finally {
    await closeGracefully(app);
  }

  // ===== Session 2: relaunch restores the saved layout =========================
  app = await launchApp({ dataDir });
  try {
    const { page } = app;
    await setViewport(page, WIDE);
    await page.getByTestId("workbench").waitFor({ timeout: 15000 });
    await runCommand(page, "color theme light", /Color Theme: Light/);
    await openProject(page, root);
    await showGraph(page);
    const canvas = page.getByTestId("graph-canvas");
    for (const key of ["thm", "lem", "def", "hyp", "prf", "col"]) await node(page, ids[key]).waitFor({ timeout: 15000 });
    for (const key of ["thm", "lem", "def"]) {
      const p = await nodeTransform(page, ids[key]);
      const s = savedPositions[ids[key]];
      assert.ok(Math.abs(p.x - s.x) <= 1 && Math.abs(p.y - s.y) <= 1, `${key} restored at its saved position (${JSON.stringify(p)} vs ${JSON.stringify(s)})`);
    }
    await waitFor(async () => {
      const v = await viewportTransform(page);
      return v && Math.abs(v.zoom - savedViewport.zoom) < 0.002 && Math.abs(v.x - savedViewport.x) <= 1 && Math.abs(v.y - savedViewport.y) <= 1;
    }, { message: `viewport restored to ${JSON.stringify(savedViewport)}` });
    assert.deepEqual(await semanticRelations(page), relationsBefore, "moving, panning, and zooming changed no relationship");
    await canvas.getByRole("button", { name: "Fit to view" }).click();
    await page.waitForTimeout(400);

    // Collapsed groups (view-only): the collection folds its member; relations re-route to it.
    await node(page, ids.col).getByRole("button", { name: "Collapse Reading list" }).click();
    await node(page, ids.lem).waitFor({ state: "detached" });
    assert.match(await node(page, ids.col).innerText(), /1 inside/);
    assert.ok(await canvas.locator(`.react-flow__edge[data-id^="folded:${ids.thm}:${ids.col}"]`).count(), "the theorem's dependency on the folded lemma re-routes to the collection");
    await shot(page, "graph-collapsed-group-light");
    await node(page, ids.col).getByRole("button", { name: "Expand Reading list" }).click();
    await node(page, ids.lem).waitFor();
    assert.deepEqual(await semanticRelations(page), relationsBefore, "collapsing changed no relationship");

    // --- 4. Typed relationship authoring ---------------------------------------------
    // Valid: Closed subsets of compacts (lemma) → Hypothesis H (assumption), "assumes".
    await connect(page, ids.lem, ids.hyp);
    const dialog = page.getByTestId("graph-relate-dialog");
    await dialog.waitFor({ timeout: 5000 });
    await chooseOption(page, dialog, "Relationship", /^assumes$/);
    const review = page.getByTestId("graph-proposal-review");
    await waitFor(async () => (await review.getAttribute("data-state")) === "valid", { message: "valid proposal" });
    assert.ok((await semanticRelations(page)).length === relationsBefore.length, "nothing applied while reviewing");
    await shot(page, "graph-relate-valid-light");
    await page.getByRole("button", { name: "Add relationship" }).click();
    await dialog.waitFor({ state: "detached" });
    const added = await waitFor(async () => {
      const detail = await invoke(page, "graph_entity", { entity: ids.lem });
      return detail.outgoing.find((r) => r.kind === "assumes" && r.to === ids.hyp);
    }, { message: "assumes relation applied" });
    assert.equal(added.status, "accepted");
    await page.locator(`[data-testid="graph-canvas"] .react-flow__edge[data-id="${added.id}"]`).waitFor({ timeout: 10000 });
    const relationsAfterValid = await semanticRelations(page);
    assert.equal(relationsAfterValid.length, relationsBefore.length + 1);

    // Invalid: Compactness (definition) "is a proof of" Proof — refused with the reason; nothing changes.
    await connect(page, ids.def, ids.prf);
    await dialog.waitFor({ timeout: 5000 });
    await chooseOption(page, dialog, "Relationship", /^is a proof of \(not listed/);
    await waitFor(async () => (await review.getAttribute("data-state")) === "invalid", { message: "invalid proposal" });
    const reason = await review.innerText();
    assert.match(reason, /Not allowed/);
    assert.ok(await review.locator('[data-code="graph.endpointRule"]').count(), `endpoint rule diagnostic shown: ${reason}`);
    assert.equal(await page.getByRole("button", { name: "Add relationship" }).isDisabled(), true, "Apply is disabled for an invalid proposal");
    await shot(page, "graph-relate-invalid-light");
    await page.getByRole("button", { name: "Cancel" }).click();
    await dialog.waitFor({ state: "detached" });
    assert.deepEqual(await semanticRelations(page), relationsAfterValid, "the refused connection changed nothing");
    assert.equal(await page.locator('[data-testid="graph-canvas"] .xp-edge--math').count() >= 3, true);

    // --- 5a. Keyboard alternative: table ---------------------------------------------
    await page.getByRole("radio", { name: "Table" }).click();
    const grid = page.getByRole("grid", { name: "Entities" });
    await grid.waitFor();
    const row = grid.locator(`[data-entity-id="${ids.def}"]`);
    await row.focus();
    await page.keyboard.press("Space");
    await page.getByTestId("graph-entity-inspector").and(page.locator(`[data-entity-id="${ids.def}"]`)).waitFor();
    await shot(page, "graph-table-light");
    await page.getByRole("radio", { name: "Canvas" }).click();
    await canvas.waitFor();

    // --- 5b. Mathematics ----------------------------------------------------------------
    await page.getByRole("radio", { name: "Mathematics" }).click();
    const mathCanvas = page.getByTestId("graph-math-canvas");
    await mathCanvas.waitFor({ timeout: 10000 });
    await mathCanvas.locator(`.react-flow__node[data-id="${ids.thm}"]`).waitFor({ timeout: 10000 });
    assert.equal(await mathCanvas.locator(`.react-flow__node[data-id="${ids.col}"]`).count(), 0, "collections are not in the Mathematics view");
    await page.getByTestId("graph-save-state").first().waitFor();
    await shot(page, "graph-math-dependencies-light");
    await page.getByRole("radio", { name: "Verification" }).click();
    const verification = page.getByRole("grid", { name: "Verification" });
    await verification.waitFor();
    for (const column of ["Statement", "Informal proof", "Formal association", "Verifier verdict", "Freshness", "Source"]) {
      assert.ok(await verification.getByRole("columnheader", { name: column }).count(), `verification column ${column}`);
    }
    const thmRow = verification.locator(`[data-entity-id="${ids.thm}"]`);
    assert.equal(await thmRow.getAttribute("data-verdict"), "none", "no verifier verdict without formal evidence");
    assert.match(await thmRow.innerText(), /Proof linked/);
    assert.match(await thmRow.innerText(), /No formal evidence/);
    assert.doesNotMatch(await thmRow.innerText(), /Checked/, "nothing claims Checked");
    await thmRow.click();
    const mathInspector = page.getByTestId("graph-entity-inspector");
    await mathInspector.waitFor();
    await waitFor(async () => (await page.getByTestId("graph-depends-on").count()) >= 3, { message: "dependency closure listed" });
    const dependsOn = await page.getByTestId("graph-depends-on").evaluateAll((els) => els.map((e) => e.dataset.entityId));
    for (const key of ["lem", "hyp", "def"]) assert.ok(dependsOn.includes(ids[key]), `${key} is among what the theorem depends on`);
    assert.ok(await page.getByTestId("graph-proof").and(page.locator(`[data-entity-id="${ids.prf}"]`)).count(), "the proof targeting the theorem is listed");
    assert.match(await page.getByTestId("graph-formal-section").innerText(), /No formal evidence/);
    assert.match(await page.getByTestId("graph-formal-deps-unavailable").innerText(), /Formal dependency edges unavailable/);
    // Associating a Lean declaration needs a formal declaration entity (from a Lean audit; see graph-lean-association).
    assert.equal(await page.getByTestId("graph-associate-button").isDisabled(), true, "no declaration to associate yet");
    assert.match(await page.getByTestId("graph-associate-unavailable").innerText(), /No formal declarations/);
    const notDeclaration = await invokeError(page, "lean_associate_declaration", {
      request: { declaration: { kind: "entity", entity: ids.lem }, target: ids.thm, note: null },
    });
    assert.equal(notDeclaration.error?.code, "lean.notDeclaration", `only a formal declaration can formalize (${JSON.stringify(notDeclaration)})`);
    await shot(page, "graph-math-verification-light");

    // --- 5c. Editor dependency inspector at a Monaco caret ------------------------------
    await page.keyboard.press("Control+1");
    await page.locator('[data-page="editor"]:not([hidden])').waitFor();
    await openFromTree(page, "paper.tex");
    const paper = editorLocator(page, "paper.tex");
    await paper.locator(".monaco-editor").waitFor({ timeout: 15000 });
    await runCommand(page, "Show Dependencies at Cursor", /Show Dependencies at Cursor/);
    await paper.locator(".view-lines").click();
    const thmLine = paperText.slice(0, thmStart).split("\n").length + 1; // a line inside the theorem
    await page.keyboard.press("Control+Home");
    for (let i = 1; i < thmLine; i++) await page.keyboard.press("ArrowDown");
    const deps = page.getByTestId("dependency-inspector");
    await deps.waitFor({ timeout: 10000 });
    const depsEntity = deps.locator(`[data-testid="deps-entity"][data-entity-id="${ids.thm}"]`);
    await depsEntity.waitFor({ timeout: 10000 });
    const families = await depsEntity.locator('[data-testid="deps-family"]').evaluateAll((els) => els.map((e) => e.dataset.family));
    assert.ok(families.includes("math"), `mathematical dependencies shown (${families})`);
    assert.ok(await deps.getByTestId("deps-coverage").count(), "extraction coverage stated");
    await shot(page, "graph-editor-dependencies-light");
    await depsEntity.getByRole("button", { name: "Open in Mathematics" }).click();
    await page.getByTestId("graph-mathematics").waitFor();
    await page.getByTestId("graph-entity-inspector").and(page.locator(`[data-entity-id="${ids.thm}"]`)).waitFor({ timeout: 10000 });

    // --- 6b. Dark theme screens -----------------------------------------------------------
    await runCommand(page, "color theme dark", /Color Theme: Dark/);
    await page.getByRole("radio", { name: "Dependencies" }).click();
    await page.getByTestId("graph-math-canvas").waitFor();
    await shot(page, "graph-math-dependencies-dark");
    await page.getByRole("radio", { name: "Verification" }).click();
    await shot(page, "graph-math-verification-dark");
    await page.getByRole("radio", { name: "Project" }).click();
    await canvas.waitFor();
    await node(page, ids.thm).click();
    await page.getByTestId("graph-entity-inspector").waitFor();
    await shot(page, "graph-canvas-dark");
    await connect(page, ids.def, ids.prf);
    await page.getByTestId("graph-relate-dialog").waitFor();
    await chooseOption(page, page.getByTestId("graph-relate-dialog"), "Relationship", /^is a proof of \(not listed/);
    await waitFor(async () => (await page.getByTestId("graph-proposal-review").getAttribute("data-state")) === "invalid", { message: "invalid (dark)" });
    await shot(page, "graph-relate-invalid-dark");
    await page.getByRole("button", { name: "Cancel" }).click();
    await page.getByRole("radio", { name: "Table" }).click();
    await page.getByRole("grid", { name: "Entities" }).waitFor();
    await shot(page, "graph-table-dark");
    await page.getByRole("radio", { name: "Canvas" }).click();
    await page.keyboard.press("Control+1");
    await page.getByTestId("dependency-inspector").waitFor();
    await shot(page, "graph-editor-dependencies-dark");

    // --- 6c. Narrow window: the inspector becomes a drawer --------------------------------
    await page.keyboard.press("Control+4");
    await canvas.waitFor();
    await setViewport(page, { width: 900, height: 640 });
    await node(page, ids.lem).click();
    const drawer = page.locator('[data-region="graph-inspector"][data-narrow]');
    await drawer.waitFor({ timeout: 5000 });
    await shot(page, "graph-narrow-dark");
    await runCommand(page, "color theme light", /Color Theme: Light/);
    await shot(page, "graph-narrow-light");
    await setViewport(page, WIDE);

    assert.deepEqual(consoleErrors(app), [], "no console errors in session 2");
  } finally {
    await closeGracefully(app);
  }

  const devHome = process.env.EXPOSITION_DEV_HOME;
  if (devHome) {
    const target = join(devHome, "ui-reference", "graph");
    await mkdir(target, { recursive: true });
    const names = shots.map((path) => path.split(/[\\/]/).pop());
    for (const stale of (await readdir(target)).filter((f) => f.endsWith(".png") && f.startsWith("graph-") && !f.startsWith("graph-lean-") && !names.includes(f))) await rm(join(target, stale));
    for (const [i, path] of shots.entries()) await copyFile(path, join(target, names[i]));
    console.log(`copied ${shots.length} screenshots to ${target}`);
  }
  await rm(base, { recursive: true, force: true }).catch(() => {});
}
