// Regression for the R-m4 attachment findings (#6, #11) plus hostile captions
// built through the real typesetting services:
//
// - figure + table with hostile captions attached into Typst and Markdown
//   documents build with the typesetting service, and the captions render
//   literally on the page (read back through the page map);
// - LaTeX: a document with attachments builds with pdfLaTeX, and a figure
//   attached to chapters/ch1.tex (included by the root book.tex) builds,
//   because its graphics path is relative to book.tex;
// - artifact_attach takes identifiers only: caller-supplied artifact records
//   (forged paths/kinds/hashes) and artifact ids no run produced are refused.
//
// Observations: test-results/e2e/research-attach-build/observations.json.
import { cp, mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { invoke, invokeError, waitFor } from "../workspace-helpers.mjs";

const KERNEL_TIMEOUT = 120_000;
/** Characters that inject markup in at least one of the formats. */
const CAPTION = "Cost $5 #1 *x* ] <!-- c \\ end";
const CELLS = ['#panic("CELL")', "$", "}", "{", "\\", "&", "|", "<!-- x", "% c", "a]b", "*y*"];

const LATEX = `\\documentclass{article}
\\begin{document}
Intro paragraph before the attachments.

Closing paragraph.
\\end{document}
`;

async function submit(page, sessionId, code) {
  const run = await invoke(page, "research_execute", { request: { sessionId, source: { kind: "input", code } } });
  return waitFor(
    async () => {
      const m = await invoke(page, "research_run", { runId: run.id });
      return m.record.status.kind !== "running" ? m : null;
    },
    { timeout: KERNEL_TIMEOUT, message: `run ${run.id}` },
  );
}

async function attach(page, document, runId, artifactId, placement, caption) {
  const state = await invoke(page, "document_state", { document });
  const offset =
    placement === "end"
      ? state.text.length
      : placement === "beforeClosing"
        ? state.text.indexOf("Closing paragraph.")
        : placement;
  return invokeError(page, "artifact_attach", {
    request: {
      document,
      baseRevision: state.info.revision,
      runId,
      artifactId,
      placement: { kind: "insert", offset },
      caption,
      attachmentKey: null,
    },
  });
}

async function texBuild(page, target) {
  const before = (await invoke(page, "tex_status", { target }))?.last?.result.id ?? null;
  const started = await invokeError(page, "tex_build", { target, mode: "draft" });
  if (started.error) throw new Error(`tex_build refused: ${JSON.stringify(started.error)}`);
  return waitFor(
    async () => {
      const last = (await invoke(page, "tex_status", { target }))?.last;
      return last && last.result.id !== before && last.mode === "draft" ? last : null;
    },
    { timeout: 180_000, interval: 300, message: `TeX build ${target.entry}` },
  );
}

/** UTF-16 range of `needle` inside the attachment block that starts with `marker`. */
function rangeIn(text, marker, needle) {
  const from = text.indexOf(marker);
  const start = text.indexOf(needle, from);
  if (from < 0 || start < 0) return null;
  return { start, end: start + needle.length };
}

const squash = (s) => s.replace(/\s+/g, " ").trim();

export default async function researchAttachBuild({ launchApp, artifactsDir, fixture, assert }) {
  const base = await mkdtemp(join(tmpdir(), "xp-research-attach-"));
  const root = join(base, "project");
  const dataDir = join(base, "data");
  await cp(fixture("research-python"), root, { recursive: true });
  await mkdir(join(root, "chapters"), { recursive: true });
  await mkdir(dataDir, { recursive: true });
  await writeFile(join(root, "main.tex"), LATEX);
  await writeFile(join(root, "chapters", "ch1.tex"), "\\section{Results}\nChapter text.\n");
  await writeFile(
    join(root, "book.tex"),
    "\\documentclass{article}\n\\usepackage{graphicx}\n\\begin{document}\n\\input{chapters/ch1}\n\\end{document}\n",
  );

  const observations = [];
  const note = (id, ok, detail) => {
    observations.push({ id, ok, detail });
    console.log(`${ok ? "ok    " : "FAIL  "} ${id}: ${typeof detail === "string" ? detail : JSON.stringify(detail)}`);
  };

  const app = await launchApp({ dataDir, executable: process.env.EXPOSITION_E2E_EXE || undefined });
  try {
    const { page } = app;
    const info = await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"]').waitFor({ timeout: 15_000 });
    await invoke(page, "trust_grant", { root: info.root });
    const report = await invoke(page, "research_environments", { refresh: true });
    const python = report.runtimes.find((r) => r.language === "python" && r.availability.kind === "available");
    const session = await invoke(page, "research_session_start", { request: { environmentId: python.id } });

    const figRun = await submit(
      page,
      session.id,
      '%config InlineBackend.figure_formats = ["png"]\nimport matplotlib.pyplot as plt\nplt.figure(figsize=(3, 2)); plt.plot([0, 1, 2], [0, 1, 4]); plt.show()',
    );
    const figure = figRun.record.outputs.find((a) => a.kind === "figure" && a.mediaType === "image/png");
    assert.ok(figure, "PNG figure produced");
    const tabRun = await submit(
      page,
      session.id,
      `import pandas as pd\npd.DataFrame({"name": ${JSON.stringify(CELLS)}, "value": list(range(${CELLS.length}))})`,
    );
    const table = tabRun.record.outputs.find((a) => a.kind === "table");
    assert.ok(table, "table produced");

    const docs = {};
    for (const path of ["paper.typ", "notes.md", "main.tex", "chapters/ch1.tex"]) {
      docs[path] = (await invoke(page, "document_open", { path })).state.info.id;
    }
    for (const path of ["paper.typ", "notes.md", "main.tex"]) {
      const where = path.endsWith(".tex") ? "beforeClosing" : "end";
      const f = await attach(page, docs[path], figRun.record.id, figure.id, where, CAPTION);
      const t = await attach(page, docs[path], tabRun.record.id, table.id, where, CAPTION);
      note(`attach-${path}`, !f.error && !t.error, f.error ?? t.error ?? "figure and table inserted");
    }
    const ch1 = await attach(page, docs["chapters/ch1.tex"], figRun.record.id, figure.id, "end", "Chapter figure");
    note("attach-chapters/ch1.tex", !ch1.error, ch1.error ?? ch1.unexpectedSuccess.warnings);
    const ch1Text = (await invoke(page, "document_state", { document: docs["chapters/ch1.tex"] })).text;
    note(
      "chapter-graphics-path-relative-to-main-file",
      ch1Text.includes(`\\includegraphics[width=0.8\\linewidth]{.exposition/artifacts/${figRun.record.id}/`),
      ch1Text.split("\n").find((l) => l.includes("includegraphics")) ?? "no includegraphics",
    );
    for (const [path, id] of Object.entries(docs)) {
      await invoke(page, "document_save", { document: id, overwrite: false });
      await writeFile(join(artifactsDir, path.replace(/\//g, "_") + ".txt"), await readFile(join(root, path), "utf8"));
    }

    // ---- Typst and Markdown: build, then read the captions off the page -----
    for (const path of ["paper.typ", "notes.md"]) {
      const built = await invoke(page, "typesetting_build", { entry: path });
      const errors = built.result.diagnostics.filter((d) => d.severity === "error");
      note(
        `build-${path}`,
        built.result.status.kind === "succeeded" && errors.length === 0,
        `${built.result.status.kind}; ${built.result.pages.length} pages; ${JSON.stringify(errors.map((d) => d.message)).slice(0, 300)}`,
      );
      if (built.result.status.kind !== "succeeded") continue;
      const text = (await invoke(page, "document_state", { document: docs[path] })).text;
      const escaped = path.endsWith(".typ")
        ? text.slice(text.indexOf("caption: [", text.indexOf("exposition:begin fig-")) + 10).split("],\n")[0]
        : text.slice(text.indexOf("![", text.indexOf("exposition:begin fig-")) + 2).split("](")[0];
      const figRange = rangeIn(text, "exposition:begin fig-", escaped);
      const rendered = await invoke(page, "page_selection", { build: built.result.id, path, ...figRange });
      note(`${path}-figure-caption-renders-literally`, squash(rendered.text).includes(CAPTION), rendered.text);
      if (path.endsWith(".md")) {
        const line = text.slice(text.indexOf("Table: ", text.indexOf("exposition:begin tab-"))).split("\n")[0];
        const tabRange = rangeIn(text, "exposition:begin tab-", line);
        const tabRendered = await invoke(page, "page_selection", { build: built.result.id, path, ...tabRange });
        note("notes.md-table-caption-line-renders-literally", squash(tabRendered.text) === `Table: ${CAPTION}`, tabRendered.text);
      }
    }

    // ---- LaTeX: attachments in main.tex and in an included chapter ----------
    for (const entry of ["main.tex", "book.tex"]) {
      const target = { format: "latex", engine: "pdfLatex", entry };
      let last;
      try {
        last = await texBuild(page, target);
      } catch (error) {
        note(`latex-build-${entry}`, false, error.message);
        continue;
      }
      const errors = last.result.diagnostics.filter((d) => d.severity === "error");
      note(
        `latex-build-${entry}`,
        last.result.status.kind === "succeeded" && errors.length === 0,
        `${last.result.status.kind}; ${last.result.pages.length} pages; ${JSON.stringify(errors.map((d) => d.message)).slice(0, 300)}`,
      );
    }

    // ---- #11 identifiers only ------------------------------------------------
    await writeFile(join(root, "not-from-a-run.csv"), "made,up\n1,2\n");
    const state = await invoke(page, "document_state", { document: docs["notes.md"] });
    const forgedRecord = await invokeError(page, "artifact_attach", {
      request: {
        document: docs["notes.md"],
        baseRevision: state.info.revision,
        artifact: { ...table, id: `${tabRun.record.id}/output-99.csv`, path: "not-from-a-run.csv" },
        run: tabRun.record.id,
        placement: { kind: "insert", offset: 0 },
        caption: "Forged provenance",
        attachmentKey: null,
        origin: { kind: "research", runId: tabRun.record.id },
      },
    });
    note("caller-artifact-record-refused", !!forgedRecord.error, forgedRecord.error ?? "accepted");
    for (const artifactId of [`${tabRun.record.id}/output-99.csv`, "../not-from-a-run.csv", `${figRun.record.id}/output-1.png`]) {
      const r = await attach(page, docs["notes.md"], tabRun.record.id, artifactId, 0, "Forged");
      note(`unknown-artifact-refused ${artifactId}`, r.error?.code === "research.unknownArtifact", r.error ?? "accepted");
    }
    const after = (await invoke(page, "document_state", { document: docs["notes.md"] })).text;
    note("no-forged-content-inserted", !after.includes("Forged"), "document unchanged by refused requests");
    await invoke(page, "research_session_shutdown", { sessionId: session.id });
  } finally {
    await writeFile(join(artifactsDir, "observations.json"), JSON.stringify(observations, null, 2));
    await app.close();
  }
  const failed = observations.filter((o) => !o.ok);
  assert.deepEqual(failed.map((d) => d.id), [], `violations (see ${join(artifactsDir, "observations.json")})`);
}
