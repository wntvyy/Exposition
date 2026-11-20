// Mathlib setup, the part that needs no network: a package that requires
// Mathlib but was never prepared (the tiny tests/fixtures/lean-mathlib).
//
// - Opening one of its Lean files in a trusted folder starts no Lean server,
//   downloads nothing, and creates no `.lake` or manifest: the infoview says
//   why and offers Set Up Mathlib.
// - The dialog shows the whole plan before anything runs (package folder,
//   pinned Lean, Mathlib revision, files kept, every command).
// - A new package is refused inside or around an existing package and is
//   planned beside it; a tampered plan and an untrusted folder are refused.
// - Nothing in the project changed, and standalone Lean in the same project
//   still checks.
//
// Nothing is started, so nothing is downloaded. The full path (downloads,
// several GB) is `mathlib-setup` and `mathlib`, both opt-in.
//
// Requires a debug build: it calls the debug-only probe commands lean_check,
// lean_package_inspect.
import { existsSync } from "node:fs";
import { cp, mkdir, mkdtemp, readFile, readdir, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";
import { invoke, invokeError, openInEditor, waitFor } from "../workspace-helpers.mjs";

const TOOLCHAIN_DIR = "leanprover--lean4---v4.35.0";
const THEOREM = "formal/OddSums/Basic.lean";
const SCRATCH = "notes/Scratch.lean";

async function tree(dir, prefix = "") {
  const out = [];
  for (const entry of (await readdir(dir, { withFileTypes: true })).sort((a, b) => a.name.localeCompare(b.name))) {
    const rel = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.name === ".exposition") continue;
    if (entry.isDirectory()) out.push(`${rel}/`, ...(await tree(join(dir, entry.name), rel)));
    else out.push(`${rel}:${(await readFile(join(dir, entry.name))).toString("base64")}`);
  }
  return out;
}

export default async function mathlibPlan({ launchApp, artifactsDir, assert, fixture }) {
  const elanHome = process.env.ELAN_HOME || join(homedir(), ".elan");
  if (!existsSync(join(elanHome, "toolchains", TOOLCHAIN_DIR, "bin"))) {
    const message = `SKIPPED: Lean toolchain ${TOOLCHAIN_DIR} is not installed through elan`;
    if (process.env.EXPOSITION_REQUIRE_LEAN === "1") throw new Error(message);
    console.log(message);
    return;
  }
  const base = await mkdtemp(join(tmpdir(), "xp-mathlib-plan-"));
  const root = join(base, "project");
  const dataDir = join(base, "data");
  await mkdir(join(root, "notes"), { recursive: true });
  await mkdir(dataDir, { recursive: true });
  await cp(fixture("lean-mathlib"), join(root, "formal"), { recursive: true });
  await writeFile(join(root, "paper.typ"), "= Odd sums\n\nThe sum of the first $n$ odd numbers is $n^2$.\n");
  await writeFile(join(root, "notes", "Scratch.lean"), "theorem scratch : 1 + 1 = 2 := rfl\n");
  const before = await tree(root);

  const observations = [];
  const note = (id, ok, detail) => {
    observations.push({ id, ok: !!ok, detail });
    console.log(`${ok ? "ok    " : "FAIL  "} ${id}: ${typeof detail === "string" ? detail : JSON.stringify(detail)?.slice(0, 700)}`);
  };
  const app = await launchApp({ dataDir });
  const { page } = app;
  const shot = (name) => page.screenshot({ path: join(artifactsDir, `${name}.png`) });
  try {
    await page.setViewportSize({ width: 1440, height: 1000 }).catch(() => {});
    const info = await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="paper.typ"]').waitFor({ timeout: 15_000 });
    await invoke(page, "trust_grant", { root: info.root });
    await runCommand(page, "show lean infoview", /Show Lean Infoview/);

    // ---- detection (files only) ---------------------------------------------
    const inspection = await invoke(page, "lean_package_inspect", { folder: "formal" });
    note(
      "unprepared-package-detected",
      inspection.readiness.kind === "dependenciesUnresolved" &&
        inspection.lakefile === "lakefile.toml" &&
        inspection.toolchain === "leanprover/lean4:v4.35.0" &&
        inspection.toolchainInstalled &&
        inspection.usesMathlib &&
        inspection.mathlibInputRevision === "v4.35.0" &&
        inspection.manifestDigest === null,
      inspection,
    );
    const rootInspection = await invoke(page, "lean_package_inspect", { folder: "" });
    note("project-folder-is-not-a-package", rootInspection.readiness.kind === "notAPackage" && rootInspection.lakefile === null, rootInspection.readiness);

    // ---- the editor does not start Lake on an unprepared Mathlib package ------
    const doc = await openInEditor(page, THEOREM);
    const panel = page.getByTestId("lean-panel");
    await waitFor(async () => (await panel.getAttribute("data-status")) === "unavailable", { timeout: 30_000, message: "the file is held back" });
    const preparation = page.getByTestId("lean-package-preparation");
    await preparation.waitFor({ timeout: 15_000 });
    const preparationText = await preparation.innerText();
    note(
      "infoview-names-the-supported-preparation",
      (await preparation.getAttribute("data-readiness")) === "dependenciesUnresolved" &&
        (await preparation.getAttribute("data-folder")) === "formal" &&
        /have not been resolved/.test(preparationText) &&
        /Set Up Mathlib/.test(preparationText),
      preparationText,
    );
    await page.waitForTimeout(4000);
    const jobs = await invoke(page, "job_list");
    const projects = await invoke(page, "lean_projects");
    const formal = projects.find((p) => p.path === "formal");
    note(
      "no-server-no-download-no-files",
      !existsSync(join(root, "formal", ".lake")) &&
        !existsSync(join(root, "formal", "lake-manifest.json")) &&
        formal?.server.kind === "stopped" &&
        !jobs.some((j) => /lake serve/.test(j.info?.spec?.label ?? j.spec?.label ?? JSON.stringify(j))),
      { server: formal?.server, jobs: jobs.length },
    );
    const check = await invoke(page, "lean_check", { request: { target: { kind: "path", path: THEOREM }, goalsAt: [], timeoutMs: 20_000 } });
    note("a-check-of-an-unprepared-package-is-not-checked", check.checked === false && check.status.kind === "unavailable" && /Set Up Mathlib/.test(check.status.reason), check.status);
    await shot("mathlib-unprepared-infoview-light");

    // ---- the plan, before anything runs ---------------------------------------
    await page.getByTestId("lean-prepare-open").click();
    const plan = page.getByTestId("lean-setup-plan");
    await waitFor(async () => (await plan.getAttribute("data-state")) === "ready", { timeout: 20_000, message: "the prepare plan" });
    const summary = await page.getByTestId("lean-setup-summary").innerText();
    await page.getByRole("button", { name: /Commands/ }).click();
    const commands = await page.getByTestId("lean-setup-commands").innerText();
    note(
      "prepare-plan-shows-root-toolchain-revision-and-what-stays",
      summary.includes(join(info.root, "formal")) &&
        /leanprover\/lean4:v4\.35\.0/.test(summary) &&
        /installed/.test(summary) &&
        /default toolchain is not changed/.test(summary) &&
        /v4\.35\.0/.test(summary) &&
        /resolved once into lake-manifest\.json/.test(summary) &&
        /Keeps unchanged[\s\S]*lakefile\.toml[\s\S]*lean-toolchain/.test(summary) &&
        /your account.s privileges/.test(summary),
      summary,
    );
    note(
      "prepare-plan-lists-every-command",
      /MATHLIB_NO_CACHE_ON_UPDATE=1 .*lake(\.exe)? --keep-toolchain update/.test(commands) &&
        /lake(\.exe)? exe cache get/.test(commands) &&
        /lake(\.exe)? build --no-build Mathlib/.test(commands) &&
        commands.includes(TOOLCHAIN_DIR) &&
        !/elan(\.exe)? default/.test(commands),
      commands,
    );
    // Where the dependencies come from: the lakefile's one requirement, and
    // what is decided only when Lake resolves it.
    const sourcesText = await page.getByTestId("lean-setup-sources").innerText();
    const sourcesNote = await page.getByTestId("lean-setup-sources-note").innerText();
    note(
      "prepare-plan-names-where-the-dependencies-come-from",
      /mathlib/.test(sourcesText) &&
        /leanprover-community\/mathlib/.test(sourcesText) &&
        /v4\.35\.0/.test(sourcesText) &&
        /Resolved by Lake/.test(sourcesText) &&
        /stops before the cache step unless `mathlib` then comes from https:\/\/github\.com\/leanprover-community\/mathlib4/.test(sourcesNote) &&
        /from the addresses listed under Sources/.test(summary),
      { sourcesText, sourcesNote },
    );
    await shot("mathlib-setup-plan-existing-light");
    if (process.env.EXPOSITION_DEV_HOME) {
      const dir = join(process.env.EXPOSITION_DEV_HOME, "ui-reference", "research");
      await mkdir(dir, { recursive: true });
      await cp(join(artifactsDir, "mathlib-unprepared-infoview-light.png"), join(dir, "mathlib-unprepared-infoview-light.png"));
      await cp(join(artifactsDir, "mathlib-setup-plan-existing-light.png"), join(dir, "mathlib-setup-plan-existing-light.png"));
    }

    // ---- a new package: refused inside or around a package, planned beside it --
    const dialog = page.getByRole("dialog", { name: "Set up Mathlib" });
    await dialog.getByRole("button", { name: /Package/ }).click();
    await page.getByRole("option", { name: "New package…", exact: true }).click();
    const folderField = dialog.getByRole("textbox", { name: "Folder" });
    const nameField = dialog.getByRole("textbox", { name: "Library name" });
    const start = page.getByTestId("lean-setup-start");
    await folderField.fill("formal/sub");
    await waitFor(async () => /inside the Lake package/.test(await page.getByTestId("lean-setup-folder-field").innerText()), { message: "nested package refused" });
    note("new-package-inside-a-package-refused", await start.isDisabled(), await page.getByTestId("lean-setup-folder-field").innerText());
    await folderField.fill("");
    await waitFor(async () => /contains the Lake package/.test(await page.getByTestId("lean-setup-folder-field").innerText()), { message: "enclosing package refused" });
    note("new-package-around-a-package-refused", await start.isDisabled(), await page.getByTestId("lean-setup-folder-field").innerText());
    await folderField.fill("formal");
    await waitFor(async () => /already is a Lake package/.test(await page.getByTestId("lean-setup-folder-field").innerText()), { message: "existing package not overwritten" });
    note("existing-package-not-overwritten", await start.isDisabled(), await page.getByTestId("lean-setup-folder-field").innerText());
    await folderField.fill("lean");
    await nameField.fill("odd sums");
    await waitFor(async () => /is not a library name/.test(await page.getByTestId("lean-setup-name-field").innerText()), { message: "invalid name refused" });
    await nameField.fill("OddSums");
    await waitFor(async () => (await plan.getAttribute("data-state")) === "ready", { timeout: 20_000, message: "the new-package plan" });
    const newSummary = await page.getByTestId("lean-setup-summary").innerText();
    note(
      "new-package-plan-pins-release-and-matching-mathlib",
      newSummary.includes(join(info.root, "lean")) &&
        /a new Lake package/.test(newSummary) &&
        /leanprover\/lean4:v\d+\.\d+\.\d+/.test(newSummary) &&
        /the release made for this Lean version/.test(newSummary) &&
        /Creates[\s\S]*lakefile\.toml[\s\S]*OddSums\/Basic\.lean/.test(newSummary) &&
        /The new package's files are removed/.test(newSummary) &&
        (await start.innerText()).includes("Create package"),
      newSummary,
    );
    await shot("mathlib-setup-plan-new-light");
    if (process.env.EXPOSITION_DEV_HOME) {
      await cp(join(artifactsDir, "mathlib-setup-plan-new-light.png"), join(process.env.EXPOSITION_DEV_HOME, "ui-reference", "research", "mathlib-setup-plan-new-light.png"));
    }
    await dialog.getByRole("button", { name: "Cancel", exact: true }).click();
    await dialog.waitFor({ state: "detached", timeout: 10_000 });

    // ---- the start is bound to the reviewed plan and to trust -------------------
    const reviewed = await invoke(page, "lean_setup_plan", { request: { kind: "prepareMathlibPackage", folder: "formal" } });
    const tampered = structuredClone(reviewed);
    tampered.steps[0].args = ["update"];
    const stale = await invokeError(page, "lean_setup_start", { plan: tampered });
    note("a-changed-plan-is-refused", stale.error?.code === "lean.setup.planStale", stale.error ?? stale);
    const elsewhere = structuredClone(reviewed);
    elsewhere.packageRoot = base;
    const moved = await invokeError(page, "lean_setup_start", { plan: elsewhere });
    note("a-plan-for-another-folder-is-refused", moved.error?.code === "lean.setup.planStale", moved.error ?? moved);
    const outside = await invokeError(page, "lean_setup_plan", { request: { kind: "newMathlibPackage", folder: "../outside", name: "Formal" } });
    note("a-folder-outside-the-project-is-refused", outside.error?.code === "lean.setup.invalidFolder", outside.error ?? outside);
    await invoke(page, "trust_revoke", { root: info.root });
    const untrusted = await invokeError(page, "lean_setup_start", { plan: reviewed });
    note("an-untrusted-folder-runs-no-setup", untrusted.error?.code === "lean.untrusted", untrusted.error ?? untrusted);
    note("no-setup-ran", (await invoke(page, "lean_setup_status")) === null, await invoke(page, "lean_setup_status"));
    await invoke(page, "trust_grant", { root: info.root });

    // ---- nothing changed; standalone Lean in the same project still checks -----
    const after = await tree(root);
    note("project-unchanged-by-planning", JSON.stringify(after) === JSON.stringify(before), { before: before.length, after: after.length });
    await openInEditor(page, SCRATCH);
    await waitFor(async () => (await panel.getAttribute("data-status")) === "checked", { timeout: 300_000, interval: 250, message: "the standalone file is checked" });
    const states = await invoke(page, "lean_documents");
    const scratch = states.find((d) => d.path === SCRATCH);
    const held = states.find((d) => d.document === doc);
    note("standalone-lean-still-checks", scratch?.status.kind === "checked" && held?.status.kind === "unavailable", { scratch: scratch?.status.kind, theorem: held?.status.kind });

    const errors = app.consoleMessages.filter((m) => m.startsWith("error") || m.startsWith("pageerror"));
    note("no-console-errors", errors.length === 0, errors.slice(0, 5));
  } finally {
    await writeFile(join(artifactsDir, "observations.json"), JSON.stringify(observations, null, 2));
    await app.close();
  }
  const failed = observations.filter((o) => !o.ok);
  assert.deepEqual(failed.map((d) => d.id), [], `violations (see ${join(artifactsDir, "observations.json")})`);
}
