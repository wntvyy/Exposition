// Open or create a project while one is open (QOL B), in the real app through
// visible controls: the page bar's project menu, the native Windows folder
// dialog (driven through UI Automation, scripts/e2e/native-dialog.ps1), the
// one switch decision, and the New Project page.
//
//  1. No folder → Open Folder… (native dialog) → gamma; clean switch to alpha
//     asks nothing; clean switch to beta asks nothing; the destination's
//     graph, research workspace, trust, Problems and build status are its own.
//  2. Unsaved work in beta: canceling the folder dialog changes nothing;
//     Cancel in the decision changes nothing.
//  3. Failed destination opening after Don't Save: the app's own data folder
//     (chosen in the native dialog) and a deleted Recent folder are refused;
//     beta stays open with its unsaved buffer intact.
//  4. Failed save (a read-only file) under Save All: the other file is saved,
//     the read-only one is reported as not saved, beta stays open.
//  5. Don't Save switches; reopening beta shows the saved work and no
//     recovery of the discarded buffer.
//  6. Running research code in beta (trusted): the decision says it stops;
//     Cancel keeps it running; Stop and Open switches; alpha is not trusted
//     and shows no beta session; reopening beta keeps its trust and run history.
//  7. New Project while alpha has unsaved work: location chosen with the
//     native dialog; a destination that stopped being empty after the check
//     is refused after Don't Save (alpha and its buffer stay); then Save
//     creates and opens the project and alpha's change is on disk.
// Screens: test-results/e2e/project-switch/ (and ui-reference/project-switch/
// when EXPOSITION_DEV_HOME is set). Windows only (native dialog driver).
import { chmod, copyFile, cp, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { withNativeDialog } from "../native-dialog.mjs";
import { runCommand } from "../palette.mjs";
import { focusEditor, invoke, openInEditor, waitFor } from "../workspace-helpers.mjs";

const OPEN_TITLE = "Open Folder";
const LOCATION_TITLE = "Choose where to create the project";

export default async function projectSwitch({ launchApp, artifactsDir, assert, fixture }) {
  if (process.platform !== "win32") {
    console.log("  skip project-switch: the native dialog driver needs Windows");
    return;
  }
  const base = await mkdtemp(join(tmpdir(), "xp-switch-"));
  const alpha = join(base, "alpha");
  const beta = join(base, "beta");
  const gamma = join(base, "gamma");
  const dataDir = join(base, "data");
  for (const root of [alpha, beta, gamma]) await cp(fixture("workspace-basic"), root, { recursive: true });
  await mkdir(dataDir, { recursive: true });
  // Where the New Project location dialog points (a short folder list).
  const newProjects = join(base, "new");
  await mkdir(newProjects, { recursive: true });

  const evidence = { base, steps: [], dialogs: [] };
  const note = (name, ok, detail = null) => {
    evidence.steps.push({ name, ok, detail });
    console.log(`  ${ok ? "ok " : "FAIL"} ${name}${detail === null ? "" : ` ${JSON.stringify(detail)}`}`);
    assert.ok(ok, `${name}: ${JSON.stringify(detail)}`);
  };
  const devHome = process.env.EXPOSITION_DEV_HOME;

  const app = await launchApp({ dataDir });
  const { page } = app;
  const pid = app.child.pid;
  const shot = async (name) => {
    // Let menus, dialogs and notifications finish their entrance transition.
    await page.waitForTimeout(400);
    const path = join(artifactsDir, `${name}.png`);
    await page.screenshot({ path });
    if (devHome) {
      const dir = join(devHome, "ui-reference", "project-switch");
      await mkdir(dir, { recursive: true });
      await copyFile(path, join(dir, `${name}.png`));
    }
  };

  const label = () => page.getByTestId("pagebar-project").innerText();
  const decision = page.getByTestId("project-switch-decision");
  const current = async () => (await invoke(page, "workspace_current"))?.root ?? null;
  const isAt = (root) => async () => {
    const now = await current();
    return now && now.toLowerCase() === root.toLowerCase() ? now : null;
  };
  const waitAt = async (root, name) => {
    await waitFor(isAt(root), { timeout: 20_000, message: `${name} open` });
    await waitFor(async () => (await label()).trim() === name, { timeout: 10_000, message: `page bar shows ${name}` });
    await page.locator('[data-testid="file-tree"] [data-path="main.typ"]').waitFor({ timeout: 15_000 });
  };
  const menuItem = async (name) => {
    await page.getByTestId("pagebar-project").click();
    const item = page.getByRole("menuitem", { name });
    await item.waitFor({ timeout: 5_000 });
    return item;
  };
  const openViaDialog = async (path) => {
    const report = await withNativeDialog({ title: OPEN_TITLE, path, pid }, async () => (await menuItem(/^Open Folder/)).click());
    evidence.dialogs.push(report);
    return report;
  };
  const openRecent = async (name) => {
    await (await menuItem(/^Open Recent/)).click();
    const item = page.getByRole("menu", { name: "Open Recent" }).getByRole("menuitem", { name: new RegExp(`^${name}`) });
    await item.waitFor({ timeout: 5_000 });
    await item.click();
  };
  const dirtyPaths = async () =>
    (await invoke(page, "document_list")).filter((d) => d.path && d.savedRevision !== d.revision).map((d) => d.path).sort();
  const docText = async (path) => {
    const doc = (await invoke(page, "document_list")).find((d) => d.path === path);
    return doc ? (await invoke(page, "document_state", { document: doc.id })).text : null;
  };
  const typeAtStart = async (path, text) => {
    await openInEditor(page, path);
    await focusEditor(page, path, "start");
    await page.keyboard.type(text);
    await waitFor(async () => (await dirtyPaths()).includes(path), { message: `${path} dirty` });
  };
  const notification = (text) => page.locator('[data-testid="notification"]').filter({ hasText: text });
  const dismissNotifications = async () => {
    for (const close of await page.locator('[data-testid="notification"] button[aria-label^="Dismiss"]').all()) {
      await close.click().catch(() => {});
    }
  };
  const destinationIsOwn = async (root, name) => {
    const graph = await waitFor(
      async () => {
        const status = await invoke(page, "graph_status");
        return status.root && status.root.toLowerCase().endsWith(name) ? status : null;
      },
      { timeout: 20_000, message: `graph of ${name}` },
    );
    const research = await invoke(page, "research_workspace");
    await page.getByRole("tab", { name: "Problems" }).click();
    await page.getByTestId("problems-panel").waitFor({ timeout: 5_000 });
    const problems = await page.locator('[data-testid="problem"]').count();
    const buildStatus = await page.locator('[data-testid="build-status"]').count();
    return { graphRoot: graph.root, graphState: graph.state, researchRoot: research?.root ?? null, trusted: research?.trusted ?? null, problems, buildStatus, root };
  };

  try {
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.getByTestId("workbench").waitFor({ timeout: 15_000 });

    // ---- 1. Open with nothing open; clean switches ask nothing ---------------------------
    const first = await openViaDialog(gamma);
    note("native-dialog-opened-gamma", first.found && first.closed, first);
    await waitAt(gamma, "gamma");
    await openViaDialog(alpha);
    await waitAt(alpha, "alpha");
    note("clean-switch-asks-nothing", (await decision.count()) === 0);
    // gamma is now only a Recent entry; delete it (a missing Recent folder later).
    await rm(gamma, { recursive: true, force: true });

    await page.getByTestId("pagebar-project").click();
    await page.getByRole("menuitem", { name: /^Open Folder/ }).waitFor();
    await shot("project-menu");
    await page.keyboard.press("Escape");

    await openViaDialog(beta);
    await waitAt(beta, "beta");
    note("clean-switch-to-beta", (await decision.count()) === 0);
    const betaStatus = await destinationIsOwn(beta, "beta");
    note(
      "destination-status-is-its-own",
      betaStatus.graphRoot.toLowerCase().endsWith("beta") &&
        betaStatus.researchRoot?.toLowerCase().endsWith("beta") &&
        betaStatus.trusted === false &&
        betaStatus.problems === 0 &&
        betaStatus.buildStatus === 0,
      betaStatus,
    );

    // ---- 2. Unsaved work: cancel the folder dialog, then the decision -----------------------
    await typeAtStart("main.typ", "UNSAVED-BETA ");
    const canceled = await withNativeDialog({ title: OPEN_TITLE, cancel: true, pid }, async () => (await menuItem(/^Open Folder/)).click());
    evidence.dialogs.push(canceled);
    await page.waitForTimeout(800);
    note(
      "folder-dialog-cancel-changes-nothing",
      canceled.closed && (await decision.count()) === 0 && Boolean(await isAt(beta)()) && (await dirtyPaths()).includes("main.typ"),
      canceled,
    );

    await openViaDialog(alpha);
    await decision.waitFor({ timeout: 10_000 });
    const title = await decision.getByRole("heading").innerText();
    const body = (await page.getByTestId("project-switch-body").innerText()).replace(/\s+/g, " ");
    note("decision-names-both-projects-and-the-work", title === "Close “beta” and open “alpha”" && /main\.typ has unsaved changes/.test(body), { title, body });
    await shot("decision-unsaved");
    await decision.getByRole("button", { name: "Cancel" }).click();
    await decision.waitFor({ state: "detached" });
    await page.waitForTimeout(500);
    note(
      "decision-cancel-changes-nothing",
      Boolean(await isAt(beta)()) && (await dirtyPaths()).includes("main.typ") && (await docText("main.typ")).startsWith("UNSAVED-BETA "),
    );

    // ---- 3. Failed destination after Don't Save --------------------------------------------
    await openViaDialog(dataDir);
    await decision.waitFor({ timeout: 10_000 });
    await decision.getByRole("button", { name: "Don't Save" }).click();
    const refused = notification("Could not open folder");
    await refused.waitFor({ timeout: 10_000 });
    const refusedText = (await refused.innerText()).replace(/\s+/g, " ");
    await shot("failed-destination");
    note(
      "data-folder-refused-beta-and-buffer-intact",
      Boolean(await isAt(beta)()) &&
        /never a project/.test(refusedText) &&
        /beta is still open; nothing was closed or discarded/.test(refusedText) &&
        (await docText("main.typ"))?.startsWith("UNSAVED-BETA ") &&
        (await dirtyPaths()).includes("main.typ"),
      refusedText,
    );
    await dismissNotifications();

    await openRecent("gamma");
    await decision.waitFor({ timeout: 10_000 });
    await decision.getByRole("button", { name: "Don't Save" }).click();
    const missing = notification("does not exist");
    await missing.waitFor({ timeout: 10_000 });
    note(
      "missing-recent-folder-refused-buffer-intact",
      Boolean(await isAt(beta)()) && (await docText("main.typ"))?.startsWith("UNSAVED-BETA ") && (await dirtyPaths()).includes("main.typ"),
      (await missing.innerText()).replace(/\s+/g, " "),
    );
    await dismissNotifications();

    // ---- 4. Save All with a read-only file -------------------------------------------------
    await typeAtStart("notes.md", "UNSAVED-NOTES ");
    const notesDisk = await readFile(join(beta, "notes.md"), "utf8");
    await chmod(join(beta, "notes.md"), 0o444);
    await openViaDialog(alpha);
    await decision.waitFor({ timeout: 10_000 });
    const saveAll = decision.getByRole("button", { name: "Save All" });
    note("decision-offers-save-all-for-two-files", (await saveAll.count()) === 1, (await page.getByTestId("project-switch-body").innerText()).replace(/\s+/g, " "));
    await saveAll.click();
    const partial = notification("Did not open alpha");
    await partial.waitFor({ timeout: 15_000 });
    const partialText = (await partial.innerText()).replace(/\s+/g, " ");
    await shot("failed-save");
    const mainDisk = await readFile(join(beta, "main.typ"), "utf8");
    note(
      "failed-save-reports-actual-state",
      Boolean(await isAt(beta)()) &&
        /Saved: main\.typ\. Not saved: notes\.md\. beta is still open\./.test(partialText) &&
        mainDisk.startsWith("UNSAVED-BETA ") &&
        (await readFile(join(beta, "notes.md"), "utf8")) === notesDisk &&
        JSON.stringify(await dirtyPaths()) === JSON.stringify(["notes.md"]),
      { partialText, dirty: await dirtyPaths() },
    );
    await dismissNotifications();

    // ---- 5. Don't Save switches; reopening beta shows saved work, nothing discarded comes back
    await openRecent("alpha");
    await decision.waitFor({ timeout: 10_000 });
    await decision.getByRole("button", { name: "Don't Save" }).click();
    await waitAt(alpha, "alpha");
    await chmod(join(beta, "notes.md"), 0o644);
    const alphaStatus = await destinationIsOwn(alpha, "alpha");
    note(
      "discard-switches-alpha-status-is-its-own",
      (await readFile(join(beta, "notes.md"), "utf8")) === notesDisk && alphaStatus.graphRoot.toLowerCase().endsWith("alpha") && alphaStatus.problems === 0,
      alphaStatus,
    );
    await openRecent("beta");
    await waitAt(beta, "beta");
    const reopened = await invoke(page, "workspace_current");
    await openInEditor(page, "main.typ");
    note(
      "reopened-beta-has-saved-work-and-no-discarded-recovery",
      reopened.recovery.length === 0 && (await docText("main.typ"))?.startsWith("UNSAVED-BETA ") && (await dirtyPaths()).length === 0,
      { recovery: reopened.recovery },
    );

    // ---- 6. Running research code stays with beta -----------------------------------------
    const workspace = await invoke(page, "research_workspace");
    await invoke(page, "trust_grant", { root: workspace.root });
    const report = await invoke(page, "research_environments", { refresh: true });
    const python = report.runtimes.find((r) => r.language === "python" && r.availability.kind === "available");
    if (!python) {
      note("research-running-work (skipped: no Python on this host)", true, report.runtimes.map((r) => r.availability.kind));
    } else {
      const session = await invoke(page, "research_session_start", { request: { environmentId: python.id, owner: { kind: "user" } } });
      const run = await invoke(page, "research_execute", {
        request: { sessionId: session.id, source: { kind: "input", code: "import time\nprint('started', flush=True)\ntime.sleep(120)" } },
      });
      await waitFor(
        async () => (await invoke(page, "research_sessions")).some((s) => s.id === session.id && s.activeRun === run.id),
        { timeout: 60_000, message: "research run active" },
      );
      await openViaDialog(alpha);
      await decision.waitFor({ timeout: 10_000 });
      const researchBody = (await page.getByTestId("project-switch-body").innerText()).replace(/\s+/g, " ");
      const stop = decision.getByRole("button", { name: "Stop and Open" });
      note("decision-says-running-research-stops", /Research code running in .+ stops\./.test(researchBody) && (await stop.count()) === 1, researchBody);
      await shot("decision-running-work");
      await decision.getByRole("button", { name: "Cancel" }).click();
      await decision.waitFor({ state: "detached" });
      note(
        "cancel-keeps-research-running",
        Boolean(await isAt(beta)()) && (await invoke(page, "research_sessions")).some((s) => s.id === session.id && s.activeRun === run.id),
      );
      await openViaDialog(alpha);
      await decision.waitFor({ timeout: 10_000 });
      await decision.getByRole("button", { name: "Stop and Open" }).click();
      await waitAt(alpha, "alpha");
      const alphaResearch = await invoke(page, "research_workspace");
      const alphaSessions = await invoke(page, "research_sessions");
      note(
        "no-trust-or-session-leaks-into-alpha",
        alphaResearch.trusted === false && !alphaSessions.some((s) => s.id === session.id && s.state.kind !== "detached" && s.state.kind !== "stopped"),
        { trusted: alphaResearch.trusted, sessions: alphaSessions.map((s) => ({ id: s.id, state: s.state.kind })) },
      );
      await openRecent("beta");
      await waitAt(beta, "beta");
      const betaResearch = await invoke(page, "research_workspace");
      const record = await waitFor(
        async () => {
          const runs = await invoke(page, "research_runs");
          const found = runs.find((r) => r.id === run.id);
          return found && found.status.kind !== "running" ? found : null;
        },
        { timeout: 30_000, message: "beta's run settled" },
      );
      note("beta-keeps-trust-and-its-run-history", betaResearch.trusted === true && record.status.kind !== "succeeded", {
        trusted: betaResearch.trusted,
        run: record.status,
      });
      await openRecent("alpha");
      await waitAt(alpha, "alpha");
    }

    // ---- 7. New Project while alpha has unsaved work ---------------------------------------
    if (!(await isAt(alpha)())) {
      await openRecent("alpha");
      await waitAt(alpha, "alpha");
    }
    await typeAtStart("main.typ", "UNSAVED-ALPHA ");
    await (await menuItem(/^New Project/)).click();
    await page.getByTestId("new-project").waitFor({ timeout: 15_000 });
    await page.getByTestId("project-title").locator("input").fill("Delta");
    const browse = await withNativeDialog({ title: LOCATION_TITLE, path: newProjects, pid }, async () =>
      page.getByRole("button", { name: /^Browse/ }).click(),
    );
    evidence.dialogs.push(browse);
    const delta = join(newProjects, "delta");
    await waitFor(
      async () => (await page.getByTestId("project-location").locator("input").inputValue()).toLowerCase() === delta.toLowerCase(),
      { message: "location from the native dialog" },
    );
    note("native-location-dialog-sets-location", browse.closed, browse);
    await page.waitForSelector('[data-testid="project-destination-status"][data-ok="true"]', { timeout: 20_000 });
    await page.waitForSelector('[data-testid="project-preview"][data-valid="true"]', { timeout: 20_000 });
    // The destination stops being empty after the form checked it.
    await mkdir(delta, { recursive: true });
    await writeFile(join(delta, "occupied.txt"), "someone else's file\n");
    await page.getByTestId("project-create").click();
    await decision.waitFor({ timeout: 10_000 });
    const createTitle = await decision.getByRole("heading").innerText();
    await shot("decision-create");
    await decision.getByRole("button", { name: "Don't Save" }).click();
    const notCreated = notification("The project was not created");
    await notCreated.waitFor({ timeout: 20_000 });
    const notCreatedText = (await notCreated.innerText()).replace(/\s+/g, " ");
    note(
      "refused-create-keeps-alpha-and-its-buffer",
      createTitle === "Close “alpha” and create “Delta”" &&
        /alpha is still open; nothing was closed or discarded/.test(notCreatedText) &&
        Boolean(await isAt(alpha)()) &&
        (await docText("main.typ"))?.startsWith("UNSAVED-ALPHA ") &&
        (await dirtyPaths()).includes("main.typ") &&
        (await page.getByTestId("new-project").count()) === 1,
      { createTitle, notCreatedText },
    );
    await dismissNotifications();

    await rm(join(delta, "occupied.txt"));
    // Edit the location so the form checks it again (now empty).
    await page.getByTestId("project-location").locator("input").fill("");
    await page.getByTestId("project-location").locator("input").fill(delta);
    await page.waitForSelector('[data-testid="project-destination-status"][data-ok="true"]', { timeout: 20_000 });
    await page.getByTestId("project-create").click();
    await decision.waitFor({ timeout: 10_000 });
    await decision.getByRole("button", { name: "Save", exact: true }).click();
    await waitFor(isAt(delta), { timeout: 30_000, message: "delta open" });
    await waitFor(async () => (await label()).trim() === "delta", { timeout: 10_000, message: "page bar shows delta" });
    const alphaDisk = await readFile(join(alpha, "main.typ"), "utf8");
    note(
      "create-after-save-opens-the-new-project",
      alphaDisk.startsWith("UNSAVED-ALPHA ") && (await page.getByTestId("new-project").count()) === 0,
      { alphaDisk: alphaDisk.slice(0, 40) },
    );
    const deltaStatus = await destinationIsOwn(delta, "delta");
    note("new-project-status-is-its-own", deltaStatus.graphRoot.toLowerCase().endsWith("delta") && deltaStatus.trusted === false, deltaStatus);
    await shot("created-delta");

    // Dark theme reference of the decision.
    await openRecent("alpha");
    await waitAt(alpha, "alpha");
    await typeAtStart("main.typ", "DARK ");
    await runCommand(page, "color theme dark", /Color Theme: Dark/);
    await openRecent("beta");
    await decision.waitFor({ timeout: 10_000 });
    await shot("decision-unsaved-dark");
    await decision.getByRole("button", { name: "Cancel" }).click();
    await runCommand(page, "color theme light", /Color Theme: Light/);

    const errors = app.consoleMessages.filter((m) => m.startsWith("error:") || m.startsWith("pageerror:"));
    note("no-console-errors", errors.length === 0, errors.slice(0, 5));
  } finally {
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2));
    await app.close();
    await chmod(join(beta, "notes.md"), 0o644).catch(() => {});
  }
}
