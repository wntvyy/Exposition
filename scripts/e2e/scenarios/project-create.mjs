// Project creation end to end in the real app (debug build): presets and an
// agent-described objective, without ever opening the Graph page.
//
// Covers (spec §11 items 12, 6, and the draft part of 11):
// A. Mathematical paper (Typst) from the New Project page: preview, location
//    check, create; the manuscript opens in the Editor; the graph has the
//    resource, a Claim bound to the paper, accepted typed relations, and a
//    collection; nothing is trusted, no job or research session started.
// B. Research-only project (Blank research project): research space,
//    question, notes; no manuscript outside the research space; fresh ids.
// C. Refusals: an invalid blueprint (traversal path, `trust` field), a
//    too-broad destination, the application's data folder, a non-empty
//    folder — nothing is written.
// D. Interrupted creation: the app is killed (debug-only simulated crash)
//    after two files; after a restart the project reports what was created
//    and what is missing, is never complete, and Finish Setup completes it.
// E. Workflow drafts: an incomplete draft from the palette is saved with
//    diagnostics and is never runnable; forged formal evidence and hostile
//    imports are refused; an imported draft is not runnable.
// F. Objective path with the scripted development provider: an empty
//    project is created and opened and a whole-project agent task starts
//    there (no commands), visible in the Agents view.
// Screenshots go to test-results/e2e/project-create/ and, when
// EXPOSITION_DEV_HOME is set, to $EXPOSITION_DEV_HOME/ui-reference/projects/.
import { existsSync } from "node:fs";
import { copyFile, mkdir, mkdtemp, readFile, readdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";
import { invoke, invokeError, waitFor } from "../workspace-helpers.mjs";

function consoleErrors(app) {
  return app.consoleMessages.filter((m) => m.startsWith("error:") || m.startsWith("pageerror:"));
}

async function setTheme(page, theme) {
  await runCommand(page, `color theme ${theme}`, theme === "dark" ? /Color Theme: Dark/ : /Color Theme: Light/);
  await page.waitForFunction((t) => document.documentElement.dataset.theme === t, theme);
  await page.waitForTimeout(200);
}

async function selectOption(page, scope, option) {
  await scope.locator("button").first().click();
  await page.getByRole("option", { name: option, exact: true }).click();
}

const graphPageMounted = (page) => page.evaluate(() => document.querySelector('[data-page="graph"]') !== null);

async function entitiesOf(page, kinds) {
  const snap = await invoke(page, "graph_snapshot", {
    query: { kinds, text: null, around: null, includeInactive: false, limit: 500 },
  });
  return snap;
}

async function listTree(dir, prefix = "") {
  const out = [];
  for (const e of await readdir(dir, { withFileTypes: true })) {
    const rel = prefix ? `${prefix}/${e.name}` : e.name;
    out.push(rel + (e.isDirectory() ? "/" : ""));
    if (e.isDirectory()) out.push(...(await listTree(join(dir, e.name), rel)));
  }
  return out.sort();
}

export default async function projectCreate({ launchApp, artifactsDir, assert }) {
  const base = await mkdtemp(join(tmpdir(), "xp-proj-"));
  const parent = join(base, "projects");
  const dataDir = join(base, "data");
  await mkdir(parent, { recursive: true });
  await mkdir(dataDir, { recursive: true });
  const env = { EXPOSITION_CREDENTIAL_NAMESPACE: `e2e-proj-${Date.now()}` };
  const evidence = { base, steps: [] };
  const step = (name, data = {}) => {
    evidence.steps.push({ name, ...data });
    console.log(`  ok  ${name}`);
  };
  const devHome = process.env.EXPOSITION_DEV_HOME;
  const shot = async (page, name) => {
    const path = join(artifactsDir, `${name}.png`);
    await page.screenshot({ path });
    if (devHome) {
      const dir = join(devHome, "ui-reference", "projects");
      await mkdir(dir, { recursive: true });
      await copyFile(path, join(dir, `${name}.png`));
    }
  };

  let app = await launchApp({ dataDir, env });
  const info = await app.page.evaluate(() => window.__TAURI_INTERNALS__.invoke("app_info"));
  assert.ok(info.debugBuild, "project-create requires a debug build (simulated crash, scripted provider)");
  try {
    const { page } = app;
    await page.setViewportSize({ width: 1440, height: 900 });
    await setTheme(page, "light");

    // ---------------------------------------------------------------- A
    await page.getByTestId("files-new-project").waitFor({ timeout: 15_000 });
    step("the no-folder Files view offers New Project…");
    await runCommand(page, "File: New Project", /File: ?New Project…$/);
    const view = page.getByTestId("new-project");
    await view.waitFor();
    await page.getByTestId("project-title").locator("input").fill("Spectral gaps");
    const paperRoot = join(parent, "spectral-gaps");
    await page.getByTestId("project-location").locator("input").fill(paperRoot);
    await page.waitForSelector('[data-testid="project-destination-status"][data-ok="true"]');
    await page.waitForSelector('[data-testid="project-preview"][data-valid="true"]');
    const previewFiles = await page.getByTestId("project-preview-files").textContent();
    assert.match(previewFiles, /main\.typ/);
    assert.match(await page.getByTestId("project-preview-entities").textContent(), /Main result/);
    await shot(page, "new-project-preset-light");
    await setTheme(page, "dark");
    await shot(page, "new-project-preset-dark");
    await setTheme(page, "light");
    await page.getByTestId("project-create").click();
    await page.locator('[data-testid="file-tree"] [data-path="main.typ"]').waitFor({ timeout: 30_000 });
    // The manuscript opens in the Editor (never a graph setup screen).
    await page.waitForFunction(() => document.querySelector('[data-page="editor"]')?.hidden === false);
    await waitFor(
      async () => (await page.locator('[role="tab"][aria-selected="true"]').allTextContents()).some((t) => t.includes("main.typ")),
      { message: "main.typ tab active" },
    );
    assert.equal(await graphPageMounted(page), false, "the Graph page was never opened");
    await shot(page, "paper-created-light");
    const ws = await invoke(page, "workspace_current");
    assert.equal(ws.name, "spectral-gaps");
    const typ = await readFile(join(paperRoot, "main.typ"), "utf8");
    assert.match(typ, /= Spectral gaps/);
    assert.ok(existsSync(join(paperRoot, "figures")), "figures folder");
    const settings = JSON.parse(await readFile(join(paperRoot, ".exposition", "project.json"), "utf8"));
    assert.equal(settings.targets[0].engine, "typst");
    const status = await invoke(page, "graph_status");
    assert.equal(status.state.kind, "attached");
    const paperProject = status.project;
    const snapA = await entitiesOf(page, ["claim", "definition", "assumption", "collection", "resource"]);
    const claim = snapA.entities.find((e) => e.kind === "claim" && e.label === "Main result");
    assert.ok(claim, "a Claim for the main result");
    const detail = await invoke(page, "graph_entity", { entity: claim.id });
    assert.equal(detail.bindings[0].path, "main.typ", "the claim is bound to the paper");
    const depends = detail.outgoing.find((r) => r.kind === "dependsOn");
    assert.equal(depends.status, "accepted");
    assert.equal(depends.origin.kind, "preset");
    assert.ok(detail.outgoing.some((r) => r.kind === "assumes"), "assumes the standing assumptions");
    assert.ok(snapA.entities.some((e) => e.kind === "collection" && e.label === "Manuscript"));
    const resource = snapA.entities.find((e) => e.kind === "resource" && e.key === "main.typ");
    assert.deepEqual(resource.roles, ["manuscript"]);
    // Nothing trusted, run, or started.
    const trust = await invoke(page, "trust_status", { root: ws.root });
    assert.equal(trust.trusted, false, "creating a project grants no trust");
    const jobs = await invoke(page, "job_list");
    assert.ok(!jobs.some((j) => j.workingDirectory.toLowerCase().includes("spectral-gaps")), "no job ran in the project");
    assert.deepEqual(await invoke(page, "research_sessions"), [], "no research session started");
    assert.deepEqual(await invoke(page, "project_creation_status"), [], "creation completed");
    step("A: paper preset created and opened in the Editor", {
      project: paperProject,
      entities: snapA.entities.length,
      claim: claim.id,
      tree: await listTree(paperRoot),
    });

    // ---------------------------------------------------------------- B
    await runCommand(page, "File: New Project", /File: ?New Project…$/);
    await view.waitFor();
    await selectOption(page, page.getByTestId("preset-select"), "Blank research project");
    await page.getByTestId("project-title").locator("input").fill("Graph spectra notes");
    const researchRoot = join(parent, "graph-spectra-notes");
    await page.getByTestId("project-location").locator("input").fill(researchRoot);
    await page.waitForSelector('[data-testid="project-destination-status"][data-ok="true"]');
    await waitFor(async () => /Research space/.test(await page.getByTestId("project-preview-entities").textContent()), {
      message: "research preview",
    });
    await page.waitForSelector('[data-testid="project-preview"][data-valid="true"]');
    await page.getByTestId("project-create").click();
    await waitFor(async () => (await invoke(page, "workspace_current"))?.name === "graph-spectra-notes", {
      timeout: 30_000,
      message: "research project open",
    });
    // The Research page when it exists; otherwise the notes open in the Editor.
    await waitFor(
      async () =>
        page.evaluate(
          () =>
            document.querySelector('[data-page="research"]')?.hidden === false ||
            [...document.querySelectorAll('[role="tab"][aria-selected="true"]')].some((t) => t.textContent.includes("notes.md")),
        ),
      { message: "research page or research notes shown" },
    );
    assert.equal(await graphPageMounted(page), false, "the Graph page was never opened");
    const snapB = await entitiesOf(page, ["researchSpace", "question", "resource", "claim"]);
    const space = snapB.entities.find((e) => e.kind === "researchSpace");
    assert.ok(space && /^\.exposition\/research\/rs-[a-z0-9]+$/.test(space.key), "a research space");
    assert.ok(snapB.entities.some((e) => e.kind === "question"), "a research question");
    assert.ok(
      snapB.entities.every((e) => e.kind !== "resource" || !e.roles.includes("manuscript") || e.key.startsWith(".exposition/research/")),
      "no manuscript: research-only",
    );
    assert.ok(!snapB.entities.some((e) => e.kind === "claim"), "no paper claims");
    const statusB = await invoke(page, "graph_status");
    assert.notEqual(statusB.project, paperProject, "a fresh project id");
    assert.ok(!snapB.entities.some((e) => snapA.entities.some((a) => a.id === e.id)), "fresh entity ids");
    assert.equal((await invoke(page, "trust_status", { root: (await invoke(page, "workspace_current")).root })).trusted, false);
    await shot(page, "research-created-light");
    step("B: research-only project created", { project: statusB.project, tree: await listTree(researchRoot) });

    // ---------------------------------------------------------------- C
    const paperBlueprint = await invoke(page, "project_preset_blueprint", {
      request: { preset: "paper", title: "Hostile", format: "typst", runtime: null, leanToolchain: null, includeWorkflow: false, purpose: null },
    });
    const traversal = { ...paperBlueprint, files: [...paperBlueprint.files, { path: "../escape.txt", content: "x" }] };
    const preview = await invoke(page, "project_blueprint_validate", { blueprint: JSON.stringify(traversal) });
    assert.equal(preview.valid, false);
    assert.ok(preview.diagnostics.some((d) => d.code === "blueprint.path" && /would leave the project folder/.test(d.message)));
    const hostileRoot = join(parent, "hostile");
    let refused = await invokeError(page, "project_create", { request: { destination: hostileRoot, blueprint: JSON.stringify(traversal) } });
    assert.equal(refused.error?.code, "blueprint.invalid");
    const trusting = { ...paperBlueprint, trust: { grant: true }, onOpen: "run" };
    refused = await invokeError(page, "project_create", { request: { destination: hostileRoot, blueprint: JSON.stringify(trusting) } });
    assert.equal(refused.error?.code, "blueprint.invalid");
    assert.match(refused.error.message, /unknown field `trust`/);
    assert.ok(!existsSync(hostileRoot), "nothing was created for an invalid blueprint");
    assert.ok(!existsSync(join(parent, "escape.txt")), "nothing outside the destination");
    const profile = process.env.USERPROFILE ?? process.env.HOME;
    const broad = await invoke(page, "project_check_destination", { destination: profile });
    assert.equal(broad.ok, false);
    assert.match(broad.message, /profile|users|Documents|Desktop/i);
    refused = await invokeError(page, "project_create", {
      request: { destination: join(dataDir, "inside-data"), blueprint: JSON.stringify(paperBlueprint) },
    });
    assert.equal(refused.error?.code, "workspace.tooBroad");
    assert.ok(!existsSync(join(dataDir, "inside-data")));
    const occupied = join(parent, "occupied");
    await mkdir(occupied);
    await writeFile(join(occupied, "main.typ"), "my own work");
    refused = await invokeError(page, "project_create", { request: { destination: occupied, blueprint: JSON.stringify(paperBlueprint) } });
    assert.equal(refused.error?.code, "project.notEmpty");
    assert.equal(await readFile(join(occupied, "main.typ"), "utf8"), "my own work");
    // The page shows a refused location and keeps Create disabled.
    await runCommand(page, "File: New Project", /File: ?New Project…$/);
    await view.waitFor();
    await page.getByTestId("project-location").locator("input").fill(profile);
    await page.waitForSelector('[data-testid="project-destination-status"][data-ok="false"]');
    assert.equal(await page.getByTestId("project-create").isDisabled(), true);
    await shot(page, "new-project-refused-location-light");
    step("C: invalid blueprints and destinations refused; nothing written");

    // ---------------------------------------------------------------- objective form screenshot
    await page.getByTestId("new-project-mode").getByText("Describe an objective").click();
    await page.getByTestId("objective-text").fill("Investigate whether the spectral gap of random regular graphs concentrates.");
    await shot(page, "new-project-objective-light");
    assert.deepEqual(consoleErrors(app), [], "no console errors so far");
    await app.close();

    // ---------------------------------------------------------------- D
    app = await launchApp({ dataDir, env: { ...env, EXPOSITION_TEST_INTERRUPT_PROJECT_CREATION: "2" } });
    const leanRoot = join(parent, "lean-investigation");
    {
      const { page } = app;
      const leanBlueprint = await invoke(page, "project_preset_blueprint", {
        request: { preset: "lean", title: "Lean investigation", format: null, runtime: null, leanToolchain: null, includeWorkflow: true, purpose: null },
      });
      assert.equal(leanBlueprint.files.length, 3);
      // Fire and forget: the app aborts after the second file (simulated crash).
      await page.evaluate(
        ([destination, blueprint]) => {
          void window.__TAURI_INTERNALS__.invoke("project_create", { request: { destination, blueprint } }).catch(() => {});
        },
        [leanRoot, JSON.stringify(leanBlueprint)],
      );
      await waitFor(() => app.child.exitCode !== null || app.child.signalCode !== null, { timeout: 30_000, message: "simulated crash" });
    }
    await app.close().catch(() => {});
    const afterCrash = await listTree(leanRoot);
    assert.equal(afterCrash.filter((p) => !p.endsWith("/") && !p.startsWith(".exposition")).length, 2, `two files before the crash: ${afterCrash}`);
    step("D1: app crashed after two of three files", { tree: afterCrash });

    app = await launchApp({ dataDir, env });
    {
      const { page } = app;
      await page.setViewportSize({ width: 1440, height: 900 });
      await setTheme(page, "light");
      await invoke(page, "workspace_open", { path: leanRoot });
      const notice = page.getByRole("button", { name: "Review" });
      await notice.waitFor({ timeout: 15_000 });
      const statuses = await invoke(page, "project_creation_status");
      assert.equal(statuses.length, 1);
      assert.equal(statuses[0].state.kind, "needsReconciliation", "interrupted, never complete");
      assert.equal(statuses[0].created, 2);
      assert.equal(statuses[0].missing, 1);
      assert.equal(statuses[0].graphApplied, false);
      const ops = await invoke(page, "graph_operations");
      assert.ok(ops.some((o) => o.kind === "projectCreation" && o.state.kind === "needsReconciliation"), "journal says so too");
      await notice.click();
      const setup = page.getByTestId("project-setup");
      await setup.waitFor();
      assert.equal(await setup.locator('[data-state="missing"]').count(), 1);
      assert.equal(await setup.locator('[data-state="created"]').count(), 2);
      await shot(page, "project-setup-recovery-light");
      await setTheme(page, "dark");
      await shot(page, "project-setup-recovery-dark");
      await setTheme(page, "light");
      await page.getByTestId("project-setup-finish").click();
      await waitFor(async () => (await invoke(page, "project_creation_status")).length === 0, { message: "setup resolved" });
      for (const f of ["lean-toolchain", "lakefile.toml", "Investigation.lean"]) assert.ok(existsSync(join(leanRoot, f)), f);
      assert.ok(!existsSync(join(leanRoot, "lakefile.lean")), "declarative lakefile only");
      const snapD = await entitiesOf(page, ["conjecture", "workflow", "collection"]);
      assert.ok(snapD.entities.some((e) => e.kind === "conjecture" && e.label === "Main conjecture"));
      assert.ok(snapD.entities.some((e) => e.kind === "workflow"), "workflow draft admitted");
      assert.equal(await graphPageMounted(page), false);
      step("D2: interrupted creation reported and finished explicitly", { entities: snapD.entities.map((e) => `${e.kind}:${e.label}`) });

      // -------------------------------------------------------------- E
      let drafts = await invoke(page, "workflow_list");
      const audit = drafts.find((d) => d.title === "Formal audit");
      assert.ok(audit, "the Lean preset's Formal audit draft");
      assert.equal(audit.state, "wellFormed");
      assert.equal(audit.runnable, false);
      await runCommand(page, "Workflow: New Numerical Experiment Draft", /New Numerical Experiment Draft/);
      const editor = page.getByTestId("workflow-editor");
      await editor.waitFor({ timeout: 15_000 });
      const wfStatus = page.getByTestId("workflow-status");
      await page.waitForSelector('[data-testid="workflow-status"][data-state="incomplete"]');
      assert.equal(await wfStatus.getAttribute("data-runnable"), "false");
      await waitFor(async () => /choose the project file/.test(await editor.textContent()), { message: "unset input diagnostic" });
      await page.getByTestId("input-path-script").locator("input").fill("analysis/missing.py");
      await waitFor(async () => /does not exist in the project/.test(await editor.textContent()), { message: "missing file diagnostic" });
      await page.getByTestId("workflow-save").click();
      await waitFor(async () => (await editor.getAttribute("data-revision")) === "2", { message: "revision 2 saved" });
      await shot(page, "workflow-draft-incomplete-light");
      await setTheme(page, "dark");
      await shot(page, "workflow-draft-incomplete-dark");
      await setTheme(page, "light");
      drafts = await invoke(page, "workflow_list");
      const numerical = drafts.find((d) => d.title === "Numerical experiment");
      assert.equal(numerical.state, "incomplete");
      assert.equal(numerical.runnable, false);
      assert.ok(numerical.errors > 0);
      const savedDraft = await invoke(page, "workflow_get", { id: numerical.id, revision: null });
      assert.equal(savedDraft.revisions.length, 2);
      assert.ok(savedDraft.validation.diagnostics.some((d) => d.code === "workflow.inputMissing" && d.rule === 1));
      const entity = await invoke(page, "graph_entity", { entity: savedDraft.entity });
      assert.equal(entity.entity.kind, "workflow");
      assert.equal(entity.entity.attributes["workflow.runnable"], "false");
      // Forged formal evidence: a native Python output wired into the evidence input.
      const auditDef = (await invoke(page, "workflow_get", { id: audit.id, revision: null })).definition;
      const forged = structuredClone(auditDef);
      forged.inputs.push({ id: "forger", label: "Forger", portType: "codeSource", source: { kind: "resource", path: "forge.py", version: { kind: "atRunStart" } } });
      forged.steps.push({ id: "forge", label: "Print JSON", operation: { id: "research.nativePython", version: 1 }, parameters: {}, payload: null });
      forged.connections = forged.connections.filter((c) => !(c.kind === "data" && c.to.step === "associate"));
      forged.connections.push({ kind: "data", from: { kind: "input", input: "forger" }, to: { step: "forge", port: "script" } });
      forged.connections.push({ kind: "data", from: { kind: "step", step: "forge", port: "dataset" }, to: { step: "associate", port: "evidence" } });
      const forgedResult = await invoke(page, "workflow_validate", { definition: forged });
      assert.ok(forgedResult.diagnostics.some((d) => d.code === "workflow.serviceOwnedType" && d.rule === 6), "forged evidence refused");
      assert.equal(forgedResult.runnable, false);
      // Imports: hostile refused, valid one never runnable.
      const hostile = await invokeError(page, "workflow_import", { text: JSON.stringify({ ...auditDef, trust: true, autoRun: true }) });
      assert.equal(hostile.error?.code, "workflow.invalid");
      const imported = await invoke(page, "workflow_import", { text: JSON.stringify(auditDef) });
      assert.equal(imported.imported, true);
      assert.equal(imported.validation.runnable, false);
      assert.notEqual(imported.id, audit.id, "fresh id for an import");
      assert.equal((await invoke(page, "trust_status", { root: (await invoke(page, "workspace_current")).root })).trusted, false);
      assert.deepEqual(await invoke(page, "research_sessions"), []);
      step("E: incomplete draft saved with diagnostics; nothing runnable; forged evidence and hostile import refused", {
        numerical: numerical.id,
        imported: imported.id,
      });

      // -------------------------------------------------------------- F
      await invoke(page, "agents_provider_update", {
        settings: { provider: "scripted", model: "claude-opus-5-5", effort: "medium", consent: "scopeContent" },
      });
      await invoke(page, "agents_dev_load_script", {
        script: {
          turns: [
            {
              steps: [
                { text: "I propose a research-only organization: a plan note and a research question." },
                { toolUse: { id: "toolu_plan", name: "create_file", input: { path: "notes/plan.md", content: "# Plan\n\nResearch question: does the gap concentrate?\n" } } },
              ],
            },
            { steps: [{ text: "Created notes/plan.md." }] },
          ],
        },
      });
      await runCommand(page, "File: New Project from an Objective", /New Project from an Objective…/);
      const objectiveView = page.getByTestId("new-project");
      await objectiveView.waitFor();
      await page.waitForSelector('[data-testid="new-project"][data-mode="objective"]');
      await page.getByTestId("objective-text").fill("Investigate whether the spectral gap of random regular graphs concentrates.");
      await page.getByTestId("project-title").locator("input").fill("Gap concentration");
      const objectiveRoot = join(parent, "gap-concentration");
      await page.getByTestId("project-location").locator("input").fill(objectiveRoot);
      await page.waitForSelector('[data-testid="project-destination-status"][data-ok="true"]');
      await selectOption(page, page.getByRole("button", { name: /Document changes/ }).locator(".."), "Apply changes, review afterwards");
      await page.getByTestId("objective-create").click();
      // The task is shown on the Agents page.
      await page.waitForFunction(() => document.querySelector('[data-page="agents"]')?.hidden === false, null, { timeout: 30_000 });
      await page.getByTestId("agents-page-task-detail").waitFor({ timeout: 30_000 });
      const tasks = await waitFor(
        async () => {
          const list = await invoke(page, "agents_task_list");
          const t = list.find((x) => x.status.kind === "completed" || x.status.kind === "failed");
          return t ? list : null;
        },
        { timeout: 30_000, message: "objective task finished" },
      );
      assert.equal(tasks.length, 1, "exactly one task");
      const task = tasks[0];
      const detailTask = await invoke(page, "agents_task_detail", { task: task.id });
      assert.equal(detailTask.summary.status.kind, "completed");
      const spec = detailTask.summary.spec;
      assert.deepEqual(spec.scope, { kind: "workspace" }, "whole-project scope");
      assert.equal(spec.execution, "disabled", "no commands by default");
      assert.equal(spec.permission, "autoApply", "the permission chosen on the page");
      assert.match(spec.prompt, /spectral gap of random regular graphs/);
      assert.match(spec.prompt, /research-only/, "a research-only organization is allowed");
      assert.match(spec.workspaceRoot.toLowerCase(), /gap-concentration$/);
      assert.equal((await invoke(page, "workspace_current")).name, "gap-concentration");
      assert.ok(existsSync(join(objectiveRoot, ".exposition", "graph.sqlite3")), "the empty project has its graph");
      assert.equal(await readFile(join(objectiveRoot, "notes", "plan.md"), "utf8"), "# Plan\n\nResearch question: does the gap concentrate?\n");
      assert.equal((await invoke(page, "trust_status", { root: (await invoke(page, "workspace_current")).root })).trusted, false);
      assert.equal(await graphPageMounted(page), false);
      // The previous project's workflow draft tab no longer edits it.
      await page.getByTestId("workflow-detached").waitFor({ state: "attached", timeout: 10_000 });
      await shot(page, "objective-task-light");
      step("F: objective path created an empty project and ran the agent task there", { task: task.id });
      assert.deepEqual(consoleErrors(app), [], "no console errors");
    }
  } finally {
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2)).catch(() => {});
    await app.close().catch(() => {});
  }
}
