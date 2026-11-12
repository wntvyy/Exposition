// LaTeX builds in the real app: the open workspace's default target (from
// .exposition/project.json) is refused until the workspace is trusted, then
// builds through the execution service (a supervised `pdflatex` job), serves
// page PNG bytes and the glyph mapping by build id, compiles an unsaved buffer
// without touching the file on disk, follows a draft with an automatic full
// build (bibliography resolved), and validates command input.
//
// Requires TeX (pdflatex + bibtex) discoverable by the execution service.
import { readFile, writeFile, mkdir, mkdtemp, cp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

const invoke = (page, cmd, args) => page.evaluate(([c, a]) => window.__TAURI_INTERNALS__.invoke(c, a), [cmd, args]);
const rejection = (page, cmd, args) =>
  page.evaluate(([c, a]) => window.__TAURI_INTERNALS__.invoke(c, a).then(() => null, (e) => e), [cmd, args]);
/** Page bytes as a plain array (the command returns an ArrayBuffer). */
const pageImage = (page, args) =>
  page.evaluate(
    (a) => window.__TAURI_INTERNALS__.invoke("tex_page_image", a).then((b) => Array.from(new Uint8Array(b))),
    args,
  );

async function waitFor(what, fn, timeoutMs = 60_000) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = await fn();
    if (value) return value;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 200));
  }
}

const PNG = [0x89, 0x50, 0x4e, 0x47];

export default async function texBuild({ launchApp, artifactsDir, fixture, assert }) {
  const app = await launchApp();
  const evidence = {};
  try {
    const { page } = app;
    // Outside the app's data folder, which is never a project.
    const project = join(await mkdtemp(join(tmpdir(), "xp-tex-")), "paper");
    await cp(fixture("latex-basic"), project, { recursive: true });
    await mkdir(join(project, ".exposition"), { recursive: true });
    await writeFile(
      join(project, ".exposition", "project.json"),
      JSON.stringify({
        version: 1,
        entry: "main.tex",
        targets: [{ name: "pdf", format: "latex", engine: "pdfLatex", entry: "main.tex" }],
        defaultTarget: "pdf",
      }),
    );
    const mainOnDisk = await readFile(join(project, "main.tex"));

    // No workspace yet: nothing to build.
    assert.equal((await rejection(page, "tex_build", { target: null, mode: "draft" }))?.code, "tex.workspaceNotOpen");
    const info = await invoke(page, "workspace_open", { path: project });
    const root = info.root;

    // --- Untrusted: the build is refused before anything runs.
    const target = await invoke(page, "tex_build", { target: null, mode: "draft" });
    assert.equal(target.engine, "pdfLatex");
    assert.equal(target.entry, "main.tex");
    assert.ok(!target.root.startsWith("\\\\?\\"), `plain workspace root: ${target.root}`);
    const refused = await waitFor("refused build", async () => (await invoke(page, "tex_status", { target: null }))?.last);
    assert.equal(refused.refusal?.code, "untrustedWorkspace");
    assert.equal(refused.refusal.workspaceRoot, target.root, "the trust prompt gets the workspace root");
    assert.equal(refused.result.status.kind, "error");
    assert.equal(refused.passes.length, 0);
    assert.ok(!(await invoke(page, "job_list")).some((j) => j.info.label.startsWith("pdflatex")), "no engine may run");
    evidence.untrusted = { refusal: refused.refusal, status: refused.result.status };

    // --- Trusted (granted as the trust dialog would): a draft build via the execution service.
    assert.equal((await invoke(page, "trust_grant", { root })).trusted, true);
    await invoke(page, "tex_build", { target: null, mode: "draft" });
    const draft = await waitFor("draft build", async () => {
      const status = await invoke(page, "tex_status", { target: null });
      const last = status?.last;
      return last && last.result.id !== refused.result.id && last.mode === "draft" ? last : null;
    });
    assert.equal(draft.result.status.kind, "succeeded", JSON.stringify(draft.result.diagnostics));
    assert.ok(draft.hasOutput);
    const engineJob = (await invoke(page, "job_list")).find((j) => j.info.label === "pdflatex main.tex");
    assert.ok(engineJob, "the engine ran as a supervised job");
    assert.equal(engineJob.info.result.outcome.kind, "exited");
    evidence.draft = { id: draft.result.id, durationMs: draft.result.durationMs, passes: draft.passes };

    // --- Pages: PNG bytes and glyph mapping by build id.
    const png = await pageImage(page, { build: draft.result.id, page: 0, scale: 2 });
    assert.deepEqual(png.slice(0, 4), PNG);
    await writeFile(join(artifactsDir, "page-1.png"), Buffer.from(png));
    const map = await invoke(page, "tex_page_map", { build: draft.result.id, page: 0 });
    assert.ok(map.stats.mapped > 600, JSON.stringify(map.stats));
    const ffi = map.glyphs.findIndex((g) => g.text === "ffi");
    assert.equal(map.targets[ffi].kind, "source");
    assert.equal(map.targets[ffi].path, "main.tex");
    evidence.mapping = map.stats;

    // --- Full build follows automatically once idle: bibliography resolved.
    const full = await waitFor("automatic full build", async () => {
      const last = (await invoke(page, "tex_status", { target: null }))?.last;
      return last?.mode === "full" && last.automatic ? last : null;
    });
    assert.equal(full.result.status.kind, "succeeded");
    assert.ok(full.passes.some((p) => p.tool === "bibtex"), JSON.stringify(full.passes));
    const fullMap = await invoke(page, "tex_page_map", { build: full.result.id, page: 0 });
    assert.ok(fullMap.glyphs.map((g) => g.text ?? "").join("").includes("[1]"), "citation resolved");
    evidence.full = { id: full.result.id, durationMs: full.result.durationMs, passes: full.passes };

    // --- An unsaved buffer is compiled; the file on disk is untouched.
    const opened = await invoke(page, "document_open", { path: "main.tex" });
    const text = opened.state.text;
    const at = text.indexOf("The first paragraph");
    await invoke(page, "document_apply", {
      request: {
        id: "e2e-tex-1",
        origin: { kind: "page" },
        edits: [
          {
            document: opened.state.info.id,
            baseRevision: opened.state.info.revision,
            patches: [{ range: { start: at, end: at }, text: "Unsaved " }],
          },
        ],
        undo: { kind: "newGroup" },
        conflict: "reject",
        label: null,
        selection: null,
      },
    });
    const stale = await invoke(page, "tex_status", { target: null });
    assert.ok(stale.stale, "the shown pages predate the edit");
    await invoke(page, "tex_build", { target: null, mode: "draft" });
    const edited = await waitFor("draft of the edit", async () => {
      const last = (await invoke(page, "tex_status", { target: null }))?.last;
      const source = last?.result.sources.find((s) => s.path === "main.tex");
      return source?.revision === opened.state.info.revision + 1 && last.result.status.kind === "succeeded"
        ? last
        : null;
    });
    const editedMap = await invoke(page, "tex_page_map", { build: edited.result.id, page: 0 });
    assert.ok(editedMap.glyphs.map((g) => g.text ?? "").join("").includes("UnsavedThe"), "the buffer was compiled");
    assert.deepEqual(await readFile(join(project, "main.tex")), mainOnDisk, "the file on disk is unchanged");

    // --- Input validation.
    assert.equal((await rejection(page, "tex_page_image", { build: "../x", page: 0, scale: 1 }))?.code, "tex.invalidBuildId");
    assert.equal(
      (await rejection(page, "tex_page_image", { build: draft.result.id, page: 0, scale: 100 }))?.code,
      "tex.invalidScale",
    );
    assert.equal((await rejection(page, "tex_page_map", { build: draft.result.id, page: 99 }))?.code, "tex.noSuchPage");
    assert.equal((await rejection(page, "tex_page_map", { build: "tex-0-999999", page: 0 }))?.code, "tex.unknownBuild");
    const typst = { format: "typst", engine: "typst", entry: "main.tex" };
    assert.equal((await rejection(page, "tex_build", { target: typst, mode: "draft" }))?.code, "tex.unsupportedTarget");
    assert.ok((await rejection(page, "tex_build", { target: { ...typst, entry: "../x.tex" }, mode: "draft" })) !== null);

    await page.screenshot({ path: join(artifactsDir, "app.png") });
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2));
  } finally {
    await app.close();
  }
}
