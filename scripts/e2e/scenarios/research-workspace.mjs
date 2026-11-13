// M7 lane B: the Research page and research in the project graph, end to end
// in the real app with a real Python kernel (trusted folder).
//
// 1. A project with no manuscript: the Research page offers "Enable Research";
//    enabling creates exactly one research space and its folders and starts
//    no code. Execution is labelled trusted native; restricted research
//    execution is shown as not available.
// 2. Data imported by explicit copy (origin recorded); a source created and
//    edited in the Research page's Monaco editor (shared document model).
// 3. An experiment (procedure + declared input) saved; its procedure run in
//    the native Python session (declared input pinned); the figure and table
//    kept (one resource identity each; CSV = dataset + artifact) with
//    service-owned computational evidence; provenance and graph dependencies
//    inspected (UI + IPC); change impact from the data file.
// 4. Promotion into Typst (UI review: exact markup, revision, digest), Markdown
//    and LaTeX (IPC): stale approval refused, altered artifact refused, undo/redo
//    followed by the graph, an injected graph-write failure leaves a visible
//    pending record that reconciliation resolves without touching the source.
//
// Observations: test-results/e2e/research-workspace/observations.json.
import { cp, mkdir, mkdtemp, readFile, readdir, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";
import { invoke, invokeError, waitFor } from "../workspace-helpers.mjs";

const KERNEL_TIMEOUT = 180_000;
const OBS = "x,y\n0,1\n1,3\n2,5\n3,7\n";

const script = (space) => `# slope of the observations
import pandas as pd
import matplotlib.pyplot as plt
from IPython import get_ipython
from IPython.display import display
get_ipython().run_line_magic("config", "InlineBackend.figure_formats = ['png']")
df = pd.read_csv("${space}/data/obs.csv")
slope = (df.y.iloc[-1] - df.y.iloc[0]) / (df.x.iloc[-1] - df.x.iloc[0])
print("slope", slope)
plt.figure(figsize=(3, 2)); plt.plot(df.x, df.y, "o-"); plt.title("observations"); plt.show()
display(df.describe().round(3))
df.assign(y2=df.y * 2).to_csv("${space}/data/obs_scaled.csv", index=False)
`;

const TYPST = "= Results\n\nThe slope is estimated below.\n";
const MARKDOWN = "# Results\n\nThe slope is estimated below.\n";
const LATEX = "\\documentclass{article}\n\\usepackage{graphicx}\n\\usepackage{booktabs}\n\\begin{document}\nResults.\n\\end{document}\n";

export default async function researchWorkspace({ launchApp, artifactsDir, assert }) {
  const base = await mkdtemp(join(tmpdir(), "xp-research-ws-"));
  const root = join(base, "project");
  const dataDir = join(base, "data");
  const outside = join(base, "outside");
  await mkdir(root, { recursive: true });
  await mkdir(dataDir, { recursive: true });
  await mkdir(outside, { recursive: true });
  await writeFile(join(outside, "obs.csv"), OBS);
  await writeFile(join(root, "README.txt"), "A research-only project.\n");

  const observations = [];
  const note = (id, ok, detail) => {
    observations.push({ id, ok, detail });
    console.log(`${ok ? "ok    " : "FAIL  "} ${id}: ${typeof detail === "string" ? detail : JSON.stringify(detail)}`);
  };
  const shots = [];
  const app = await launchApp({ dataDir });
  const { page } = app;
  const rp = page.locator('[data-page="research"]');
  const shot = async (name) => {
    const dismiss = page.getByRole("button", { name: /^Dismiss notification/ });
    for (let i = 0; i < 10 && (await dismiss.count()) > 0; i++) await dismiss.first().click().catch(() => {});
    await page.waitForTimeout(250);
    const path = join(artifactsDir, `${name}.png`);
    await page.screenshot({ path });
    shots.push(path);
  };
  try {
    await page.setViewportSize({ width: 1600, height: 1000 }).catch(() => {});
    const info = await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"]').waitFor({ timeout: 15_000 });
    await invoke(page, "trust_grant", { root: info.root });

    // ---- 1. Enable Research without a manuscript; nothing runs -------------
    await page.getByRole("toolbar", { name: "Pages" }).getByRole("button", { name: "Research", exact: true }).click();
    await rp.locator('[data-testid="research-page"][data-state="disabled"]').waitFor({ timeout: 15_000 });
    const mode = await rp.getByTestId("research-execution-mode").first().getAttribute("aria-label");
    note("execution-labelled-trusted-native", /Trusted native/.test(mode) && /privileges/.test(mode) && /only for workflow Python batch steps/.test(mode), mode);
    const restricted = rp.locator('[data-testid="research-execution-modes"] [data-mode="restricted"]');
    note("restricted-research-not-available-without-image", (await restricted.getAttribute("data-available")) === "false" && /declared inputs/.test(await restricted.innerText()), await restricted.innerText());
    await shot("research-page-disabled-light");
    const sessionsBefore = await invoke(page, "research_sessions");
    await rp.getByRole("button", { name: "Enable Research" }).click();
    await rp.locator('[data-testid="research-page"][data-state="enabled"]').waitFor({ timeout: 15_000 });
    const space = await invoke(page, "research_space");
    const spaceDir = join(root, ...space.root.split("/"));
    const folders = await readdir(spaceDir);
    note("one-space-with-folders", ["data", "notes", "sources"].every((f) => folders.includes(f)), { root: space.root, folders });
    await rp.getByRole("button", { name: "Enable Research" }).waitFor({ state: "detached" });
    const again = await invoke(page, "research_space_enable");
    note("enable-is-idempotent", again.entity === space.entity, again.entity);
    const snapshot = await invoke(page, "graph_snapshot", {
      query: { kinds: ["researchSpace"], text: null, around: null, includeInactive: true, limit: 10 },
    });
    note("exactly-one-research-space-entity", snapshot.entities.length === 1, snapshot.entities.map((e) => e.key));
    const sessionsAfter = await invoke(page, "research_sessions");
    note(
      "enabling-launches-nothing",
      sessionsBefore.length === 0 && sessionsAfter.length === 0 && !existsSync(join(root, ".exposition", "runs")),
      { sessions: sessionsAfter.length },
    );
    const manuscripts = await invoke(page, "graph_snapshot", {
      query: { kinds: ["resource"], text: null, around: null, includeInactive: false, limit: 100 },
    });
    note("no-manuscript-in-project", !manuscripts.entities.some((e) => e.roles.includes("manuscript")), manuscripts.entities.map((e) => e.key));

    // ---- 2. Data import (copy, origin recorded) and a new source ----------
    const imported = await invoke(page, "research_space_import", {
      request: { group: "data", source: join(outside, "obs.csv"), mode: "copy" },
    });
    const dataPath = imported.file.path;
    note("data-copied-with-origin", imported.kind === "copied" && imported.file.origin?.contentHash?.length === 64, imported.file);
    await rp.locator(`[data-testid="research-tree"] [data-path="${dataPath}"]`).waitFor({ timeout: 10_000 });
    await rp.getByRole("button", { name: "New research file…" }).click();
    const dialog = page.getByRole("dialog", { name: "New research file" });
    await dialog.getByRole("textbox", { name: "File name" }).fill("fit.py");
    await dialog.getByRole("button", { name: "Create" }).click();
    const sourcePath = `${space.root}/sources/fit.py`;
    const editor = rp.locator(`[data-testid="source-editor"][data-path="${sourcePath}"]`);
    await editor.locator(".monaco-editor").waitFor({ timeout: 15_000 });
    const sourceDoc = await waitFor(() => editor.getAttribute("data-document-id"), { message: "source document id" });
    // Type into the Research page's Monaco editor, then complete the script
    // through the same document (one model, one revision history).
    await editor.locator(".view-lines").click();
    await page.keyboard.type("# typed in the Research page\n");
    await waitFor(async () => (await invoke(page, "document_state", { document: sourceDoc })).text.startsWith("# typed"), {
      message: "typed text reaches the document service",
    });
    const state0 = await invoke(page, "document_state", { document: sourceDoc });
    await invoke(page, "document_apply", {
      request: {
        id: `script-${Date.now()}`,
        origin: { kind: "source" },
        edits: [{ document: sourceDoc, baseRevision: state0.info.revision, patches: [{ range: { start: state0.text.length, end: state0.text.length }, text: script(space.root) }] }],
        undo: { kind: "newGroup" },
        conflict: "reject",
        label: null,
        selection: null,
      },
    });
    await waitFor(async () => (await editor.locator(".view-lines").innerText()).includes("read_csv"), { message: "Monaco shows the script" });
    await invoke(page, "document_save", { document: sourceDoc, overwrite: false });
    const onDisk = await readFile(join(root, ...sourcePath.split("/")), "utf8");
    note("source-edited-in-research-page", onDisk.startsWith("# typed in the Research page") && onDisk.includes("read_csv"), onDisk.split("\n")[0]);

    // ---- 3. Experiment, native run, retention, provenance -----------------
    await rp.getByRole("button", { name: "New experiment" }).click();
    const inspector = rp.locator('[data-testid="research-inspector"][data-kind="experiment"]');
    await inspector.waitFor();
    await inspector.getByRole("textbox", { name: "Title" }).fill("Slope fit");
    await inspector.getByRole("textbox", { name: "Objective" }).fill("Estimate the slope of the observations");
    await inspector.getByRole("button", { name: /Procedure/ }).click();
    await page.getByRole("option", { name: "fit.py" }).click();
    await inspector.locator("label.ui-checkbox", { hasText: "obs.csv" }).click();
    await inspector.getByRole("button", { name: "Save experiment" }).click();
    const saved = rp.locator('[data-testid="research-inspector"][data-kind="experiment"]:not([data-entity="new"])');
    await saved.waitFor({ timeout: 15_000 });
    const experimentId = await saved.getAttribute("data-entity");
    note("experiment-saved", Boolean(experimentId), experimentId);

    await rp.getByRole("button", { name: "Start session" }).click();
    await rp.locator('[data-testid="research-session-status"][data-state="idle"]').waitFor({ timeout: KERNEL_TIMEOUT });
    await saved.getByRole("button", { name: "Run procedure" }).click();
    const output = rp.getByTestId("research-output");
    await rp.locator('[data-testid="research-output"][data-status="succeeded"]').waitFor({ timeout: KERNEL_TIMEOUT });
    const runId = await output.getAttribute("data-run-id");
    const manifest = await invoke(page, "research_run", { runId });
    const inputs = manifest.record.inputs.map((i) => i.path);
    note("run-pins-procedure-and-declared-input", inputs.includes(sourcePath) && inputs.includes(dataPath), inputs);
    const stream = await output.getByTestId("research-stream").first().innerText();
    note("native-run-computed", stream.includes("slope 2.0"), stream.trim());
    await output.getByTestId("research-figure").waitFor({ timeout: 30_000 });
    await output.getByTestId("research-table").waitFor({ timeout: 30_000 });
    await shot("research-page-run-light");

    await output.getByTestId("research-figure").getByTestId("research-retain").getByRole("button", { name: "Keep" }).click();
    await output.locator('[data-testid="research-figure"] [data-testid="research-retain"][data-state="active"]').waitFor({ timeout: 15_000 });
    await output.getByTestId("research-table").getByTestId("research-retain").getByRole("button", { name: "Keep" }).click();
    await output.locator('[data-testid="research-table"] [data-testid="research-retain"][data-state="active"]').waitFor({ timeout: 15_000 });
    const listing = await invoke(page, "research_listing");
    const figure = listing.results.find((r) => r.kind === "figure");
    const table = listing.results.find((r) => r.kind === "table");
    note("figure-retained-artifact-role", figure && figure.roles.join("+") === "artifact", figure?.roles);
    note("table-one-identity-dataset-and-artifact", table && table.roles.join("+") === "dataset+artifact", table?.roles);
    const scaled = listing.data.find((f) => f.name === "obs_scaled.csv");
    note("dataset-transformed-through-code", Boolean(scaled?.entity) && scaled.roles.includes("dataset"), scaled ?? listing.data.map((f) => f.name));

    // Outcomes of the experiment (through the experiment editor).
    await rp.locator(`[data-testid="research-tree"] [data-entity="${experimentId}"]`).first().click();
    const exp = rp.locator(`[data-testid="research-inspector"][data-kind="experiment"][data-entity="${experimentId}"]`);
    await exp.locator("label.ui-checkbox", { hasText: figure.label }).click();
    await exp.locator("label.ui-checkbox", { hasText: table.label }).click();
    await exp.getByRole("button", { name: "Save experiment" }).click();
    const detail = await waitFor(
      async () => {
        const d = await invoke(page, "research_experiment", { entity: experimentId });
        return d.outcomes.length === 2 ? d : null;
      },
      { timeout: 15_000, message: "experiment outcomes saved" },
    );
    note(
      "experiment-links-procedure-inputs-outcomes-attempts",
      detail.procedure?.path === sourcePath && detail.inputs.some((i) => i.path === dataPath) && detail.attempts.some((a) => a.id === runId),
      { procedure: detail.procedure?.path, inputs: detail.inputs.map((i) => i.path), attempts: detail.attempts.map((a) => a.id) },
    );

    // Provenance and graph dependencies of the kept figure.
    const figDetail = await invoke(page, "graph_entity", { entity: figure.entity });
    const evidence = figDetail.evidence.find((e) => e.kind === "computational");
    const neighbor = (id) => figDetail.neighbors.find((n) => n.id === id);
    const derived = figDetail.outgoing.filter((r) => r.kind === "derivedFrom").map((r) => neighbor(r.to)?.key ?? neighbor(r.to)?.label);
    note(
      "computational-evidence-service-owned",
      evidence?.recordedBy?.kind === "service" && evidence.operation.id === runId && evidence.inputs.some((i) => i.path === dataPath && i.contentHash?.length === 64),
      { operation: evidence?.operation, inputs: evidence?.inputs, outputs: evidence?.outputs },
    );
    note("figure-derived-from-script-data-experiment", derived.includes(sourcePath) && derived.includes(dataPath) && derived.includes("Slope fit"), derived);
    const forged = await invokeError(page, "graph_propose_change", {
      request: {
        baseRevision: 0,
        operations: [
          {
            op: "addRelation",
            kind: "evidenceFor",
            from: { kind: "id", id: figure.entity },
            to: { kind: "id", id: experimentId },
            evidenceKind: "computational",
            evidence: [],
            note: null,
          },
        ],
        summary: null,
      },
    });
    const forgedValid = forged.unexpectedSuccess?.valid;
    note("people-cannot-mint-computational-evidence", forgedValid === false, forged.unexpectedSuccess?.diagnostics ?? forged.error);

    await rp.locator(`[data-testid="research-tree"] [data-entity="${figure.entity}"]`).first().click();
    const resultInspector = rp.locator(`[data-testid="research-inspector"][data-kind="result"][data-entity="${figure.entity}"]`);
    await resultInspector.getByTestId("graph-dependencies").waitFor({ timeout: 15_000 });
    const relationRows = await resultInspector.getByTestId("graph-relation").allInnerTexts();
    const evidenceRows = await resultInspector.locator('[data-testid="graph-evidence"][data-kind="computational"]').count();
    note("inspector-shows-relations-and-evidence", relationRows.some((t) => t.includes("is derived from")) && evidenceRows === 1, relationRows);
    await shot("research-page-result-inspector-light");

    // Impact from the data file reaches the experiment and its outcomes.
    const dataEntity = listing.data.find((f) => f.path === dataPath).entity;
    const impact = await invoke(page, "graph_impact", { request: { entities: [dataEntity], maxDepth: 4, maxItems: 100 } });
    const affected = impact.items.map((i) => i.entity.id);
    note("impact-from-data", affected.includes(experimentId) && affected.includes(figure.entity), impact.items.map((i) => `${i.entity.label}: ${i.reason}`));

    // ---- 4. Promotion into manuscripts (Typst via UI review) --------------
    await writeFile(join(root, "paper.typ"), TYPST);
    await writeFile(join(root, "notes.md"), MARKDOWN);
    await writeFile(join(root, "main.tex"), LATEX);
    const doc = {};
    for (const p of ["paper.typ", "notes.md", "main.tex"]) doc[p] = (await invoke(page, "document_open", { path: p })).state.info.id;
    const textOf = async (p) => (await invoke(page, "document_state", { document: doc[p] })).text;

    const promote = resultInspector.getByRole("button", { name: "Insert or update in document…" });
    await promote.click();
    const group = resultInspector.getByTestId("research-attach");
    const docSelect = group.getByRole("button", { name: /Document/ }).first();
    if (!(await docSelect.innerText()).includes("paper.typ")) {
      await docSelect.click();
      await page.getByRole("option", { name: "paper.typ", exact: true }).click();
    }
    await group.getByRole("textbox", { name: "Caption" }).fill("Observations and fitted slope");
    await group.getByRole("button", { name: "Insert into document" }).click();
    const review = group.getByTestId("research-promotion-review");
    await review.waitFor({ timeout: 15_000 });
    const markup = await review.getByTestId("research-promotion-markup").first().innerText();
    const reviewedRevision = Number(await review.getAttribute("data-revision"));
    const digest = await review.getByTestId("research-promotion-digest").innerText();
    note(
      "review-shows-markup-revision-digest",
      markup.includes("// exposition:begin fig-") && reviewedRevision >= 1 && digest.includes(figure.contentHash),
      { revision: reviewedRevision, digest, markup: markup.split("\n").slice(0, 3) },
    );
    await shot("research-promotion-review-light");

    // Stale approval: the document changes after the review.
    const before = await invoke(page, "document_state", { document: doc["paper.typ"] });
    await invoke(page, "document_apply", {
      request: {
        id: `user-${Date.now()}`,
        origin: { kind: "source" },
        edits: [{ document: doc["paper.typ"], baseRevision: before.info.revision, patches: [{ range: { start: 0, end: 0 }, text: "// draft\n" }] }],
        undo: { kind: "newGroup" },
        conflict: "reject",
        label: null,
        selection: null,
      },
    });
    const userEdited = await textOf("paper.typ");
    await group.getByRole("button", { name: "Approve insertion" }).click();
    await group.locator('[data-testid="research-promotion-error"][data-code="promotion.stale"]').waitFor({ timeout: 15_000 });
    note("stale-approval-refused-source-untouched", (await textOf("paper.typ")) === userEdited, "promotion.stale");
    await shot("research-promotion-stale-light");
    await group.getByRole("button", { name: "Review again" }).click();
    await review.waitFor({ timeout: 15_000 });
    await group.getByRole("button", { name: "Approve insertion" }).click();
    await review.waitFor({ state: "detached", timeout: 15_000 });
    const withFigure = await waitFor(
      async () => {
        const t = await textOf("paper.typ");
        return t.includes("// exposition:begin fig-") ? t : null;
      },
      { message: "figure inserted into paper.typ" },
    );
    const key = /\/\/ exposition:begin (\S+)/.exec(withFigure)[1];
    const usesAsset = async () => {
      const d = await invoke(page, "graph_entity", { entity: figure.entity });
      return d.incoming.some((r) => r.kind === "usesAsset");
    };
    note("promotion-applied-once-and-recorded", withFigure.split(`exposition:begin ${key}`).length === 2 && (await usesAsset()), key);

    // Undo removes the use; redo restores it.
    await invoke(page, "document_undo", { document: doc["paper.typ"], origin: { kind: "source" } });
    await waitFor(async () => !(await textOf("paper.typ")).includes(key), { message: "undo removed the attachment" });
    await waitFor(async () => !(await usesAsset()), { timeout: 10_000, message: "graph follows the undo" });
    note("undo-updates-graph", true, "usesAsset removed after undo");
    await invoke(page, "document_redo", { document: doc["paper.typ"], origin: { kind: "source" } });
    await waitFor(async () => (await textOf("paper.typ")).includes(key), { message: "redo restored the attachment" });
    await waitFor(usesAsset, { timeout: 10_000, message: "graph follows the redo" });
    note("redo-updates-graph", true, "usesAsset restored after redo");

    // ---- Markdown (IPC): altered artifact refused, then promoted ----------
    const figureRecord = manifest.record.outputs.find((a) => a.kind === "figure");
    const md = await invoke(page, "document_state", { document: doc["notes.md"] });
    const prepared = await invoke(page, "research_promotion_prepare", {
      request: {
        runId,
        artifactId: figureRecord.id,
        document: doc["notes.md"],
        baseRevision: md.info.revision,
        placement: { kind: "insert", offset: md.text.length },
        caption: "Observations",
        attachmentKey: null,
      },
    });
    const figureFile = join(root, ...figureRecord.path.split("/"));
    const original = await readFile(figureFile);
    await writeFile(figureFile, Buffer.concat([original, Buffer.from("tampered")]));
    const altered = await invokeError(page, "research_promotion_approve", { promotion: prepared.promotion });
    note("altered-artifact-refused-source-untouched", altered.error?.code === "research.artifactChanged" && (await textOf("notes.md")) === MARKDOWN, altered.error);
    await writeFile(figureFile, original);
    const md2 = await invoke(page, "document_state", { document: doc["notes.md"] });
    const prepared2 = await invoke(page, "research_promotion_prepare", {
      request: {
        runId,
        artifactId: figureRecord.id,
        document: doc["notes.md"],
        baseRevision: md2.info.revision,
        placement: { kind: "insert", offset: md2.text.length },
        caption: "Observations",
        attachmentKey: null,
      },
    });
    const mdOutcome = await invoke(page, "research_promotion_approve", { promotion: prepared2.promotion });
    note("markdown-promotion-recorded", mdOutcome.graph.kind === "recorded" && (await textOf("notes.md")).includes("<!-- exposition:begin fig-"), mdOutcome.graph);

    // ---- LaTeX (UI): injected graph-write failure → pending record -------
    const tableRecord = manifest.record.outputs.find((a) => a.kind === "table");
    await invoke(page, "research_dev_fail_next_graph_write");
    await rp.locator(`[data-testid="research-tree"] [data-entity="${table.entity}"]`).first().click();
    const tableInspector = rp.locator(`[data-testid="research-inspector"][data-kind="result"][data-entity="${table.entity}"]`);
    await tableInspector.getByRole("button", { name: "Insert or update in document…" }).click();
    const tgroup = tableInspector.getByTestId("research-attach");
    const tdoc = tgroup.getByRole("button", { name: /Document/ }).first();
    await tdoc.waitFor();
    if (!(await tdoc.innerText()).includes("main.tex")) {
      await tdoc.click();
      await page.getByRole("option", { name: "main.tex", exact: true }).click();
    }
    await tgroup.getByRole("button", { name: "Insert into document" }).click();
    await tgroup.getByTestId("research-promotion-review").waitFor({ timeout: 15_000 });
    await tgroup.getByRole("button", { name: "Approve insertion" }).click();
    const pending = tgroup.getByTestId("research-promotion-pending");
    await pending.waitFor({ timeout: 15_000 });
    const operation = await pending.getAttribute("data-operation");
    const latex = await textOf("main.tex");
    const tableUsed = async () => {
      const d = await invoke(page, "graph_entity", { entity: table.entity });
      return d.incoming.some((r) => r.kind === "usesAsset");
    };
    const ops = await invoke(page, "graph_operations");
    const record = ops.find((o) => o.id === operation);
    note(
      "graph-failure-leaves-visible-pending-record",
      latex.includes("% exposition:begin tab-") && !(await tableUsed()) && record?.state.kind === "needsReconciliation",
      { state: record?.state, steps: record?.steps.map((s) => `${s.name}:${s.completed}`) },
    );
    await rp.getByTestId("research-pending-promotions").waitFor({ timeout: 10_000 });
    await shot("research-promotion-pending-light");
    await pending.getByRole("button", { name: "Reconcile now" }).click();
    await waitFor(
      async () => (await tableUsed()) && !(await invoke(page, "graph_operations")).some((o) => o.id === operation),
      { timeout: 15_000, message: "reconciliation records the attachment and resolves the operation" },
    );
    const opsAfter = await invoke(page, "graph_operations");
    note(
      "reconciliation-records-without-reapplying",
      !opsAfter.some((o) => o.id === operation) && (await textOf("main.tex")) === latex,
      "resolved; main.tex unchanged",
    );
    await rp.getByTestId("research-pending-promotions").waitFor({ state: "detached", timeout: 10_000 });

    // Discard never removes an attachment.
    await invoke(page, "research_result_state", { request: { entity: figure.entity, state: "discarded" } });
    note("discard-keeps-attachment", (await textOf("paper.typ")).includes(key) && (await usesAsset()), "paper.typ still uses the figure");
    await invoke(page, "research_result_state", { request: { entity: figure.entity, state: "active" } });

    // Files explorer: the Research subsection opens the Research page at an item.
    await page.getByRole("toolbar", { name: "Pages" }).getByRole("button", { name: "Editor", exact: true }).click();
    const filesResearch = page.locator('[data-page="editor"] [data-testid="files-research"][data-enabled="true"]');
    await filesResearch.waitFor({ timeout: 10_000 });
    const sourcesGroup = filesResearch.locator('[data-research-group="sources"]');
    await sourcesGroup.locator(".ui-tree-item__chevron").first().click();
    await filesResearch.locator(`[data-research-item="file:${sourcePath}"]`).locator(".ui-tree-item__title").click();
    await rp.locator(`[data-testid="research-inspector"][data-kind="file"][data-path="${sourcePath}"]`).waitFor({ timeout: 10_000 });
    note("files-explorer-research-subsection-opens-page-at-item", await rp.isVisible(), await filesResearch.innerText());

    // Dark theme reference screen of the page.
    await page.getByRole("toolbar", { name: "Pages" }).getByRole("button", { name: "Research", exact: true }).click();
    await runCommand(page, "color theme dark", /Color Theme: Dark/);
    await page.waitForTimeout(300);
    await shot("research-page-result-inspector-dark");
    await runCommand(page, "color theme light", /Color Theme: Light/);

    const errors = app.consoleMessages.filter((m) => m.startsWith("error") || m.startsWith("pageerror"));
    note("no-console-errors", errors.length === 0, errors.slice(0, 5));
    await invoke(page, "research_session_shutdown", { sessionId: (await invoke(page, "research_sessions"))[0].id });
  } finally {
    await writeFile(join(artifactsDir, "observations.json"), JSON.stringify(observations, null, 2));
    if (process.env.EXPOSITION_DEV_HOME && shots.length) {
      const dir = join(process.env.EXPOSITION_DEV_HOME, "ui-reference", "research");
      await mkdir(dir, { recursive: true });
      for (const shotPath of shots) await cp(shotPath, join(dir, shotPath.split(/[\\/]/).pop()));
    }
    await app.close();
  }
  const failed = observations.filter((o) => !o.ok);
  assert.deepEqual(failed.map((d) => d.id), [], `violations (see ${join(artifactsDir, "observations.json")})`);
}
